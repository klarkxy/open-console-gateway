//! Official API financial evidence for matching saved presets, never arbitrary URLs.
//!
//! Inference stays on Configurable HTTP. Prices are reference estimates, not a
//! bill, wallet, quota authority, or exchange-rate conversion. Explicit refresh
//! is the only network path. Account balance failure never changes routing.
pub(crate) mod balance;
pub(crate) mod pricing;
#[cfg(debug_assertions)]
#[doc(hidden)]
pub use balance::{OfficialApiTestGuard, install_official_api_endpoint_for_test};

use crate::dynamic::DynamicProviderRuntime;
use crate::models::Account;
use crate::provider::UpstreamProtocolKind;
use chrono::{DateTime, Datelike, Timelike, Utc, Weekday};
use ocg_domain::dynamic::DynamicAuthKind;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

pub const PRICE_MAX_AGE_DAYS: i64 = 30;
pub const BALANCE_URL: &str = "https://api.deepseek.com/user/balance";
pub const DEEPSEEK_PRICING_URL: &str = "https://api-docs.deepseek.com/quick_start/pricing/";
pub const ZHIPU_PRICING_URL: &str = "https://docs.bigmodel.cn/cn/guide/start/pricing.md";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum OfficialApiKind {
    Deepseek,
    Zhipu,
}

impl OfficialApiKind {
    pub fn pricing_url(self) -> &'static str {
        match self {
            Self::Deepseek => DEEPSEEK_PRICING_URL,
            Self::Zhipu => ZHIPU_PRICING_URL,
        }
    }
    pub fn currency(self) -> &'static str {
        match self {
            Self::Deepseek => "USD",
            Self::Zhipu => "CNY",
        }
    }
    pub fn balance_available(self) -> bool {
        self == Self::Deepseek
    }
    pub fn id(self) -> &'static str {
        match self {
            Self::Deepseek => "deepseek",
            Self::Zhipu => "zhipu",
        }
    }
}

/// Preset provenance is necessary, not sufficient: edited destinations and
/// Coding Plan paths never inherit official API financial evidence.
pub fn kind_for_runtime(runtime: &DynamicProviderRuntime) -> Option<OfficialApiKind> {
    if runtime.auth_kind != DynamicAuthKind::Bearer || runtime.offering != "api" {
        return None;
    }
    let kind = match runtime.preset_id.as_deref()? {
        "deepseek" => OfficialApiKind::Deepseek,
        "zhipu" => OfficialApiKind::Zhipu,
        _ => return None,
    };
    route_is_official(kind, &runtime.endpoint_url, runtime.upstream_protocol).then_some(kind)
}

pub fn route_is_official(
    kind: OfficialApiKind,
    endpoint: &str,
    protocol: UpstreamProtocolKind,
) -> bool {
    let Ok(url) = reqwest::Url::parse(endpoint) else {
        return false;
    };
    if url.scheme() != "https"
        || url.port_or_known_default() != Some(443)
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return false;
    }
    let path = url.path().trim_end_matches('/');
    match kind {
        OfficialApiKind::Deepseek if url.host_str() == Some("api.deepseek.com") => match protocol {
            UpstreamProtocolKind::ChatCompletions => matches!(
                path,
                "" | "/v1" | "/chat/completions" | "/v1/chat/completions"
            ),
            UpstreamProtocolKind::Responses => {
                matches!(path, "" | "/v1" | "/responses" | "/v1/responses")
            }
            UpstreamProtocolKind::Messages => matches!(
                path,
                "/anthropic" | "/anthropic/v1" | "/anthropic/v1/messages"
            ),
        },
        OfficialApiKind::Zhipu if url.host_str() == Some("open.bigmodel.cn") => {
            protocol == UpstreamProtocolKind::ChatCompletions
                && matches!(path, "/api/paas/v4" | "/api/paas/v4/chat/completions")
        }
        _ => false,
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct OfficialPriceRow {
    pub model: String,
    pub currency: String,
    /// `all`, or DeepSeek `peak` / `off_peak` at the frozen attempt time.
    pub period: String,
    pub input_per_million: f64,
    pub output_per_million: f64,
    pub cache_read_per_million: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct OfficialPriceSheet {
    pub kind: OfficialApiKind,
    pub revision: String,
    pub source_url: String,
    pub observed_at: DateTime<Utc>,
    pub valid_until: DateTime<Utc>,
    pub rows: Vec<OfficialPriceRow>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct OfficialBalance {
    pub currency: String,
    pub total: f64,
    pub granted: f64,
    pub topped_up: f64,
    pub observed_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct OfficialSpend {
    pub currency: String,
    pub amount: f64,
    pub priced_requests: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct OfficialApiStatus {
    pub account_id: String,
    pub provider_id: String,
    pub kind: OfficialApiKind,
    pub balance_available: bool,
    pub balances: Vec<OfficialBalance>,
    pub prices: OfficialPriceSheet,
    pub month_started_at: DateTime<Utc>,
    pub month_spend: Vec<OfficialSpend>,
    pub unpriced_requests: u64,
    pub revision: u64,
    pub process_generation: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct OfficialApiPrices {
    pub provider_id: String,
    pub prices: OfficialPriceSheet,
    pub revision: u64,
    pub process_generation: u64,
}

/// The hash binds balance evidence to ciphertext + provider configuration;
/// neither old-Key results nor results for a changed endpoint are displayed.
pub(crate) fn balance_source(account: &Account, runtime: &DynamicProviderRuntime) -> String {
    let mut hash = Sha256::new();
    for value in [
        &account.id,
        &account.key_cipher,
        &runtime.id,
        &runtime.endpoint_url,
    ] {
        hash.update(value.len().to_le_bytes());
        hash.update(value.as_bytes());
    }
    format!("official-api-balance:{}", hex::encode(hash.finalize()))
}

pub(crate) fn peak_at(now: DateTime<Utc>) -> bool {
    !matches!(now.weekday(), Weekday::Sat | Weekday::Sun)
        && ((1..4).contains(&now.hour()) || (6..10).contains(&now.hour()))
}

/// Frozen per-attempt reference; a refresh cannot reprice a stream in flight.
#[derive(Clone)]
pub(crate) struct OfficialAttemptPrice {
    pub provider_id: String,
    pub sheet: OfficialPriceSheet,
    pub model: String,
    pub at: DateTime<Utc>,
}

impl OfficialAttemptPrice {
    pub fn amount(&self, prompt: i64, output: i64, cached: i64, created: i64) -> Option<f64> {
        if self.at < self.sheet.observed_at
            || self.at >= self.sheet.valid_until
            || created != 0
            || prompt < 0
            || output < 0
            || cached < 0
            || cached > prompt
        {
            return None;
        }
        let period = if peak_at(self.at) { "peak" } else { "off_peak" };
        let mut rows =
            self.sheet.rows.iter().filter(|row| {
                row.model == self.model && (row.period == "all" || row.period == period)
            });
        let row = rows.next()?;
        if rows.next().is_some() || row.currency != self.sheet.kind.currency() {
            return None;
        }
        let cache_rate = if cached == 0 {
            0.0
        } else {
            row.cache_read_per_million?
        };
        let value = ((prompt - cached) as f64 * row.input_per_million
            + output as f64 * row.output_per_million
            + cached as f64 * cache_rate)
            / 1_000_000.0;
        (value.is_finite() && value >= 0.0).then_some(value)
    }
}

#[cfg(test)]
pub(crate) mod tests;
