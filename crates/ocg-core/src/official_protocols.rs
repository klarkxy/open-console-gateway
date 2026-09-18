//! Official-docs protocol baselines for OpenCode Go, Zen Free, and Command Code.
//!
//! Catalog refresh fetches the documented pages on that explicit user action.
//! A failed fetch or omitted model supplies no new protocol evidence.
//! Existing evidence survives. Zen Free reuses the Go endpoint table and looks up the
//! paid id (strip `-free`).

use anyhow::{Result, anyhow, bail};
use std::collections::BTreeMap;

use crate::kernel::ids::{
    COMMAND_CODE_PROVIDER_ID, OPENCODE_PROVIDER_ID, OPENCODE_ZEN_FREE_PROVIDER_ID,
    normalize_model_name,
};
use crate::models::AppConfig;
use crate::pricing::{
    collapse_whitespace, extract_tables, fetch_approved_host_html, has_headers, strip_tags,
};
use crate::provider::UpstreamProtocolKind;

pub const GO_PROTOCOL_DOCS_URL: &str = crate::kernel::pricing::SOURCE_URL;
pub const COMMAND_CODE_PROTOCOL_DOCS_URL: &str = "https://commandcode.ai/docs/provider";
const GO_PROTOCOL_DOCS_HOST: &str = "opencode.ai";
const COMMAND_CODE_PROTOCOL_DOCS_HOST: &str = "commandcode.ai";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OfficialProtocolBaseline {
    /// Per-model mapping parsed from an official endpoint table.
    Mapped(BTreeMap<String, UpstreamProtocolKind>),
    /// Command Code provider docs state the Anthropic/Chat family split.
    FamilyRule,
    /// Fetch or parse failed; preserve saved evidence instead of guessing a protocol.
    Unavailable,
}

impl OfficialProtocolBaseline {
    pub fn mapped(
        pairs: impl IntoIterator<Item = (impl Into<String>, UpstreamProtocolKind)>,
    ) -> Self {
        Self::Mapped(
            pairs
                .into_iter()
                .map(|(id, protocol)| (normalize_model_name(&id.into()), protocol))
                .filter(|(id, _)| !id.is_empty())
                .collect(),
        )
    }

    /// Only protocols explicitly described by this document are evidence.
    pub fn protocol_for(&self, provider_id: &str, model_id: &str) -> Option<UpstreamProtocolKind> {
        if model_id.trim().is_empty()
            || (provider_id == COMMAND_CODE_PROVIDER_ID
                && model_id.eq_ignore_ascii_case("stealth/ox-alpha"))
        {
            return None;
        }
        match self {
            Self::Mapped(map) => lookup_mapped(map, model_id).or_else(|| {
                (provider_id == OPENCODE_ZEN_FREE_PROVIDER_ID)
                    .then(|| lookup_mapped(map, &strip_zen_free_suffix(model_id)))
                    .flatten()
            }),
            Self::FamilyRule if provider_id == COMMAND_CODE_PROVIDER_ID => {
                ocg_domain::protocol::command_code_preferred_format(model_id).map(api_to_upstream)
            }
            Self::FamilyRule | Self::Unavailable => None,
        }
    }
}

/// A reviewed per-model offline default, never a model-directory whitelist.
/// Unknown IDs have no default. A known explicitly unsupported row stays unknown.
pub(crate) fn known_opencode_default(
    model_id: &str,
    zen_free: bool,
) -> Option<UpstreamProtocolKind> {
    if model_id.trim().is_empty() || (zen_free && !crate::kernel::ids::is_free_model(model_id)) {
        return None;
    }
    let paid_id = if zen_free {
        strip_zen_free_suffix(model_id)
    } else {
        model_id.to_string()
    };
    let profile = crate::kernel::protocol::model_protocol(model_id)
        .or_else(|| crate::kernel::protocol::model_protocol(&paid_id))?;
    if profile.supported.is_empty() {
        return None;
    }
    Some(api_to_upstream(profile.preferred))
}

pub fn uses_official_docs_protocol_baseline(provider_id: &str) -> bool {
    matches!(
        provider_id,
        OPENCODE_PROVIDER_ID | OPENCODE_ZEN_FREE_PROVIDER_ID | COMMAND_CODE_PROVIDER_ID
    )
}

pub async fn fetch_official_protocol_baseline(
    config: &AppConfig,
    provider_id: &str,
    process_generation: u64,
) -> OfficialProtocolBaseline {
    #[cfg(debug_assertions)]
    if let Some(fetch) = official_protocol_fetch::override_for(process_generation) {
        return fetch(provider_id);
    }
    let _ = process_generation;
    match provider_id {
        OPENCODE_PROVIDER_ID | OPENCODE_ZEN_FREE_PROVIDER_ID => {
            match fetch_go_official_protocols(config).await {
                Ok(map) => OfficialProtocolBaseline::Mapped(map),
                Err(_) => OfficialProtocolBaseline::Unavailable,
            }
        }
        COMMAND_CODE_PROVIDER_ID => match fetch_command_code_official_protocols(config).await {
            Ok(baseline) => baseline,
            Err(_) => OfficialProtocolBaseline::Unavailable,
        },
        _ => OfficialProtocolBaseline::Unavailable,
    }
}

async fn fetch_go_official_protocols(
    config: &AppConfig,
) -> Result<BTreeMap<String, UpstreamProtocolKind>> {
    let html = fetch_approved_host_html(
        config,
        GO_PROTOCOL_DOCS_URL,
        GO_PROTOCOL_DOCS_HOST,
        "OpenCode Go protocol docs",
    )
    .await?;
    parse_go_official_protocols(&html)
}

async fn fetch_command_code_official_protocols(
    config: &AppConfig,
) -> Result<OfficialProtocolBaseline> {
    let html = fetch_approved_host_html(
        config,
        COMMAND_CODE_PROTOCOL_DOCS_URL,
        COMMAND_CODE_PROTOCOL_DOCS_HOST,
        "Command Code protocol docs",
    )
    .await?;
    parse_command_code_official_protocols(&html)
}

pub fn parse_go_official_protocols(html: &str) -> Result<BTreeMap<String, UpstreamProtocolKind>> {
    let tables = extract_tables(html)?;
    let endpoint_table = tables
        .iter()
        .find(|table| {
            table_has_model_and_endpoint(table)
                && (has_headers(table, &["model", "model id", "endpoint", "ai sdk package"])
                    || model_id_column_index(table).is_some())
        })
        .ok_or_else(|| anyhow!("OpenCode Go endpoint table was not found"))?;
    let id_index = model_id_column_index(endpoint_table).unwrap_or(1);
    let map = parse_endpoint_protocol_rows(endpoint_table, id_index)?;
    if map.is_empty() {
        bail!("OpenCode Go endpoint table contains no recognized protocols");
    }
    Ok(map)
}

pub fn parse_command_code_official_protocols(html: &str) -> Result<OfficialProtocolBaseline> {
    let tables = extract_tables(html).unwrap_or_default();
    if let Some(table) = tables
        .iter()
        .find(|table| table_has_model_and_endpoint(table))
    {
        let id_index = model_id_column_index(table).unwrap_or(0);
        if let Ok(map) = parse_endpoint_protocol_rows(table, id_index)
            && !map.is_empty()
        {
            return Ok(OfficialProtocolBaseline::Mapped(map));
        }
    }
    let plain = collapse_whitespace(&strip_tags(html)).to_ascii_lowercase();
    if looks_like_command_code_provider_docs(&plain) {
        return Ok(OfficialProtocolBaseline::FamilyRule);
    }
    bail!("Command Code official protocol documentation was not recognized")
}

fn table_has_model_and_endpoint(table: &[Vec<String>]) -> bool {
    let Some(headers) = table.first() else {
        return false;
    };
    let normalized: Vec<String> = headers
        .iter()
        .map(|cell| {
            cell.trim()
                .trim_end_matches('↕')
                .trim()
                .to_ascii_lowercase()
        })
        .collect();
    let has_model = normalized.iter().any(|cell| cell.contains("model"));
    let has_endpoint = normalized.iter().any(|cell| cell.contains("endpoint"));
    has_model && has_endpoint
}

fn model_id_column_index(table: &[Vec<String>]) -> Option<usize> {
    let headers = table.first()?;
    headers.iter().position(|cell| {
        let value = cell
            .trim()
            .trim_end_matches('↕')
            .trim()
            .to_ascii_lowercase();
        value == "model id" || value == "modelid"
    })
}

fn parse_endpoint_protocol_rows(
    table: &[Vec<String>],
    id_index: usize,
) -> Result<BTreeMap<String, UpstreamProtocolKind>> {
    let endpoint_index = table
        .first()
        .and_then(|headers| {
            headers.iter().position(|cell| {
                cell.trim()
                    .trim_end_matches('↕')
                    .trim()
                    .eq_ignore_ascii_case("endpoint")
            })
        })
        .ok_or_else(|| anyhow!("official endpoint table is missing an Endpoint column"))?;
    let mut map = BTreeMap::new();
    for row in table.iter().skip(1) {
        if row.iter().all(|cell| cell.trim().is_empty()) {
            continue;
        }
        let Some(raw_id) = row.get(id_index) else {
            continue;
        };
        let id = normalize_model_name(raw_id.trim());
        if id.is_empty() {
            continue;
        }
        let Some(endpoint) = row.get(endpoint_index) else {
            continue;
        };
        let Some(protocol) = protocol_from_endpoint_url(endpoint) else {
            continue;
        };
        map.insert(id, protocol);
    }
    Ok(map)
}

pub fn protocol_from_endpoint_url(endpoint: &str) -> Option<UpstreamProtocolKind> {
    let lower = endpoint.trim().to_ascii_lowercase();
    if lower.is_empty() {
        return None;
    }
    if lower.contains("/v1/responses") || lower.ends_with("/responses") {
        Some(UpstreamProtocolKind::Responses)
    } else if lower.contains("/v1/chat/completions") || lower.contains("/chat/completions") {
        Some(UpstreamProtocolKind::ChatCompletions)
    } else if lower.contains("/v1/messages")
        || lower.ends_with("/messages")
        || lower.contains("/anthropic/v1/messages")
    {
        Some(UpstreamProtocolKind::Messages)
    } else {
        None
    }
}

fn looks_like_command_code_provider_docs(plain: &str) -> bool {
    (plain.contains("chat/completions") || plain.contains("chat completions"))
        && (plain.contains("/messages") || plain.contains("anthropic messages"))
        && (plain.contains("anthropic") || plain.contains("claude"))
}

fn strip_zen_free_suffix(model_id: &str) -> String {
    let normalized = normalize_model_name(model_id);
    normalized
        .strip_suffix("-free")
        .unwrap_or(normalized.as_str())
        .to_string()
}

fn lookup_mapped(
    map: &BTreeMap<String, UpstreamProtocolKind>,
    model_id: &str,
) -> Option<UpstreamProtocolKind> {
    let normalized = normalize_model_name(model_id);
    map.get(&normalized)
        .copied()
        .or_else(|| map.get(model_id).copied())
        .or_else(|| {
            let leaf = normalized.rsplit('/').next().unwrap_or(normalized.as_str());
            map.get(leaf).copied()
        })
}

fn api_to_upstream(format: ocg_domain::protocol::ApiFormat) -> UpstreamProtocolKind {
    match format {
        ocg_domain::protocol::ApiFormat::ChatCompletions => UpstreamProtocolKind::ChatCompletions,
        ocg_domain::protocol::ApiFormat::Responses => UpstreamProtocolKind::Responses,
        ocg_domain::protocol::ApiFormat::Messages => UpstreamProtocolKind::Messages,
        ocg_domain::protocol::ApiFormat::Gemini => UpstreamProtocolKind::ChatCompletions,
    }
}

#[cfg(debug_assertions)]
mod official_protocol_fetch {
    use super::OfficialProtocolBaseline;
    use parking_lot::Mutex;
    use std::collections::HashMap;
    use std::sync::{Arc, OnceLock};

    type OfficialFetch = Arc<dyn Fn(&str) -> OfficialProtocolBaseline + Send + Sync>;

    static OFFICIAL_FETCH_OVERRIDES: OnceLock<Mutex<HashMap<u64, OfficialFetch>>> = OnceLock::new();

    fn official_fetch_overrides() -> &'static Mutex<HashMap<u64, OfficialFetch>> {
        OFFICIAL_FETCH_OVERRIDES.get_or_init(|| Mutex::new(HashMap::new()))
    }

    pub struct OfficialProtocolFetchGuard {
        process_generation: u64,
    }

    impl Drop for OfficialProtocolFetchGuard {
        fn drop(&mut self) {
            official_fetch_overrides()
                .lock()
                .remove(&self.process_generation);
        }
    }

    pub fn install_official_protocol_fetch_for_tests(
        process_generation: u64,
        fetch: impl Fn(&str) -> OfficialProtocolBaseline + Send + Sync + 'static,
    ) -> OfficialProtocolFetchGuard {
        official_fetch_overrides()
            .lock()
            .insert(process_generation, Arc::new(fetch));
        OfficialProtocolFetchGuard { process_generation }
    }

    pub fn install_official_protocol_fetch_unavailable_for_tests(
        process_generation: u64,
    ) -> OfficialProtocolFetchGuard {
        install_official_protocol_fetch_for_tests(process_generation, |_| {
            OfficialProtocolBaseline::Unavailable
        })
    }

    pub fn override_for(process_generation: u64) -> Option<OfficialFetch> {
        official_fetch_overrides()
            .lock()
            .get(&process_generation)
            .cloned()
    }
}

#[cfg(debug_assertions)]
pub use official_protocol_fetch::{
    OfficialProtocolFetchGuard, install_official_protocol_fetch_for_tests,
    install_official_protocol_fetch_unavailable_for_tests,
};

#[cfg(test)]
mod tests;
