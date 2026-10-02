//! models.dev public catalog as the lowest-priority model metadata source.
//!
//! Fields a route never learned — no operator declaration, and no
//! upstream-observed value for that field — are filled from
//! <https://models.dev> so downstream clients (DSH) still see verified context
//! windows and modalities; operator declarations stay untouched. The
//! catalog is cached in a local setting and refreshed in the background; a
//! failed refresh keeps the previous cache and never blocks `/v1/models`.
//! Matching is exact upstream id, then its last path segment, then the exact
//! public id — never a fuzzy name guess.

use crate::db::Database;
use crate::model_metadata::ModelMetadata;
use crate::models::AppConfig;
use crate::state::CoreState;
use chrono::{DateTime, Duration, Utc};
use futures_util::StreamExt;
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

/// Modalities the OCG metadata contract (and the DSH text adapter) accepts.
/// models.dev also reports values like `pdf`; they are dropped at ingestion
/// so a downstream row can never declare an unsupported modality.
const SUPPORTED_MODALITIES: [&str; 4] = ["text", "image", "audio", "video"];

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub(crate) struct ModelsDevCatalog {
    #[serde(default)]
    pub fetched_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub models: BTreeMap<String, ModelMetadata>,
}

impl ModelsDevCatalog {
    pub(crate) fn is_fresh(&self, now: DateTime<Utc>) -> bool {
        self.fetched_at
            .is_some_and(|fetched| now - fetched < FRESH_FOR)
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

/// Exact upstream id, then its last path segment, then the exact public id.
pub(crate) fn lookup<'a>(
    catalog: &'a ModelsDevCatalog,
    public_model: &str,
    upstream_model: &str,
) -> Option<&'a ModelMetadata> {
    if let Some(found) = catalog.models.get(upstream_model) {
        return Some(found);
    }
    if let Some((_, tail)) = upstream_model.rsplit_once('/')
        && let Some(found) = catalog.models.get(tail)
    {
        return Some(found);
    }
    catalog.models.get(public_model)
}

/// models.dev `api.json` is providers → models. Flattened into the
/// `/v1/models` row shape, its `limit` / `modalities` / `tool_call` fields
/// are exactly what `model_metadata::parse_catalog` already normalizes.
/// Duplicate ids across providers merge through the same conservative
/// common-guarantee rule as duplicate upstream rows.
pub(crate) fn parse_api(bytes: &[u8]) -> BTreeMap<String, ModelMetadata> {
    let Ok(value) = serde_json::from_slice::<Value>(bytes) else {
        return BTreeMap::new();
    };
    let mut rows = Vec::new();
    if let Some(providers) = value.as_object() {
        for provider in providers.values() {
            let Some(models) = provider.get("models").and_then(Value::as_object) else {
                continue;
            };
            for (id, row) in models {
                let Some(object) = row.as_object() else {
                    continue;
                };
                let mut row = object.clone();
                row.entry("id").or_insert_with(|| json!(id));
                trim_modalities(&mut row);
                translate_reasoning_options(&mut row);
                rows.push(Value::Object(row));
            }
        }
    }
    let envelope = json!({ "data": rows });
    crate::model_metadata::parse_catalog_limit(
        &serde_json::to_vec(&envelope).unwrap_or_default(),
        MAX_ROWS,
    )
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

/// models.dev expresses thinking levels as `reasoning_options`; translate the
/// effort variant into the contract's level → wire-spelling map that
/// `parse_catalog_limit` already reads as `reasoningEfforts`. The OpenAI-family
/// wire spelling `none` fills the DSH `off` selector; values outside the
/// selector table are dropped, never invention. Toggle-only and budget-token
/// options carry no selectable wire level, so they leave efforts unknown
/// rather than fabricating a spelling.
fn translate_reasoning_options(row: &mut serde_json::Map<String, Value>) {
    let Some(options) = row.get("reasoning_options").and_then(Value::as_array) else {
        return;
    };
    let mut efforts = BTreeMap::new();
    for option in options {
        if option.get("type").and_then(Value::as_str) != Some("effort") {
            continue;
        }
        let Some(values) = option.get("values").and_then(Value::as_array) else {
            continue;
        };
        for value in values.iter().filter_map(Value::as_str) {
            let level = if value == "none" { "off" } else { value };
            if crate::model_metadata::EFFORTS.contains(&level) {
                efforts.insert(level.to_string(), value.to_string());
            }
        }
    }
    if !efforts.is_empty() {
        row.insert("reasoningEfforts".to_string(), json!(efforts));
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
    Ok(ModelsDevCatalog {
        fetched_at: Some(Utc::now()),
        models: parse_api(&body),
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
        match fetch_catalog(&config).await {
            Ok(catalog) => {
                // Persist before swapping the live copy; db is released before
                // the catalog write lock so the two are never held together.
                let saved = save(&state.db.lock(), &catalog);
                *state.modelsdev_catalog.write() = Arc::new(catalog);
                if let Err(error) = saved {
                    tracing::warn!("failed to persist models.dev catalog: {error}");
                }
            }
            Err(error) => {
                tracing::warn!("models.dev catalog refresh failed: {error}");
            }
        }
    });
}

#[cfg(test)]
mod tests;
