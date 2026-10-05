//! Route-bound model metadata. Directory reads are local; discovery and explicit
//! operator declarations are the only writers. Unknown facts stay absent.
//! Published protocol profiles are derived for `/v1/models` and are not stored.

use crate::db::Database;
use ocg_domain::destination::{CatalogModel, Destination, Protocol};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};

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

    /// Supply still-unknown fields from a lower-priority source, leaving every
    /// known fact untouched. Returns true when at least one field was filled.
    pub(crate) fn fill_missing(&mut self, fallback: &ModelMetadata) -> bool {
        let mut filled = false;
        if self.name.is_none() {
            self.name = fallback.name.clone();
            filled |= self.name.is_some();
        }
        if self.context_window.is_none() {
            self.context_window = fallback.context_window;
            filled |= self.context_window.is_some();
        }
        if self.max_output_tokens.is_none() {
            self.max_output_tokens = fallback.max_output_tokens;
            filled |= self.max_output_tokens.is_some();
        }
        if self.input_modalities.is_none() {
            self.input_modalities = fallback.input_modalities.clone();
            filled |= self.input_modalities.is_some();
        }
        if self.output_modalities.is_none() {
            self.output_modalities = fallback.output_modalities.clone();
            filled |= self.output_modalities.is_some();
        }
        if self.reasoning.is_none() {
            self.reasoning = fallback.reasoning;
            filled |= self.reasoning.is_some();
        }
        if self.reasoning_efforts.is_none() {
            self.reasoning_efforts = fallback.reasoning_efforts.clone();
            filled |= self.reasoning_efforts.is_some();
        }
        if self.tool_calling.is_none() {
            self.tool_calling = fallback.tool_calling;
            filled |= self.tool_calling.is_some();
        }
        if self.parallel_tool_calls.is_none() {
            self.parallel_tool_calls = fallback.parallel_tool_calls;
            filled |= self.parallel_tool_calls.is_some();
        }
        filled
    }
}

/// Upstream protocols a published model row can actually be called with.
///
/// Derived from saved per-model preference and authorized routes. This is not
/// persisted [`ModelMetadata`], not a new recommendation table, and not a claim
/// that every supported protocol preserves every capability.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PublishedModelProtocolProfile {
    pub preferred: PublishedUpstreamProtocol,
    pub supported: Vec<PublishedUpstreamProtocol>,
}

/// Wire vocabulary for [`PublishedModelProtocolProfile`]. Local so the domain
/// protocol enum does not gain a schema dependency.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum PublishedUpstreamProtocol {
    ChatCompletions,
    Responses,
    Messages,
}

impl PublishedUpstreamProtocol {
    fn from_domain(protocol: Protocol) -> Self {
        match protocol {
            Protocol::ChatCompletions => Self::ChatCompletions,
            Protocol::Responses => Self::Responses,
            Protocol::Messages => Self::Messages,
        }
    }
}

/// Why `ocg.protocols` did not become a profile. Neither variant is Chat.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PublishedProtocolProfileError {
    /// The protocols value is missing or JSON null.
    Unknown,
    /// A protocols value is present but preferred is missing, illegal, or
    /// outside `supported`.
    Invalid,
}

impl std::fmt::Display for PublishedProtocolProfileError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unknown => formatter.write_str("published model protocol profile is unknown"),
            Self::Invalid => formatter.write_str("published model protocol profile is invalid"),
        }
    }
}

impl std::error::Error for PublishedProtocolProfileError {}

impl PublishedModelProtocolProfile {
    pub fn validate(&self) -> Result<(), PublishedProtocolProfileError> {
        let mut seen = BTreeSet::new();
        if self.supported.is_empty()
            || !self.supported.contains(&self.preferred)
            || self
                .supported
                .iter()
                .any(|protocol| !seen.insert(*protocol))
        {
            return Err(PublishedProtocolProfileError::Invalid);
        }
        Ok(())
    }
}

/// Read a derived profile from an `ocg.protocols` value.
///
/// Missing and JSON null are [`PublishedProtocolProfileError::Unknown`].
/// Illegal preferred values and a preferred protocol outside `supported` are
/// [`PublishedProtocolProfileError::Invalid`]. This does not default to Chat
/// Completions and does not check `schemaVersion`; the client checks exact 2.
pub fn read_published_protocol_profile(
    protocols: Option<&Value>,
) -> Result<PublishedModelProtocolProfile, PublishedProtocolProfileError> {
    let Some(protocols) = protocols.filter(|value| !value.is_null()) else {
        return Err(PublishedProtocolProfileError::Unknown);
    };
    let profile = PublishedModelProtocolProfile::deserialize(protocols)
        .map_err(|_| PublishedProtocolProfileError::Invalid)?;
    profile.validate()?;
    Ok(profile)
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

/// Effective facts with the models.dev catalog filling fields the route never
/// learned. Per-field priority is operator declaration, upstream observation,
/// models.dev, then unknown. A public-catalog hit supplies only absent facts and
/// never overrides a route-specific one, and an operator declaration stays a
/// whole-record replacement. The bool reports whether models.dev contributed at
/// least one field, so callers can credit it alongside the primary source.
pub(crate) fn effective_with_catalog(
    records: &[Record],
    modelsdev: &crate::modelsdev::ModelsDevCatalog,
    destination: &Destination,
    model: &CatalogModel,
) -> (ModelMetadata, &'static str, bool) {
    let (mut metadata, source) = effective(records, destination, model);
    if source == "operator" {
        return (metadata, source, false);
    }
    let Some(found) = crate::modelsdev::lookup(modelsdev, destination, model) else {
        return (metadata, source, false);
    };
    let filled = metadata.fill_missing(&found);
    let source = if source == "unknown" {
        "modelsdev"
    } else {
        source
    };
    (metadata, source, filled)
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
    parse_catalog_limit(bytes, 1000)
}

/// Same normalization with a caller-chosen row cap. One upstream page stays
/// small. The models.dev legacy flat index reuses this cap across providers.
pub(crate) fn parse_catalog_limit(
    bytes: &[u8],
    max_rows: usize,
) -> BTreeMap<String, ModelMetadata> {
    let Ok(value) = serde_json::from_slice::<Value>(bytes) else {
        return BTreeMap::new();
    };
    let Some(rows) = value.get("data").and_then(Value::as_array) else {
        return BTreeMap::new();
    };
    let mut result = BTreeMap::new();
    for row in rows.iter().take(max_rows) {
        let Some((id, metadata)) = metadata_from_catalog_row(row) else {
            continue;
        };
        // Duplicate rows are not authoritative. Keep only common guarantees.
        result
            .entry(id)
            .and_modify(|old: &mut ModelMetadata| *old = common(&[old.clone(), metadata.clone()]))
            .or_insert(metadata);
    }
    result
}

/// `ocg` carries the capability whitelist only at published schema 1 or 2.
/// A derived protocol profile, an unknown field, and raw JSON stay outside
/// [`ModelMetadata`]. Any other version leaves the row root in charge.
fn recognized_capability_extension(row: &Value) -> Option<&Value> {
    let extension = row.get("ocg")?;
    match extension.get("schemaVersion").and_then(Value::as_u64) {
        Some(1 | 2) => Some(extension),
        _ => None,
    }
}

/// One catalog object, already shaped like a `/v1/models` row or a models.dev
/// model object. Invalid ids are skipped. A malformed fact list becomes an
/// empty metadata record for that id so a stale fact is not kept.
pub(crate) fn metadata_from_catalog_row(row: &Value) -> Option<(String, ModelMetadata)> {
    let id = row.get("id").and_then(Value::as_str)?;
    let id = crate::provider::validate_custom_model_id(id).ok()?;
    if id.chars().any(char::is_control) {
        return None;
    }
    let mut metadata = ModelMetadata::default();
    let source = recognized_capability_extension(row).unwrap_or(row);
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
    // An explicit efforts field wins, including an empty list. reasoning_options
    // is only the fallback when that field was not sent.
    if let Some(efforts) = source
        .get("reasoningEfforts")
        .or_else(|| source.get("reasoning_efforts"))
        .or_else(|| source.pointer("/reasoning/efforts"))
    {
        metadata.reasoning_efforts = parse_reasoning_efforts_value(efforts);
    } else {
        metadata.reasoning_efforts =
            reasoning_efforts_from_options(source.get("reasoning_options"));
    }
    if metadata.validate().is_err() {
        metadata = ModelMetadata::default();
    }
    Some((id, metadata))
}

fn parse_reasoning_efforts_value(efforts: &Value) -> Option<BTreeMap<String, String>> {
    if let Some(values) = efforts.as_array() {
        values
            .iter()
            .map(|v| v.as_str().map(|s| (s.to_string(), s.to_string())))
            .collect()
    } else if let Some(values) = efforts.as_object() {
        values
            .iter()
            .map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string())))
            .collect()
    } else {
        None
    }
}

/// Effort-style `reasoning_options` become the selector → wire map.
/// `none` fills the `off` selector. Toggle-only and budget-token options do
/// not invent levels. An effort entry with an empty value list is an explicit
/// empty set. Values outside the selector table are dropped; if that removes
/// every value, the set stays unknown.
pub(crate) fn reasoning_efforts_from_options(
    options: Option<&Value>,
) -> Option<BTreeMap<String, String>> {
    let options = options?.as_array()?;
    let mut saw_effort = false;
    let mut saw_explicit_empty = false;
    let mut efforts = BTreeMap::new();
    for option in options {
        if option.get("type").and_then(Value::as_str) != Some("effort") {
            continue;
        }
        saw_effort = true;
        let Some(values) = option.get("values").and_then(Value::as_array) else {
            continue;
        };
        if values.is_empty() {
            saw_explicit_empty = true;
        }
        for value in values.iter().filter_map(Value::as_str) {
            let level = if value == "none" { "off" } else { value };
            if EFFORTS.contains(&level) {
                efforts.insert(level.to_string(), value.to_string());
            }
        }
    }
    if !saw_effort || (efforts.is_empty() && !saw_explicit_empty) {
        None
    } else {
        Some(efforts)
    }
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

struct PublishedModelFacts {
    metadata: ModelMetadata,
    sources: BTreeSet<&'static str>,
    protocols: Option<PublishedModelProtocolProfile>,
}

struct QualifiedMapping<'a> {
    destination: &'a Destination,
    model: &'a CatalogModel,
    /// Lowest `routing_rank` among credentials that can carry this mapping.
    rank: u32,
    authorized: Vec<Protocol>,
    choice: Protocol,
}

/// Directory records captured under the same DB lock as routing, then applied
/// after that lock is released.
pub(crate) struct CapturedModelMetadata {
    records: Vec<Record>,
}

impl CapturedModelMetadata {
    pub(crate) fn load(db: &Database) -> anyhow::Result<Self> {
        Ok(Self { records: load(db)? })
    }
}

pub(crate) fn enrich_captured(
    captured: &CapturedModelMetadata,
    modelsdev: &crate::modelsdev::ModelsDevCatalog,
    snapshot: &crate::gateway::handler::RuntimeCatalogSnapshot,
    rows: &mut [Value],
) -> anyhow::Result<()> {
    for row in rows {
        let Some(id) = row.get("id").and_then(Value::as_str) else {
            continue;
        };
        let Some(facts) = published_model_facts(&captured.records, modelsdev, snapshot, id) else {
            continue;
        };
        if let Some(n) = facts.metadata.context_window {
            row["contextWindow"] = json!(n);
        }
        if let Some(n) = facts.metadata.max_output_tokens {
            row["maxTokens"] = json!(n);
        }
        if let Some(name) = &facts.metadata.name {
            row["name"] = json!(name);
        }
        let mut extension = serde_json::to_value(&facts.metadata)?;
        extension["schemaVersion"] = json!(2);
        extension["sources"] = json!(facts.sources);
        extension["status"] = json!(if facts.metadata == ModelMetadata::default() {
            "unknown"
        } else {
            "declared"
        });
        if let Some(protocols) = &facts.protocols {
            extension["protocols"] = serde_json::to_value(protocols)?;
        }
        row["ocg"] = extension;
    }
    Ok(())
}

/// Capability intersection and protocol facts share one qualified-candidate set.
/// Rows that do not resolve stay untouched. A resolved row with no carrying
/// credential still receives unknown metadata and omits `protocols`; the
/// `/v1/models` publisher then drops those rows.
fn published_model_facts(
    records: &[Record],
    modelsdev: &crate::modelsdev::ModelsDevCatalog,
    snapshot: &crate::gateway::handler::RuntimeCatalogSnapshot,
    requested: &str,
) -> Option<PublishedModelFacts> {
    let resolved = snapshot.resolve(requested).ok()?;
    let mappings = qualified_publication_mappings(snapshot, requested, &resolved);
    let mut candidates = Vec::new();
    let mut sources = BTreeSet::new();
    for mapping in &mappings {
        let (metadata, source, modelsdev_filled) =
            effective_with_catalog(records, modelsdev, mapping.destination, mapping.model);
        candidates.push(metadata);
        sources.insert(source);
        if modelsdev_filled {
            sources.insert("modelsdev");
        }
    }
    Some(PublishedModelFacts {
        metadata: common(&candidates),
        sources,
        protocols: profile_from_mappings(&mappings),
    })
}

fn qualified_publication_mappings<'a>(
    snapshot: &'a crate::gateway::handler::RuntimeCatalogSnapshot,
    requested: &str,
    resolved: &crate::alias::ResolvedModel,
) -> Vec<QualifiedMapping<'a>> {
    let mut found = Vec::new();
    for destination in &snapshot.routing.projection.destinations {
        if !destination.enabled {
            continue;
        }
        for model in &destination.catalog {
            if !model.enabled {
                continue;
            }
            if !crate::gateway::materialize::resolved_contains_model(
                resolved,
                destination,
                model,
                requested,
            ) {
                continue;
            }
            let Some((authorized, rank, choice)) =
                mapping_authorization(snapshot, destination, model, requested)
            else {
                continue;
            };
            found.push(QualifiedMapping {
                destination,
                model,
                rank,
                authorized,
                choice,
            });
        }
    }
    found.sort_by(|left, right| {
        left.rank
            .cmp(&right.rank)
            .then_with(|| left.destination.id.cmp(&right.destination.id))
            .then_with(|| left.model.upstream_model.cmp(&right.model.upstream_model))
    });
    found
}

/// Authorized protocols are the union across carrying credentials, in the
/// saved `model.protocols` order. The mapping's choice is the saved preferred
/// protocol only when that protocol is in the union; otherwise it is the first
/// saved protocol that is. Nothing here substitutes Chat.
fn mapping_authorization(
    snapshot: &crate::gateway::handler::RuntimeCatalogSnapshot,
    destination: &Destination,
    model: &CatalogModel,
    requested: &str,
) -> Option<(Vec<Protocol>, u32, Protocol)> {
    let mut authorized = Vec::new();
    let mut rank = u32::MAX;
    for credential in &snapshot.routing.credentials {
        if !credential_can_carry(credential, destination, model, requested) {
            continue;
        }
        let mut carried = false;
        for protocol in &model.protocols {
            if !protocol_available(credential, destination, model, *protocol) {
                continue;
            }
            if !authorized.contains(protocol) {
                authorized.push(*protocol);
            }
            carried = true;
        }
        if carried {
            rank = rank.min(credential_routing_rank(snapshot, credential));
        }
    }
    let choice = mapping_choice(model, &authorized)?;
    Some((authorized, rank, choice))
}

fn credential_can_carry(
    credential: &crate::routing_snapshot::ExecutionCredential,
    destination: &Destination,
    model: &CatalogModel,
    requested: &str,
) -> bool {
    credential.destination_id == destination.id
        && credential.enabled
        && credential.ready
        && credential.binding_enabled
        && crate::gateway::materialize::binding_allows_requested_model(
            &credential.scope,
            requested,
            requested,
            [&model.upstream_model, &model.public_model],
        )
}

fn protocol_available(
    credential: &crate::routing_snapshot::ExecutionCredential,
    destination: &Destination,
    model: &CatalogModel,
    protocol: Protocol,
) -> bool {
    if protocol_requires_key(destination, model, protocol) && credential.key_cipher.is_empty() {
        return false;
    }
    crate::route_availability::protocol_is_authorized(credential, destination, model, protocol)
}

fn protocol_requires_key(
    destination: &Destination,
    model: &CatalogModel,
    protocol: Protocol,
) -> bool {
    use ocg_domain::destination::{AdapterKind, AuthScheme};
    if destination.adapter == AdapterKind::Http {
        return ocg_domain::destination::http_model_route(destination, model, protocol)
            .is_some_and(|route| route.auth_scheme != AuthScheme::None);
    }
    destination.auth_scheme != AuthScheme::None
}

fn mapping_choice(model: &CatalogModel, authorized: &[Protocol]) -> Option<Protocol> {
    if let Some(preferred) = model.preferred
        && authorized.contains(&preferred)
    {
        return Some(preferred);
    }
    model
        .protocols
        .iter()
        .copied()
        .find(|protocol| authorized.contains(protocol))
}

/// Persisted projection rank for this credential id. A legacy account id does
/// not supply a rank when that projection row is a different credential.
fn credential_routing_rank(
    snapshot: &crate::gateway::handler::RuntimeCatalogSnapshot,
    credential: &crate::routing_snapshot::ExecutionCredential,
) -> u32 {
    snapshot
        .routing
        .projection
        .credentials
        .iter()
        .find(|row| row.id == credential.credential_id)
        .map(|row| row.routing_rank)
        .unwrap_or(u32::MAX)
}

fn profile_from_mappings(
    mappings: &[QualifiedMapping<'_>],
) -> Option<PublishedModelProtocolProfile> {
    let preferred = PublishedUpstreamProtocol::from_domain(mappings.first()?.choice);
    let mut supported = Vec::new();
    for protocol in Protocol::ALL {
        if mappings
            .iter()
            .any(|mapping| mapping.authorized.contains(&protocol))
        {
            supported.push(PublishedUpstreamProtocol::from_domain(protocol));
        }
    }
    let profile = PublishedModelProtocolProfile {
        preferred,
        supported,
    };
    profile.validate().ok()?;
    Some(profile)
}

#[cfg(test)]
mod tests;
