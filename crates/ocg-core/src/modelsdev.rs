//! models.dev public catalog as the lowest-priority model metadata source.
//!
//! Fields a route never learned — no operator declaration, and no
//! upstream-observed value for that field — are filled from
//! <https://models.dev> so downstream clients still see verified context
//! windows, modalities, and reasoning tiers. Operator declarations stay
//! whole-record replacements. The catalog is cached in the existing
//! `modelsdev_catalog_v1` setting and refreshed in the background. A failed
//! refresh keeps the previous cache and never blocks `/v1/models`.
//!
//! Current caches store each provider's own model row and that row's public
//! API identity. A route that matches a provider API uses that provider's row.
//! The longest matching path wins, and a model `provider.api` replaces the
//! provider address for that model. A matched provider that does not list the
//! model is terminal. A route that matches nothing uses the model id's single
//! canonical baseline when that link is exact; that baseline is inference for
//! an unrecognized proxy, not a verified provider. The older flat `models`
//! index is still written and still read when the provider index is absent,
//! so an old cache remains usable offline and an older binary can still open
//! a new cache. A cache without the current provider index is refresh-eligible
//! immediately. An HTTP 200 body that is not a usable catalog is a failed
//! refresh and does not replace the last good cache.

use crate::db::Database;
use crate::model_metadata::{self, ModelMetadata};
use crate::models::AppConfig;
use crate::state::CoreState;
use chrono::{DateTime, Duration, Utc};
use futures_util::StreamExt;
use ocg_domain::destination::{
    AdapterKind, CatalogModel, Destination, http_model_route, http_protocol_routes,
};
use parking_lot::{Mutex, RwLock};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::sync::Arc;

pub const MODELSDEV_SOURCE_URL: &str = "https://models.dev/api.json";
const SETTING_KEY: &str = "modelsdev_catalog_v1";
const MAX_BODY_BYTES: usize = 16 * 1024 * 1024;
const MAX_ROWS: usize = 50_000;
const FETCH_TIMEOUT_SECS: u64 = 30;
const FRESH_FOR: Duration = Duration::hours(24);
const RETRY_AFTER: Duration = Duration::hours(1);
/// Provider rows plus API identity. Older flat caches omit this and stay readable.
pub(crate) const OFFERINGS_VERSION: u32 = 1;

/// Modalities the OCG metadata contract accepts. models.dev also reports
/// values like `pdf`; they are dropped at ingestion so a downstream row can
/// never declare an unsupported modality.
const SUPPORTED_MODALITIES: [&str; 4] = ["text", "image", "audio", "video"];

/// Official origins whose models.dev provider rows omit `api`.
/// Compared as scheme, host, and effective port. The path is not fixed:
/// chat, responses, and messages routes on that origin select the provider.
const FIRST_PARTY_ORIGINS: &[(&str, &str, u16, &str)] = &[
    ("https", "api.openai.com", 443, "openai"),
    ("https", "api.anthropic.com", 443, "anthropic"),
];

/// models.dev provider ids that a sealed adapter owns when several catalog
/// rows share one exact API. The ids are the catalog keys whose published
/// `api` is that adapter's fixed origin. MiniMax has two ids on one API, so
/// it does not choose. Configurable HTTP never chooses.
const SEALED_PROVIDER_IDS: &[(AdapterKind, &[&str])] = &[
    (AdapterKind::Kimi, &["kimi-code-plan-cn"]),
    (AdapterKind::OpencodeGo, &["opencode-go"]),
    (AdapterKind::Zen, &["opencode"]),
    (AdapterKind::Ollama, &["ollama-cloud"]),
];

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct ApiIdentity {
    pub scheme: String,
    pub host: String,
    pub port: u16,
    /// Path segments joined by `/`, without a leading or trailing slash.
    /// An empty path matches every path on the origin.
    pub path: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub(crate) struct ModelsDevModelRow {
    #[serde(default)]
    pub metadata: ModelMetadata,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api: Option<ApiIdentity>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub canonical_model_id: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub(crate) struct ModelsDevProvider {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api: Option<ApiIdentity>,
    #[serde(default)]
    pub models: BTreeMap<String, ModelsDevModelRow>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub(crate) struct ModelsDevOfferings {
    #[serde(default)]
    pub version: u32,
    #[serde(default)]
    pub providers: BTreeMap<String, ModelsDevProvider>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub(crate) struct ModelsDevCatalog {
    #[serde(default)]
    pub fetched_at: Option<DateTime<Utc>>,
    /// Flat index kept for caches written before provider identity, and so an
    /// older reader still sees model facts. New lookups use [`Self::offerings`]
    /// when that index is current.
    #[serde(default)]
    pub models: BTreeMap<String, ModelMetadata>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub offerings: Option<ModelsDevOfferings>,
}

impl ModelsDevCatalog {
    /// Fresh catalog whose only rows are `models`. Each id is one provider
    /// row without an API, so a generic route resolves that exact id.
    #[cfg(test)]
    pub(crate) fn fresh_flat(models: BTreeMap<String, ModelMetadata>) -> Self {
        let mut provider_models = BTreeMap::new();
        for (id, metadata) in &models {
            provider_models.insert(
                id.clone(),
                ModelsDevModelRow {
                    metadata: metadata.clone(),
                    ..ModelsDevModelRow::default()
                },
            );
        }
        let mut providers = BTreeMap::new();
        if !provider_models.is_empty() {
            providers.insert(
                "flat".to_string(),
                ModelsDevProvider {
                    models: provider_models,
                    ..ModelsDevProvider::default()
                },
            );
        }
        Self {
            fetched_at: Some(Utc::now()),
            models,
            offerings: Some(ModelsDevOfferings {
                version: OFFERINGS_VERSION,
                providers,
            }),
        }
    }

    pub(crate) fn is_fresh(&self, now: DateTime<Utc>) -> bool {
        self.fetched_at
            .is_some_and(|fetched| now - fetched < FRESH_FOR)
            && self
                .offerings
                .as_ref()
                .is_some_and(|offerings| offerings.version >= OFFERINGS_VERSION)
    }
}

pub(crate) fn load(db: &Database) -> anyhow::Result<ModelsDevCatalog> {
    db.get_setting(SETTING_KEY)?
        .map(|raw| serde_json::from_str(&raw).map_err(Into::into))
        .transpose()
        .map(Option::unwrap_or_default)
}

fn save(db: &Database, catalog: &ModelsDevCatalog) -> anyhow::Result<()> {
    db.set_setting(SETTING_KEY, &serde_json::to_string(catalog)?)
}

/// Persist and publish a successful refresh. `Err` leaves both copies alone.
/// The database guard is dropped before the catalog write.
fn install_refresh(
    db: &Mutex<Database>,
    live: &RwLock<Arc<ModelsDevCatalog>>,
    fetched: Result<ModelsDevCatalog, String>,
) {
    let Ok(catalog) = fetched else {
        return;
    };
    let saved = {
        let guard = db.lock();
        save(&guard, &catalog)
    };
    *live.write() = Arc::new(catalog);
    if let Err(error) = saved {
        tracing::warn!("failed to persist models.dev catalog: {error}");
    }
}

fn current_offerings(catalog: &ModelsDevCatalog) -> Option<&ModelsDevOfferings> {
    catalog
        .offerings
        .as_ref()
        .filter(|offerings| offerings.version >= OFFERINGS_VERSION)
}

/// Facts for this configured route. Each enabled protocol route contributes
/// its verified provider row, or, when the route matches no provider, that
/// model id's exact generic baseline. A missing baseline is unknown and
/// blocks positive claims from the other routes. A legacy flat cache keeps
/// the old id lookup.
pub(crate) fn lookup(
    catalog: &ModelsDevCatalog,
    destination: &Destination,
    model: &CatalogModel,
) -> Option<ModelMetadata> {
    let Some(offerings) = current_offerings(catalog) else {
        return legacy_lookup(catalog, &model.public_model, &model.upstream_model).cloned();
    };
    let urls = serving_endpoints(destination, model);
    if urls.is_empty() {
        return None;
    }
    let mut matched = Vec::new();
    let mut saw_fact = false;
    for url in urls {
        if let Some(endpoint) = parse_http_identity(&url)
            && let Some(metadata) =
                verified_metadata(offerings, destination.adapter, model, &endpoint)
        {
            saw_fact = true;
            matched.push(metadata);
            continue;
        }
        if let Some(baseline) =
            generic_baseline(offerings, &model.public_model, &model.upstream_model)
        {
            saw_fact = true;
            matched.push(baseline);
        } else {
            matched.push(ModelMetadata::default());
        }
    }
    if !saw_fact {
        return None;
    }
    Some(if matched.len() == 1 {
        matched.remove(0)
    } else {
        model_metadata::common(&matched)
    })
}

fn legacy_lookup<'a>(
    catalog: &'a ModelsDevCatalog,
    public_model: &str,
    upstream_model: &str,
) -> Option<&'a ModelMetadata> {
    for key in lookup_keys(public_model, upstream_model) {
        if let Some(found) = catalog.models.get(&key) {
            return Some(found);
        }
    }
    None
}

fn lookup_keys(public_model: &str, upstream_model: &str) -> Vec<String> {
    let mut keys = Vec::new();
    let mut push = |value: &str| {
        if !value.is_empty() && !keys.iter().any(|existing| existing == value) {
            keys.push(value.to_string());
        }
    };
    push(upstream_model);
    if let Some((_, tail)) = upstream_model.rsplit_once('/') {
        push(tail);
    }
    push(public_model);
    keys
}

fn serving_endpoints(destination: &Destination, model: &CatalogModel) -> Vec<String> {
    let mut urls = Vec::new();
    let mut push = |url: String| {
        if !url.is_empty() && !urls.contains(&url) {
            urls.push(url);
        }
    };
    if let Some(route) = &model.upstream_override {
        if let Some(resolved) = http_model_route(destination, model, route.protocol) {
            push(resolved.endpoint_url);
        }
        return urls;
    }
    let protocols = if !model.protocols.is_empty() {
        model.protocols.clone()
    } else {
        http_protocol_routes(destination)
            .into_iter()
            .map(|route| route.protocol)
            .collect()
    };
    for protocol in protocols {
        if let Some(route) = http_model_route(destination, model, protocol) {
            push(route.endpoint_url);
        }
    }
    urls
}

struct EndpointCandidate {
    identity: ApiIdentity,
    provider_id: String,
    /// `None` is a known API whose model row is absent. It stays in the
    /// winning set so a sibling row cannot fill the gap.
    metadata: Option<ModelMetadata>,
}

/// The serving endpoint selects every provider API it matches, including a
/// provider that does not list this model. The longest matching path wins.
/// A model `provider.api` replaces the provider address for that row. Absent
/// rows stay in that winning set: one sealed owner with no row is terminal
/// unknown, and any other tie keeps the unknown candidate in the common
/// facts. The generic baseline is not consulted. An endpoint that matches
/// nothing returns `None`.
fn verified_metadata(
    offerings: &ModelsDevOfferings,
    adapter: AdapterKind,
    model: &CatalogModel,
    endpoint: &ApiIdentity,
) -> Option<ModelMetadata> {
    let keys = lookup_keys(&model.public_model, &model.upstream_model);
    let mut candidates = Vec::new();
    for (provider_id, provider) in &offerings.providers {
        let provider_identity = provider_level_identity(provider_id, provider);
        if let Some(row) = row_for_keys(provider, &keys) {
            let bound = row_identity(provider_id, provider, row);
            if let Some(identity) = bound.filter(|identity| identity_matches(identity, endpoint)) {
                candidates.push(EndpointCandidate {
                    identity,
                    provider_id: provider_id.clone(),
                    metadata: Some(row.metadata.clone()),
                });
            } else if let Some(identity) =
                provider_identity.filter(|identity| identity_matches(identity, endpoint))
            {
                // The row is served at its own API, so this provider address
                // does not carry it.
                candidates.push(EndpointCandidate {
                    identity,
                    provider_id: provider_id.clone(),
                    metadata: None,
                });
            }
        } else if let Some(identity) =
            provider_identity.filter(|identity| identity_matches(identity, endpoint))
        {
            candidates.push(EndpointCandidate {
                identity,
                provider_id: provider_id.clone(),
                metadata: None,
            });
        }
    }
    if candidates.is_empty() {
        return None;
    }
    let best = candidates
        .iter()
        .map(|candidate| path_specificity(&candidate.identity.path))
        .max()
        .unwrap_or(0);
    let best_hits: Vec<EndpointCandidate> = candidates
        .into_iter()
        .filter(|candidate| path_specificity(&candidate.identity.path) == best)
        .collect();
    if best_hits.len() == 1 {
        return Some(
            best_hits
                .into_iter()
                .next()
                .unwrap()
                .metadata
                .unwrap_or_default(),
        );
    }
    let same_api = best_hits
        .iter()
        .all(|hit| hit.identity == best_hits[0].identity);
    if same_api && let Some(metadata) = sealed_choice(adapter, &best_hits) {
        return Some(metadata);
    }
    Some(model_metadata::common(
        &best_hits
            .iter()
            .map(|hit| hit.metadata.clone().unwrap_or_default())
            .collect::<Vec<_>>(),
    ))
}

fn sealed_choice(adapter: AdapterKind, hits: &[EndpointCandidate]) -> Option<ModelMetadata> {
    if adapter == AdapterKind::Http {
        return None;
    }
    let ids = SEALED_PROVIDER_IDS
        .iter()
        .find(|(kind, _)| *kind == adapter)
        .map(|(_, ids)| *ids)
        .unwrap_or(&[]);
    let matched: Vec<&EndpointCandidate> = hits
        .iter()
        .filter(|hit| ids.contains(&hit.provider_id.as_str()))
        .collect();
    // The sealed id owns this API. A missing row there is unknown.
    (matched.len() == 1).then(|| matched[0].metadata.clone().unwrap_or_default())
}

fn row_for_keys<'a>(
    provider: &'a ModelsDevProvider,
    keys: &[String],
) -> Option<&'a ModelsDevModelRow> {
    keys.iter().find_map(|key| provider.models.get(key))
}

fn row_identity(
    provider_id: &str,
    provider: &ModelsDevProvider,
    row: &ModelsDevModelRow,
) -> Option<ApiIdentity> {
    if let Some(api) = &row.api {
        return Some(api.clone());
    }
    provider_level_identity(provider_id, provider)
}

fn provider_level_identity(provider_id: &str, provider: &ModelsDevProvider) -> Option<ApiIdentity> {
    if let Some(api) = &provider.api {
        return Some(api.clone());
    }
    first_party_origin(provider_id)
}

fn first_party_origin(provider_id: &str) -> Option<ApiIdentity> {
    FIRST_PARTY_ORIGINS
        .iter()
        .find(|(_, _, _, id)| *id == provider_id)
        .map(|(scheme, host, port, _)| ApiIdentity {
            scheme: (*scheme).to_string(),
            host: (*host).to_string(),
            port: *port,
            path: String::new(),
        })
}

fn generic_baseline(
    offerings: &ModelsDevOfferings,
    public_model: &str,
    upstream_model: &str,
) -> Option<ModelMetadata> {
    for key in lookup_keys(public_model, upstream_model) {
        let group = rows_with_id(offerings, &key);
        if !group.is_empty() {
            return Some(resolve_exact_group(offerings, &group));
        }
        if let Some(metadata) = namespace_metadata(offerings, &key) {
            return Some(metadata);
        }
    }
    None
}

fn rows_with_id<'a>(
    offerings: &'a ModelsDevOfferings,
    id: &str,
) -> Vec<(&'a str, &'a ModelsDevModelRow)> {
    let mut rows = Vec::new();
    for (provider_id, provider) in &offerings.providers {
        if let Some(row) = provider.models.get(id) {
            rows.push((provider_id.as_str(), row));
        }
    }
    rows
}

/// Canonical links on this exact id. A single existing target is the
/// baseline. A dangling target, conflicting links, or no usable link keeps
/// the conservative aggregate of the rows that actually use this id.
fn resolve_exact_group(
    offerings: &ModelsDevOfferings,
    group: &[(&str, &ModelsDevModelRow)],
) -> ModelMetadata {
    let mut agreed: Option<(String, String)> = None;
    for (_, row) in group {
        let Some(raw) = row.canonical_model_id.as_deref() else {
            continue;
        };
        let Some(parsed) = parse_canonical(raw) else {
            continue;
        };
        if let Some(existing) = &agreed {
            if existing != &parsed {
                return common_group(group);
            }
        } else {
            agreed = Some(parsed);
        }
    }
    if let Some((provider, model)) = agreed
        && let Some(target) = offerings
            .providers
            .get(&provider)
            .and_then(|provider| provider.models.get(&model))
    {
        return target.metadata.clone();
    }
    common_group(group)
}

fn common_group(group: &[(&str, &ModelsDevModelRow)]) -> ModelMetadata {
    model_metadata::common(
        &group
            .iter()
            .map(|(_, row)| row.metadata.clone())
            .collect::<Vec<_>>(),
    )
}

/// `provider/model` when that exact catalog row exists. The first slash
/// separates the provider id; the model id may itself contain slashes.
/// This does not walk the target's own canonical link.
fn namespace_metadata(offerings: &ModelsDevOfferings, id: &str) -> Option<ModelMetadata> {
    let (provider, model) = parse_canonical(id)?;
    offerings
        .providers
        .get(&provider)
        .and_then(|provider| provider.models.get(&model))
        .map(|row| row.metadata.clone())
}

fn parse_canonical(value: &str) -> Option<(String, String)> {
    let (provider, model) = value.split_once('/')?;
    if provider.is_empty() || model.is_empty() {
        return None;
    }
    Some((provider.to_string(), model.to_string()))
}

fn parse_http_identity(raw: &str) -> Option<ApiIdentity> {
    let url = reqwest::Url::parse(raw.trim()).ok()?;
    if !url.username().is_empty() || url.password().is_some() {
        return None;
    }
    if url.scheme() != "http" && url.scheme() != "https" {
        return None;
    }
    let host = url.host_str()?.to_ascii_lowercase();
    if host.is_empty() {
        return None;
    }
    let port = url.port_or_known_default()?;
    Some(ApiIdentity {
        scheme: url.scheme().to_ascii_lowercase(),
        host,
        port,
        path: normalize_path(url.path()),
    })
}

fn normalize_path(path: &str) -> String {
    path.split('/')
        .filter(|segment| !segment.is_empty())
        .collect::<Vec<_>>()
        .join("/")
}

fn identity_matches(api: &ApiIdentity, endpoint: &ApiIdentity) -> bool {
    api.scheme == endpoint.scheme
        && api.host == endpoint.host
        && api.port == endpoint.port
        && path_is_boundary_prefix(&api.path, &endpoint.path)
}

fn path_is_boundary_prefix(api_path: &str, endpoint_path: &str) -> bool {
    if api_path.is_empty() || api_path == endpoint_path {
        return true;
    }
    endpoint_path.starts_with(api_path)
        && endpoint_path.as_bytes().get(api_path.len()) == Some(&b'/')
}

fn path_specificity(path: &str) -> usize {
    if path.is_empty() {
        0
    } else {
        path.split('/').count()
    }
}

fn parse_api_identity(value: Option<&Value>) -> Option<ApiIdentity> {
    let raw = value?.as_str()?;
    parse_http_identity(raw)
}

fn empty_current() -> ModelsDevCatalog {
    ModelsDevCatalog {
        offerings: Some(ModelsDevOfferings {
            version: OFFERINGS_VERSION,
            providers: BTreeMap::new(),
        }),
        ..ModelsDevCatalog::default()
    }
}

/// models.dev `api.json` is providers → models. Each provider keeps its own
/// rows. The flat `models` map remains the conservative aggregate so an older
/// reader still has one metadata record per id.
pub(crate) fn parse_api(bytes: &[u8]) -> ModelsDevCatalog {
    let Ok(value) = serde_json::from_slice::<Value>(bytes) else {
        return empty_current();
    };
    let Some(providers) = value.as_object() else {
        return empty_current();
    };
    let mut offerings = ModelsDevOfferings {
        version: OFFERINGS_VERSION,
        providers: BTreeMap::new(),
    };
    let mut flat_rows = Vec::new();
    for (provider_id, provider_value) in providers {
        if provider_id.is_empty() || flat_rows.len() >= MAX_ROWS {
            continue;
        }
        let Some(provider) = provider_value.as_object() else {
            continue;
        };
        let Some(models) = provider.get("models").and_then(Value::as_object) else {
            continue;
        };
        let api = parse_api_identity(provider.get("api"));
        let mut parsed_models = BTreeMap::new();
        for (id, row) in models {
            if flat_rows.len() >= MAX_ROWS {
                break;
            }
            let Some(object) = row.as_object() else {
                continue;
            };
            let mut owned = object.clone();
            owned.entry("id").or_insert_with(|| json!(id));
            trim_modalities(&mut owned);
            let row_value = Value::Object(owned);
            let Some((parsed_id, metadata)) = model_metadata::metadata_from_catalog_row(&row_value)
            else {
                continue;
            };
            let canonical = object
                .get("canonical_model_id")
                .and_then(Value::as_str)
                .filter(|value| !value.is_empty())
                .map(str::to_owned);
            // models.dev puts a per-model address at `provider.api` inside
            // the model object. A top-level `api` string is not that field.
            let model_api = object.get("provider").and_then(|value| value.get("api"));
            parsed_models.insert(
                parsed_id,
                ModelsDevModelRow {
                    metadata,
                    api: parse_api_identity(model_api),
                    canonical_model_id: canonical,
                },
            );
            flat_rows.push(row_value);
        }
        // Keep an API that publishes no models. Its path can still be the
        // most specific match, and that match is terminal unknown.
        let known_endpoint = api.is_some() || first_party_origin(provider_id).is_some();
        if parsed_models.is_empty() && !known_endpoint {
            continue;
        }
        offerings.providers.insert(
            provider_id.clone(),
            ModelsDevProvider {
                api,
                models: parsed_models,
            },
        );
    }
    let envelope = json!({ "data": flat_rows });
    let models = model_metadata::parse_catalog_limit(
        &serde_json::to_vec(&envelope).unwrap_or_default(),
        MAX_ROWS,
    );
    ModelsDevCatalog {
        fetched_at: None,
        models,
        offerings: Some(offerings),
    }
}

/// Drop unsupported modality entries at the boundary. An array that becomes
/// empty is removed entirely: unknown, not a fabricated `["text"]`.
fn trim_modalities(row: &mut serde_json::Map<String, Value>) {
    let Some(modalities) = row.get_mut("modalities").and_then(Value::as_object_mut) else {
        return;
    };
    for key in ["input", "output"] {
        let Some(list) = modalities.get_mut(key).and_then(Value::as_array_mut) else {
            continue;
        };
        list.retain(|item| {
            item.as_str()
                .is_some_and(|value| SUPPORTED_MODALITIES.contains(&value))
        });
        if list.is_empty() {
            modalities.remove(key);
        }
    }
}

async fn fetch_catalog_at(
    client: reqwest::Client,
    source_url: &str,
) -> Result<ModelsDevCatalog, String> {
    let response = tokio::time::timeout(
        std::time::Duration::from_secs(FETCH_TIMEOUT_SECS),
        client
            .get(source_url)
            .header(reqwest::header::ACCEPT, "application/json")
            .timeout(std::time::Duration::from_secs(FETCH_TIMEOUT_SECS))
            .send(),
    )
    .await
    .map_err(|_| "models.dev catalog refresh timed out".to_string())?
    .map_err(|error| format!("models.dev catalog request failed: {error}"))?;
    let status = response.status();
    if !status.is_success() {
        return Err(format!(
            "models.dev catalog upstream returned HTTP {}",
            status.as_u16()
        ));
    }
    if response
        .content_length()
        .is_some_and(|length| length > MAX_BODY_BYTES as u64)
    {
        return Err("models.dev catalog response is too large".to_string());
    }
    let mut body = Vec::new();
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|error| format!("models.dev catalog body failed: {error}"))?;
        if body.len().saturating_add(chunk.len()) > MAX_BODY_BYTES {
            return Err("models.dev catalog response is too large".to_string());
        }
        body.extend_from_slice(&chunk);
    }
    let mut catalog = parse_api(&body);
    if !has_usable_offerings(&catalog) {
        return Err("models.dev catalog response has no usable offerings".to_string());
    }
    catalog.fetched_at = Some(Utc::now());
    Ok(catalog)
}

fn has_usable_offerings(catalog: &ModelsDevCatalog) -> bool {
    catalog.offerings.as_ref().is_some_and(|offerings| {
        offerings
            .providers
            .values()
            .any(|provider| !provider.models.is_empty())
    })
}

pub async fn fetch_catalog(config: &AppConfig) -> Result<ModelsDevCatalog, String> {
    let client = crate::http_client::build_no_redirect(config)
        .map_err(|error| format!("failed to build models.dev catalog client: {error}"))?;
    fetch_catalog_at(client, MODELSDEV_SOURCE_URL).await
}

/// Lazy background refresh: the current request keeps serving the cached
/// catalog. At most one fetch runs; failures wait out `RETRY_AFTER` before
/// the next attempt, and a stale cache keeps serving in the meantime.
pub(crate) fn ensure_fresh(state: &CoreState) {
    let now = Utc::now();
    if state.modelsdev_catalog().is_fresh(now) {
        return;
    }
    {
        let last_attempt = state.modelsdev_last_attempt.lock();
        if last_attempt.is_some_and(|attempt| now - attempt < RETRY_AFTER) {
            return;
        }
    }
    let Ok(guard) = state.modelsdev_refresh.clone().try_lock_owned() else {
        return;
    };
    *state.modelsdev_last_attempt.lock() = Some(now);
    let state = Arc::clone(state);
    tokio::spawn(async move {
        let _guard = guard;
        let config = state.config.lock().clone();
        let fetched = fetch_catalog(&config).await;
        if let Err(error) = &fetched {
            tracing::warn!("models.dev catalog refresh failed: {error}");
        }
        // A failed fetch, including an unusable HTTP 200 body, leaves the
        // live cache and the stored copy in place.
        install_refresh(&state.db, &state.modelsdev_catalog, fetched);
    });
}

#[cfg(test)]
mod tests;
