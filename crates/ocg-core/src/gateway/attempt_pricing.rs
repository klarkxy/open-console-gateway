//! Usage metrics for unpriced attempts. Stored prices never price a new request.

use crate::gateway::protocol::RequestPlan;
use crate::kernel::pricing::PricingSnapshot;
use crate::models::ForwardMetrics;
use crate::provider::ProviderAdapterKind;
use crate::routing_snapshot::ExecutionCredential;
use crate::state::CoreState;
use std::sync::Arc;

#[derive(Clone)]
pub(crate) enum RequestPricingSnapshot {
    Unpriced,
}

impl From<Arc<PricingSnapshot>> for RequestPricingSnapshot {
    fn from(_snapshot: Arc<PricingSnapshot>) -> Self {
        Self::Unpriced
    }
}

pub(crate) fn capture_execution_pricing(
    _state: &CoreState,
    _account: &ExecutionCredential,
    _adapter: ProviderAdapterKind,
    _plan: &RequestPlan,
) -> RequestPricingSnapshot {
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
