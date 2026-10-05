//! One private HTTP hop from the public ingress to the owned CPA child.
//!
//! Client-key authentication stays in the handler. This module keeps alias
//! authorization, request constraints, and the raw protocol body. Pricing,
//! credits, and estimates never govern execution. CPA owns provider execution,
//! and synchronous OCG policy controls mandatory quota. The hop registers
//! `CorrelationAuthorization::Client` from the access key matched at
//! authentication. There is no Rust provider selector and no retry. The same
//! captured correlation deadline is sent as `X-OCG-Request-Deadline` in UTC
//! RFC3339 with fractional seconds retained. Inbound `X-OCG-*` values, including
//! a caller deadline, are stripped before that header is set.

use crate::gateway::diagnostics::RequestTrace;
use crate::gateway::protocol::{ParsedClientRequest, validate_client_request_features};
use crate::gateway::response::{local_protocol_failure, protocol_error_response};
use crate::kernel::protocol::ApiFormat;
use crate::models::AppConfig;
use crate::state::CoreState;
use axum::body::{Body, Bytes};
use axum::http::{HeaderMap, StatusCode};
use axum::response::Response;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::Duration;
use uuid::Uuid;
use zeroize::Zeroize;

const UNAVAILABLE: &str = "owned CPA hop is unavailable";

/// Public protocol path. The body is not rewritten to an upstream model.
pub(crate) enum PublicPath {
    ChatCompletions,
    Responses,
    Messages,
    /// Anthropic SDK compatibility count. Not a generation.
    MessagesCount,
    /// Origin-form path, including `:generateContent`, `:streamGenerateContent`,
    /// or `:countTokens`.
    Gemini(String),
}

impl PublicPath {
    fn is_count(&self) -> bool {
        match self {
            Self::MessagesCount => true,
            Self::Gemini(path) => path.ends_with(":countTokens"),
            Self::ChatCompletions | Self::Responses | Self::Messages => false,
        }
    }

    fn owned_path(&self) -> Result<String, ()> {
        match self {
            Self::ChatCompletions => Ok("/v1/chat/completions".to_string()),
            Self::Responses => Ok("/v1/responses".to_string()),
            Self::Messages => Ok("/v1/messages".to_string()),
            Self::MessagesCount => Ok("/v1/messages/count_tokens".to_string()),
            Self::Gemini(path) => gemini_path(path).ok_or(()),
        }
    }
}

/// Key id and fingerprint taken from one successful access-key match.
#[derive(Clone)]
pub(crate) struct ClientAccess {
    pub key_id: String,
    pub captured_key_fingerprint: String,
}

/// One authenticated, already-parsed public request.
pub(crate) struct PublicHop {
    pub trace: RequestTrace,
    pub headers: HeaderMap,
    pub body: Bytes,
    pub parsed: ParsedClientRequest,
    pub client: ClientAccess,
    pub path: PublicPath,
    pub query: String,
}

struct CapturedDeadline {
    wall: chrono::DateTime<chrono::Utc>,
    mono: tokio::time::Instant,
}

struct OwnedHop {
    base_url: String,
    secret: String,
    child_generation: u64,
    applied_revision: u64,
}

impl std::fmt::Debug for OwnedHop {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("OwnedHop")
            .field("base_url", &self.base_url)
            .field("secret", &"[redacted]")
            .field("child_generation", &self.child_generation)
            .field("applied_revision", &self.applied_revision)
            .finish()
    }
}

impl Drop for OwnedHop {
    fn drop(&mut self) {
        self.secret.zeroize();
    }
}

impl OwnedHop {
    fn from_connection(connection: crate::cpa_execution::OwnedInferenceConnection) -> Self {
        Self {
            base_url: connection.base_url,
            secret: connection.hop.expose().to_string(),
            child_generation: connection.child_generation,
            applied_revision: connection.applied_revision,
        }
    }

    fn fixture(
        base_url: impl Into<String>,
        secret: impl Into<String>,
        child_generation: u64,
        applied_revision: u64,
    ) -> Self {
        Self {
            base_url: base_url.into(),
            secret: secret.into(),
            child_generation,
            applied_revision,
        }
    }
}

struct CorrelationGuard {
    state: CoreState,
    request_id: Uuid,
    disarmed: bool,
    cancels: Option<Arc<AtomicU32>>,
}

impl CorrelationGuard {
    fn arm(state: CoreState, request_id: Uuid, cancels: Option<Arc<AtomicU32>>) -> Self {
        Self {
            state,
            request_id,
            disarmed: false,
            cancels,
        }
    }

    fn disarm(&mut self) {
        self.disarmed = true;
    }
}

impl Drop for CorrelationGuard {
    fn drop(&mut self) {
        if self.disarmed {
            return;
        }
        if let Some(cancels) = &self.cancels {
            cancels.fetch_add(1, Ordering::SeqCst);
        }
        crate::cpa_execution::cancel_correlation(&self.state, self.request_id);
    }
}

struct LiveBody {
    response: reqwest::Response,
    guard: CorrelationGuard,
    deadline: tokio::time::Instant,
}

#[derive(Debug)]
struct HopBodyError;

impl std::fmt::Display for HopBodyError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(UNAVAILABLE)
    }
}

impl std::error::Error for HopBodyError {}

#[cfg(test)]
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct IntentRecord {
    pub requested_model: String,
    pub key_id: String,
    pub fingerprint: String,
    pub validated: bool,
    pub deadline: chrono::DateTime<chrono::Utc>,
    pub request_id: Uuid,
}

struct Dispatch {
    fixture: Option<OwnedHop>,
    deadline: Option<CapturedDeadline>,
    cancels: Option<Arc<AtomicU32>>,
    #[cfg(test)]
    intents: Option<Arc<std::sync::Mutex<Vec<IntentRecord>>>>,
}

/// Production entry. The owned child comes only from `owned_inference_connection`.
pub(crate) async fn forward_public(state: CoreState, hop: PublicHop) -> Response {
    forward_inner(
        state,
        hop,
        Dispatch {
            fixture: None,
            deadline: None,
            cancels: None,
            #[cfg(test)]
            intents: None,
        },
    )
    .await
}

#[cfg(test)]
async fn forward_for_test(
    state: CoreState,
    hop: PublicHop,
    owned: OwnedHop,
    deadline: CapturedDeadline,
    cancels: Arc<AtomicU32>,
) -> Response {
    forward_inner(
        state,
        hop,
        Dispatch {
            fixture: Some(owned),
            deadline: Some(deadline),
            cancels: Some(cancels),
            intents: None,
        },
    )
    .await
}

#[cfg(test)]
async fn forward_observing(
    state: CoreState,
    hop: PublicHop,
    owned: OwnedHop,
    deadline: CapturedDeadline,
    cancels: Arc<AtomicU32>,
    intents: Arc<std::sync::Mutex<Vec<IntentRecord>>>,
) -> Response {
    forward_inner(
        state,
        hop,
        Dispatch {
            fixture: Some(owned),
            deadline: Some(deadline),
            cancels: Some(cancels),
            intents: Some(intents),
        },
    )
    .await
}

async fn forward_inner(state: CoreState, hop: PublicHop, mut dispatch: Dispatch) -> Response {
    let format = hop.parsed.client;
    if hop.client.key_id.is_empty() || hop.client.captured_key_fingerprint.is_empty() {
        return protocol_error_response(
            format,
            StatusCode::UNAUTHORIZED,
            "invalid gateway key",
            None,
        );
    }

    if let Err(error) = validate_client_request_features(&hop.parsed) {
        return local_protocol_failure(
            &state,
            &hop.trace,
            format,
            error,
            Some(hop.body.len()),
            Some(&hop.body),
        );
    }
    if let Err(error) = authorize_public_model(&state, &hop.parsed.requested_model) {
        return local_protocol_failure(
            &state,
            &hop.trace,
            format,
            error,
            Some(hop.body.len()),
            Some(&hop.body),
        );
    }
    let path = match hop.path.owned_path() {
        Ok(path) => path,
        Err(()) => {
            return protocol_error_response(
                format,
                StatusCode::NOT_FOUND,
                "unknown Gemini model action",
                None,
            );
        }
    };
    let deadline = match dispatch.deadline.take() {
        Some(deadline) => deadline,
        None => {
            match capture_deadline(&state.config(), hop.parsed.stream && !hop.path.is_count()) {
                Some(deadline) => deadline,
                None => return unavailable(format),
            }
        }
    };
    let owned = match dispatch.fixture.take() {
        Some(owned) => owned,
        None => match crate::cpa_execution::owned_inference_connection(&state) {
            Ok(connection) => OwnedHop::from_connection(connection),
            Err(error) => {
                return protocol_error_response(
                    format,
                    StatusCode::SERVICE_UNAVAILABLE,
                    &error.to_string(),
                    None,
                );
            }
        },
    };

    let request_id = Uuid::new_v4();
    let intent = crate::cpa_execution::CorrelationIntent {
        request_id,
        deadline: deadline.wall,
        requested_model: hop.parsed.requested_model.clone(),
        authorization: crate::cpa_execution::CorrelationAuthorization::Client {
            key_id: hop.client.key_id.clone(),
            captured_key_fingerprint: hop.client.captured_key_fingerprint.clone(),
        },
    };
    #[cfg(test)]
    record_intent(&dispatch, &intent);
    let guard = CorrelationGuard::arm(state.clone(), request_id, dispatch.cancels.clone());
    if let Err(error) = crate::cpa_execution::register_correlation_intent(&state, intent) {
        drop(guard);
        return protocol_error_response(
            format,
            StatusCode::SERVICE_UNAVAILABLE,
            &error.to_string(),
            None,
        );
    }
    if let Err(error) =
        crate::cpa_execution::register_correlation_trace(&state, request_id, &hop.trace.request_id)
    {
        crate::cpa_execution::cancel_correlation(&state, request_id);
        drop(guard);
        return protocol_error_response(
            format,
            StatusCode::SERVICE_UNAVAILABLE,
            &error.to_string(),
            None,
        );
    }
    if deadline_elapsed(&deadline) {
        drop(guard);
        return unavailable(format);
    }

    let url = match hop_url(&owned.base_url, &path, &forward_query(&hop.query)) {
        Ok(url) => url,
        Err(()) => {
            drop(guard);
            return unavailable(format);
        }
    };
    let headers = match outbound_headers(
        &hop.headers,
        &owned.secret,
        request_id,
        owned.child_generation,
        owned.applied_revision,
        deadline.wall,
    ) {
        Ok(headers) => headers,
        Err(()) => {
            drop(guard);
            return unavailable(format);
        }
    };
    let remaining = deadline
        .mono
        .checked_duration_since(tokio::time::Instant::now())
        .unwrap_or_default();
    if remaining.is_zero() {
        drop(guard);
        return unavailable(format);
    }

    let pending = hop_client()
        .post(url)
        .timeout(remaining)
        .headers(headers)
        .body(hop.body.clone())
        .send();
    let sent = tokio::select! {
        biased;
        _ = tokio::time::sleep_until(deadline.mono) => None,
        result = pending => Some(result),
    };
    let response = match sent {
        Some(Ok(response)) => response,
        Some(Err(_)) | None => {
            drop(guard);
            return unavailable(format);
        }
    };
    passthrough(format, response, guard, deadline.mono)
}

fn authorize_public_model(
    state: &CoreState,
    requested: &str,
) -> Result<(), crate::gateway::protocol::ProtocolError> {
    let _settings = state.settings_update.lock();
    let routing = state.capture_routing_snapshot().map_err(|_| {
        crate::gateway::protocol::ProtocolError::with_status(
            StatusCode::INTERNAL_SERVER_ERROR,
            "failed to load routing configuration",
        )
    })?;
    let wall = state.sample_gateway_clock().0;
    super::handler::RuntimeCatalogSnapshot::from_routing(routing, wall)
        .resolve(requested)
        .map(|_| ())
        .map_err(super::materialize::protocol_error_from_resolve)
}

fn capture_deadline(config: &AppConfig, stream: bool) -> Option<CapturedDeadline> {
    let secs = if stream {
        config.stream_idle_timeout_secs
    } else {
        config.non_stream_timeout_secs
    }
    .max(1);
    let secs_i64 = i64::try_from(secs).unwrap_or(i64::MAX);
    let wall = chrono::Utc::now().checked_add_signed(chrono::Duration::seconds(secs_i64))?;
    Some(CapturedDeadline {
        wall,
        mono: tokio::time::Instant::now() + Duration::from_secs(secs),
    })
}

fn deadline_elapsed(deadline: &CapturedDeadline) -> bool {
    chrono::Utc::now() >= deadline.wall || tokio::time::Instant::now() >= deadline.mono
}

fn unavailable(format: ApiFormat) -> Response {
    protocol_error_response(format, StatusCode::SERVICE_UNAVAILABLE, UNAVAILABLE, None)
}

fn gemini_path(path: &str) -> Option<String> {
    let path = path.split('?').next().unwrap_or(path);
    if path.contains("..") || path.contains('\\') || path.contains("://") || path.contains("//") {
        return None;
    }
    let prefixed = path.starts_with("/v1beta/models/") || path.starts_with("/v1/models/");
    let action = path.ends_with(":generateContent")
        || path.ends_with(":streamGenerateContent")
        || path.ends_with(":countTokens");
    if !(prefixed && action) {
        return None;
    }
    // The owned child serves these actions under /v1beta. The public
    // /v1/models action is the same Gemini operation.
    let owned = path
        .strip_prefix("/v1/models/")
        .map(|rest| format!("/v1beta/models/{rest}"))
        .unwrap_or_else(|| path.to_string());
    Some(owned)
}

/// Path and raw query from the middleware trace target.
/// `RequestTrace::path` is `request.uri().to_string()`.
pub(crate) fn public_target(raw: &str) -> (String, String) {
    let Ok(uri) = raw.parse::<axum::http::Uri>() else {
        return (String::new(), String::new());
    };
    (
        uri.path().to_string(),
        uri.query().unwrap_or("").to_string(),
    )
}

fn credential_query_name(name: &str) -> bool {
    matches!(
        name,
        "key"
            | "auth_token"
            | "api_key"
            | "api-key"
            | "x-api-key"
            | "x-goog-api-key"
            | "pinned_auth_id"
            | "pinned-auth-id"
    )
}

fn forward_query(query: &str) -> String {
    query
        .split('&')
        .filter(|pair| !pair.is_empty())
        .filter(|pair| {
            let name = pair.split_once('=').map(|(name, _)| name).unwrap_or(pair);
            !credential_query_name(&name.to_ascii_lowercase())
        })
        .collect::<Vec<_>>()
        .join("&")
}

#[cfg(test)]
fn record_intent(dispatch: &Dispatch, intent: &crate::cpa_execution::CorrelationIntent) {
    let Some(intents) = &dispatch.intents else {
        return;
    };
    let (key_id, fingerprint, validated) = match &intent.authorization {
        crate::cpa_execution::CorrelationAuthorization::Client {
            key_id,
            captured_key_fingerprint,
        } => (key_id.clone(), captured_key_fingerprint.clone(), false),
        crate::cpa_execution::CorrelationAuthorization::Validated { .. } => {
            (String::new(), String::new(), true)
        }
    };
    intents.lock().expect("intent log").push(IntentRecord {
        requested_model: intent.requested_model.clone(),
        key_id,
        fingerprint,
        validated,
        deadline: intent.deadline,
        request_id: intent.request_id,
    });
}

fn strip_request_header(name: &str) -> bool {
    let name = name.to_ascii_lowercase();
    name.starts_with("x-ocg-")
        || name.contains("pinned-auth")
        || name.contains("pinned_auth")
        || matches!(
            name.as_str(),
            "authorization"
                | "proxy-authorization"
                | "cookie"
                | "set-cookie"
                | "x-api-key"
                | "api-key"
                | "x-goog-api-key"
                | "host"
                | "content-length"
                | "connection"
                | "transfer-encoding"
                | "keep-alive"
                | "upgrade"
                | "te"
                | "trailer"
        )
}

fn strip_response_header(name: &str) -> bool {
    let name = name.to_ascii_lowercase();
    name.starts_with("x-ocg-")
        || name.contains("pinned-auth")
        || name.contains("pinned_auth")
        || matches!(
            name.as_str(),
            "authorization"
                | "proxy-authorization"
                | "cookie"
                | "set-cookie"
                | "connection"
                | "transfer-encoding"
                | "keep-alive"
                | "upgrade"
                | "te"
                | "trailer"
                | "content-length"
        )
}

fn request_deadline_value(deadline: chrono::DateTime<chrono::Utc>) -> String {
    deadline.to_rfc3339_opts(chrono::SecondsFormat::Nanos, true)
}

fn outbound_headers(
    incoming: &HeaderMap,
    secret: &str,
    request_id: Uuid,
    generation: u64,
    revision: u64,
    deadline: chrono::DateTime<chrono::Utc>,
) -> Result<reqwest::header::HeaderMap, ()> {
    let mut outbound = reqwest::header::HeaderMap::new();
    for (name, value) in incoming.iter() {
        if strip_request_header(name.as_str()) {
            continue;
        }
        let Ok(name) = reqwest::header::HeaderName::from_bytes(name.as_str().as_bytes()) else {
            continue;
        };
        let Ok(value) = reqwest::header::HeaderValue::from_bytes(value.as_bytes()) else {
            continue;
        };
        outbound.append(name, value);
    }
    let mut bearer = format!("Bearer {secret}");
    let authorization = match reqwest::header::HeaderValue::from_str(&bearer) {
        Ok(value) => value,
        Err(_) => {
            bearer.zeroize();
            return Err(());
        }
    };
    bearer.zeroize();
    outbound.insert(reqwest::header::AUTHORIZATION, authorization);
    outbound.insert(
        "x-ocg-request-id",
        reqwest::header::HeaderValue::from_str(&request_id.to_string()).map_err(|_| ())?,
    );
    outbound.insert(
        "x-ocg-process-generation",
        reqwest::header::HeaderValue::from_str(&generation.to_string()).map_err(|_| ())?,
    );
    outbound.insert(
        "x-ocg-projection-revision",
        reqwest::header::HeaderValue::from_str(&revision.to_string()).map_err(|_| ())?,
    );
    outbound.insert(
        "x-ocg-request-deadline",
        reqwest::header::HeaderValue::from_str(&request_deadline_value(deadline))
            .map_err(|_| ())?,
    );
    Ok(outbound)
}

fn hop_url(base: &str, path: &str, query: &str) -> Result<reqwest::Url, ()> {
    let base = base.trim_end_matches('/');
    let raw = if query.is_empty() {
        format!("{base}{path}")
    } else {
        format!("{base}{path}?{query}")
    };
    reqwest::Url::parse(&raw).map_err(|_| ())
}

fn hop_client() -> &'static reqwest::Client {
    static CLIENT: OnceLock<reqwest::Client> = OnceLock::new();
    CLIENT.get_or_init(|| {
        reqwest::Client::builder()
            .redirect(crate::http_client::no_redirect_policy())
            .no_proxy()
            .build()
            .expect("loopback CPA hop client")
    })
}

fn passthrough(
    format: ApiFormat,
    response: reqwest::Response,
    guard: CorrelationGuard,
    deadline: tokio::time::Instant,
) -> Response {
    let status =
        StatusCode::from_u16(response.status().as_u16()).unwrap_or(StatusCode::BAD_GATEWAY);
    let headers = copy_response_headers(response.headers());
    let stream = passthrough_body(LiveBody {
        response,
        guard,
        deadline,
    });
    let mut builder = Response::builder().status(status);
    for (name, value) in headers.iter() {
        builder = builder.header(name, value);
    }
    builder
        .body(Body::from_stream(stream))
        .unwrap_or_else(|_| unavailable(format))
}

fn copy_response_headers(headers: &reqwest::header::HeaderMap) -> HeaderMap {
    let mut copied = HeaderMap::new();
    for (name, value) in headers.iter() {
        if strip_response_header(name.as_str()) {
            continue;
        }
        let Ok(name) = axum::http::HeaderName::from_bytes(name.as_str().as_bytes()) else {
            continue;
        };
        let Ok(value) = axum::http::HeaderValue::from_bytes(value.as_bytes()) else {
            continue;
        };
        copied.append(name, value);
    }
    copied
}

fn passthrough_body(
    live: LiveBody,
) -> impl futures_util::Stream<Item = Result<Bytes, HopBodyError>> + Send {
    futures_util::stream::unfold(Some(live), |live| async move {
        let mut live = live?;
        match next_chunk(&mut live.response, live.deadline).await {
            None => None,
            Some(Ok(None)) => {
                live.guard.disarm();
                None
            }
            Some(Ok(Some(bytes))) => Some((Ok(bytes), Some(live))),
            Some(Err(())) => Some((Err(HopBodyError), None)),
        }
    })
}

async fn next_chunk(
    response: &mut reqwest::Response,
    deadline: tokio::time::Instant,
) -> Option<Result<Option<Bytes>, ()>> {
    if tokio::time::Instant::now() >= deadline {
        return None;
    }
    tokio::select! {
        biased;
        _ = tokio::time::sleep_until(deadline) => None,
        chunk = response.chunk() => Some(chunk.map_err(|_| ())),
    }
}

#[cfg(test)]
mod tests;
