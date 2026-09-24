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
        match adapter {
            ProviderAdapterKind::OpenCodeGo => return Self::OpenCode(go),
            ProviderAdapterKind::CommandCodeGoat | ProviderAdapterKind::OllamaCloud => {}
            _ => return Self::Unpriced,
        }
        let loaded = latest_provider_pricing_snapshot(&state.db.lock(), &account.provider_id);
        match loaded {
            Ok(Some(snapshot)) if snapshot.evidence() == ProviderPricingEvidence::Verified => {
                Self::Provider {
                    snapshot: Arc::new(snapshot),
                    at: state.sample_gateway_clock().0,
                }
            }
            Ok(_) => Self::Unpriced,
            Err(error) => {
                eprintln!(
                    "warning: failed to load provider pricing for {}/{}: {error}",
                    account.provider_id, account.provider_id
                );
                Self::Unpriced
            }
        }
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
        match self {
            Self::OpenCode(snapshot) => snapshot.estimate(
                model,
                prompt_tokens,
                completion_tokens,
                cached_tokens,
                cache_creation_tokens,
                service_tier,
            ),
            Self::Provider { snapshot, at } => snapshot.estimate(
                model,
                prompt_tokens,
                completion_tokens,
                cached_tokens,
                cache_creation_tokens,
                *at,
            ),
            Self::Platform(price) => price.estimate(),
            Self::OfficialApi(price) => {
                let amount = (model == price.model)
                    .then(|| {
                        price.amount(
                            prompt_tokens,
                            completion_tokens,
                            cached_tokens,
                            cache_creation_tokens,
                        )
                    })
                    .flatten();
                let usd = amount.filter(|_| price.sheet.kind.currency() == "USD");
                crate::kernel::pricing::PricingEstimate {
                    raw_cost_usd: usd,
                    quota_debit: None,
                    effective_paid_cost_usd: None,
                    cost: usd,
                    pricing_revision_id: Some(price.sheet.revision.clone()),
                    quota_multiplier: None,
                    local_adjustment_multiplier: None,
                    cost_state: if usd.is_some() {
                        "priced"
                    } else if amount.is_some() {
                        "unknown"
                    } else {
                        "unpriced"
                    },
                }
            }
            Self::Credits { revision, .. } => crate::kernel::pricing::PricingEstimate {
                raw_cost_usd: None,
                quota_debit: None,
                effective_paid_cost_usd: None,
                cost: None,
                pricing_revision_id: Some(revision.clone()),
                quota_multiplier: None,
                local_adjustment_multiplier: None,
                cost_state: "unknown",
            },
            Self::Unpriced => crate::kernel::pricing::PricingEstimate {
                raw_cost_usd: None,
                quota_debit: None,
                effective_paid_cost_usd: None,
                cost: None,
                pricing_revision_id: None,
                quota_multiplier: None,
                local_adjustment_multiplier: None,
                cost_state: "unpriced",
            },
        }
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
    ocg_domain::billing::token_charge(
        ocg_domain::billing::BillingTokens::clamped(
            prompt_tokens,
            completion_tokens,
            cached_tokens,
            cache_creation_tokens,
        ),
        ocg_domain::billing::TokenRates {
            input: price.input,
            output: price.output,
            cache_read: price.cache_read,
            cache_write: price.cache_write,
            per_tokens: 1.0,
        },
    )
}

pub(crate) fn platform_price_for_attempt(
    state: &CoreState,
    account: &ExecutionCredential,
    upstream_model: &str,
    endpoint: Option<&str>,
) -> Option<PlatformAttemptPrice> {
    let db = state.db.lock();
    let links = db.list_platform_links().ok()?;
    let link = links
        .into_iter()
        .find(|link| link.account_id == account.id)?;
    if db
        .credential_key_cipher_for_legacy_account(&account.id)
        .ok()
        .flatten()
        .is_none_or(|current| current != account.key_cipher)
        || endpoint.is_some_and(|url| {
            crate::destination_projection::load_runtime(&db)
                .ok()
                .and_then(|projection| {
                    let id = &projection
                        .credentials
                        .iter()
                        .find(|c| c.legacy_account_id == account.id)?
                        .destination_id;
                    projection
                        .destinations
                        .iter()
                        .find(|d| &d.id == id)
                        .cloned()
                })
                .is_none_or(|destination| {
                    !ocg_domain::destination::http_configured_routes(&destination)
                        .iter()
                        .any(|route| route.url.as_deref() == Some(url))
                })
        })
    {
        return Some(PlatformAttemptPrice::Unknown {
            provenance: Some("platform:attempt_identity_changed".into()),
        });
    }
    let parent = match db.platform_account(&link.platform_account_id) {
        Ok(Some(parent)) => parent,
        _ => {
            return Some(PlatformAttemptPrice::Unknown {
                provenance: Some(format!("{}:missing", link.platform_account_id)),
            });
        }
    };
    Some(select_link_platform_price(
        &link,
        &parent,
        upstream_model,
        Utc::now().timestamp(),
    ))
}

fn select_link_platform_price(
    link: &PlatformLink,
    parent: &PlatformAccount,
    upstream_model: &str,
    now: i64,
) -> PlatformAttemptPrice {
    let unknown = |reason: &str| PlatformAttemptPrice::Unknown {
        provenance: Some(format!(
            "{}:{}:{}:{}:{reason}",
            parent.id,
            parent.version,
            link.group.id.as_deref().unwrap_or(""),
            upstream_model
        )),
    };
    let Some(group_id) = link
        .group
        .id
        .as_deref()
        .map(str::trim)
        .filter(|id| !id.is_empty())
    else {
        return unknown("auto");
    };
    if group_id == "auto" || !link.group.auto_groups.is_empty() {
        return unknown("auto");
    }
    let Some(snapshot) = link.snapshot.as_ref() else {
        return unknown("nosnap");
    };
    if snapshot.stale {
        return unknown("stale");
    }
    let matches = snapshot
        .prices
        .iter()
        .filter(|price| {
            price.model == upstream_model
                && price.group_id.as_deref() == Some(group_id)
                && !price.official_reference
        })
        .collect::<Vec<_>>();
    if matches.len() != 1 {
        return unknown("nomatch");
    }
    let price = matches[0];
    if price.unavailable_reason.is_some() {
        return unknown("unavailable");
    }
    if now >= price.valid_until {
        return unknown("expired");
    }
    let Some(input) = finite_nonneg_rate(price.input) else {
        return unknown("incomplete");
    };
    let Some(output) = finite_nonneg_rate(price.output) else {
        return unknown("incomplete");
    };
    let currency = price.currency.trim();
    if currency.is_empty() {
        return unknown("incomplete");
    }
    let cache_read = match optional_finite_nonneg_rate(price.cache_read) {
        Ok(rate) => rate,
        Err(()) => return unknown("incomplete"),
    };
    let cache_write = match optional_finite_nonneg_rate(price.cache_write) {
        Ok(rate) => rate,
        Err(()) => return unknown("incomplete"),
    };
    PlatformAttemptPrice::Frozen(FrozenPlatformPrice {
        provenance: format!(
            "{}:{}:{group_id}:{upstream_model}:{}:{}:{}",
            parent.id, parent.version, price.valid_until, price.source, snapshot.observed_at
        ),
        currency: currency.to_string(),
        input,
        output,
        cache_read,
        cache_write,
    })
}

fn finite_nonneg_rate(value: Option<f64>) -> Option<f64> {
    value.filter(|rate| rate.is_finite() && *rate >= 0.0)
}

fn optional_finite_nonneg_rate(value: Option<f64>) -> Result<Option<f64>, ()> {
    match value {
        None => Ok(None),
        Some(rate) if rate.is_finite() && rate >= 0.0 => Ok(Some(rate)),
        Some(_) => Err(()),
    }
}

pub(crate) fn bind_platform_attempt_price(
    state: &CoreState,
    account: &ExecutionCredential,
    upstream_model: &str,
    pricing: RequestPricingSnapshot,
    endpoint: Option<&str>,
) -> RequestPricingSnapshot {
    let Some(platform) = platform_price_for_attempt(state, account, upstream_model, endpoint)
    else {
        return pricing;
    };
    RequestPricingSnapshot::Platform(platform)
}

fn restrict_platform_to_token_coverage(
    plan: &RequestPlan,
    pricing: RequestPricingSnapshot,
) -> RequestPricingSnapshot {
    if !matches!(&pricing, RequestPricingSnapshot::Platform(_)) {
        return pricing;
    }
    let hosted_tools = serde_json::from_slice::<Value>(&plan.body)
        .ok()
        .is_some_and(|body| request_has_hosted_tool_charges(&body));
    if hosted_tools || !token_pricing_covers_request(&plan.body, plan.service_tier.as_deref()) {
        let reason = if hosted_tools {
            "platform:hosted_tool_unpriced"
        } else {
            "platform:unsupported_request_pricing"
        };
        RequestPricingSnapshot::Platform(PlatformAttemptPrice::Unknown {
            provenance: Some(reason.into()),
        })
    } else {
        pricing
    }
}

pub(crate) fn bind_official_execution_price(
    state: &CoreState,
    account: &ExecutionCredential,
    plan: &RequestPlan,
    original: RequestPricingSnapshot,
) -> RequestPricingSnapshot {
    if !matches!(original, RequestPricingSnapshot::Unpriced)
        || !token_pricing_covers_request(&plan.body, plan.service_tier.as_deref())
    {
        return original;
    }
    let Ok(body) = serde_json::from_slice::<Value>(&plan.body) else {
        return original;
    };
    // Hosted tools have charges outside token pricing; ordinary function tools do not.
    if request_has_hosted_tool_charges(&body) {
        return original;
    }
    let Some(kind) = account.official_pricing_kind else {
        return original;
    };
    let Some(endpoint) = plan
        .custom_route
        .as_ref()
        .map(|route| route.endpoint_url.as_str())
    else {
        return original;
    };
    let protocol = match plan.upstream {
        ApiFormat::ChatCompletions => crate::provider::UpstreamProtocolKind::ChatCompletions,
        ApiFormat::Responses => crate::provider::UpstreamProtocolKind::Responses,
        ApiFormat::Messages => crate::provider::UpstreamProtocolKind::Messages,
        ApiFormat::Gemini => return original,
    };
    if !crate::official_api::route_is_official(kind, endpoint, protocol) {
        return original;
    }
    let Ok(sheet) = state
        .db
        .lock()
        .official_api_prices(&account.provider_id, kind)
    else {
        return original;
    };
    let price = crate::official_api::OfficialAttemptPrice {
        provider_id: account.provider_id.clone(),
        sheet,
        model: plan.model.clone(),
        at: state.sample_gateway_clock().0,
    };
    RequestPricingSnapshot::OfficialApi(price)
}

fn bind_credit_attempt_price(
    state: &CoreState,
    account: &ExecutionCredential,
    plan: &RequestPlan,
    upstream_model: &str,
    pricing: RequestPricingSnapshot,
) -> RequestPricingSnapshot {
    let Some(endpoint) = plan
        .custom_route
        .as_ref()
        .map(|route| route.endpoint_url.as_str())
    else {
        return pricing;
    };
    let captured = crate::db::billing::capture_on(
        &state.db.lock().conn,
        &account.id,
        endpoint,
        upstream_model,
        state.sample_gateway_clock().0,
    );
    match captured {
        Ok(Some(credit)) => {
            use sha2::{Digest, Sha256};
            let revision = format!(
                "credit-estimate:{}",
                hex::encode(Sha256::digest(
                    serde_json::to_vec(&credit).unwrap_or_default()
                )),
            );
            let hosted_tools = serde_json::from_slice::<Value>(&plan.body)
                .ok()
                .is_none_or(|body| request_has_hosted_tool_charges(&body));
            RequestPricingSnapshot::Credits {
                attempt: credit,
                provider_id: account.provider_id.clone(),
                revision,
                token_pricing_supported: !hosted_tools
                    && token_pricing_covers_request(&plan.body, plan.service_tier.as_deref()),
            }
        }
        Ok(None) => pricing,
        Err(error) => {
            eprintln!("warning: credit estimate could not be captured: {error}");
            pricing
        }
    }
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
    service_tier.is_none_or(|tier| tier == "default")
        && serde_json::from_slice::<Value>(body)
            .is_ok_and(|value| !request_has_unpriced_media(&value))
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
    if let Some(PlatformAttemptPrice::Frozen(price)) = platform_price
        && metrics.cost_state == "unknown"
        && let Some(value) = estimate_platform_native(
            price,
            metrics.prompt_tokens,
            metrics.completion_tokens,
            metrics.cached_tokens,
            metrics.cache_creation_tokens,
        )
    {
        attribution.native_cost_value = Some(value);
        attribution.native_cost_unit = Some(price.currency.clone());
        attribution.native_cost_currency = Some(price.currency.clone());
    }
    if let Some(price) = official_price
        && matches!(metrics.cost_state, "priced" | "unknown")
        && metrics.pricing_provider_id.as_deref() == Some(price.provider_id.as_str())
        && metrics.pricing_revision_id.as_deref() == Some(price.sheet.revision.as_str())
        && let Some(amount) = price.amount(
            metrics.prompt_tokens,
            metrics.completion_tokens,
            metrics.cached_tokens,
            metrics.cache_creation_tokens,
        )
    {
        attribution.native_cost_value = Some(amount);
        attribution.native_cost_unit = Some(price.sheet.kind.currency().into());
        attribution.native_cost_currency = Some(price.sheet.kind.currency().into());
    }
}

pub(crate) fn capture_execution_pricing(
    state: &CoreState,
    account: &ExecutionCredential,
    adapter: ProviderAdapterKind,
    plan: &RequestPlan,
    pricing_snapshot: Arc<PricingSnapshot>,
) -> RequestPricingSnapshot {
    let upstream_model = native_log_identity(plan).upstream_model;
    let endpoint = plan
        .custom_route
        .as_ref()
        .map(|route| route.endpoint_url.as_str());
    let pricing = bind_platform_attempt_price(
        state,
        account,
        &upstream_model,
        RequestPricingSnapshot::for_account(state, account, adapter, pricing_snapshot),
        endpoint,
    );
    let pricing = restrict_platform_to_token_coverage(plan, pricing);
    let pricing = bind_official_execution_price(state, account, plan, pricing);
    if matches!(pricing, RequestPricingSnapshot::Platform(_)) {
        return pricing;
    }
    bind_credit_attempt_price(state, account, plan, &upstream_model, pricing)
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
    let estimate = snapshot.estimate(
        model,
        prompt_tokens,
        completion_tokens,
        cached_tokens,
        cache_creation_tokens,
        service_tier,
    );
    let provider_identity = snapshot.provider_identity();
    ForwardMetrics {
        prompt_tokens,
        completion_tokens,
        cached_tokens,
        cache_creation_tokens,
        cost: estimate.cost.unwrap_or(0.0),
        raw_cost_usd: estimate.raw_cost_usd,
        quota_debit: estimate.quota_debit,
        effective_paid_cost_usd: estimate.effective_paid_cost_usd,
        pricing_revision_id: estimate.pricing_revision_id,
        quota_multiplier: estimate.quota_multiplier,
        local_adjustment_multiplier: estimate.local_adjustment_multiplier,
        pricing_provider_id: provider_identity.map(str::to_string),

        service_tier: service_tier.map(str::to_string),
        cost_state: estimate.cost_state,
    }
}

pub(crate) fn metadata_metrics(
    snapshot: &RequestPricingSnapshot,
    service_tier: Option<&str>,
    cost_state: &'static str,
) -> ForwardMetrics {
    let provider_identity = snapshot.provider_identity();
    ForwardMetrics {
        pricing_revision_id: snapshot.revision().map(str::to_string),
        pricing_provider_id: provider_identity.map(str::to_string),

        service_tier: service_tier.map(str::to_string),
        cost_state,
        ..ForwardMetrics::default()
    }
}

#[cfg(test)]
mod tests;
