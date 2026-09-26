use crate::gateway::diagnostics::{
    ErrorDiagnostic, REQUEST_ID_HEADER, RequestTrace, emit_failure, log_request_failure,
    serialize_diagnostic,
};
use crate::gateway::executor::GatewayExecutor;
use crate::gateway::forwarder::UpstreamPayloadTooLargeResponse;
use crate::gateway::materialize::{mapping_adapter_kind, mapping_is_custom_http_catalog};
use crate::gateway::protocol::{ProtocolError, parse_client_request, parse_gemini_request};
use crate::gateway::response::{
    local_protocol_failure, protocol_error_from, protocol_error_response,
};
use crate::kernel::protocol::ApiFormat;
use crate::provider::ProviderAdapterKind;
use crate::state::CoreState;
use axum::body::{Body, Bytes};
use axum::extract::{Extension, Path, State};
use axum::http::{HeaderMap, HeaderValue, Request, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};

pub async fn request_trace_middleware(
    State(state): State<CoreState>,
    mut request: Request<Body>,
    next: Next,
) -> Response {
    let credential = extract_client_key(request.headers(), &state);
    let trace = credential
        .as_ref()
        .map(|entry| RequestTrace::new().with_client_key(entry.id.clone(), entry.name.clone()))
        .unwrap_or_default();
    let path = request.uri().path().to_string();
    let client_body_bytes = request
        .headers()
        .get(axum::http::header::CONTENT_LENGTH)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<usize>().ok());
    let authenticated = credential.is_some();
    request.extensions_mut().insert(trace.clone());
    let mut response = next.run(request).await;

    if response.status() == StatusCode::PAYLOAD_TOO_LARGE
        && authenticated
        && response
            .extensions()
            .get::<UpstreamPayloadTooLargeResponse>()
            .is_none()
    {
        let mut diagnostic = ErrorDiagnostic::new(
            &trace,
            1,
            "client",
            "body_limit",
            client_format_for_path(&path),
        );
        diagnostic.client_body_bytes = client_body_bytes;
        diagnostic.downstream_status = Some(StatusCode::PAYLOAD_TOO_LARGE.as_u16());
        let encoded = serialize_diagnostic(diagnostic.clone());
        log_request_failure(
            &state.db.lock(),
            &trace,
            &diagnostic,
            &encoded,
            "gateway request body exceeded the configured limit",
        );
        emit_failure(&encoded);
    }

    response.headers_mut().insert(
        REQUEST_ID_HEADER,
        HeaderValue::from_str(&trace.request_id)
            .expect("generated request id must be a valid header value"),
    );
    response
}

fn client_format_for_path(path: &str) -> ApiFormat {
    if path.ends_with("/responses") {
        ApiFormat::Responses
    } else if path.ends_with("/messages") {
        ApiFormat::Messages
    } else if path.starts_with("/v1beta/models/")
        || (path.starts_with("/v1/models/") && path.contains(':'))
    {
        ApiFormat::Gemini
    } else {
        ApiFormat::ChatCompletions
    }
}

pub async fn chat_completions(
    State(state): State<CoreState>,
    Extension(trace): Extension<RequestTrace>,
    headers: HeaderMap,
    body: Bytes,
) -> axum::response::Response {
    proxy_handler(state, trace, headers, body, ApiFormat::ChatCompletions).await
}

pub async fn responses(
    State(state): State<CoreState>,
    Extension(trace): Extension<RequestTrace>,
    headers: HeaderMap,
    body: Bytes,
) -> axum::response::Response {
    proxy_handler(state, trace, headers, body, ApiFormat::Responses).await
}

pub async fn messages(
    State(state): State<CoreState>,
    Extension(trace): Extension<RequestTrace>,
    headers: HeaderMap,
    body: Bytes,
) -> axum::response::Response {
    proxy_handler(state, trace, headers, body, ApiFormat::Messages).await
}

pub async fn gemini_model_action(
    State(state): State<CoreState>,
    Extension(trace): Extension<RequestTrace>,
    Path(model_action): Path<String>,
    headers: HeaderMap,
    body: Bytes,
) -> axum::response::Response {
    let client_body_bytes = body.len();
    let Some((model, action)) = model_action.rsplit_once(':') else {
        return gemini_error(
            &state,
            &trace,
            &headers,
            StatusCode::NOT_FOUND,
            "Gemini model action is required",
            Some(client_body_bytes),
        );
    };
    if model.is_empty() {
        return gemini_error(
            &state,
            &trace,
            &headers,
            StatusCode::BAD_REQUEST,
            "Gemini model is required",
            Some(client_body_bytes),
        );
    }
    match action {
        "generateContent" => {
            gemini_proxy_handler(state, trace, headers, body, model.to_string(), false).await
        }
        "streamGenerateContent" => {
            gemini_proxy_handler(state, trace, headers, body, model.to_string(), true).await
        }
        "countTokens" => gemini_expected_fallback(
            &state,
            &headers,
            StatusCode::NOT_IMPLEMENTED,
            "Gemini countTokens is not available; Gemini CLI falls back to local estimation",
        ),
        "embedContent" => gemini_error(
            &state,
            &trace,
            &headers,
            StatusCode::NOT_IMPLEMENTED,
            "Gemini embeddings are not supported by this gateway",
            Some(client_body_bytes),
        ),
        _ => gemini_error(
            &state,
            &trace,
            &headers,
            StatusCode::NOT_FOUND,
            "unknown Gemini model action",
            Some(client_body_bytes),
        ),
    }
}

/// GET /v1/models: authenticated, local public model inventory.
///
/// Includes curated aliases, exact Go catalog pins, and eligible Custom,
/// CPA and dynamic names. Protocol and publication switches still apply.
/// Catalog discovery never creates arbitrary shared aliases. No upstream I/O.
pub async fn models(
    State(state): State<CoreState>,
    headers: HeaderMap,
) -> axum::response::Response {
    if !check_auth(&headers, &state) {
        return protocol_error_response(
            ApiFormat::ChatCompletions,
            StatusCode::UNAUTHORIZED,
            "invalid gateway key",
            None,
        );
    }
    published_alias_models_response(&state)
}

fn published_alias_models_response(state: &CoreState) -> axum::response::Response {
    let _settings_update = state.settings_update.lock();
    let snapshot = match runtime_catalog_snapshot(state) {
        Ok(snapshot) => snapshot,
        Err(error) => {
            return protocol_error_response(
                ApiFormat::ChatCompletions,
                StatusCode::INTERNAL_SERVER_ERROR,
                &format!("failed to load routing configuration: {error}"),
                None,
            );
        }
    };
    let catalogs = snapshot.catalogs();
    let custom_ids = &snapshot.custom;
    let cpa_ids = &snapshot.cpa;
    let unpublished = state.unpublished_public_models();
    let published = crate::alias::published_routeable_models_with_runtime_catalogs(catalogs);
    let mut data: Vec<serde_json::Value> = published
        .iter()
        .filter(|item| {
            crate::alias_publication::is_downstream_visible(&item.alias, &unpublished)
                && snapshot.model_has_enabled_protocol(&item.alias)
        })
        .map(|item| {
            serde_json::json!({
                "id": item.alias,
                "object": "model",
                "created": 0,
                "owned_by": snapshot.output_provider_id(&item.owned_by)
            })
        })
        .collect();
    for id in custom_ids {
        let routeable_custom_alias = matches!(
            snapshot.resolve(id),
            Ok(crate::alias::ResolvedModel::Alias { mappings, .. })
                if mappings.iter().any(|mapping| {
                    mapping.routeable && mapping_is_custom_http_catalog(mapping)
                })
        );
        if !routeable_custom_alias {
            continue;
        }
        if !crate::alias_publication::is_downstream_visible(id, &unpublished) {
            continue;
        }
        if data.iter().any(|item| {
            item.get("id")
                .and_then(|value| value.as_str())
                .is_some_and(|existing| crate::custom::custom_model_id_matches(existing, id))
        }) {
            continue;
        }
        data.push(serde_json::json!({
            "id": id,
            "object": "model",
            "created": 0,
            "owned_by": crate::provider::CUSTOM_PROVIDER_ID
        }));
    }
    for id in cpa_ids.iter() {
        let exact_cpa_raw = matches!(
            snapshot.resolve(id),
            Ok(crate::alias::ResolvedModel::PinnedRaw { mapping, .. })
                if mapping.routeable
                    && mapping_adapter_kind(&mapping) == Some(ProviderAdapterKind::Cpa)
        );
        if exact_cpa_raw
            && crate::alias_publication::is_downstream_visible(id, &unpublished)
            && !data
                .iter()
                .any(|item| item.get("id").and_then(|value| value.as_str()) == Some(id))
        {
            data.push(serde_json::json!({
                "id": id,
                "object": "model",
                "created": 0,
                "owned_by": crate::provider::CPA_PROVIDER_ID
            }));
        }
    }
    for catalog in &snapshot.extra {
        for (public_model, _upstream_model) in &catalog.mappings {
            if published_model_ids_contain(&data, public_model) {
                continue;
            }
            if !crate::alias_publication::is_downstream_visible(public_model, &unpublished) {
                continue;
            }
            if !snapshot.model_has_enabled_protocol(public_model) {
                continue;
            }
            data.push(serde_json::json!({
                "id": public_model,
                "object": "model",
                "created": 0,
                "owned_by": snapshot.output_provider_id(&catalog.provider_id)
            }));
        }
    }
    if crate::model_metadata::enrich(&state.db.lock(), &snapshot, &mut data).is_err() {
        return protocol_error_response(
            ApiFormat::ChatCompletions,
            StatusCode::INTERNAL_SERVER_ERROR,
            "failed to load model metadata",
            None,
        );
    }
    axum::Json(serde_json::json!({
        "object": "list",
        "data": data
    }))
    .into_response()
}

fn published_model_ids_contain(data: &[serde_json::Value], id: &str) -> bool {
    data.iter().any(|item| {
        item.get("id")
            .and_then(|value| value.as_str())
            .is_some_and(|existing| crate::custom::custom_model_id_matches(existing, id))
    })
}

/// Owned runtime catalog inputs used by live send and the read-only explain
/// path. Callers borrow [`Self::catalogs`] for alias resolution.
pub(crate) struct RuntimeCatalogSnapshot {
    pub routing: crate::routing_snapshot::RoutingSnapshot,
    pub go: Vec<String>,
    pub zen_free: Vec<String>,
    pub custom: Vec<String>,
    pub command_code: Vec<String>,
    pub minimax: Vec<String>,
    pub kimi: Vec<String>,
    pub cpa: Vec<String>,
    pub ollama: Vec<String>,
    pub ollama_pinned: Vec<String>,
    pub extra: Vec<crate::alias::ExtraProviderCatalog>,
}

impl RuntimeCatalogSnapshot {
    pub(crate) fn catalogs(&self) -> crate::alias::RuntimeCatalogs<'_> {
        crate::alias::RuntimeCatalogs {
            go: &self.go,
            zen_free: &self.zen_free,
            custom: &self.custom,
            command_code: &self.command_code,
            minimax: &self.minimax,
            kimi: &self.kimi,
            cpa: &self.cpa,
            ollama: &self.ollama,
            ollama_pinned: &self.ollama_pinned,
            extra: &self.extra,
        }
    }
}

pub(crate) fn runtime_catalog_snapshot(
    state: &CoreState,
) -> anyhow::Result<RuntimeCatalogSnapshot> {
    let mut routing = crate::routing_snapshot::RoutingSnapshot::load(&state.db.lock())?;
    {
        let probes = state.quota_probes.lock();
        routing.apply_quota_probes(&probes);
    }
    Ok(RuntimeCatalogSnapshot::from_routing(
        routing,
        state.sample_gateway_clock().0,
    ))
}

impl RuntimeCatalogSnapshot {
    pub(crate) fn from_routing(
        routing: crate::routing_snapshot::RoutingSnapshot,
        wall: chrono::DateTime<chrono::Utc>,
    ) -> Self {
        use ocg_domain::destination::{AdapterKind, AuthScheme, ModelResolution};
        let mut result = Self {
            go: vec![],
            zen_free: vec![],
            custom: vec![],
            command_code: vec![],
            minimax: vec![],
            kimi: vec![],
            cpa: vec![],
            ollama: vec![],
            ollama_pinned: vec![],
            extra: vec![],
            routing,
        };
        for d in &result.routing.projection.destinations {
            let active = d.enabled
                && result.routing.credentials.iter().any(|c| {
                    c.destination_id == d.id
                        && c.enabled
                        && c.ready
                        && c.binding_enabled
                        && (d.auth_scheme == AuthScheme::None || !c.key_cipher.is_empty())
                        && (d.adapter != AdapterKind::Cpa
                            || (c.auth_error.is_none()
                                && !c.is_cooling_for(crate::models::UpstreamChannel::Go, wall)))
                });
            let ids = d
                .catalog
                .iter()
                .map(|m| m.upstream_model.clone())
                .collect::<Vec<_>>();
            match d.adapter {
                AdapterKind::OpencodeGo => result.go.extend(ids),
                AdapterKind::Zen => result.zen_free.extend(ids),
                AdapterKind::Goat => result.command_code.extend(ids),
                AdapterKind::Minimax => result.minimax.extend(ids),
                AdapterKind::Kimi => result.kimi.extend(ids),
                AdapterKind::Ollama => {
                    result.ollama.extend(ids);
                    result
                        .ollama_pinned
                        .extend(result.routing.ollama_pinned.iter().cloned());
                }
                AdapterKind::Cpa if active => result.cpa.extend(
                    d.catalog
                        .iter()
                        .filter(|m| m.enabled && !m.protocols.is_empty())
                        .map(|m| m.upstream_model.clone()),
                ),
                AdapterKind::Http => {
                    if d.model_resolution == ModelResolution::PublicOnly {
                        if active {
                            result.custom.extend(
                                d.catalog
                                    .iter()
                                    .filter(|m| m.enabled && !m.protocols.is_empty())
                                    .map(|m| m.public_model.clone()),
                            );
                        }
                    } else {
                        result.extra.push(crate::alias::ExtraProviderCatalog {
                            provider_id: d.id.clone(),
                            mappings: d
                                .catalog
                                .iter()
                                .map(|m| (m.public_model.clone(), m.upstream_model.clone()))
                                .collect(),
                        });
                    }
                }
                AdapterKind::Cpa => {}
            }
        }
        result
    }

    /// Preserve public-name precedence, then require a raw pin to identify
    /// one transport within its destination as well as one destination.
    pub(crate) fn resolve(
        &self,
        requested: &str,
    ) -> Result<crate::alias::ResolvedModel, crate::alias::ResolveError> {
        let resolved = crate::alias::resolve_with_runtime_catalogs(requested, self.catalogs())?;
        let crate::alias::ResolvedModel::PinnedRaw { mapping, .. } = &resolved else {
            return Ok(resolved);
        };
        use ocg_domain::destination::{AdapterKind, ModelResolution};
        if let Some(destination) = self.routing.projection.destinations.iter().find(|d| {
            d.id == mapping.provider_id
                && d.adapter == AdapterKind::Http
                && d.model_resolution == ModelResolution::PublicAndUpstream
        }) && !destination
            .catalog
            .iter()
            .any(|row| crate::custom::custom_model_id_matches(&row.public_model, requested))
        {
            let rows = destination
                .catalog
                .iter()
                .filter(|row| {
                    crate::custom::custom_model_id_matches(
                        &row.upstream_model,
                        &mapping.upstream_model,
                    )
                })
                .collect::<Vec<_>>();
            if let Some(first) = rows.first()
                && rows
                    .iter()
                    .skip(1)
                    .any(|row| !same_http_transport(destination, first, row))
            {
                return Err(crate::alias::ResolveError::Ambiguous {
                    requested: requested.into(),
                    mappings: rows.iter().map(|_| mapping.clone()).collect(),
                });
            }
        }
        Ok(resolved)
    }

    /// Compatibility attribution only; never used for resolution or dispatch.
    pub(crate) fn output_provider_id(&self, identity: &str) -> String {
        let Some(destination) = self
            .routing
            .projection
            .destinations
            .iter()
            .find(|d| d.id == identity)
        else {
            return identity.into();
        };
        self.routing
            .credentials
            .iter()
            .find(|c| c.destination_id == destination.id)
            .map(|c| c.provider_id.clone())
            .unwrap_or_else(|| match &destination.legacy {
                ocg_domain::destination::LegacyDestinationRef::Dynamic(id)
                | ocg_domain::destination::LegacyDestinationRef::Builtin(id) => id.clone(),
                _ => crate::provider::CUSTOM_PROVIDER_ID.into(),
            })
    }

    pub(crate) fn model_has_enabled_protocol(&self, name: &str) -> bool {
        self.resolve(name).is_ok_and(|resolved| {
            self.routing.projection.destinations.iter().any(|d| {
                d.enabled
                    && d.catalog.iter().any(|m| {
                        m.enabled
                            && !m.protocols.is_empty()
                            && crate::gateway::materialize::resolved_contains_model(
                                &resolved, d, m, name,
                            )
                            && (d.adapter != ocg_domain::destination::AdapterKind::Http
                                || m.protocols.iter().any(|protocol| {
                                    let Some(route) =
                                        ocg_domain::destination::http_model_route(d, m, *protocol)
                                    else {
                                        return false;
                                    };
                                    self.routing.credentials.iter().any(|c| {
                                        c.destination_id == d.id
                                            && c.enabled
                                            && c.ready
                                            && c.binding_enabled
                                            && (route.auth_scheme
                                                == ocg_domain::destination::AuthScheme::None
                                                || !c.key_cipher.is_empty())
                                    })
                                }))
                    })
            })
        })
    }
}

fn same_http_transport(
    destination: &ocg_domain::destination::Destination,
    first: &ocg_domain::destination::CatalogModel,
    second: &ocg_domain::destination::CatalogModel,
) -> bool {
    let route = |model: &ocg_domain::destination::CatalogModel| {
        let mut routes = Vec::new();
        for protocol in &model.protocols {
            let saved = ocg_domain::destination::http_model_route(destination, model, *protocol)?;
            let resolved =
                crate::custom_http::resolve_custom_endpoints(&saved.endpoint_url, *protocol)
                    .ok()?;
            routes.push((
                protocol.as_str(),
                resolved.inference.to_string(),
                saved.auth_scheme.as_str(),
            ));
        }
        routes.sort_unstable();
        Some(routes)
    };
    match (route(first), route(second)) {
        (Some(first), Some(second)) => first == second,
        _ => false,
    }
}

async fn proxy_handler(
    state: CoreState,
    trace: RequestTrace,
    headers: HeaderMap,
    body: Bytes,
    client_format: ApiFormat,
) -> axum::response::Response {
    proxy_handler_inner(state, trace, headers, body, client_format).await
}

async fn proxy_handler_inner(
    state: CoreState,
    trace: RequestTrace,
    headers: HeaderMap,
    body: Bytes,
    client_format: ApiFormat,
) -> axum::response::Response {
    let client_body_bytes = body.len();

    let Some(client_key_id) = extract_client_key_id(&headers, &state) else {
        return protocol_error_response(
            client_format,
            StatusCode::UNAUTHORIZED,
            "invalid gateway key",
            None,
        );
    };

    let client_body = body.clone();
    let parsed = match parse_client_request(client_format, body) {
        Ok(parsed) => parsed,
        Err(error) => {
            return local_protocol_failure(
                &state,
                &trace,
                client_format,
                error,
                Some(client_body_bytes),
                Some(&client_body),
            );
        }
    };
    let client_model = parsed.requested_model.clone();
    let routing_model = parsed.requested_model.clone();
    GatewayExecutor::run(
        state,
        trace,
        client_body,
        headers,
        client_format,
        parsed,
        client_model,
        routing_model,
        Some(client_key_id),
    )
    .await
}

async fn gemini_proxy_handler(
    state: CoreState,
    trace: RequestTrace,
    headers: HeaderMap,
    body: Bytes,
    model: String,
    stream: bool,
) -> axum::response::Response {
    let client_body_bytes = body.len();
    let Some(client_key_id) = extract_client_key_id(&headers, &state) else {
        return protocol_error_response(
            ApiFormat::Gemini,
            StatusCode::UNAUTHORIZED,
            "invalid gateway key",
            None,
        );
    };
    let parsed = match parse_gemini_request(model, stream, body.clone()) {
        Ok(parsed) => parsed,
        Err(error) => {
            return local_protocol_failure(
                &state,
                &trace,
                ApiFormat::Gemini,
                error,
                Some(client_body_bytes),
                Some(&body),
            );
        }
    };
    let client_model = parsed.requested_model.clone();
    let routing_model = parsed.requested_model.clone();
    GatewayExecutor::run(
        state,
        trace,
        body,
        headers,
        ApiFormat::Gemini,
        parsed,
        client_model,
        routing_model,
        Some(client_key_id),
    )
    .await
}

/// Candidate credential values a client may present, in fixed priority
/// order: the Bearer token, then `x-api-key`, then `x-goog-api-key`. Every
/// non-empty candidate is an independent credential claim; a wrong value
/// alongside a correct one never downgrades the request.
fn candidate_key_values(headers: &HeaderMap) -> Vec<&str> {
    let mut candidates = Vec::with_capacity(3);
    let bearer = headers
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|auth| auth.trim().strip_prefix("Bearer "))
        .map(str::trim)
        .filter(|value| !value.is_empty());
    if let Some(value) = bearer {
        candidates.push(value);
    }
    for name in ["x-api-key", "x-goog-api-key"] {
        let value = headers
            .get(name)
            .and_then(|value| value.to_str().ok())
            .map(str::trim)
            .filter(|value| !value.is_empty());
        if let Some(value) = value {
            candidates.push(value);
        }
    }
    candidates
}

/// Extracts the id of the credential that authenticates this request.
/// Authentication succeeds when ANY non-empty candidate header matches ANY
/// currently valid credential (the primary key value or an enabled,
/// non-deleted sub key) in the credential snapshot; the first candidate hit,
/// in header order, attributes the request (the primary key resolves to the
/// fixed `PRIMARY_KEY_ID`).
pub(crate) fn extract_client_key_id(headers: &HeaderMap, state: &CoreState) -> Option<String> {
    extract_client_key(headers, state).map(|entry| entry.id)
}

fn extract_client_key(
    headers: &HeaderMap,
    state: &CoreState,
) -> Option<crate::gateway_keys::CredentialEntry> {
    candidate_key_values(headers)
        .into_iter()
        .find_map(|value| state.credential_entry_for_value(value))
}

fn check_auth(headers: &HeaderMap, state: &CoreState) -> bool {
    extract_client_key_id(headers, state).is_some()
}

fn gemini_error(
    state: &CoreState,
    trace: &RequestTrace,
    headers: &HeaderMap,
    status: StatusCode,
    message: &str,
    client_body_bytes: Option<usize>,
) -> axum::response::Response {
    if !check_auth(headers, state) {
        return protocol_error_response(
            ApiFormat::Gemini,
            StatusCode::UNAUTHORIZED,
            "invalid gateway key",
            None,
        );
    }
    local_failure_response(
        state,
        trace,
        ApiFormat::Gemini,
        status,
        message,
        "client",
        "validation",
        client_body_bytes,
        None,
    )
}

fn gemini_expected_fallback(
    state: &CoreState,
    headers: &HeaderMap,
    status: StatusCode,
    message: &str,
) -> axum::response::Response {
    if !check_auth(headers, state) {
        return protocol_error_response(
            ApiFormat::Gemini,
            StatusCode::UNAUTHORIZED,
            "invalid gateway key",
            None,
        );
    }
    protocol_error_response(ApiFormat::Gemini, status, message, None)
}

#[allow(clippy::too_many_arguments)]
fn local_failure_response(
    state: &CoreState,
    trace: &RequestTrace,
    format: ApiFormat,
    status: StatusCode,
    message: &str,
    error_source: &str,
    error_stage: &str,
    client_body_bytes: Option<usize>,
    summary_body: Option<&[u8]>,
) -> axum::response::Response {
    let mut diagnostic = ErrorDiagnostic::new(trace, 1, error_source, error_stage, format);
    diagnostic.client_body_bytes = client_body_bytes;
    diagnostic.downstream_status = Some(status.as_u16());
    if let Some(body) = summary_body {
        diagnostic = diagnostic.with_request_summary(body);
    }
    let encoded = serialize_diagnostic(diagnostic.clone());
    log_request_failure(&state.db.lock(), trace, &diagnostic, &encoded, message);
    emit_failure(&encoded);
    protocol_error_from(
        format,
        ProtocolError::with_status(status, message.to_string()),
    )
}

#[cfg(test)]
fn active_cpa_model_ids(state: &CoreState) -> std::sync::Arc<Vec<String>> {
    std::sync::Arc::new(runtime_catalog_snapshot(state).unwrap().cpa)
}

#[cfg(test)]
mod tests {
    use super::{active_cpa_model_ids, check_auth, extract_client_key_id};
    use crate::gateway_keys::{CredentialEntry, CredentialSnapshot, PRIMARY_KEY_ID};
    use crate::models::AppConfig;
    use crate::state::{CoreState, CoreStateInner};
    use axum::http::{HeaderMap, HeaderValue};
    use std::collections::HashMap;

    /// Owns the temp data dir and releases the SQLite connection (and thus
    /// the open database file) before removing the directory on Windows.
    struct StateDir {
        state: Option<CoreState>,
        dir: Option<std::path::PathBuf>,
    }

    impl std::ops::Deref for StateDir {
        type Target = CoreState;
        fn deref(&self) -> &CoreState {
            self.state.as_ref().expect("state present during use")
        }
    }

    impl Drop for StateDir {
        fn drop(&mut self) {
            self.state.take();
            if let Some(dir) = self.dir.take() {
                std::fs::remove_dir_all(dir).ok();
            }
        }
    }

    fn state_with_snapshot() -> StateDir {
        let mut dir = std::env::temp_dir();
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system clock should be valid")
            .as_nanos();
        dir.push(format!("ocg-auth-matrix-{nanos}"));
        std::fs::create_dir_all(&dir).expect("test data directory should be created");
        let db = crate::db::Database::open(dir.clone()).expect("test database should open");
        let cipher: std::sync::Arc<dyn crate::crypto::KeyCipher + Send + Sync> =
            std::sync::Arc::new(crate::crypto::StaticKeyCipher::new("test"));
        let state = CoreStateInner::new(db, dir.clone(), cipher).expect("state should initialize");
        StateDir {
            state: Some(std::sync::Arc::new(state)),
            dir: Some(dir),
        }
    }

    fn entry(id: &str, name: &str) -> CredentialEntry {
        CredentialEntry {
            id: id.to_string(),
            name: name.to_string(),
        }
    }

    fn snapshot() -> CredentialSnapshot {
        HashMap::from([
            ("ocg-primary".to_string(), entry(PRIMARY_KEY_ID, "Primary")),
            ("ocg-laptop".to_string(), entry("laptop", "Laptop")),
        ])
    }

    #[test]
    fn auth_matrix_across_headers_credentials_and_states() {
        let state = state_with_snapshot();
        *state.credential_snapshot.write() = snapshot();
        let cases = [
            // (header name, presented value, expected key id)
            ("authorization", "Bearer ocg-primary", PRIMARY_KEY_ID),
            ("authorization", "Bearer ocg-laptop", "laptop"),
            ("x-api-key", "ocg-laptop", "laptop"),
            ("x-goog-api-key", "ocg-primary", PRIMARY_KEY_ID),
            ("authorization", "Bearer wrong-key", ""),
            ("authorization", "Bearer ", ""),
            ("x-api-key", "", ""),
            ("x-goog-api-key", "   ", ""),
        ];
        for (header, presented, expected) in cases {
            let mut headers = HeaderMap::new();
            headers.insert(
                axum::http::HeaderName::from_static(header),
                HeaderValue::from_str(presented).expect("test header value should be valid"),
            );
            let matched = extract_client_key_id(&headers, &state);
            if expected.is_empty() {
                assert!(
                    matched.is_none(),
                    "{header}: {presented} should not authenticate"
                );
            } else {
                assert_eq!(
                    matched.as_deref(),
                    Some(expected),
                    "{header}: {presented} should match {expected}"
                );
            }
        }

        let no_headers = HeaderMap::new();
        assert!(extract_client_key_id(&no_headers, &state).is_none());
    }

    #[test]
    fn wrong_x_api_key_alongside_correct_x_goog_api_key_passes() {
        let state = state_with_snapshot();
        *state.credential_snapshot.write() = snapshot();
        let mut headers = HeaderMap::new();
        headers.insert("x-api-key", HeaderValue::from_static("wrong-key"));
        headers.insert("x-goog-api-key", HeaderValue::from_static("ocg-laptop"));
        assert!(check_auth(&headers, &state));
        assert_eq!(
            extract_client_key_id(&headers, &state).as_deref(),
            Some("laptop")
        );

        // Bearer wins attribution when several candidates hit: it comes first
        // in the fixed candidate order.
        headers.insert(
            "authorization",
            HeaderValue::from_static("Bearer ocg-primary"),
        );
        assert_eq!(
            extract_client_key_id(&headers, &state).as_deref(),
            Some(PRIMARY_KEY_ID)
        );

        // Two wrong candidates still fail.
        let mut wrong = HeaderMap::new();
        wrong.insert("x-api-key", HeaderValue::from_static("wrong-key"));
        wrong.insert("x-goog-api-key", HeaderValue::from_static("also-wrong"));
        assert!(!check_auth(&wrong, &state));
    }

    #[test]
    fn bearer_without_prefix_falls_back_to_api_key_headers() {
        let state = state_with_snapshot();
        *state.credential_snapshot.write() = snapshot();
        let mut headers = HeaderMap::new();
        headers.insert("authorization", HeaderValue::from_static("ocg-primary"));
        assert!(extract_client_key_id(&headers, &state).is_none());
        headers.insert("x-api-key", HeaderValue::from_static("ocg-laptop"));
        assert_eq!(
            extract_client_key_id(&headers, &state).as_deref(),
            Some("laptop")
        );
    }

    #[test]
    fn disabled_and_deleted_sub_keys_leave_the_snapshot() {
        // The snapshot only ever contains the primary value and enabled
        // non-deleted sub keys; disabling or soft-deleting removes the entry
        // (covered end to end by the key lifecycle integration tests).
        let state = state_with_snapshot();
        let mut snapshot = snapshot();
        assert!(snapshot.remove("ocg-laptop").is_some());
        *state.credential_snapshot.write() = snapshot;
        let mut headers = HeaderMap::new();
        headers.insert("x-api-key", HeaderValue::from_static("ocg-laptop"));
        assert!(!check_auth(&headers, &state));
    }

    #[test]
    fn app_config_still_compiles_without_a_key_list() {
        // Compile-time guard: the config shape no longer embeds key entries.
        let config = AppConfig {
            gateway_key: "k".into(),
            ..AppConfig::default()
        };
        config.validate().expect("scalar-key config validates");
    }

    #[test]
    fn disabled_cpa_catalog_does_not_enter_request_alias_resolution() {
        let state = state_with_snapshot();
        let now = chrono::Utc::now();
        let account = crate::models::Account {
            id: crate::provider::CPA_ACCOUNT_ID.to_string(),
            provider_id: crate::provider::CPA_PROVIDER_ID.to_string(),

            credential_kind: crate::provider::CredentialKind::ApiKey,
            quota_scope: crate::provider::QuotaScope::Key,
            name: crate::provider::CPA_ACCOUNT_NAME.to_string(),
            username: None,
            password_cipher: None,
            key_cipher: state.encrypt_key("inference").unwrap(),
            enabled: true,
            account_type: crate::models::AccountType::Key,
            setup_step: crate::models::AccountSetupStep::Ready,
            referral_code: None,
            purchase_date: String::new(),
            expires_on: String::new(),
            cooldown_until: None,
            cooldown_generic_until: None,
            cooldown_5h_until: None,
            cooldown_week_until: None,
            cooldown_month_until: None,
            cooldown_free_until: None,
            last_error: None,
            auth_error: None,
            notes: None,
            created_at: now,
            updated_at: now,
        };
        let management = state.encrypt_key("management").unwrap();
        state
            .db
            .lock()
            .upsert_cpa_integration(&account, crate::cpa::DEFAULT_CPA_BASE_URL, &management)
            .unwrap();
        state
            .activate_cpa_model_catalog(
                vec!["grok-4.5".into()],
                crate::cpa::DEFAULT_CPA_BASE_URL,
                now,
            )
            .unwrap();
        assert_eq!(active_cpa_model_ids(&state).as_slice(), ["grok-4.5"]);

        state
            .db
            .lock()
            .update_account(
                crate::provider::CPA_ACCOUNT_ID,
                &crate::models::AccountUpdate {
                    enabled: Some(false),
                    ..Default::default()
                },
                None,
                None,
            )
            .unwrap();
        assert!(active_cpa_model_ids(&state).is_empty());
    }
}
