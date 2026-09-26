//! Route-bound model metadata. Directory reads are local; discovery and explicit
//! operator declarations are the only writers. Unknown facts stay absent.

use crate::db::Database;
use ocg_domain::destination::{CatalogModel, Destination};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::BTreeMap;

const SETTING_KEY: &str = "model_metadata_v1";
const MAX_TOKENS: u64 = 9_007_199_254_740_991;
pub(crate) const EFFORTS: &[&str] = &["off", "minimal", "low", "medium", "high", "xhigh", "max"];

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ModelMetadata {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_window: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_output_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_modalities: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_modalities: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<bool>,
    /// Exact DSH selector level -> Chat Completions reasoning_effort spelling.
    /// Absence is unknown, an empty map explicitly offers no selectable levels.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_efforts: Option<BTreeMap<String, String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_calling: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parallel_tool_calls: Option<bool>,
}

impl ModelMetadata {
    pub(crate) fn redact_secret(&mut self, secret: &str) {
        if !secret.is_empty()
            && serde_json::to_string(self).is_ok_and(|encoded| encoded.contains(secret))
        {
            *self = Self::default();
        }
    }

    pub(crate) fn validate(&self) -> Result<(), String> {
        if [self.context_window, self.max_output_tokens]
            .into_iter()
            .flatten()
            .any(|n| n == 0 || n > MAX_TOKENS)
        {
            return Err("token limits must be positive safe integers".into());
        }
        if matches!((self.context_window, self.max_output_tokens), (Some(c), Some(o)) if o > c) {
            return Err("maximum output must not exceed the context window".into());
        }
        if self.name.as_ref().is_some_and(|s| {
            s.trim().is_empty() || s.len() > 200 || s.chars().any(char::is_control)
        }) {
            return Err("invalid model display name".into());
        }
        for modalities in [&self.input_modalities, &self.output_modalities]
            .into_iter()
            .flatten()
        {
            let unique: std::collections::BTreeSet<_> = modalities.iter().collect();
            if modalities.is_empty()
                || unique.len() != modalities.len()
                || modalities
                    .iter()
                    .any(|m| !["text", "image", "audio", "video"].contains(&m.as_str()))
            {
                return Err("modalities must be a nonempty distinct supported list".into());
            }
        }
        if let Some(efforts) = &self.reasoning_efforts {
            if efforts.len() > EFFORTS.len()
                || efforts.iter().any(|(k, v)| {
                    !EFFORTS.contains(&k.as_str())
                        || v.is_empty()
                        || v.len() > 32
                        || !v
                            .chars()
                            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
                })
            {
                return Err("invalid reasoning effort or wire spelling".into());
            }
            if self.reasoning == Some(false) && !efforts.is_empty() {
                return Err("non-reasoning models cannot declare reasoning efforts".into());
            }
        }
        if self.tool_calling == Some(false) && self.parallel_tool_calls == Some(true) {
            return Err("parallel tool calls require tool calling".into());
        }
        Ok(())
    }
}

/// Deliberately stores only whitelisted facts, never raw upstream JSON.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct Record {
    destination_id: String,
    public_model: String,
    binding: String,
    observed: Option<ModelMetadata>,
    declared: Option<ModelMetadata>,
}

pub(crate) fn load(db: &Database) -> anyhow::Result<Vec<Record>> {
    db.get_setting(SETTING_KEY)?
        .map(|s| serde_json::from_str(&s).map_err(Into::into))
        .transpose()
        .map(Option::unwrap_or_default)
}

fn save(db: &Database, records: &[Record]) -> anyhow::Result<()> {
    db.set_setting(SETTING_KEY, &serde_json::to_string(records)?)
}

// Route identity, not model-name inference. Changing a route invalidates old
// observations and overrides. Credentials are deliberately not serialized.
fn binding(destination: &Destination, model: &CatalogModel) -> String {
    json!([
        destination.adapter,
        destination.base_url,
        destination.auth_scheme,
        destination.protocol_routes,
        destination.protocols,
        model.public_model,
        model.upstream_model,
        model.upstream_override,
        model.protocols,
        model.preferred,
        destination.model_resolution
    ])
    .to_string()
}

pub(crate) fn effective(
    records: &[Record],
    destination: &Destination,
    model: &CatalogModel,
) -> (ModelMetadata, &'static str) {
    let key = binding(destination, model);
    let record = records.iter().find(|r| {
        r.destination_id == destination.id
            && r.public_model == model.public_model
            && r.binding == key
    });
    match record {
        Some(r) if r.declared.is_some() => (r.declared.clone().unwrap_or_default(), "operator"),
        Some(r) if r.observed.is_some() => (r.observed.clone().unwrap_or_default(), "upstream"),
        _ => (ModelMetadata::default(), "unknown"),
    }
}

pub(crate) fn declare(
    db: &Database,
    destination: &Destination,
    model: &CatalogModel,
    metadata: Option<ModelMetadata>,
) -> anyhow::Result<()> {
    if let Some(m) = &metadata {
        m.validate().map_err(anyhow::Error::msg)?;
    }
    let mut records = load(db)?;
    let record = record_for(&mut records, destination, model);
    record.declared = metadata;
    save(db, &records)
}

fn record_for<'a>(
    records: &'a mut Vec<Record>,
    destination: &Destination,
    model: &CatalogModel,
) -> &'a mut Record {
    let key = binding(destination, model);
    let index = match records
        .iter()
        .position(|r| r.destination_id == destination.id && r.public_model == model.public_model)
    {
        Some(index) => index,
        None => {
            records.push(Record {
                destination_id: destination.id.clone(),
                public_model: model.public_model.clone(),
                binding: key.clone(),
                observed: None,
                declared: None,
            });
            records.len() - 1
        }
    };
    let record = &mut records[index];
    if record.binding != key {
        record.binding = key;
        record.observed = None;
        record.declared = None;
    }
    record
}

/// Called under the settings/CAS lock after successful discovery. No grants,
/// route switches or inference attempts are changed here.
pub(crate) fn observe(
    db: &Database,
    destination: &Destination,
    metadata: &BTreeMap<String, ModelMetadata>,
) -> anyhow::Result<()> {
    let mut records = load(db)?;
    for model in &destination.catalog {
        // A model-specific route was not queried by destination discovery.
        if model.upstream_override.is_some() {
            continue;
        }
        if let Some(value) = metadata.get(&model.upstream_model) {
            record_for(&mut records, destination, model).observed = Some(value.clone());
        }
    }
    save(db, &records)
}

/// Normalize explicit directory facts. No model-family guessing and no I/O.
pub(crate) fn parse_catalog(bytes: &[u8]) -> BTreeMap<String, ModelMetadata> {
    let Ok(value) = serde_json::from_slice::<Value>(bytes) else {
        return BTreeMap::new();
    };
    let Some(rows) = value.get("data").and_then(Value::as_array) else {
        return BTreeMap::new();
    };
    let mut result = BTreeMap::new();
    for row in rows.iter().take(1000) {
        let Some(id) = row.get("id").and_then(Value::as_str) else {
            continue;
        };
        let Ok(id) = crate::provider::validate_custom_model_id(id) else {
            continue;
        };
        if id.chars().any(char::is_control) {
            continue;
        }
        let mut metadata = ModelMetadata::default();
        let ext = row
            .get("ocg")
            .filter(|v| v.get("schemaVersion").and_then(Value::as_u64) == Some(1));
        let source = ext.unwrap_or(row);
        let number = |keys: &[&str]| {
            keys.iter().find_map(|k| {
                source
                    .pointer(k)
                    .and_then(Value::as_u64)
                    .filter(|n| *n > 0 && *n <= MAX_TOKENS)
            })
        };
        metadata.context_window = number(&[
            "/contextWindow",
            "/context_length",
            "/context_window",
            "/limit/context",
        ])
        .or_else(|| {
            row.get("contextWindow")
                .and_then(Value::as_u64)
                .filter(|n| *n > 0 && *n <= MAX_TOKENS)
        });
        metadata.max_output_tokens = number(&[
            "/maxOutputTokens",
            "/maxTokens",
            "/max_output_tokens",
            "/max_completion_tokens",
            "/limit/output",
        ])
        .or_else(|| {
            row.get("maxTokens")
                .and_then(Value::as_u64)
                .filter(|n| *n > 0 && *n <= MAX_TOKENS)
        });
        metadata.name = source
            .get("name")
            .or_else(|| row.get("name"))
            .and_then(Value::as_str)
            .map(str::to_owned);
        metadata.input_modalities = modalities(
            source
                .get("inputModalities")
                .or_else(|| source.pointer("/modalities/input"))
                .or_else(|| source.get("input")),
        );
        metadata.output_modalities = modalities(
            source
                .get("outputModalities")
                .or_else(|| source.pointer("/modalities/output")),
        );
        metadata.reasoning = source
            .get("reasoning")
            .and_then(Value::as_bool)
            .or_else(|| {
                source
                    .pointer("/reasoning/supported")
                    .and_then(Value::as_bool)
            });
        metadata.tool_calling = source
            .get("toolCalling")
            .or_else(|| source.get("tool_call"))
            .and_then(Value::as_bool);
        metadata.parallel_tool_calls = source.get("parallelToolCalls").and_then(Value::as_bool);
        let efforts = source
            .get("reasoningEfforts")
            .or_else(|| source.get("reasoning_efforts"))
            .or_else(|| source.pointer("/reasoning/efforts"));
        if let Some(efforts) = efforts {
            let parsed = if let Some(values) = efforts.as_array() {
                values
                    .iter()
                    .map(|v| v.as_str().map(|s| (s.to_string(), s.to_string())))
                    .collect::<Option<BTreeMap<_, _>>>()
            } else if let Some(values) = efforts.as_object() {
                values
                    .iter()
                    .map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string())))
                    .collect()
            } else {
                None
            };
            metadata.reasoning_efforts = parsed;
        }
        if metadata.validate().is_err() {
            // A malformed new declaration withdraws old facts for this ID.
            metadata = ModelMetadata::default();
        }
        // Duplicate rows are not authoritative. Keep only common guarantees.
        result
            .entry(id)
            .and_modify(|old: &mut ModelMetadata| *old = common(&[old.clone(), metadata.clone()]))
            .or_insert(metadata);
    }
    result
}

fn modalities(value: Option<&Value>) -> Option<Vec<String>> {
    value?
        .as_array()?
        .iter()
        .map(|v| v.as_str().map(str::to_owned))
        .collect()
}

/// Only publish facts every potential fallback can honor. An unknown route
/// blocks a positive claim; effort wire spellings must agree as well.
pub(crate) fn common(models: &[ModelMetadata]) -> ModelMetadata {
    let Some(first) = models.first() else {
        return ModelMetadata::default();
    };
    let minimum = |get: fn(&ModelMetadata) -> Option<u64>| {
        models
            .iter()
            .map(get)
            .collect::<Option<Vec<_>>>()
            .and_then(|v| v.into_iter().min())
    };
    let shared_list = |get: fn(&ModelMetadata) -> &Option<Vec<String>>| {
        let lists = models
            .iter()
            .map(|m| get(m).as_ref())
            .collect::<Option<Vec<_>>>()?;
        let values: Vec<_> = lists[0]
            .iter()
            .filter(|v| lists.iter().all(|l| l.contains(v)))
            .cloned()
            .collect();
        Some(values)
    };
    let shared_bool = |get: fn(&ModelMetadata) -> Option<bool>| {
        if models.iter().any(|m| get(m) == Some(false)) {
            Some(false)
        } else if models.iter().all(|m| get(m) == Some(true)) {
            Some(true)
        } else {
            None
        }
    };
    let efforts = models
        .iter()
        .map(|m| m.reasoning_efforts.as_ref())
        .collect::<Option<Vec<_>>>()
        .map(|maps| {
            maps[0]
                .iter()
                .filter(|(k, v)| maps.iter().all(|map| map.get(*k) == Some(*v)))
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect()
        });
    ModelMetadata {
        name: first
            .name
            .clone()
            .filter(|n| models.iter().all(|m| m.name.as_ref() == Some(n))),
        context_window: minimum(|m| m.context_window),
        max_output_tokens: minimum(|m| m.max_output_tokens),
        input_modalities: shared_list(|m| &m.input_modalities),
        output_modalities: shared_list(|m| &m.output_modalities),
        reasoning: shared_bool(|m| m.reasoning),
        reasoning_efforts: efforts,
        tool_calling: shared_bool(|m| m.tool_calling),
        parallel_tool_calls: shared_bool(|m| m.parallel_tool_calls),
    }
}

pub(crate) fn enrich(
    db: &Database,
    snapshot: &crate::gateway::handler::RuntimeCatalogSnapshot,
    rows: &mut [Value],
) -> anyhow::Result<()> {
    let records = load(db)?;
    for row in rows {
        let Some(id) = row.get("id").and_then(Value::as_str) else {
            continue;
        };
        let Ok(resolved) = snapshot.resolve(id) else {
            continue;
        };
        let mut candidates = Vec::new();
        let mut sources = std::collections::BTreeSet::new();
        for destination in &snapshot.routing.projection.destinations {
            if !destination.enabled {
                continue;
            }
            for model in &destination.catalog {
                if model.enabled
                    && !model.protocols.is_empty()
                    && crate::gateway::materialize::resolved_contains_model(
                        &resolved,
                        destination,
                        model,
                        id,
                    )
                {
                    let (metadata, source) = effective(&records, destination, model);
                    candidates.push(metadata);
                    sources.insert(source);
                }
            }
        }
        let metadata = common(&candidates);
        if let Some(n) = metadata.context_window {
            row["contextWindow"] = json!(n);
        }
        if let Some(n) = metadata.max_output_tokens {
            row["maxTokens"] = json!(n);
        }
        if let Some(name) = &metadata.name {
            row["name"] = json!(name);
        }
        let mut extension = serde_json::to_value(&metadata)?;
        extension["schemaVersion"] = json!(1);
        extension["sources"] = json!(sources);
        extension["clientProtocol"] = json!("openai-completions");
        extension["status"] = json!(if metadata == ModelMetadata::default() {
            "unknown"
        } else {
            "declared"
        });
        row["ocg"] = extension;
    }
    Ok(())
}

#[cfg(test)]
mod tests;
