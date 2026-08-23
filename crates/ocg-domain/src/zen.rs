//! I/O-free OpenCode Zen Free catalog types and JSON normalization.
//!
//! HTTP refresh stays in the host crate's zen_models module. Parse and alias
//! derivation here do not touch the network, filesystem, or clocks.

use super::ids::is_free_model;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashSet;

pub const ZEN_MODELS_SOURCE_URL: &str = "https://opencode.ai/zen/v1/models";
pub(crate) const MAX_MODELS: usize = 256;
pub(crate) const MAX_MODEL_ID_CHARS: usize = 200;

const SEEDED_FREE_MODELS: &[&str] = &[
    "deepseek-v4-flash-free",
    "hy3-free",
    "laguna-s-2.1-free",
    "mimo-v2.5-free",
    "muse-spark-1.2-contributor-free",
    "nemotron-3-ultra-free",
    "nemotron-3.5-lightning-free",
    "x-preview-f-free",
];

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ZenFreeModelCatalog {
    pub models: Vec<String>,
    pub refreshed_at: Option<DateTime<Utc>>,
    pub source_url: String,
}

impl Default for ZenFreeModelCatalog {
    fn default() -> Self {
        Self {
            models: SEEDED_FREE_MODELS
                .iter()
                .map(|id| (*id).to_string())
                .collect(),
            refreshed_at: None,
            source_url: ZEN_MODELS_SOURCE_URL.to_string(),
        }
    }
}

impl ZenFreeModelCatalog {
    pub fn aliases(&self) -> Vec<String> {
        let mut aliases = Vec::with_capacity(self.models.len() * 2);
        for model in &self.models {
            aliases.push(model.clone());
            if let Some(alias) = stripped_free_alias(model) {
                aliases.push(alias.to_string());
            }
        }
        aliases.sort();
        aliases.dedup();
        aliases
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ZenFreeModelView {
    pub model_id: String,
    pub alias: String,
}

pub fn model_views(catalog: &ZenFreeModelCatalog) -> Vec<ZenFreeModelView> {
    catalog
        .models
        .iter()
        .filter_map(|model_id| {
            stripped_free_alias(model_id).map(|alias| ZenFreeModelView {
                model_id: model_id.clone(),
                alias: alias.to_string(),
            })
        })
        .collect()
}

pub fn stripped_free_alias(model: &str) -> Option<&str> {
    let alias = model.strip_suffix("-free")?;
    (!alias.is_empty()).then_some(alias)
}

pub fn parse_catalog(body: &[u8]) -> Result<Vec<String>, String> {
    let value: Value = serde_json::from_slice(body)
        .map_err(|error| format!("Zen model catalog returned invalid JSON: {error}"))?;
    let data = value
        .get("data")
        .and_then(Value::as_array)
        .ok_or_else(|| "Zen model catalog response is missing a data array".to_string())?;
    let mut seen = HashSet::new();
    let mut models = Vec::new();
    for item in data {
        let Some(id) = item.get("id").and_then(Value::as_str).map(str::trim) else {
            continue;
        };
        let normalized = id.to_ascii_lowercase();
        if !is_free_model(&normalized)
            || normalized.chars().count() > MAX_MODEL_ID_CHARS
            || normalized.chars().any(char::is_control)
            || normalized.contains('/')
            || normalized.contains('_')
            || normalized.chars().any(char::is_whitespace)
        {
            continue;
        }
        if seen.insert(normalized.clone()) {
            models.push(normalized);
            if models.len() == MAX_MODELS {
                break;
            }
        }
    }
    models.sort();
    if models.is_empty() {
        return Err("Zen model catalog contains no model IDs ending in `-free`".to_string());
    }
    Ok(models)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_keeps_only_free_suffix_and_derives_aliases() {
        let models = parse_catalog(
            br#"{"object":"list","data":[{"id":"paid"},{"id":"MIMO-V2.5-FREE"},{"id":"big-pickle"},{"id":"ox-alpha-free"},{"id":"hy3-free"},{"id":"hy3-free"}]}"#,
        )
        .unwrap();
        assert_eq!(models, vec!["hy3-free", "mimo-v2.5-free"]);
        let catalog = ZenFreeModelCatalog {
            models,
            refreshed_at: None,
            source_url: ZEN_MODELS_SOURCE_URL.to_string(),
        };
        assert_eq!(
            model_views(&catalog),
            vec![
                ZenFreeModelView {
                    model_id: "hy3-free".into(),
                    alias: "hy3".into(),
                },
                ZenFreeModelView {
                    model_id: "mimo-v2.5-free".into(),
                    alias: "mimo-v2.5".into(),
                },
            ]
        );
    }

    #[test]
    fn empty_filtered_catalog_is_rejected() {
        assert!(parse_catalog(br#"{"data":[{"id":"big-pickle"}]}"#).is_err());
    }

    #[test]
    fn oversized_free_model_ids_are_filtered() {
        let oversized = format!("{}-free", "a".repeat(MAX_MODEL_ID_CHARS));
        let body = serde_json::to_vec(&serde_json::json!({
            "data": [{"id": oversized}, {"id": "valid-free"}]
        }))
        .unwrap();
        assert_eq!(parse_catalog(&body).unwrap(), ["valid-free"]);
    }
}
