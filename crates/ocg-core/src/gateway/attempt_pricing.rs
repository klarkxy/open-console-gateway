//! Frozen per-attempt pricing: source selection, platform/official binding,
//! token-pricing coverage, and native-currency attribution.
//!
//! The forwarder obtains a [`RequestPricingSnapshot`] then submits usage and
//! the attempt result. Pending credit receipts, stream lifecycle, and current
//! bucket settlement stay on the forwarder.

use crate::gateway::materialize::native_log_identity;
use crate::gateway::protocol::RequestPlan;
use crate::kernel::pricing::PricingSnapshot;
use crate::kernel::protocol::ApiFormat;
use crate::models::{ForwardLogNativeAttribution, ForwardMetrics};
use crate::platform::{PlatformAccount, PlatformLink};
use crate::pricing::{
    ProviderPricingEvidence, ProviderScopedPricingSnapshot, latest_provider_pricing_snapshot,
};
use crate::provider::ProviderAdapterKind;
use crate::routing_snapshot::ExecutionCredential;
use crate::state::CoreState;
use anyhow::Result;
use chrono::Utc;
use serde_json::Value;
use std::sync::Arc;

#[derive(Clone)]
pub(crate) enum RequestPricingSnapshot {
    OpenCode(Arc<PricingSnapshot>),
    Provider {
        snapshot: Arc<ProviderScopedPricingSnapshot>,
        at: chrono::DateTime<Utc>,
    },
    /// Linked Custom Key: exact frozen platform price, or fail-closed unknown.
    /// Never inherits Go / GOAT / Ollama / USD provider rows.
    Platform(PlatformAttemptPrice),
    OfficialApi(crate::official_api::OfficialAttemptPrice),
    Credits {
        attempt: crate::billing::CreditAttempt,
        provider_id: String,
        revision: String,
        token_pricing_supported: bool,
    },
    Unpriced,
}

/// Per-attempt platform price captured from the link snapshot only.
#[derive(Clone)]
pub(crate) enum PlatformAttemptPrice {
    Frozen(FrozenPlatformPrice),
    Unknown { provenance: Option<String> },
}

#[derive(Clone)]
pub(crate) struct FrozenPlatformPrice {
    provenance: String,
    currency: String,
    input: f64,
    output: f64,
    cache_read: Option<f64>,
    cache_write: Option<f64>,
}

impl From<Arc<PricingSnapshot>> for RequestPricingSnapshot {
    fn from(snapshot: Arc<PricingSnapshot>) -> Self {
        Self::OpenCode(snapshot)
    }
}

impl RequestPricingSnapshot {
    pub(crate) fn for_account(
        state: &CoreState,
        account: &ExecutionCredential,
        adapter: ProviderAdapterKind,
        go: Arc<PricingSnapshot>,
    ) -> Self {
        let _ = (state, account, adapter, go);
        Self::Unpriced
    }

    #[allow(clippy::too_many_arguments)]
    fn estimate(
        &self,
        model: &str,
        prompt_tokens: i64,
        completion_tokens: i64,
        cached_tokens: i64,
        cache_creation_tokens: i64,
        service_tier: Option<&str>,
    ) -> crate::kernel::pricing::PricingEstimate {
        let _ = (
            self,
            model,
            prompt_tokens,
            completion_tokens,
            cached_tokens,
            cache_creation_tokens,
            service_tier,
        );
        crate::kernel::pricing::PricingEstimate::unknown()
    }

    fn revision(&self) -> Option<&str> {
        match self {
            Self::OpenCode(snapshot) => Some(&snapshot.revision),
            Self::Provider { snapshot, .. } => Some(snapshot.revision()),
            Self::Platform(price) => price.provenance(),
            Self::OfficialApi(price) => Some(&price.sheet.revision),
            Self::Credits { revision, .. } => Some(revision),
            Self::Unpriced => None,
        }
    }

    fn provider_identity(&self) -> Option<&str> {
        match self {
            Self::OpenCode(_) => Some(crate::provider::OPENCODE_PROVIDER_ID),
            Self::Provider { snapshot, .. } => Some(snapshot.provider_id()),
            Self::Platform(_) => Some(crate::provider::CUSTOM_PROVIDER_ID),
            Self::OfficialApi(price) => Some(&price.provider_id),
            Self::Credits { provider_id, .. } => Some(provider_id),
            Self::Unpriced => None,
        }
    }
}

impl PlatformAttemptPrice {
    fn provenance(&self) -> Option<&str> {
        match self {
            Self::Frozen(price) => Some(&price.provenance),
            Self::Unknown { provenance } => provenance.as_deref(),
        }
    }

    fn estimate(&self) -> crate::kernel::pricing::PricingEstimate {
        crate::kernel::pricing::PricingEstimate {
            raw_cost_usd: None,
            quota_debit: None,
            effective_paid_cost_usd: None,
            cost: None,
            pricing_revision_id: self.provenance().map(str::to_string),
            quota_multiplier: None,
            local_adjustment_multiplier: None,
            cost_state: "unknown",
        }
    }
}

fn estimate_platform_native(
    price: &FrozenPlatformPrice,
    prompt_tokens: i64,
    completion_tokens: i64,
    cached_tokens: i64,
    cache_creation_tokens: i64,
) -> Option<f64> {
    let _ = (
        price,
        prompt_tokens,
        completion_tokens,
        cached_tokens,
        cache_creation_tokens,
    );
    None
}

pub(crate) fn platform_price_for_attempt(
    state: &CoreState,
    account: &ExecutionCredential,
    upstream_model: &str,
    endpoint: Option<&str>,
) -> Option<PlatformAttemptPrice> {
    let _ = (state, account, upstream_model, endpoint);
    None
}

pub(crate) fn bind_platform_attempt_price(
    state: &CoreState,
    account: &ExecutionCredential,
    upstream_model: &str,
    pricing: RequestPricingSnapshot,
    endpoint: Option<&str>,
) -> RequestPricingSnapshot {
    let _ = (state, account, upstream_model, endpoint, pricing);
    RequestPricingSnapshot::Unpriced
}

fn restrict_platform_to_token_coverage(
    plan: &RequestPlan,
    pricing: RequestPricingSnapshot,
) -> RequestPricingSnapshot {
    let _ = plan;
    match pricing {
        RequestPricingSnapshot::Platform(_) => RequestPricingSnapshot::Unpriced,
        other => other,
    }
}

pub(crate) fn bind_official_execution_price(
    state: &CoreState,
    account: &ExecutionCredential,
    plan: &RequestPlan,
    original: RequestPricingSnapshot,
) -> RequestPricingSnapshot {
    let _ = (state, account, plan, original);
    RequestPricingSnapshot::Unpriced
}

fn bind_credit_attempt_price(
    state: &CoreState,
    account: &ExecutionCredential,
    plan: &RequestPlan,
    upstream_model: &str,
    pricing: RequestPricingSnapshot,
) -> RequestPricingSnapshot {
    let _ = (state, account, plan, upstream_model, pricing);
    RequestPricingSnapshot::Unpriced
}

fn request_has_hosted_tool_charges(body: &Value) -> bool {
    body.get("web_search_options").is_some()
        || body
            .get("tools")
            .and_then(Value::as_array)
            .is_some_and(|tools| {
                tools.iter().any(|tool| {
                    tool.get("type")
                        .and_then(Value::as_str)
                        .is_some_and(|kind| !matches!(kind, "function" | "custom"))
                })
            })
}

/// True when token rates can cover this request: absent or `default` service
/// tier, and a JSON body without non-text media. Unparseable bodies are not
/// covered. Hosted-tool charges are a separate gate.
pub(crate) fn token_pricing_covers_request(body: &[u8], service_tier: Option<&str>) -> bool {
    token_pricing_covers_service_tier(service_tier)
        && serde_json::from_slice::<Value>(body)
            .is_ok_and(|value| !request_has_unpriced_media(&value))
}

/// The tier half of [`token_pricing_covers_request`], split out so a caller
/// holding an already-parsed body does not parse it a second time.
fn token_pricing_covers_service_tier(service_tier: Option<&str>) -> bool {
    service_tier.is_none_or(|tier| tier == "default")
}

/// Media that token rates do not cover, read from protocol content positions.
/// Tool schemas, examples, and parameter names are not content.
fn request_has_unpriced_media(body: &Value) -> bool {
    if modalities_include_non_text(body.get("modalities")) {
        return true;
    }
    if body
        .get("messages")
        .and_then(Value::as_array)
        .is_some_and(|messages| {
            messages
                .iter()
                .any(|message| content_has_media(message.get("content")))
        })
    {
        return true;
    }
    if body
        .get("input")
        .is_some_and(|input| content_has_media(Some(input)))
    {
        return true;
    }
    body.get("contents")
        .and_then(Value::as_array)
        .is_some_and(|contents| {
            contents
                .iter()
                .any(|content| part_is_media(content) || content_has_media(content.get("parts")))
        })
}

fn modalities_include_non_text(value: Option<&Value>) -> bool {
    value
        .and_then(Value::as_array)
        .is_some_and(|values| values.iter().any(|value| value.as_str() != Some("text")))
}

fn content_has_media(content: Option<&Value>) -> bool {
    match content {
        Some(Value::Array(parts)) => parts.iter().any(item_has_media),
        Some(value) => item_has_media(value),
        None => false,
    }
}

fn item_has_media(part: &Value) -> bool {
    if part_is_media(part) {
        return true;
    }
    part.get("type").and_then(Value::as_str) == Some("message")
        && content_has_media(part.get("content"))
}

fn part_is_media(part: &Value) -> bool {
    if part
        .get("type")
        .and_then(Value::as_str)
        .is_some_and(|kind| {
            matches!(
                kind,
                "image"
                    | "image_url"
                    | "input_image"
                    | "input_audio"
                    | "audio"
                    | "video"
                    | "input_video"
                    | "file"
                    | "input_file"
            )
        })
    {
        return true;
    }
    part.get("inlineData")
        .or_else(|| part.get("inline_data"))
        .is_some_and(|data| {
            data.as_object().is_some_and(|object| {
                object.contains_key("data")
                    || object.contains_key("mimeType")
                    || object.contains_key("mime_type")
            })
        })
}

pub(crate) fn apply_native_cost_attribution(
    attribution: &mut ForwardLogNativeAttribution,
    platform_price: Option<&PlatformAttemptPrice>,
    official_price: Option<&crate::official_api::OfficialAttemptPrice>,
    metrics: &ForwardMetrics,
) {
    let _ = (attribution, platform_price, official_price, metrics);
}

pub(crate) fn capture_execution_pricing(
    state: &CoreState,
    account: &ExecutionCredential,
    adapter: ProviderAdapterKind,
    plan: &RequestPlan,
) -> RequestPricingSnapshot {
    let _ = (state, account, adapter, plan);
    RequestPricingSnapshot::Unpriced
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn pricing_metrics(
    snapshot: &RequestPricingSnapshot,
    model: &str,
    prompt_tokens: i64,
    completion_tokens: i64,
    cached_tokens: i64,
    cache_creation_tokens: i64,
    service_tier: Option<&str>,
) -> ForwardMetrics {
    let _ = (snapshot, model);
    ForwardMetrics {
        prompt_tokens,
        completion_tokens,
        cached_tokens,
        cache_creation_tokens,
        cost: 0.0,
        raw_cost_usd: None,
        quota_debit: None,
        effective_paid_cost_usd: None,
        pricing_revision_id: None,
        quota_multiplier: None,
        local_adjustment_multiplier: None,
        pricing_provider_id: None,
        service_tier: service_tier.map(str::to_string),
        cost_state: "unknown",
    }
}

pub(crate) fn metadata_metrics(
    snapshot: &RequestPricingSnapshot,
    service_tier: Option<&str>,
    cost_state: &'static str,
) -> ForwardMetrics {
    let _ = snapshot;
    let cost_state = match cost_state {
        "outcome_unknown" => "outcome_unknown",
        "usage_missing" => "usage_missing",
        _ => "unknown",
    };
    ForwardMetrics {
        pricing_revision_id: None,
        pricing_provider_id: None,
        service_tier: service_tier.map(str::to_string),
        cost_state,
        ..ForwardMetrics::default()
    }
}

#[cfg(test)]
mod tests;
