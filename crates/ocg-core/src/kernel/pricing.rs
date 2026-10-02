//! Pricing snapshot value types and the historical limit constant.
//!
//! HTML fetch and snapshot storage live in `crate::pricing`. This module does
//! not seed a runtime snapshot and does not price a request. `SEED_LIMITS` is
//! the v13/v22 fallback when a database has no stored pricing snapshot.

use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};

pub const SOURCE_URL: &str = "https://opencode.ai/docs/go/";

/// Evidence level attached to a provider-scoped pricing snapshot.
///
/// `experimental` is reserved for a captured but not yet promoted contract;
/// callers must not present it as authoritative pricing.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ProviderPricingEvidence {
    Verified,
    Experimental,
    Unavailable,
}

/// One immutable provider/model pricing value. Unknown official fields stay
/// `None`; the manager never manufactures prices or allowances.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct ProviderPricingValue {
    pub(crate) model_id: String,
    pub(crate) display_name: String,
    pub(crate) input_per_million: Option<f64>,
    pub(crate) output_per_million: Option<f64>,
    pub(crate) cache_read_per_million: Option<f64>,
    pub(crate) cache_write_per_million: Option<f64>,
    pub(crate) plan_limit: Option<f64>,
    pub(crate) model_allowance: Option<f64>,
    pub(crate) quota_multiplier: Option<f64>,
    pub(crate) paid_plan_price: Option<f64>,
    pub(crate) currency: Option<String>,
    pub(crate) min_input_tokens: Option<i64>,
    pub(crate) max_input_tokens: Option<i64>,
    pub(crate) time_window: PricingTimeWindow,
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct ProviderPricingValueWire {
    model_id: String,
    display_name: String,
    input_per_million: Option<f64>,
    output_per_million: Option<f64>,
    cache_read_per_million: Option<f64>,
    cache_write_per_million: Option<f64>,
    plan_limit: Option<f64>,
    model_allowance: Option<f64>,
    quota_multiplier: Option<f64>,
    paid_plan_price: Option<f64>,
    currency: Option<String>,
    #[serde(default)]
    min_input_tokens: Option<i64>,
    #[serde(default)]
    max_input_tokens: Option<i64>,
    #[serde(default)]
    time_window: PricingTimeWindow,
}

impl ProviderPricingValue {
    /// History-only constructor for stored snapshot decode.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        model_id: impl Into<String>,
        display_name: impl Into<String>,
        input_per_million: Option<f64>,
        output_per_million: Option<f64>,
        cache_read_per_million: Option<f64>,
        cache_write_per_million: Option<f64>,
        plan_limit: Option<f64>,
        model_allowance: Option<f64>,
        paid_plan_price: Option<f64>,
        currency: Option<String>,
        min_input_tokens: Option<i64>,
        max_input_tokens: Option<i64>,
        time_window: PricingTimeWindow,
    ) -> Result<Self> {
        let model_id = model_id.into();
        let display_name = display_name.into();
        if model_id.trim().is_empty() || display_name.trim().is_empty() {
            bail!("provider pricing model id and display name must be non-empty");
        }
        for (name, value) in [
            ("input price", input_per_million),
            ("output price", output_per_million),
            ("cache read price", cache_read_per_million),
            ("cache write price", cache_write_per_million),
            ("paid plan price", paid_plan_price),
        ] {
            ensure_optional_non_negative_finite(name, value)?;
        }
        ensure_optional_positive_finite("plan limit", plan_limit)?;
        ensure_optional_positive_finite("model allowance", model_allowance)?;
        if min_input_tokens.is_some_and(|value| value < 0)
            || max_input_tokens.is_some_and(|value| value < 0)
            || matches!((min_input_tokens, max_input_tokens), (Some(min), Some(max)) if min > max)
        {
            bail!("provider pricing token tier bounds are invalid");
        }
        let quota_multiplier = match (plan_limit, model_allowance) {
            (Some(limit), Some(allowance)) => Some(quota_multiplier(limit, allowance)?),
            _ => None,
        };
        Ok(Self {
            model_id,
            display_name,
            input_per_million,
            output_per_million,
            cache_read_per_million,
            cache_write_per_million,
            plan_limit,
            model_allowance,
            quota_multiplier,
            paid_plan_price,
            currency,
            min_input_tokens,
            max_input_tokens,
            time_window,
        })
    }

    pub fn model_id(&self) -> &str {
        &self.model_id
    }

    pub fn display_name(&self) -> &str {
        &self.display_name
    }

    pub fn input_per_million(&self) -> Option<f64> {
        self.input_per_million
    }

    pub fn output_per_million(&self) -> Option<f64> {
        self.output_per_million
    }

    pub fn cache_read_per_million(&self) -> Option<f64> {
        self.cache_read_per_million
    }

    pub fn cache_write_per_million(&self) -> Option<f64> {
        self.cache_write_per_million
    }

    pub fn plan_limit(&self) -> Option<f64> {
        self.plan_limit
    }

    pub fn model_allowance(&self) -> Option<f64> {
        self.model_allowance
    }

    pub fn quota_multiplier(&self) -> Option<f64> {
        self.quota_multiplier
    }

    pub fn paid_plan_price(&self) -> Option<f64> {
        self.paid_plan_price
    }

    pub fn currency(&self) -> Option<&str> {
        self.currency.as_deref()
    }

    pub fn min_input_tokens(&self) -> Option<i64> {
        self.min_input_tokens
    }

    pub fn max_input_tokens(&self) -> Option<i64> {
        self.max_input_tokens
    }

    pub fn time_window(&self) -> PricingTimeWindow {
        self.time_window
    }

    pub(crate) fn from_wire(wire: ProviderPricingValueWire) -> Result<Self> {
        let applied_multiplier = wire.quota_multiplier;
        let mut value = Self::new(
            wire.model_id,
            wire.display_name,
            wire.input_per_million,
            wire.output_per_million,
            wire.cache_read_per_million,
            wire.cache_write_per_million,
            wire.plan_limit,
            wire.model_allowance,
            wire.paid_plan_price,
            wire.currency,
            wire.min_input_tokens,
            wire.max_input_tokens,
            wire.time_window,
        )?;
        if let Some(multiplier) = applied_multiplier {
            ensure_positive_finite("quota multiplier", multiplier)?;
            value.quota_multiplier = Some(multiplier);
        }
        Ok(value)
    }
}

/// Plan limit divided by model allowance. Used when a historical value is
/// constructed. A stored multiplier on the current wire shape replaces it.
fn quota_multiplier(plan_limit: f64, model_allowance: f64) -> Result<f64> {
    ensure_positive_finite("plan limit", plan_limit)?;
    ensure_positive_finite("model allowance", model_allowance)?;
    Ok(plan_limit / model_allowance)
}

pub(crate) fn ensure_non_negative_finite(name: &str, value: f64) -> Result<()> {
    if !value.is_finite() || value < 0.0 {
        bail!("{name} must be finite and non-negative");
    }
    Ok(())
}

pub(crate) fn ensure_positive_finite(name: &str, value: f64) -> Result<()> {
    if !value.is_finite() || value <= 0.0 {
        bail!("{name} must be finite and positive");
    }
    Ok(())
}

pub(crate) fn ensure_optional_non_negative_finite(name: &str, value: Option<f64>) -> Result<()> {
    if let Some(value) = value {
        ensure_non_negative_finite(name, value)?;
    }
    Ok(())
}

pub(crate) fn ensure_optional_positive_finite(name: &str, value: Option<f64>) -> Result<()> {
    if let Some(value) = value {
        ensure_positive_finite(name, value)?;
    }
    Ok(())
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PricingLimits {
    pub window_5h: f64,
    pub window_week: f64,
    pub window_month: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PricingAdjustment {
    pub label: String,
    pub multiplier: f64,
    pub applies_to: String,
}

#[derive(
    Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash,
)]
#[serde(rename_all = "snake_case")]
pub enum PricingTimeWindow {
    #[default]
    Always,
    OffPeak,
    Peak,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PricingModel {
    pub model_id: String,
    pub display_name: String,
    pub input: f64,
    pub output: f64,
    pub cache_read: f64,
    pub cache_write: Option<f64>,
    pub usage: f64,
    /// Editable OpenCode Go multiplier applied after the official token rates.
    /// Fresh official snapshots derive it as monthly limit / model Usage.
    pub quota_multiplier: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub min_input_tokens: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_input_tokens: Option<i64>,
    /// Official Peak / Off-Peak row. Missing in older snapshots means `always`.
    #[serde(default)]
    pub time_window: PricingTimeWindow,
    pub adjustments: Vec<PricingAdjustment>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PricingSnapshot {
    pub revision: String,
    pub activated_at: String,
    pub document_updated_at: String,
    pub source_url: String,
    pub content_hash: String,
    pub limits: PricingLimits,
    pub models: Vec<PricingModel>,
    pub adjustment_policy_version: String,
}

/// Persistence-shaped provider pricing snapshot record stored by
/// `provider_pricing_snapshots`. The value shape lives here so `db` can own
/// storage without depending on the clocked `crate::pricing` module.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderPricingSnapshot {
    pub provider_id: String,

    pub revision: String,
    pub activated_at: String,
    pub document_updated_at: Option<String>,
    pub source_url: String,
    pub content_hash: String,
    pub snapshot_json: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PricingEstimate {
    /// Raw provider-priced token value before the account plan's quota
    /// multiplier. This is USD for the verified OpenCode Go table.
    pub raw_cost_usd: Option<f64>,
    /// Account/key quota debit. This intentionally remains identical to the
    /// legacy `cost` field for OpenCode Go.
    pub quota_debit: Option<f64>,
    /// User-paid equivalent remains unknown without account-specific official
    /// plan-price evidence (for example first-month vs recurring pricing).
    pub effective_paid_cost_usd: Option<f64>,
    pub cost: Option<f64>,
    pub pricing_revision_id: Option<String>,
    pub quota_multiplier: Option<f64>,
    pub local_adjustment_multiplier: Option<f64>,
    pub cost_state: &'static str,
}

impl PricingEstimate {
    /// No money, revision, or multiplier. New attempts use this instead of priced or free.
    pub(crate) fn unknown() -> Self {
        Self {
            raw_cost_usd: None,
            quota_debit: None,
            effective_paid_cost_usd: None,
            cost: None,
            pricing_revision_id: None,
            quota_multiplier: None,
            local_adjustment_multiplier: None,
            cost_state: "unknown",
        }
    }
}

/// Historical OpenCode Go usage limits. v13 and v22 read these only when
/// a database has no stored pricing snapshot.
pub const SEED_LIMITS: PricingLimits = PricingLimits {
    window_5h: 12.0,
    window_week: 30.0,
    window_month: 60.0,
};
