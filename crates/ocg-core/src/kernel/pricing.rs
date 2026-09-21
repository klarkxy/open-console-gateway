//! I/O-free pricing identities, snapshot value types, cost arithmetic, and
//! the immutable embedded OpenCode Go seed view.
//!
//! HTML fetch, database storage, and clocked `estimate()` stay in
//! `crate::pricing`. This module is the typed seam later control-plane and
//! GatewayExecutor work can share without pulling db or HTTP. The seed view
//! takes `activated_at` as an argument; it does not read a clock.

use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};

use super::ids::{
    COMMAND_CODE_PROVIDER_ID, OLLAMA_CLOUD_PRICING_URL, OLLAMA_PROVIDER_ID, OPENCODE_PROVIDER_ID,
    OPENCODE_ZEN_FREE_PROVIDER_ID,
};

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

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProviderPricingCapability {
    pub provider_id: &'static str,

    pub evidence: ProviderPricingEvidence,
    pub experimental: bool,
    pub source_url: Option<&'static str>,
    pub manual_refresh_available: bool,
}

pub fn provider_pricing_capability(provider_id: &str) -> Option<ProviderPricingCapability> {
    match provider_id {
        OPENCODE_PROVIDER_ID => Some(ProviderPricingCapability {
            provider_id: OPENCODE_PROVIDER_ID,
            evidence: ProviderPricingEvidence::Verified,
            experimental: false,
            source_url: Some(SOURCE_URL),
            manual_refresh_available: true,
        }),
        COMMAND_CODE_PROVIDER_ID => Some(ProviderPricingCapability {
            provider_id: COMMAND_CODE_PROVIDER_ID,
            evidence: ProviderPricingEvidence::Verified,
            experimental: false,
            source_url: Some("https://commandcode.ai/docs/plans/goat"),
            manual_refresh_available: true,
        }),
        OLLAMA_PROVIDER_ID => Some(ProviderPricingCapability {
            provider_id: OLLAMA_PROVIDER_ID,
            evidence: ProviderPricingEvidence::Verified,
            experimental: false,
            source_url: Some(OLLAMA_CLOUD_PRICING_URL),
            manual_refresh_available: true,
        }),
        OPENCODE_ZEN_FREE_PROVIDER_ID => Some(ProviderPricingCapability {
            provider_id: OPENCODE_ZEN_FREE_PROVIDER_ID,
            evidence: ProviderPricingEvidence::Unavailable,
            experimental: false,
            source_url: None,
            manual_refresh_available: false,
        }),
        _ => None,
    }
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
    #[allow(clippy::too_many_arguments)]
    pub fn new(
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

    pub fn with_quota_multiplier(mut self, multiplier: f64) -> Result<Self> {
        ensure_positive_finite("quota multiplier", multiplier)?;
        self.quota_multiplier = Some(multiplier);
        Ok(self)
    }
}

/// Provider-neutral cost accounting. Raw supplier value and account-quota
/// debit are distinct; paid equivalent stays unknown without an official paid
/// plan price.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
pub struct ProviderCostEstimate {
    pub raw_cost: Option<f64>,
    pub quota_debit: Option<f64>,
    pub paid_cost: Option<f64>,
    pub cost_state: ProviderCostState,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ProviderCostState {
    Priced,
    Unpriced,
    Free,
}

impl ProviderCostEstimate {
    pub fn from_raw(
        raw_cost: f64,
        plan_limit: Option<f64>,
        model_allowance: Option<f64>,
        paid_plan_price: Option<f64>,
    ) -> Result<Self> {
        ensure_non_negative_finite("raw cost", raw_cost)?;
        ensure_optional_positive_finite("plan limit", plan_limit)?;
        ensure_optional_positive_finite("model allowance", model_allowance)?;
        ensure_optional_non_negative_finite("paid plan price", paid_plan_price)?;
        let multiplier = match (plan_limit, model_allowance) {
            (Some(limit), Some(allowance)) => Some(quota_multiplier(limit, allowance)?),
            _ => None,
        };
        Self::from_raw_with_multiplier(raw_cost, multiplier, paid_plan_price, plan_limit)
    }

    pub fn from_raw_with_multiplier(
        raw_cost: f64,
        quota_multiplier: Option<f64>,
        paid_plan_price: Option<f64>,
        plan_limit: Option<f64>,
    ) -> Result<Self> {
        ensure_non_negative_finite("raw cost", raw_cost)?;
        ensure_optional_positive_finite("quota multiplier", quota_multiplier)?;
        ensure_optional_non_negative_finite("paid plan price", paid_plan_price)?;
        ensure_optional_positive_finite("plan limit", plan_limit)?;
        let quota_debit = quota_multiplier
            .and_then(|multiplier| ocg_domain::billing::convert_charge(raw_cost, multiplier));
        let paid_cost = match (quota_debit, paid_plan_price, plan_limit) {
            (Some(_), Some(0.0), Some(_)) => Some(0.0),
            (Some(debit), Some(price), Some(limit)) => {
                ocg_domain::billing::convert_charge(debit, price / limit)
            }
            _ => None,
        };
        Ok(Self {
            raw_cost: Some(raw_cost),
            quota_debit,
            paid_cost,
            cost_state: if quota_debit.is_some() {
                ProviderCostState::Priced
            } else {
                ProviderCostState::Unpriced
            },
        })
    }

    /// Zen Free is neither a supplier charge nor an account-quota debit.
    pub const fn zen_free() -> Self {
        Self {
            raw_cost: Some(0.0),
            quota_debit: Some(0.0),
            paid_cost: Some(0.0),
            cost_state: ProviderCostState::Free,
        }
    }
}

/// Converts an official plan limit and model allowance into the initial
/// account-level quota debit multiplier. A persisted provider snapshot may
/// later carry a validated local override without changing those source fields.
pub fn quota_multiplier(plan_limit: f64, model_allowance: f64) -> Result<f64> {
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
    pub(crate) fn unpriced(revision: &str) -> Self {
        Self {
            raw_cost_usd: None,
            quota_debit: None,
            effective_paid_cost_usd: None,
            cost: None,
            pricing_revision_id: Some(revision.to_string()),
            quota_multiplier: None,
            local_adjustment_multiplier: None,
            cost_state: "unpriced",
        }
    }

    pub(crate) fn free(revision: &str) -> Self {
        Self {
            raw_cost_usd: Some(0.0),
            quota_debit: Some(0.0),
            effective_paid_cost_usd: Some(0.0),
            cost: None,
            pricing_revision_id: Some(revision.to_string()),
            quota_multiplier: None,
            local_adjustment_multiplier: None,
            cost_state: "free",
        }
    }
}

/// Official Go usage limits captured into the embedded seed.
pub const SEED_LIMITS: PricingLimits = PricingLimits {
    window_5h: 12.0,
    window_week: 30.0,
    window_month: 60.0,
};

const SEED_REVISION: &str = "seed-2026-08-16-local-v4";
const SEED_CONTENT_HASH: &str = "embedded-opencode-go-2026-08-16";
const SEED_DOCUMENT_UPDATED_AT: &str = "2026-08-16T00:00:00.000Z";
const SEED_ADJUSTMENT_POLICY_VERSION: &str = "local-v4";

/// Immutable embedded OpenCode Go seed snapshot. The activation timestamp
/// is supplied by the caller; this function does not read a clock.
pub fn seed_snapshot(activated_at: String) -> PricingSnapshot {
    let mut models = seed_models();
    apply_seed_pricing_policy(&mut models, SEED_LIMITS.window_month);
    sort_seed_models(&mut models);
    PricingSnapshot {
        revision: SEED_REVISION.to_string(),
        activated_at,
        document_updated_at: SEED_DOCUMENT_UPDATED_AT.to_string(),
        source_url: SOURCE_URL.to_string(),
        content_hash: SEED_CONTENT_HASH.to_string(),
        limits: SEED_LIMITS,
        models,
        adjustment_policy_version: SEED_ADJUSTMENT_POLICY_VERSION.to_string(),
    }
}

fn seed_models() -> Vec<PricingModel> {
    vec![
        seed_model("grok-4.5", "Grok 4.5", 2.0, 6.0, 0.3, None, 15.0),
        seed_model("glm-5.3", "GLM-5.3", 1.4, 4.4, 0.26, None, 15.0),
        seed_model("glm-5.2", "GLM-5.2", 1.4, 4.4, 0.26, None, 60.0),
        seed_model("glm-5.1", "GLM-5.1", 1.4, 4.4, 0.26, None, 60.0),
        seed_tier_with_usage(
            "gpt-5.6-luna",
            "GPT 5.6 Luna (≤ 272K tokens)",
            0.2,
            1.2,
            0.02,
            Some(0.25),
            15.0,
            None,
            Some(272_000),
        ),
        seed_tier_with_usage(
            "gpt-5.6-luna",
            "GPT 5.6 Luna (> 272K tokens)",
            0.4,
            1.8,
            0.04,
            Some(0.5),
            15.0,
            Some(272_001),
            None,
        ),
        seed_model("kimi-k3", "Kimi K3", 3.0, 15.0, 0.3, None, 15.0),
        seed_model(
            "kimi-k2.7-code",
            "Kimi K2.7 Code",
            0.95,
            4.0,
            0.19,
            None,
            60.0,
        ),
        seed_model("kimi-k2.6", "Kimi K2.6", 0.95, 4.0, 0.16, None, 60.0),
        seed_model("mimo-v2.5", "MiMo V2.5", 0.14, 0.28, 0.0028, None, 60.0),
        seed_model(
            "mimo-v2.5-pro",
            "MiMo V2.5 Pro",
            0.435,
            0.87,
            0.003625,
            None,
            15.0,
        ),
        seed_model("minimax-m3", "MiniMax M3", 0.3, 1.2, 0.06, None, 60.0),
        seed_model(
            "minimax-m2.7",
            "MiniMax M2.7",
            0.3,
            1.2,
            0.06,
            Some(0.375),
            60.0,
        ),
        seed_model(
            "minimax-m2.5",
            "MiniMax M2.5",
            0.3,
            1.2,
            0.06,
            Some(0.375),
            60.0,
        ),
        seed_model(
            "qwen3.8-max",
            "Qwen3.8 Max",
            2.0,
            6.0,
            0.25,
            Some(2.5),
            15.0,
        ),
        seed_model(
            "qwen3.7-max",
            "Qwen3.7 Max",
            2.5,
            7.5,
            0.5,
            Some(3.125),
            60.0,
        ),
        seed_tier(
            "qwen3.7-plus",
            "Qwen3.7 Plus (≤ 256K tokens)",
            0.4,
            1.6,
            0.04,
            0.5,
            None,
            Some(256_000),
        ),
        seed_tier(
            "qwen3.7-plus",
            "Qwen3.7 Plus (> 256K tokens)",
            1.2,
            4.8,
            0.12,
            1.5,
            Some(256_001),
            None,
        ),
        seed_tier(
            "qwen3.6-plus",
            "Qwen3.6 Plus (≤ 256K tokens)",
            0.5,
            3.0,
            0.05,
            0.625,
            None,
            Some(256_000),
        ),
        seed_tier(
            "qwen3.6-plus",
            "Qwen3.6 Plus (> 256K tokens)",
            2.0,
            6.0,
            0.2,
            2.5,
            Some(256_001),
            None,
        ),
        seed_scheduled(
            "deepseek-v4-pro",
            "DeepSeek V4 Pro (Off-Peak)",
            0.66,
            1.98,
            0.022,
            None,
            15.0,
            PricingTimeWindow::OffPeak,
        ),
        seed_scheduled(
            "deepseek-v4-pro",
            "DeepSeek V4 Pro (Peak)",
            1.32,
            3.96,
            0.044,
            None,
            15.0,
            PricingTimeWindow::Peak,
        ),
        seed_scheduled(
            "deepseek-v4-flash",
            "DeepSeek V4 Flash (Off-Peak)",
            0.22,
            0.66,
            0.007,
            None,
            15.0,
            PricingTimeWindow::OffPeak,
        ),
        seed_scheduled(
            "deepseek-v4-flash",
            "DeepSeek V4 Flash (Peak)",
            0.44,
            1.32,
            0.014,
            None,
            15.0,
            PricingTimeWindow::Peak,
        ),
        seed_model(
            "muse-spark-1.2",
            "Muse Spark 1.2",
            0.10,
            0.20,
            0.002,
            None,
            60.0,
        ),
        seed_model(
            "muse-spark-1.2-contributor",
            "Muse Spark 1.2 Contributor",
            0.10,
            0.20,
            0.002,
            None,
            60.0,
        ),
        seed_model("hy3", "Hy3", 0.14, 0.58, 0.035, None, 60.0),
    ]
}

fn seed_model(
    id: &str,
    name: &str,
    input: f64,
    output: f64,
    cache_read: f64,
    cache_write: Option<f64>,
    usage: f64,
) -> PricingModel {
    PricingModel {
        model_id: id.to_string(),
        display_name: name.to_string(),
        input,
        output,
        cache_read,
        cache_write,
        usage,
        quota_multiplier: SEED_LIMITS.window_month / usage,
        min_input_tokens: None,
        max_input_tokens: None,
        time_window: PricingTimeWindow::Always,
        adjustments: Vec::new(),
    }
}

#[allow(clippy::too_many_arguments)]
fn seed_scheduled(
    id: &str,
    name: &str,
    input: f64,
    output: f64,
    cache_read: f64,
    cache_write: Option<f64>,
    usage: f64,
    time_window: PricingTimeWindow,
) -> PricingModel {
    let mut model = seed_model(id, name, input, output, cache_read, cache_write, usage);
    model.time_window = time_window;
    model
}

#[allow(clippy::too_many_arguments)]
fn seed_tier(
    id: &str,
    name: &str,
    input: f64,
    output: f64,
    cache_read: f64,
    cache_write: f64,
    min_input_tokens: Option<i64>,
    max_input_tokens: Option<i64>,
) -> PricingModel {
    seed_tier_with_usage(
        id,
        name,
        input,
        output,
        cache_read,
        Some(cache_write),
        60.0,
        min_input_tokens,
        max_input_tokens,
    )
}

#[allow(clippy::too_many_arguments)]
fn seed_tier_with_usage(
    id: &str,
    name: &str,
    input: f64,
    output: f64,
    cache_read: f64,
    cache_write: Option<f64>,
    usage: f64,
    min_input_tokens: Option<i64>,
    max_input_tokens: Option<i64>,
) -> PricingModel {
    let mut model = seed_model(id, name, input, output, cache_read, cache_write, usage);
    model.min_input_tokens = min_input_tokens;
    model.max_input_tokens = max_input_tokens;
    model
}

fn apply_seed_pricing_policy(models: &mut [PricingModel], monthly_limit: f64) {
    for model in models.iter_mut() {
        model.quota_multiplier = monthly_limit / model.usage;
    }
    add_seed_adjustments(models);
}

fn add_seed_adjustments(models: &mut [PricingModel]) {
    for model in models {
        model.adjustments.clear();
        match model.model_id.as_str() {
            "minimax-m3" => {
                model.adjustments = vec![
                    PricingAdjustment {
                        label: ">512K input".to_string(),
                        multiplier: 2.0,
                        applies_to: "input,output,cache_read,cache_write".to_string(),
                    },
                    PricingAdjustment {
                        label: "priority service tier".to_string(),
                        multiplier: 1.5,
                        applies_to: "input,output,cache_read,cache_write".to_string(),
                    },
                    PricingAdjustment {
                        label: ">512K + priority".to_string(),
                        multiplier: 3.0,
                        applies_to: "input,output,cache_read,cache_write".to_string(),
                    },
                ];
            }
            "minimax-m2.7" | "minimax-m2.5" => {
                model.adjustments = vec![PricingAdjustment {
                    label: "highspeed alias".to_string(),
                    multiplier: 2.0,
                    applies_to: "input,output".to_string(),
                }];
            }
            _ => {}
        }
    }
}

fn sort_seed_models(models: &mut [PricingModel]) {
    models.sort_by(|left, right| {
        left.model_id
            .cmp(&right.model_id)
            .then(left.min_input_tokens.cmp(&right.min_input_tokens))
            .then(left.time_window.cmp(&right.time_window))
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seed_snapshot_is_the_immutable_embedded_view() {
        let snapshot = seed_snapshot("2026-08-16T12:00:00.000Z".to_string());
        assert_eq!(snapshot.revision, "seed-2026-08-16-local-v4");
        assert_eq!(snapshot.activated_at, "2026-08-16T12:00:00.000Z");
        assert_eq!(snapshot.document_updated_at, "2026-08-16T00:00:00.000Z");
        assert_eq!(snapshot.content_hash, "embedded-opencode-go-2026-08-16");
        assert_eq!(snapshot.source_url, SOURCE_URL);
        assert_eq!(snapshot.adjustment_policy_version, "local-v4");
        assert_eq!(snapshot.limits, SEED_LIMITS);
        assert_eq!(SEED_LIMITS.window_5h, 12.0);
        assert_eq!(SEED_LIMITS.window_week, 30.0);
        assert_eq!(SEED_LIMITS.window_month, 60.0);
        let grok = snapshot
            .models
            .iter()
            .find(|entry| entry.model_id == "grok-4.5")
            .unwrap();
        assert_eq!(grok.quota_multiplier, 4.0);
        let glm = snapshot
            .models
            .iter()
            .find(|entry| entry.model_id == "glm-5.2")
            .unwrap();
        assert_eq!(glm.quota_multiplier, 1.0);
    }
}
