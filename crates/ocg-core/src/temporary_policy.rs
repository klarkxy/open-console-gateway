//! Operator-declared local admission rules. Matching never creates quota facts
//! or grants permission to replay a request. Only bounded HTTP error evidence
//! is considered; successful output and transport failures are not inputs.
use anyhow::{Result, ensure};
use ocg_gateway::classify::ProviderErrorClass;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;

pub(crate) const BUILTIN_CREDITS_RULE: &str = "builtin.goat.credits-rejection";
pub(crate) const MAX_RULES: usize = 64;
pub(crate) const MAX_MATCH_BYTES: usize = 32 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum TemporaryRuleScope {
    Credential,
    CredentialModel,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum ErrorFieldMatchMode {
    Exact,
    Contains,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ErrorFieldMatch {
    pub pointer: String,
    pub mode: ErrorFieldMatchMode,
    pub values: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "camelCase", deny_unknown_fields)]
pub enum TemporaryRuleMatch {
    /// Reuse the sealed adapter's exact classification, not a second parser.
    KnownCreditsRejection,
    HttpError {
        statuses: Vec<u16>,
        fields: Vec<ErrorFieldMatch>,
        #[serde(rename = "bodyContains")]
        body_contains: Vec<String>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TemporaryRule {
    pub id: String,
    /// None is global. A destination's same-id row replaces that global rule,
    /// including an explicitly disabled override. No provider-wide propagation.
    pub destination_id: Option<String>,
    pub enabled: bool,
    pub scope: TemporaryRuleScope,
    pub matcher: TemporaryRuleMatch,
    pub initial_seconds: u64,
    pub max_seconds: u64,
}

pub(crate) fn builtin_rule() -> TemporaryRule {
    TemporaryRule {
        id: BUILTIN_CREDITS_RULE.into(),
        destination_id: None,
        enabled: true,
        scope: TemporaryRuleScope::CredentialModel,
        matcher: TemporaryRuleMatch::KnownCreditsRejection,
        initial_seconds: 30,
        max_seconds: 300,
    }
}

fn validate_values(values: &[String]) -> Result<()> {
    ensure!(
        !values.is_empty() && values.len() <= 8,
        "a match needs 1 to 8 values"
    );
    ensure!(
        values
            .iter()
            .all(|value| !value.trim().is_empty() && value.len() <= 512),
        "match values must be nonempty and at most 512 bytes"
    );
    Ok(())
}

pub(crate) fn validate_rules(rules: &[TemporaryRule]) -> Result<()> {
    ensure!(
        rules.len() <= MAX_RULES,
        "at most {MAX_RULES} temporary rules are supported"
    );
    let mut identities = HashSet::new();
    let mut has_builtin = false;
    for rule in rules {
        ensure!(
            !rule.id.is_empty()
                && rule.id.len() <= 80
                && rule
                    .id
                    .bytes()
                    .all(|ch| ch.is_ascii_alphanumeric() || b"._-".contains(&ch)),
            "invalid temporary rule id"
        );
        ensure!(
            rule.destination_id
                .as_ref()
                .is_none_or(|id| !id.is_empty() && id.len() <= 128),
            "invalid rule destination id"
        );
        ensure!(
            identities.insert((&rule.id, &rule.destination_id)),
            "duplicate temporary rule identity"
        );
        ensure!(
            (1..=3600).contains(&rule.initial_seconds)
                && (rule.initial_seconds..=86400).contains(&rule.max_seconds),
            "initial wait must be 1..3600 seconds and maximum wait initial..86400 seconds"
        );
        if rule.id.starts_with("builtin.") {
            ensure!(
                rule.id == BUILTIN_CREDITS_RULE
                    && rule.matcher == TemporaryRuleMatch::KnownCreditsRejection,
                "built-in rule identity and matcher cannot be replaced"
            );
            has_builtin |= rule.destination_id.is_none();
        }
        if let TemporaryRuleMatch::HttpError {
            statuses,
            fields,
            body_contains,
        } = &rule.matcher
        {
            ensure!(
                !statuses.is_empty()
                    && statuses.len() <= 32
                    && statuses.iter().all(|code| (400..600).contains(code)),
                "HTTP rules need 1 to 32 error status codes"
            );
            ensure!(fields.len() <= 8, "at most 8 error fields are supported");
            for field in fields {
                // Do not silently match a request echo or successful content.
                ensure!(
                    field.pointer.len() <= 128
                        && (matches!(
                            field.pointer.as_str(),
                            "/error" | "/message" | "/code" | "/type"
                        ) || field.pointer.starts_with("/error/"))
                        && !field.pointer.contains('~'),
                    "only error JSON pointers are supported"
                );
                validate_values(&field.values)?;
            }
            if !body_contains.is_empty() {
                validate_values(body_contains)?;
            }
        }
    }
    ensure!(
        has_builtin,
        "keep the global built-in rule; disable it explicitly instead of deleting it"
    );
    Ok(())
}

impl TemporaryRule {
    pub(crate) fn matches(
        &self,
        status: u16,
        class: ProviderErrorClass,
        body: Option<&str>,
    ) -> bool {
        if !self.enabled || !(400..600).contains(&status) {
            return false;
        }
        match &self.matcher {
            TemporaryRuleMatch::KnownCreditsRejection => {
                status == 400 && class == ProviderErrorClass::InsufficientCredits
            }
            TemporaryRuleMatch::HttpError {
                statuses,
                fields,
                body_contains,
            } => {
                if !statuses.contains(&status) {
                    return false;
                }
                if fields.is_empty() && body_contains.is_empty() {
                    return true;
                }
                // Fail closed on incomplete/oversized evidence. Never match an
                // internal body-read failure message or a truncated JSON prefix.
                let Some(body) = body.filter(|body| body.len() <= MAX_MATCH_BYTES) else {
                    return false;
                };
                if !fields.is_empty() {
                    let Ok(value) = serde_json::from_str::<serde_json::Value>(body) else {
                        return false;
                    };
                    for field in fields {
                        let scalar = value.pointer(&field.pointer).and_then(|value| match value {
                            serde_json::Value::String(text) => Some(text.clone()),
                            serde_json::Value::Number(_) | serde_json::Value::Bool(_) => {
                                Some(value.to_string())
                            }
                            _ => None,
                        });
                        let Some(scalar) = scalar else {
                            return false;
                        };
                        let matched = match field.mode {
                            ErrorFieldMatchMode::Exact => {
                                field.values.iter().any(|text| text == &scalar)
                            }
                            ErrorFieldMatchMode::Contains => {
                                let scalar = scalar.to_lowercase();
                                field
                                    .values
                                    .iter()
                                    .any(|text| scalar.contains(&text.to_lowercase()))
                            }
                        };
                        if !matched {
                            return false;
                        }
                    }
                }
                body_contains.is_empty() || {
                    let lower = body.to_lowercase();
                    body_contains
                        .iter()
                        .any(|text| lower.contains(&text.to_lowercase()))
                }
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum TemporaryWaitStatus {
    Waiting,
    Ready,
    Probing,
}

/// Local scheduling evidence only. No error body, Key, URL, quota balance or
/// opaque resource digest is exposed by this read-only diagnostic projection.
#[derive(Debug, Clone, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct TemporaryWait {
    pub id: String,
    pub rule_id: String,
    pub rule_version: u64,
    pub credential_id: String,
    pub credential_name: String,
    pub destination_id: String,
    pub model: Option<String>,
    pub status: TemporaryWaitStatus,
    pub next_probe_in_seconds: Option<u64>,
    pub retry_not_before: Option<String>,
    pub retry_unbounded: bool,
    pub failures: u32,
}

#[cfg(test)]
mod tests;
