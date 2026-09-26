//! Strongly-typed local restriction policy. Evaluation is pure; admission
//! lives in [`super::recovery`]. Configuration compiles into destination-aware
//! effective rules with monotonic generations, never content hashes.
//!
//! Scope is only Credential or CredentialModel. Built-in GOAT insufficient
//! credits is `builtin.goat.credits_rejection` at CredentialModel using the
//! existing classifier. Custom matchers read only top-level `error.code` /
//! `error.type` / `error.message` plus HTTP 400..599.
use crate::db::Database;
use anyhow::Context;
use ocg_domain::provider::ProviderAdapterKind;
use ocg_gateway::classify::ProviderErrorClass;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};

pub(crate) const BUILTIN_OWNER: &str = "builtin";
pub(crate) const GLOBAL_OWNER: &str = "global";
pub(crate) const GOAT_CREDITS_REJECTION_RULE: &str = "builtin.goat.credits_rejection";
pub(crate) const DEFAULT_INITIAL_SECS: u64 = 30;
pub(crate) const DEFAULT_MAX_SECS: u64 = 300;
pub(crate) const MIN_BACKOFF_SECS: u64 = 1;
pub(crate) const MAX_BACKOFF_SECS: u64 = 86_400;
pub(crate) const MAX_CONFIGURED_RULES: usize = 128;
pub(crate) const MAX_EFFECTIVE_PER_DESTINATION: usize = 32;
pub(crate) const MAX_MATCH_ALTERNATIVES: usize = 32;
pub(crate) const MAX_MATCH_STRING_BYTES: usize = 256;
pub(crate) const SETTING_KEY: &str = "temporary_unavailability_v1";
pub(crate) const DOCUMENT_VERSION: u32 = 1;

/// Local restriction identity. Not endpoint/retry/credits slot kinds, not a
/// client alias, and not a declared quota pool.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum RestrictionScope {
    Credential,
    CredentialModel,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum PolicyAction {
    TemporaryUnavailable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum PolicySourceKind {
    Global,
    Connection,
    Builtin,
}

/// Effective configuration owner + rule id + explicit generation.
/// Generation is an assigned epoch, never a content hash: deleting a rule and
/// recreating it with the same id and payload still isolates in-flight state.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize)]
pub(crate) struct PolicySource {
    pub owner: String,
    pub rule_id: String,
    pub rule_generation: u64,
}

impl PolicySource {
    pub(crate) fn kind(&self) -> PolicySourceKind {
        if self.owner == BUILTIN_OWNER {
            PolicySourceKind::Builtin
        } else if self.owner.starts_with("connection:") {
            PolicySourceKind::Connection
        } else {
            PolicySourceKind::Global
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub(crate) struct PolicyBackoff {
    pub initial_secs: u64,
    pub max_secs: u64,
}

impl Default for PolicyBackoff {
    fn default() -> Self {
        Self {
            initial_secs: DEFAULT_INITIAL_SECS,
            max_secs: DEFAULT_MAX_SECS,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CustomMatch {
    pub status_codes: Vec<u16>,
    pub error_codes: Vec<String>,
    pub error_types: Vec<String>,
    pub message_contains: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum PolicyMatcher {
    GoatInsufficientCredits,
    Custom(CustomMatch),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PolicyInput {
    pub adapter: ProviderAdapterKind,
    pub class: ProviderErrorClass,
    pub http_status: Option<u16>,
    pub error: Option<TopLevelError>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TopLevelError {
    pub code: Option<String>,
    pub error_type: Option<String>,
    pub message: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct PolicyDecision {
    pub action: PolicyAction,
    pub scope: RestrictionScope,
    pub source: PolicySource,
    pub backoff: PolicyBackoff,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct EffectivePolicyRule {
    pub source: PolicySource,
    pub destination_id: Option<String>,
    pub enabled: bool,
    pub scope: RestrictionScope,
    pub action: PolicyAction,
    pub matcher: PolicyMatcher,
    pub backoff: PolicyBackoff,
}

/// Runtime-effective layers. Replacing the snapshot is source-diffed per
/// destination: an override on A does not retire B's global wait.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct EffectivePolicySnapshot {
    pub epoch: u64,
    pub next_generation: u64,
    pub layers: Vec<EffectivePolicyRule>,
}

impl EffectivePolicySnapshot {
    pub(crate) fn builtin() -> Self {
        Self {
            epoch: 1,
            next_generation: 2,
            layers: vec![builtin_layer(1)],
        }
    }

    pub(crate) fn effective_for(&self, destination_id: &str) -> Vec<EffectivePolicyRule> {
        let mut winners: HashMap<String, EffectivePolicyRule> = HashMap::new();
        for layer in &self.layers {
            if layer.destination_id.is_none() {
                winners.insert(layer.source.rule_id.clone(), layer.clone());
            }
        }
        for layer in &self.layers {
            if layer.destination_id.as_deref() == Some(destination_id) {
                winners.insert(layer.source.rule_id.clone(), layer.clone());
            }
        }
        winners.into_values().filter(|rule| rule.enabled).collect()
    }

    pub(crate) fn is_live(&self, source: &PolicySource, destination_id: &str) -> bool {
        self.effective_for(destination_id)
            .iter()
            .any(|rule| rule.source == *source)
    }
}

impl Default for EffectivePolicySnapshot {
    fn default() -> Self {
        Self::builtin()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ConfiguredRule {
    Custom {
        id: String,
        destination_id: Option<String>,
        enabled: bool,
        scope: RestrictionScope,
        matcher: CustomMatch,
        backoff: PolicyBackoff,
    },
    BuiltinOverride {
        id: String,
        destination_id: Option<String>,
        enabled: bool,
        backoff: Option<PolicyBackoff>,
    },
}

impl ConfiguredRule {
    pub(crate) fn id(&self) -> &str {
        match self {
            Self::Custom { id, .. } | Self::BuiltinOverride { id, .. } => id,
        }
    }

    pub(crate) fn destination_id(&self) -> Option<&str> {
        match self {
            Self::Custom { destination_id, .. } | Self::BuiltinOverride { destination_id, .. } => {
                destination_id.as_deref()
            }
        }
    }
}

pub(crate) fn builtin_goat_source(rule_generation: u64) -> PolicySource {
    PolicySource {
        owner: BUILTIN_OWNER.into(),
        rule_id: GOAT_CREDITS_REJECTION_RULE.into(),
        rule_generation,
    }
}

fn builtin_layer(generation: u64) -> EffectivePolicyRule {
    EffectivePolicyRule {
        source: builtin_goat_source(generation),
        destination_id: None,
        enabled: true,
        scope: RestrictionScope::CredentialModel,
        action: PolicyAction::TemporaryUnavailable,
        matcher: PolicyMatcher::GoatInsufficientCredits,
        backoff: PolicyBackoff::default(),
    }
}

pub(crate) fn owner_for(destination_id: Option<&str>, rule_id: &str) -> String {
    if let Some(destination_id) = destination_id {
        format!("connection:{destination_id}")
    } else if rule_id == GOAT_CREDITS_REJECTION_RULE {
        BUILTIN_OWNER.into()
    } else {
        GLOBAL_OWNER.into()
    }
}

/// Compile operator configuration into destination-aware layers. Unchanged
/// (owner, rule id, dest, payload, enabled) keep their previous generation.
pub(crate) fn compile_snapshot(
    rules: &[ConfiguredRule],
    previous: &EffectivePolicySnapshot,
    epoch: u64,
) -> EffectivePolicySnapshot {
    let mut next_generation = previous.next_generation.max(2);
    let previous_by_key: HashMap<(String, String, Option<String>), EffectivePolicyRule> = previous
        .layers
        .iter()
        .cloned()
        .map(|layer| {
            (
                (
                    layer.source.owner.clone(),
                    layer.source.rule_id.clone(),
                    layer.destination_id.clone(),
                ),
                layer,
            )
        })
        .collect();
    let assign = |next_generation: &mut u64,
                  owner: String,
                  rule_id: String,
                  destination_id: Option<String>,
                  enabled: bool,
                  scope: RestrictionScope,
                  matcher: PolicyMatcher,
                  backoff: PolicyBackoff|
     -> EffectivePolicyRule {
        let key = (owner.clone(), rule_id.clone(), destination_id.clone());
        let generation = previous_by_key.get(&key).and_then(|prior| {
            (prior.enabled == enabled
                && prior.scope == scope
                && prior.matcher == matcher
                && prior.backoff == backoff)
                .then_some(prior.source.rule_generation)
        });
        let rule_generation = generation.unwrap_or_else(|| {
            let assigned = *next_generation;
            *next_generation = next_generation.saturating_add(1);
            assigned
        });
        EffectivePolicyRule {
            source: PolicySource {
                owner,
                rule_id,
                rule_generation,
            },
            destination_id,
            enabled,
            scope,
            action: PolicyAction::TemporaryUnavailable,
            matcher,
            backoff,
        }
    };

    let mut layers = vec![assign(
        &mut next_generation,
        BUILTIN_OWNER.into(),
        GOAT_CREDITS_REJECTION_RULE.into(),
        None,
        true,
        RestrictionScope::CredentialModel,
        PolicyMatcher::GoatInsufficientCredits,
        PolicyBackoff::default(),
    )];
    for rule in rules {
        match rule {
            ConfiguredRule::Custom {
                id,
                destination_id,
                enabled,
                scope,
                matcher,
                backoff,
            } => {
                layers.push(assign(
                    &mut next_generation,
                    owner_for(destination_id.as_deref(), id),
                    id.clone(),
                    destination_id.clone(),
                    *enabled,
                    *scope,
                    PolicyMatcher::Custom(matcher.clone()),
                    *backoff,
                ));
            }
            ConfiguredRule::BuiltinOverride {
                id,
                destination_id,
                enabled,
                backoff,
            } => {
                layers.push(assign(
                    &mut next_generation,
                    owner_for(destination_id.as_deref(), id),
                    id.clone(),
                    destination_id.clone(),
                    *enabled,
                    RestrictionScope::CredentialModel,
                    PolicyMatcher::GoatInsufficientCredits,
                    backoff.unwrap_or_default(),
                ));
            }
        }
    }
    EffectivePolicySnapshot {
        epoch,
        next_generation,
        layers,
    }
}

pub(crate) fn evaluate_rules(
    rules: &[EffectivePolicyRule],
    input: &PolicyInput,
) -> Vec<PolicyDecision> {
    rules
        .iter()
        .filter(|rule| rule.enabled && matcher_hits(&rule.matcher, input))
        .map(|rule| PolicyDecision {
            action: rule.action,
            scope: rule.scope,
            source: rule.source.clone(),
            backoff: rule.backoff,
        })
        .collect()
}

#[cfg(test)]
pub(crate) fn evaluate(
    snapshot: &EffectivePolicySnapshot,
    destination_id: &str,
    input: &PolicyInput,
) -> Vec<PolicyDecision> {
    evaluate_rules(&snapshot.effective_for(destination_id), input)
}

fn matcher_hits(matcher: &PolicyMatcher, input: &PolicyInput) -> bool {
    match matcher {
        PolicyMatcher::GoatInsufficientCredits => {
            input.adapter == ProviderAdapterKind::CommandCodeGoat
                && input.class == ProviderErrorClass::InsufficientCredits
        }
        PolicyMatcher::Custom(custom) => custom_hits(custom, input),
    }
}

fn custom_hits(custom: &CustomMatch, input: &PolicyInput) -> bool {
    if !custom.status_codes.is_empty() {
        let Some(status) = input.http_status else {
            return false;
        };
        if !custom.status_codes.contains(&status) {
            return false;
        }
    }
    if !custom.error_codes.is_empty() {
        let Some(code) = input.error.as_ref().and_then(|error| error.code.as_deref()) else {
            return false;
        };
        if !custom.error_codes.iter().any(|item| item == code) {
            return false;
        }
    }
    if !custom.error_types.is_empty() {
        let Some(kind) = input
            .error
            .as_ref()
            .and_then(|error| error.error_type.as_deref())
        else {
            return false;
        };
        if !custom.error_types.iter().any(|item| item == kind) {
            return false;
        }
    }
    if !custom.message_contains.is_empty() {
        let Some(message) = input
            .error
            .as_ref()
            .and_then(|error| error.message.as_deref())
        else {
            return false;
        };
        if !custom
            .message_contains
            .iter()
            .any(|needle| message.contains(needle))
        {
            return false;
        }
    }
    true
}

/// Top-level `error` object only. Nested request echoes are ignored.
pub(crate) fn extract_top_level_error(body: &str) -> Option<TopLevelError> {
    let value: serde_json::Value = serde_json::from_str(body).ok()?;
    let error = value.get("error")?.as_object()?;
    Some(TopLevelError {
        code: string_field(error, "code"),
        error_type: string_field(error, "type"),
        message: string_field(error, "message"),
    })
}

fn string_field(object: &serde_json::Map<String, serde_json::Value>, key: &str) -> Option<String> {
    object
        .get(key)
        .and_then(|value| value.as_str())
        .map(str::to_owned)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum PolicyDocumentError {
    InvalidVersion,
    InvalidJson,
    TooManyRules,
    DuplicateRule,
    UnknownDestination,
    UnknownBuiltin,
    EmptyMatch,
    InvalidMatch,
    InvalidBackoff,
    TooManyEffective,
}

impl std::fmt::Display for PolicyDocumentError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::InvalidVersion => "temporary_unavailability_v1 version must be 1",
            Self::InvalidJson => "temporary_unavailability_v1 is not a valid policy document",
            Self::TooManyRules => "temporary unavailability allows at most 128 configured rules",
            Self::DuplicateRule => "duplicate temporary unavailability rule id for a destination",
            Self::UnknownDestination => {
                "temporary unavailability rule references an unknown destination"
            }
            Self::UnknownBuiltin => "unknown built-in temporary unavailability rule",
            Self::EmptyMatch => {
                "custom temporary unavailability match must include at least one field"
            }
            Self::InvalidMatch => "temporary unavailability match field is empty or out of range",
            Self::InvalidBackoff => {
                "temporary unavailability backoff must be 1..=86400 with max >= initial"
            }
            Self::TooManyEffective => {
                "a destination may have at most 32 enabled temporary unavailability rules"
            }
        })
    }
}

pub(crate) fn validate_configured(
    rules: &[ConfiguredRule],
    known_destinations: &HashSet<String>,
) -> Result<(), PolicyDocumentError> {
    if rules.len() > MAX_CONFIGURED_RULES {
        return Err(PolicyDocumentError::TooManyRules);
    }
    let mut seen = HashSet::new();
    for rule in rules {
        let key = (
            rule.destination_id().unwrap_or("").to_string(),
            rule.id().to_string(),
        );
        if !seen.insert(key) {
            return Err(PolicyDocumentError::DuplicateRule);
        }
        if let Some(destination_id) = rule.destination_id()
            && !known_destinations.contains(destination_id)
        {
            return Err(PolicyDocumentError::UnknownDestination);
        }
        match rule {
            ConfiguredRule::Custom {
                id,
                matcher,
                backoff,
                ..
            } => {
                if id == GOAT_CREDITS_REJECTION_RULE {
                    return Err(PolicyDocumentError::UnknownBuiltin);
                }
                validate_match(matcher)?;
                validate_backoff(*backoff)?;
            }
            ConfiguredRule::BuiltinOverride { id, backoff, .. } => {
                if id != GOAT_CREDITS_REJECTION_RULE {
                    return Err(PolicyDocumentError::UnknownBuiltin);
                }
                if let Some(backoff) = backoff {
                    validate_backoff(*backoff)?;
                }
            }
        }
    }
    let snapshot = compile_snapshot(rules, &EffectivePolicySnapshot::builtin(), 1);
    let mut destinations: HashSet<String> = known_destinations.iter().cloned().collect();
    destinations.insert(String::new());
    for destination in destinations {
        if snapshot.effective_for(&destination).len() > MAX_EFFECTIVE_PER_DESTINATION {
            return Err(PolicyDocumentError::TooManyEffective);
        }
    }
    Ok(())
}

fn validate_match(matcher: &CustomMatch) -> Result<(), PolicyDocumentError> {
    if matcher.status_codes.is_empty()
        && matcher.error_codes.is_empty()
        && matcher.error_types.is_empty()
        && matcher.message_contains.is_empty()
    {
        return Err(PolicyDocumentError::EmptyMatch);
    }
    if matcher.status_codes.len() > MAX_MATCH_ALTERNATIVES
        || matcher.error_codes.len() > MAX_MATCH_ALTERNATIVES
        || matcher.error_types.len() > MAX_MATCH_ALTERNATIVES
        || matcher.message_contains.len() > MAX_MATCH_ALTERNATIVES
    {
        return Err(PolicyDocumentError::InvalidMatch);
    }
    if !matcher.status_codes.is_empty()
        && (matcher
            .status_codes
            .iter()
            .any(|code| !(400..=599).contains(code)))
    {
        return Err(PolicyDocumentError::InvalidMatch);
    }
    for values in [
        &matcher.error_codes,
        &matcher.error_types,
        &matcher.message_contains,
    ] {
        if values
            .iter()
            .any(|item| item.is_empty() || item.len() > MAX_MATCH_STRING_BYTES)
        {
            return Err(PolicyDocumentError::InvalidMatch);
        }
    }
    Ok(())
}

fn validate_backoff(backoff: PolicyBackoff) -> Result<(), PolicyDocumentError> {
    if !(MIN_BACKOFF_SECS..=MAX_BACKOFF_SECS).contains(&backoff.initial_secs)
        || !(MIN_BACKOFF_SECS..=MAX_BACKOFF_SECS).contains(&backoff.max_secs)
        || backoff.max_secs < backoff.initial_secs
    {
        return Err(PolicyDocumentError::InvalidBackoff);
    }
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct StoredDocument {
    version: u32,
    #[serde(default)]
    rules: Vec<StoredRule>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum StoredRule {
    Custom {
        id: String,
        #[serde(default)]
        destination_id: Option<String>,
        enabled: bool,
        scope: RestrictionScope,
        #[serde(rename = "match")]
        matcher: StoredMatch,
        backoff: StoredBackoff,
    },
    BuiltinOverride {
        id: String,
        #[serde(default)]
        destination_id: Option<String>,
        enabled: bool,
        #[serde(default)]
        backoff: Option<StoredBackoff>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct StoredMatch {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    status_codes: Option<Vec<u16>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    error_codes: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    error_types: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    message_contains: Option<Vec<String>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct StoredBackoff {
    initial_seconds: u64,
    max_seconds: u64,
}

impl From<StoredBackoff> for PolicyBackoff {
    fn from(value: StoredBackoff) -> Self {
        Self {
            initial_secs: value.initial_seconds,
            max_secs: value.max_seconds,
        }
    }
}

impl From<PolicyBackoff> for StoredBackoff {
    fn from(value: PolicyBackoff) -> Self {
        Self {
            initial_seconds: value.initial_secs,
            max_seconds: value.max_secs,
        }
    }
}

fn stored_match_to_custom(matcher: StoredMatch) -> CustomMatch {
    CustomMatch {
        status_codes: matcher.status_codes.unwrap_or_default(),
        error_codes: matcher.error_codes.unwrap_or_default(),
        error_types: matcher.error_types.unwrap_or_default(),
        message_contains: matcher.message_contains.unwrap_or_default(),
    }
}

fn custom_to_stored_match(matcher: &CustomMatch) -> StoredMatch {
    StoredMatch {
        status_codes: nonempty_opt(&matcher.status_codes),
        error_codes: nonempty_opt(&matcher.error_codes),
        error_types: nonempty_opt(&matcher.error_types),
        message_contains: nonempty_opt(&matcher.message_contains),
    }
}

fn nonempty_opt<T: Clone>(values: &[T]) -> Option<Vec<T>> {
    if values.is_empty() {
        None
    } else {
        Some(values.to_vec())
    }
}

fn stored_to_configured(rule: StoredRule) -> ConfiguredRule {
    match rule {
        StoredRule::Custom {
            id,
            destination_id,
            enabled,
            scope,
            matcher,
            backoff,
        } => ConfiguredRule::Custom {
            id,
            destination_id,
            enabled,
            scope,
            matcher: stored_match_to_custom(matcher),
            backoff: backoff.into(),
        },
        StoredRule::BuiltinOverride {
            id,
            destination_id,
            enabled,
            backoff,
        } => ConfiguredRule::BuiltinOverride {
            id,
            destination_id,
            enabled,
            backoff: backoff.map(Into::into),
        },
    }
}

fn configured_to_stored(rule: &ConfiguredRule) -> StoredRule {
    match rule {
        ConfiguredRule::Custom {
            id,
            destination_id,
            enabled,
            scope,
            matcher,
            backoff,
        } => StoredRule::Custom {
            id: id.clone(),
            destination_id: destination_id.clone(),
            enabled: *enabled,
            scope: *scope,
            matcher: custom_to_stored_match(matcher),
            backoff: (*backoff).into(),
        },
        ConfiguredRule::BuiltinOverride {
            id,
            destination_id,
            enabled,
            backoff,
        } => StoredRule::BuiltinOverride {
            id: id.clone(),
            destination_id: destination_id.clone(),
            enabled: *enabled,
            backoff: backoff.map(Into::into),
        },
    }
}

pub(crate) fn parse_policy_document(raw: &str) -> Result<Vec<ConfiguredRule>, PolicyDocumentError> {
    let document: StoredDocument =
        serde_json::from_str(raw).map_err(|_| PolicyDocumentError::InvalidJson)?;
    if document.version != DOCUMENT_VERSION {
        return Err(PolicyDocumentError::InvalidVersion);
    }
    Ok(document
        .rules
        .into_iter()
        .map(stored_to_configured)
        .collect())
}

pub(crate) fn encode_policy_document(
    rules: &[ConfiguredRule],
) -> Result<String, serde_json::Error> {
    serde_json::to_string(&StoredDocument {
        version: DOCUMENT_VERSION,
        rules: rules.iter().map(configured_to_stored).collect(),
    })
}

pub(crate) fn destination_ids(db: &Database) -> anyhow::Result<HashSet<String>> {
    let mut stmt = db
        .conn
        .prepare("SELECT id FROM destinations")
        .context("list destination ids")?;
    let ids = stmt
        .query_map([], |row| row.get(0))
        .context("list destination ids")?
        .collect::<rusqlite::Result<HashSet<String>>>()
        .context("list destination ids")?;
    Ok(ids)
}

pub(crate) fn load_configured_rules(
    db: &Database,
) -> Result<Vec<ConfiguredRule>, PolicyDocumentError> {
    match db.get_setting(SETTING_KEY) {
        Ok(None) => Ok(Vec::new()),
        Ok(Some(raw)) => parse_policy_document(&raw),
        Err(_) => Err(PolicyDocumentError::InvalidJson),
    }
}

/// Startup / republish: malformed documents fail closed. Unknown destination
/// references stay persisted but are unresolved (inactive) until rewritten.
pub(crate) fn load_runtime_snapshot(db: &Database) -> anyhow::Result<EffectivePolicySnapshot> {
    let rules = load_configured_rules(db).map_err(|error| anyhow::anyhow!("{error}"))?;
    let destinations = destination_ids(db)?;
    validate_loaded(&rules, &destinations).map_err(|error| anyhow::anyhow!("{error}"))?;
    let resolved: Vec<_> = rules
        .into_iter()
        .filter(|rule| {
            rule.destination_id()
                .is_none_or(|id| destinations.contains(id))
        })
        .collect();
    Ok(compile_snapshot(
        &resolved,
        &EffectivePolicySnapshot::builtin(),
        1,
    ))
}

pub(crate) fn compile_published(
    db: &Database,
    previous: &EffectivePolicySnapshot,
) -> anyhow::Result<EffectivePolicySnapshot> {
    let rules = load_configured_rules(db).map_err(|error| anyhow::anyhow!("{error}"))?;
    let destinations = destination_ids(db)?;
    validate_loaded(&rules, &destinations).map_err(|error| anyhow::anyhow!("{error}"))?;
    let resolved: Vec<_> = rules
        .into_iter()
        .filter(|rule| {
            rule.destination_id()
                .is_none_or(|id| destinations.contains(id))
        })
        .collect();
    Ok(compile_from_previous(&resolved, previous))
}

fn validate_loaded(
    rules: &[ConfiguredRule],
    known_destinations: &HashSet<String>,
) -> Result<(), PolicyDocumentError> {
    if rules.len() > MAX_CONFIGURED_RULES {
        return Err(PolicyDocumentError::TooManyRules);
    }
    let mut seen = HashSet::new();
    for rule in rules {
        let key = (
            rule.destination_id().unwrap_or("").to_string(),
            rule.id().to_string(),
        );
        if !seen.insert(key) {
            return Err(PolicyDocumentError::DuplicateRule);
        }
        match rule {
            ConfiguredRule::Custom {
                id,
                matcher,
                backoff,
                ..
            } => {
                if id == GOAT_CREDITS_REJECTION_RULE {
                    return Err(PolicyDocumentError::UnknownBuiltin);
                }
                validate_match(matcher)?;
                validate_backoff(*backoff)?;
            }
            ConfiguredRule::BuiltinOverride { id, backoff, .. } => {
                if id != GOAT_CREDITS_REJECTION_RULE {
                    return Err(PolicyDocumentError::UnknownBuiltin);
                }
                if let Some(backoff) = backoff {
                    validate_backoff(*backoff)?;
                }
            }
        }
    }
    let resolved: Vec<_> = rules
        .iter()
        .filter(|rule| {
            rule.destination_id()
                .is_none_or(|id| known_destinations.contains(id))
        })
        .cloned()
        .collect();
    let snapshot = compile_snapshot(&resolved, &EffectivePolicySnapshot::builtin(), 1);
    let mut destinations: HashSet<String> = known_destinations.iter().cloned().collect();
    destinations.insert(String::new());
    for destination in destinations {
        if snapshot.effective_for(&destination).len() > MAX_EFFECTIVE_PER_DESTINATION {
            return Err(PolicyDocumentError::TooManyEffective);
        }
    }
    Ok(())
}

pub(crate) fn persist_configured_rules(
    db: &Database,
    rules: &[ConfiguredRule],
) -> anyhow::Result<()> {
    let encoded = encode_policy_document(rules).context("encode temporary unavailability")?;
    db.set_setting(SETTING_KEY, &encoded)
        .context("persist temporary unavailability")
}

pub(crate) fn strip_destination_rules(db: &Database, destination_id: &str) -> anyhow::Result<bool> {
    let rules = match load_configured_rules(db) {
        Ok(rules) => rules,
        Err(PolicyDocumentError::InvalidJson | PolicyDocumentError::InvalidVersion) => {
            anyhow::bail!(
                "temporary_unavailability_v1 is invalid and destination delete cannot rewrite it"
            );
        }
        Err(error) => anyhow::bail!("{error}"),
    };
    let before = rules.len();
    let next: Vec<_> = rules
        .into_iter()
        .filter(|rule| rule.destination_id() != Some(destination_id))
        .collect();
    let changed = next.len() != before;
    if changed {
        persist_configured_rules(db, &next)?;
    }
    Ok(changed)
}

pub(crate) fn compile_from_previous(
    rules: &[ConfiguredRule],
    previous: &EffectivePolicySnapshot,
) -> EffectivePolicySnapshot {
    compile_snapshot(rules, previous, previous.epoch.saturating_add(1).max(1))
}

#[cfg(test)]
mod tests;
