//! Shared billing contracts. Amounts are estimates in their explicit native units.

use chrono::{DateTime, Utc};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

pub use ocg_domain::billing::{BillingModel, BillingSource};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CreditRate {
    pub model: String,
    pub input_per_million: f64,
    pub output_per_million: f64,
    pub cache_read_per_million: Option<f64>,
    pub cache_write_per_million: Option<f64>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MonthlyCredits {
    pub amount: f64,
    /// First renewal boundary and immutable calendar anchor, including its UTC time.
    pub next_reset_at: DateTime<Utc>,
    /// Calendar boundaries use this fixed UTC offset, e.g. 480 for China.
    pub timezone_offset_minutes: i32,
    pub renewal_ends_at: Option<DateTime<Utc>>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CreditConfiguration {
    pub name: String,
    pub currency: String,
    pub credits_per_currency: f64,
    pub rates: Vec<CreditRate>,
    pub monthly: Option<MonthlyCredits>,
    pub source_url: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum CreditBucketKind {
    Monthly,
    TopUp,
    Manual,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CreditBucket {
    pub id: String,
    pub kind: CreditBucketKind,
    pub label: String,
    pub granted: f64,
    pub remaining: f64,
    pub starts_at: DateTime<Utc>,
    pub expires_at: Option<DateTime<Utc>>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CreditPreset {
    pub id: String,
    pub configuration: CreditConfiguration,
    pub initial_grant: f64,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CreditMeterView {
    pub credential_id: String,
    pub meter_id: String,
    pub configuration: CreditConfiguration,
    pub buckets: Vec<CreditBucket>,
    pub remaining: f64,
    pub active_granted: f64,
    pub spent_since_calibration: f64,
    pub overdrawn: f64,
    pub unpriced_requests: u64,
    pub pending_requests: u64,
    pub last_calibration_at: Option<DateTime<Utc>>,
    pub estimated_at: DateTime<Utc>,
    /// Next actual renewal; configuration.next_reset_at remains the calendar anchor.
    pub next_reset_at: Option<DateTime<Utc>>,
}

/// Portable personal-account baseline. Local request receipts and meter identities stay local.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PortableCreditMeter {
    pub configuration: CreditConfiguration,
    pub buckets: Vec<CreditBucket>,
    pub spent_since_calibration: f64,
    pub overdrawn: f64,
    pub unpriced_requests: u64,
    pub last_calibration_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub monthly_cursor: Option<DateTime<Utc>>,
    pub exported_at: DateTime<Utc>,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BillingStatus {
    pub account_id: String,
    pub model: BillingModel,
    pub source: BillingSource,
    pub unit: String,
    pub configurable_credits: bool,
    pub manual_calibration: bool,
    pub official_refresh: bool,
    pub usage: Option<crate::dashboard_v3::ProviderUsage>,
    pub cash: Option<crate::official_api::OfficialApiStatus>,
    pub credits: Option<CreditMeterView>,
    pub presets: Vec<CreditPreset>,
    pub revision: u64,
    pub process_generation: u64,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CreditConfigureRequest {
    pub configuration: CreditConfiguration,
    /// Required for initial setup; omitted for a rate/settings edit so balances survive.
    pub initial_buckets: Option<Vec<CreditBucket>>,
    #[serde(flatten)]
    pub expectation: crate::dashboard_v3::MutationExpectation,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CreditBalanceCorrection {
    pub bucket_id: String,
    pub remaining: f64,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CreditCalibrationRequest {
    pub balances: Vec<CreditBalanceCorrection>,
    #[serde(flatten)]
    pub expectation: crate::dashboard_v3::MutationExpectation,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CreditGrantRequest {
    pub label: String,
    pub amount: f64,
    pub expires_at: Option<DateTime<Utc>>,
    #[serde(flatten)]
    pub expectation: crate::dashboard_v3::MutationExpectation,
}
