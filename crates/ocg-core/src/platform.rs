//! Account-owned New API and Sub2API metadata. Inference remains Custom HTTP.
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

pub mod reader;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum PlatformKind {
    NewApi,
    Sub2api,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct PlatformGroup {
    pub id: Option<String>,
    pub platform: Option<String>,
    #[serde(default)]
    pub subscription_type: Option<String>,
    pub auto_groups: Vec<String>,
    pub verified: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct PlatformAccount {
    pub id: String,
    pub kind: PlatformKind,
    pub name: String,
    pub base_url: String,
    pub has_user_credential: bool,
    pub version: u64,
    pub snapshot: Option<PlatformSnapshot>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct PlatformLink {
    pub account_id: String,
    pub platform_account_id: String,
    pub group: PlatformGroup,
    pub snapshot: Option<PlatformSnapshot>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum PlatformQuotaKind {
    Wallet,
    Subscription,
    KeyLimit,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct PlatformQuota {
    pub kind: PlatformQuotaKind,
    pub scope_id: String,
    pub unit: String,
    pub used: Option<f64>,
    pub remaining: Option<f64>,
    pub limit: Option<f64>,
    pub unlimited: bool,
    pub period: Option<String>,
    pub resets_at: Option<i64>,
    pub expires_at: Option<i64>,
    pub source: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct PlatformModel {
    pub id: String,
    pub platform: Option<String>,
    pub group_id: Option<String>,
    /// A storefront row is never proof of inference permission.
    pub source: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct PlatformPrice {
    pub model: String,
    pub group_id: Option<String>,
    pub currency: String,
    /// All numeric rates are currency per token, never per million tokens.
    pub input: Option<f64>,
    pub output: Option<f64>,
    pub cache_read: Option<f64>,
    pub cache_write: Option<f64>,
    pub source: String,
    pub official_reference: bool,
    pub unavailable_reason: Option<String>,
    pub valid_until: i64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct PlatformSnapshot {
    pub observed_at: i64,
    pub stale: bool,
    /// Fixed component/error codes only, never upstream bodies or credentials.
    pub errors: Vec<String>,
    pub quotas: Vec<PlatformQuota>,
    pub models: Vec<PlatformModel>,
    pub prices: Vec<PlatformPrice>,
    pub groups: Vec<PlatformGroup>,
    pub billing_preference: Option<String>,
    pub wallet_overflow: Option<bool>,
}

/// Credentials deliberately have no Debug/Serialize implementation.
pub struct PlatformReadRequest<'a> {
    pub kind: PlatformKind,
    pub base_url: &'a str,
    pub user_credential: Option<&'a str>,
    pub key: Option<&'a str>,
    pub group: &'a PlatformGroup,
    pub now: i64,
}

/// Only portable configuration; never management credentials or observations.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PortablePlatformAccount {
    pub id: String,
    pub kind: PlatformKind,
    pub name: String,
    pub base_url: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PortablePlatformLink {
    pub account_id: String,
    pub platform_account_id: String,
    pub group: PlatformGroup,
}

pub fn inference_endpoint(
    base: &str,
    protocol: crate::provider::UpstreamProtocolKind,
) -> anyhow::Result<String> {
    let base = validate_platform_base_url(base)?;
    let suffix = match protocol {
        crate::provider::UpstreamProtocolKind::ChatCompletions => "chat/completions",
        crate::provider::UpstreamProtocolKind::Responses => "responses",
        crate::provider::UpstreamProtocolKind::Messages => "messages",
    };
    let base = base.strip_suffix("/v1").unwrap_or(&base);
    Ok(format!("{base}/v1/{suffix}"))
}

pub fn validate_platform_base_url(value: &str) -> anyhow::Result<String> {
    let base = crate::custom::validate_custom_endpoint_url(value)?;
    anyhow::ensure!(
        !["/v1/chat/completions", "/v1/messages", "/v1/responses"]
            .iter()
            .any(|suffix| base.ends_with(suffix)),
        "platform address must be the site root, not a complete inference endpoint"
    );
    Ok(base.strip_suffix("/v1").unwrap_or(&base).to_string())
}
