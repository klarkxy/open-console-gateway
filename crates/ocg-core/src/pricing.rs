use anyhow::{Context, Result, anyhow, bail};
use chrono::{DateTime, Timelike, Utc};
use futures_util::StreamExt;
use reqwest::redirect::{Attempt, Policy};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::fmt;
use std::time::Duration;

use crate::db::Database;
use crate::kernel::ids::{
    ANONYMOUS_FREE_OFFERING_ID, COMMAND_CODE_PROVIDER_ID, GO_OFFERING_ID, GOAT_OFFERING_ID,
    OPENCODE_PROVIDER_ID, OPENCODE_ZEN_FREE_PROVIDER_ID, is_free_model,
};
use crate::provider::ProviderPricingSnapshot as StoredProviderPricingSnapshot;

pub use crate::kernel::ids::normalize_model_name;
use crate::kernel::pricing::ProviderPricingValueWire;
pub use crate::kernel::pricing::{
    PricingAdjustment, PricingEstimate, PricingLimits, PricingModel, PricingSnapshot,
    PricingTimeWindow, ProviderCostEstimate, ProviderCostState, ProviderPricingCapability,
    ProviderPricingEvidence, ProviderPricingValue, SEED_LIMITS, SOURCE_URL,
    provider_pricing_capability, quota_multiplier, seed_snapshot,
};

const SOURCE_HOST: &str = "opencode.ai";
const MAX_DOCUMENT_BYTES: usize = 2 * 1024 * 1024;
const ADJUSTMENT_POLICY_VERSION: &str = "local-v4";

/// Typed, append-only value stored inside `provider_pricing_snapshots`.
/// Fields are private so a loaded snapshot cannot be mutated in place; a new
/// official observation receives a new revision.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct ProviderScopedPricingSnapshot {
    provider_id: String,
    offering_id: String,
    revision: String,
    activated_at: String,
    document_updated_at: Option<String>,
    source_url: String,
    content_hash: String,
    evidence: ProviderPricingEvidence,
    values: Vec<ProviderPricingValue>,
}

#[derive(Debug, Clone, Deserialize)]
struct ProviderScopedPricingSnapshotWire {
    provider_id: String,
    offering_id: String,
    revision: String,
    activated_at: String,
    document_updated_at: Option<String>,
    source_url: String,
    content_hash: String,
    evidence: ProviderPricingEvidence,
    values: Vec<ProviderPricingValueWire>,
}

impl ProviderScopedPricingSnapshot {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        provider_id: impl Into<String>,
        offering_id: impl Into<String>,
        revision: impl Into<String>,
        activated_at: impl Into<String>,
        document_updated_at: Option<String>,
        source_url: impl Into<String>,
        content_hash: impl Into<String>,
        evidence: ProviderPricingEvidence,
        values: Vec<ProviderPricingValue>,
    ) -> Result<Self> {
        let provider_id = provider_id.into();
        let offering_id = offering_id.into();
        let revision = revision.into();
        let activated_at = activated_at.into();
        let source_url = source_url.into();
        let content_hash = content_hash.into();
        if [
            provider_id.as_str(),
            offering_id.as_str(),
            revision.as_str(),
            activated_at.as_str(),
        ]
        .iter()
        .any(|value| value.trim().is_empty())
        {
            bail!("provider pricing identity and activation fields must be non-empty");
        }
        DateTime::parse_from_rfc3339(&activated_at)
            .context("provider pricing activated_at must be RFC3339")?;
        if evidence == ProviderPricingEvidence::Verified
            && (source_url.trim().is_empty() || content_hash.trim().is_empty())
        {
            bail!("verified provider pricing requires source URL and content hash");
        }
        let mut identities = HashSet::new();
        for value in &values {
            let identity = (
                value.model_id.clone(),
                value.time_window,
                value.min_input_tokens,
                value.max_input_tokens,
            );
            if !identities.insert(identity) {
                bail!("provider pricing contains a duplicate model/tier/time-window value");
            }
        }
        Ok(Self {
            provider_id,
            offering_id,
            revision,
            activated_at,
            document_updated_at,
            source_url,
            content_hash,
            evidence,
            values,
        })
    }

    pub fn from_opencode_go(snapshot: &PricingSnapshot) -> Result<Self> {
        let values = snapshot
            .models
            .iter()
            .map(|model| {
                ProviderPricingValue::new(
                    model.model_id.clone(),
                    model.display_name.clone(),
                    Some(model.input),
                    Some(model.output),
                    Some(model.cache_read),
                    model.cache_write,
                    Some(snapshot.limits.window_month),
                    Some(model.usage),
                    None,
                    Some("USD".to_string()),
                    model.min_input_tokens,
                    model.max_input_tokens,
                    model.time_window,
                )
            })
            .collect::<Result<Vec<_>>>()?;
        Self::new(
            OPENCODE_PROVIDER_ID,
            GO_OFFERING_ID,
            snapshot.revision.clone(),
            snapshot.activated_at.clone(),
            Some(snapshot.document_updated_at.clone()),
            snapshot.source_url.clone(),
            snapshot.content_hash.clone(),
            ProviderPricingEvidence::Verified,
            values,
        )
    }

    pub fn provider_id(&self) -> &str {
        &self.provider_id
    }

    pub fn offering_id(&self) -> &str {
        &self.offering_id
    }

    pub fn revision(&self) -> &str {
        &self.revision
    }

    pub fn evidence(&self) -> ProviderPricingEvidence {
        self.evidence
    }

    pub fn values(&self) -> &[ProviderPricingValue] {
        &self.values
    }

    pub fn to_storage_record(&self) -> Result<StoredProviderPricingSnapshot> {
        Ok(StoredProviderPricingSnapshot {
            provider_id: self.provider_id.clone(),
            offering_id: self.offering_id.clone(),
            revision: self.revision.clone(),
            activated_at: self.activated_at.clone(),
            document_updated_at: self.document_updated_at.clone(),
            source_url: self.source_url.clone(),
            content_hash: self.content_hash.clone(),
            snapshot_json: serde_json::to_string(self)?,
        })
    }

    pub fn from_storage_record(record: &StoredProviderPricingSnapshot) -> Result<Self> {
        if let Ok(wire) =
            serde_json::from_str::<ProviderScopedPricingSnapshotWire>(&record.snapshot_json)
        {
            let values = wire
                .values
                .into_iter()
                .map(ProviderPricingValue::from_wire)
                .collect::<Result<Vec<_>>>()?;
            let snapshot = Self::new(
                wire.provider_id,
                wire.offering_id,
                wire.revision,
                wire.activated_at,
                wire.document_updated_at,
                wire.source_url,
                wire.content_hash,
                wire.evidence,
                values,
            )?;
            snapshot.ensure_matches_record(record)?;
            return Ok(snapshot);
        }

        // v22 migrates old OpenCode Go snapshot JSON into the provider table.
        // Continue accepting that exact legacy value shape indefinitely.
        if record.provider_id == OPENCODE_PROVIDER_ID && record.offering_id == GO_OFFERING_ID {
            let legacy: PricingSnapshot = serde_json::from_str(&record.snapshot_json)
                .context("invalid provider pricing snapshot JSON")?;
            let snapshot = Self::from_opencode_go(&legacy)?;
            snapshot.ensure_matches_record(record)?;
            return Ok(snapshot);
        }
        bail!(
            "provider pricing snapshot `{}/{}/{}` has an unsupported value schema",
            record.provider_id,
            record.offering_id,
            record.revision
        )
    }

    fn ensure_matches_record(&self, record: &StoredProviderPricingSnapshot) -> Result<()> {
        if self.provider_id != record.provider_id
            || self.offering_id != record.offering_id
            || self.revision != record.revision
            || self.activated_at != record.activated_at
            || self.document_updated_at != record.document_updated_at
            || self.source_url != record.source_url
            || self.content_hash != record.content_hash
        {
            bail!("provider pricing metadata does not match its storage record");
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderPricingRefreshError {
    UnknownOffering,
    ExperimentalContractUnavailable,
    NotApplicable,
    FetchFailed,
}

impl fmt::Display for ProviderPricingRefreshError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownOffering => f.write_str("unknown provider pricing offering"),
            Self::ExperimentalContractUnavailable => f.write_str(
                "experimental GOAT pricing is unavailable because no verified official contract is configured",
            ),
            Self::NotApplicable => {
                f.write_str("this provider offering has no paid pricing snapshot")
            }
            Self::FetchFailed => f.write_str("verified provider pricing refresh failed"),
        }
    }
}

impl std::error::Error for ProviderPricingRefreshError {}

/// Explicit manual-only provider refresh entrypoint. There is intentionally no
/// timer/scheduler hook for pricing.
pub async fn fetch_provider_pricing_manual(
    config: &crate::models::AppConfig,
    provider_id: &str,
    offering_id: &str,
) -> std::result::Result<ProviderScopedPricingSnapshot, ProviderPricingRefreshError> {
    match (provider_id, offering_id) {
        (OPENCODE_PROVIDER_ID, GO_OFFERING_ID) => {
            let snapshot = fetch_official_snapshot(config)
                .await
                .map_err(|_| ProviderPricingRefreshError::FetchFailed)?;
            ProviderScopedPricingSnapshot::from_opencode_go(&snapshot)
                .map_err(|_| ProviderPricingRefreshError::FetchFailed)
        }
        (COMMAND_CODE_PROVIDER_ID, GOAT_OFFERING_ID) => {
            Err(ProviderPricingRefreshError::ExperimentalContractUnavailable)
        }
        (OPENCODE_ZEN_FREE_PROVIDER_ID, ANONYMOUS_FREE_OFFERING_ID) => {
            Err(ProviderPricingRefreshError::NotApplicable)
        }
        _ => Err(ProviderPricingRefreshError::UnknownOffering),
    }
}

pub fn store_provider_pricing_snapshot(
    db: &Database,
    snapshot: &ProviderScopedPricingSnapshot,
) -> Result<()> {
    db.insert_provider_pricing_snapshot(&snapshot.to_storage_record()?)
}

pub fn latest_provider_pricing_snapshot(
    db: &Database,
    provider_id: &str,
    offering_id: &str,
) -> Result<Option<ProviderScopedPricingSnapshot>> {
    db.latest_provider_pricing_snapshot(provider_id, offering_id)?
        .as_ref()
        .map(ProviderScopedPricingSnapshot::from_storage_record)
        .transpose()
}

// Audit reference only; the runtime never fetches supplier pricing pages:
// https://platform.minimaxi.com/docs/guides/pricing-paygo

impl PricingSnapshot {
    pub fn estimate(
        &self,
        model: &str,
        prompt: i64,
        completion: i64,
        cached: i64,
        cache_creation: i64,
        service_tier: Option<&str>,
    ) -> PricingEstimate {
        self.estimate_at(
            model,
            prompt,
            completion,
            cached,
            cache_creation,
            service_tier,
            Utc::now(),
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn estimate_at(
        &self,
        model: &str,
        prompt: i64,
        completion: i64,
        cached: i64,
        cache_creation: i64,
        service_tier: Option<&str>,
        at: DateTime<Utc>,
    ) -> PricingEstimate {
        let prompt = prompt.max(0) as f64;
        let completion = completion.max(0) as f64;
        let cached = (cached.max(0) as f64).min(prompt);
        let cache_creation = (cache_creation.max(0) as f64).min(prompt - cached);
        let uncached = prompt - cached - cache_creation;
        let normalized = normalize_model_name(model);
        if is_free_model(&normalized) {
            return PricingEstimate::free(&self.revision);
        }
        let highspeed = normalized.contains("minimax-m2.7-highspeed")
            || normalized.contains("minimax-m2.5-highspeed");
        let lookup_name = normalized.replace("-highspeed", "");

        let candidates = self
            .models
            .iter()
            .filter(|entry| lookup_name == entry.model_id)
            .filter(|entry| {
                entry
                    .min_input_tokens
                    .is_none_or(|minimum| prompt as i64 >= minimum)
                    && entry
                        .max_input_tokens
                        .is_none_or(|maximum| prompt as i64 <= maximum)
            })
            .collect::<Vec<_>>();
        let selected = select_priced_model(&candidates, at);
        let Some(price) = selected else {
            return PricingEstimate::unpriced(&self.revision);
        };

        // A '-' in the official Cached Write column means there is no separate
        // cache-write price. Cache creation is still new input, so it uses input.
        let cache_write = price.cache_write.unwrap_or(price.input);
        let base = (uncached * price.input
            + completion * price.output
            + cached * price.cache_read
            + cache_creation * cache_write)
            / 1_000_000.0;

        let mut adjusted_input = price.input;
        let mut adjusted_output = price.output;
        let mut adjusted_cache_read = price.cache_read;
        let mut adjusted_cache_write = cache_write;
        if highspeed {
            adjusted_input *= 2.0;
            adjusted_output *= 2.0;
        }
        if price.model_id == "minimax-m3" {
            let mut multiplier = 1.0;
            if prompt > 512_000.0 {
                multiplier *= 2.0;
            }
            if service_tier.is_some_and(|tier| tier.eq_ignore_ascii_case("priority")) {
                multiplier *= 1.5;
            }
            adjusted_input *= multiplier;
            adjusted_output *= multiplier;
            adjusted_cache_read *= multiplier;
            adjusted_cache_write *= multiplier;
        }
        let adjusted = (uncached * adjusted_input
            + completion * adjusted_output
            + cached * adjusted_cache_read
            + cache_creation * adjusted_cache_write)
            / 1_000_000.0;
        let local_adjustment_multiplier = if base > 0.0 { adjusted / base } else { 1.0 };

        let quota_debit = adjusted * price.quota_multiplier;
        PricingEstimate {
            raw_cost_usd: Some(adjusted),
            quota_debit: Some(quota_debit),
            effective_paid_cost_usd: None,
            cost: Some(quota_debit),
            pricing_revision_id: Some(self.revision.clone()),
            quota_multiplier: Some(price.quota_multiplier),
            local_adjustment_multiplier: Some(local_adjustment_multiplier),
            cost_state: "priced",
        }
    }
}

pub fn embedded_seed() -> PricingSnapshot {
    seed_snapshot(Utc::now().to_rfc3339())
}

pub(crate) fn ensure_current_adjustment_policy(mut snapshot: PricingSnapshot) -> PricingSnapshot {
    if snapshot.adjustment_policy_version == ADJUSTMENT_POLICY_VERSION {
        return snapshot;
    }

    // local-v2 and older divided the Go multiplier by a separate supplier-price
    // multiplier for two Pro models. Manual multiplier editing did not exist in
    // those revisions, so repairing them from the official Usage column is safe.
    // local-v3 already stores the correct applied multiplier, while local-v4+
    // may contain user edits and must never be silently rebased by a policy bump.
    if legacy_policy_needs_multiplier_repair(&snapshot.adjustment_policy_version) {
        apply_official_multipliers(&mut snapshot.models, snapshot.limits.window_month);
    }
    add_adjustments(&mut snapshot.models);
    snapshot.adjustment_policy_version = ADJUSTMENT_POLICY_VERSION.to_string();
    snapshot.revision = unique_revision_for_content_hash(&snapshot.content_hash);
    snapshot.activated_at = Utc::now().to_rfc3339();
    snapshot
}

// These are deliberately the only seed rows that can be backfilled into an
// official snapshot. The public Go pricing table lists Contributor but omits
// standard Muse; standard rates come from live Go measurements, not that table.
// Do not turn this into "every embedded seed row": an official removal must
// not silently revive unrelated models forever.
const SEED_COVERAGE_MODEL_IDS: &[&str] = &["muse-spark-1.2", "muse-spark-1.2-contributor"];

/// Append the explicitly allowlisted Muse rows that an existing snapshot does
/// not know about yet. Entries already present — official rows or user-edited
/// multipliers — are never overwritten.
pub(crate) fn ensure_seed_model_coverage(mut snapshot: PricingSnapshot) -> PricingSnapshot {
    let known = snapshot
        .models
        .iter()
        .map(|model| model.model_id.as_str())
        .collect::<HashSet<_>>();
    let mut missing: Vec<PricingModel> = seed_snapshot(snapshot.activated_at.clone())
        .models
        .into_iter()
        .filter(|model| {
            SEED_COVERAGE_MODEL_IDS.contains(&model.model_id.as_str())
                && !known.contains(model.model_id.as_str())
        })
        .collect();
    if missing.is_empty() {
        return snapshot;
    }
    // Recompute with the snapshot's own monthly limit so the appended rows
    // agree with the rest of the snapshot even if the official limit moved.
    apply_official_multipliers(&mut missing, snapshot.limits.window_month);
    snapshot.models.extend(missing);
    sort_models(&mut snapshot.models);
    snapshot.revision = unique_revision_for_content_hash(&snapshot.content_hash);
    snapshot.activated_at = Utc::now().to_rfc3339();
    snapshot
}

pub(crate) fn stamp_pricing_activation(mut snapshot: PricingSnapshot) -> PricingSnapshot {
    snapshot.revision = unique_revision_for_content_hash(&snapshot.content_hash);
    snapshot.activated_at = Utc::now().to_rfc3339();
    snapshot
}

pub(crate) const MAX_PRICING_MULTIPLIER: f64 = 1000.0;

/// Dashboard confirmation policy for an official pricing refresh. Shared by
/// V2 and V3 so multiplier merge / confirmation matching stays identical.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PricingRefreshConfirmPolicy {
    KeepCurrent,
    UseOfficial,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct PricingMultiplierDelta {
    pub model_id: String,
    pub current_multiplier: f64,
    pub official_multiplier: f64,
}

/// I/O-free official-refresh decision. Callers stamp, persist, and bump.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum OfficialPricingRefresh {
    Failed {
        error: String,
    },
    NeedsConfirmation {
        multiplier_changes: Vec<PricingMultiplierDelta>,
        official_content_hash: String,
    },
    Unchanged {
        multiplier_changes: Vec<PricingMultiplierDelta>,
    },
    Activate {
        candidate: PricingSnapshot,
        multiplier_changes: Vec<PricingMultiplierDelta>,
    },
}

pub(crate) fn evaluate_official_pricing_refresh(
    active: &PricingSnapshot,
    result: Result<PricingSnapshot>,
    policy: Option<PricingRefreshConfirmPolicy>,
    expected_official_content_hash: Option<&str>,
) -> OfficialPricingRefresh {
    match result {
        Ok(official) => {
            // Compare the candidate after allowlisted coverage is applied. A
            // seed-only Muse row can carry a user multiplier; comparing the
            // incomplete official table first would erase it without a prompt.
            let official = ensure_seed_model_coverage(official);
            let multiplier_changes = pricing_multiplier_deltas(active, &official);
            let official_content_hash = official.content_hash.clone();
            let confirmation_matches = expected_official_content_hash
                .is_some_and(|expected| expected == official_content_hash);
            if !multiplier_changes.is_empty() && (policy.is_none() || !confirmation_matches) {
                return OfficialPricingRefresh::NeedsConfirmation {
                    multiplier_changes,
                    official_content_hash,
                };
            }

            // Official rows win; the allowlisted seed coverage above prevents
            // the public table's omitted standard Muse row from becoming unpriced.
            let mut candidate = official;
            if matches!(policy, Some(PricingRefreshConfirmPolicy::KeepCurrent)) {
                merge_current_multipliers(active, &mut candidate);
            }
            if pricing_semantically_equal(active, &candidate) {
                return OfficialPricingRefresh::Unchanged { multiplier_changes };
            }
            OfficialPricingRefresh::Activate {
                candidate,
                multiplier_changes,
            }
        }
        Err(error) => OfficialPricingRefresh::Failed {
            error: error.to_string(),
        },
    }
}

pub(crate) fn pricing_multiplier_deltas(
    current: &PricingSnapshot,
    official: &PricingSnapshot,
) -> Vec<PricingMultiplierDelta> {
    let current = pricing_multiplier_map(current);
    let official = pricing_multiplier_map(official);
    current
        .iter()
        .filter_map(|(model_id, current_multiplier)| {
            let official_multiplier = official.get(model_id)?;
            (current_multiplier != official_multiplier).then(|| PricingMultiplierDelta {
                model_id: model_id.clone(),
                current_multiplier: *current_multiplier,
                official_multiplier: *official_multiplier,
            })
        })
        .collect()
}

pub(crate) fn pricing_multiplier_map(snapshot: &PricingSnapshot) -> BTreeMap<String, f64> {
    snapshot
        .models
        .iter()
        .map(|model| (model.model_id.clone(), model.quota_multiplier))
        .collect()
}

pub(crate) fn merge_current_multipliers(
    current: &PricingSnapshot,
    candidate: &mut PricingSnapshot,
) {
    let current = pricing_multiplier_map(current);
    for model in &mut candidate.models {
        if let Some(multiplier) = current.get(&model.model_id) {
            model.quota_multiplier = *multiplier;
        }
    }
}

pub(crate) fn pricing_semantically_equal(left: &PricingSnapshot, right: &PricingSnapshot) -> bool {
    left.content_hash == right.content_hash
        && left.document_updated_at == right.document_updated_at
        && left.limits == right.limits
        && left.models == right.models
        && left.adjustment_policy_version == right.adjustment_policy_version
}

/// Validate a multiplier batch. `Ok(None)` is a no-op; `Ok(Some)` is the
/// unstamped candidate the caller must stamp and activate.
pub(crate) fn prepare_multiplier_update(
    active: &PricingSnapshot,
    writes: &[(String, f64)],
) -> std::result::Result<Option<PricingSnapshot>, String> {
    if writes.is_empty() {
        return Err("at least one multiplier is required".to_string());
    }
    let known_models = active
        .models
        .iter()
        .map(|model| model.model_id.as_str())
        .collect::<HashSet<_>>();
    let mut requested = BTreeMap::new();
    for (model_id, multiplier) in writes {
        let model_id = model_id.trim();
        if model_id.is_empty() || !known_models.contains(model_id) {
            return Err(format!("unknown pricing model `{model_id}`"));
        }
        if !multiplier.is_finite() || *multiplier <= 0.0 || *multiplier > MAX_PRICING_MULTIPLIER {
            return Err(format!(
                "multiplier for `{model_id}` must be greater than 0 and at most {MAX_PRICING_MULTIPLIER}"
            ));
        }
        if requested
            .insert(model_id.to_string(), *multiplier)
            .is_some()
        {
            return Err(format!("duplicate multiplier for `{model_id}`"));
        }
    }

    let mut snapshot = active.clone();
    let mut changed = false;
    for model in &mut snapshot.models {
        if let Some(multiplier) = requested.get(&model.model_id)
            && model.quota_multiplier != *multiplier
        {
            model.quota_multiplier = *multiplier;
            changed = true;
        }
    }
    if changed {
        Ok(Some(snapshot))
    } else {
        Ok(None)
    }
}

fn legacy_policy_needs_multiplier_repair(version: &str) -> bool {
    version
        .strip_prefix("local-v")
        .and_then(|value| value.parse::<u32>().ok())
        .is_none_or(|version| version < 3)
}

pub async fn fetch_official_snapshot(config: &crate::models::AppConfig) -> Result<PricingSnapshot> {
    let client = crate::http_client::configured_builder(config)?
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(20))
        .redirect(Policy::custom(same_source_redirect))
        .build()
        .context("build OpenCode Go pricing client")?;
    let response = client
        .get(SOURCE_URL)
        .send()
        .await
        .context("fetch OpenCode Go pricing page")?
        .error_for_status()
        .context("OpenCode Go pricing page returned an error")?;
    if response
        .content_length()
        .is_some_and(|length| length > MAX_DOCUMENT_BYTES as u64)
    {
        bail!("OpenCode Go pricing page exceeds 2 MiB");
    }
    let mut bytes = Vec::new();
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.context("read OpenCode Go pricing page")?;
        if bytes.len() + chunk.len() > MAX_DOCUMENT_BYTES {
            bail!("OpenCode Go pricing page exceeds 2 MiB");
        }
        bytes.extend_from_slice(&chunk);
    }
    let html = String::from_utf8(bytes).context("OpenCode Go pricing page is not UTF-8")?;
    parse_official_html(&html)
}

fn same_source_redirect(attempt: Attempt<'_>) -> reqwest::redirect::Action {
    if attempt.previous().len() >= 5 {
        return attempt.error("too many OpenCode Go pricing redirects");
    }
    let url = attempt.url();
    if url.scheme() == "https"
        && url.host_str() == Some(SOURCE_HOST)
        && url.port_or_known_default() == Some(443)
    {
        attempt.follow()
    } else {
        attempt.error("OpenCode Go pricing redirect left the approved HTTPS host")
    }
}

pub fn parse_official_html(html: &str) -> Result<PricingSnapshot> {
    let plain = collapse_whitespace(&strip_tags(html));
    let limits = PricingLimits {
        window_5h: parse_limit(&plain, "5 hour limit")?,
        window_week: parse_limit(&plain, "Weekly limit")?,
        window_month: parse_limit(&plain, "Monthly limit")?,
    };
    if limits.window_5h <= 0.0 || limits.window_week <= 0.0 || limits.window_month <= 0.0 {
        bail!("OpenCode Go usage limits must be positive");
    }
    let tables = extract_tables(html)?;
    let pricing_table = tables
        .iter()
        .find(|table| {
            has_headers(
                table,
                &[
                    "model",
                    "input",
                    "output",
                    "cached read",
                    "cached write",
                    "usage",
                ],
            )
        })
        .ok_or_else(|| anyhow!("OpenCode Go pricing table was not found"))?;
    let endpoint_table = tables
        .iter()
        .find(|table| has_headers(table, &["model", "model id", "endpoint", "ai sdk package"]))
        .ok_or_else(|| anyhow!("OpenCode Go model ID table was not found"))?;

    let mut ids_by_name = HashMap::new();
    let mut seen_model_ids = HashSet::new();
    for row in endpoint_table.iter().skip(1) {
        if row.iter().all(|cell| cell.trim().is_empty()) {
            continue;
        }
        if row.len() != 4 {
            bail!("OpenCode Go model ID table contains an incomplete row");
        }
        let key = canonical_display_name(&row[0]);
        let raw_id = row[1].trim();
        let id = normalize_model_name(raw_id);
        if key.is_empty() || id.is_empty() {
            bail!("OpenCode Go model ID table contains an empty model");
        }
        if raw_id != id {
            bail!("OpenCode Go model ID `{raw_id}` is not canonical");
        }
        if !seen_model_ids.insert(id.clone()) {
            bail!("OpenCode Go model ID table contains duplicate model ID {id}");
        }
        if ids_by_name.insert(key, id).is_some() {
            bail!("OpenCode Go model ID table contains duplicate model names");
        }
    }

    let mut models = Vec::new();
    let mut seen_tiers = HashSet::new();
    let mut unpriced_ids = HashSet::new();
    for row in pricing_table.iter().skip(1) {
        if row.len() != 6 {
            bail!("OpenCode Go pricing table contains an incomplete row");
        }
        let display_name = row[0].trim().to_string();
        let id = ids_by_name
            .get(&canonical_display_name(&display_name))
            .cloned()
            .ok_or_else(|| anyhow!("no official model ID found for {display_name}"))?;
        // Official Go docs list limited-time promos such as Ox Alpha Free with
        // dash prices. They stay on `/zen/go` but have no USD rates to ingest.
        if is_unpriced_promo_row(row) {
            unpriced_ids.insert(id);
            continue;
        }
        let (minimum, maximum) = parse_token_tier(&display_name)?;
        let time_window = parse_time_window(&display_name);
        if !seen_tiers.insert((id.clone(), minimum, maximum, time_window)) {
            bail!("OpenCode Go pricing table contains duplicate row for {display_name}");
        }
        let input = parse_dollar(&row[1], false)?
            .ok_or_else(|| anyhow!("{display_name} is missing input price"))?;
        let output = parse_dollar(&row[2], false)?
            .ok_or_else(|| anyhow!("{display_name} is missing output price"))?;
        let cache_read = parse_dollar(&row[3], false)?
            .ok_or_else(|| anyhow!("{display_name} is missing cache-read price"))?;
        let cache_write = parse_dollar(&row[4], true)?;
        let usage = parse_dollar(&row[5], false)?
            .ok_or_else(|| anyhow!("{display_name} is missing Usage"))?;
        if usage <= 0.0 {
            bail!("{display_name} Usage must be positive");
        }
        models.push(PricingModel {
            model_id: id,
            display_name,
            input,
            output,
            cache_read,
            cache_write,
            usage,
            quota_multiplier: limits.window_month / usage,
            min_input_tokens: minimum,
            max_input_tokens: maximum,
            time_window,
            adjustments: Vec::new(),
        });
    }

    if models.is_empty() {
        bail!("OpenCode Go pricing and model ID tables must not be empty");
    }

    let covered = models
        .iter()
        .map(|model| model.model_id.as_str())
        .collect::<HashSet<_>>();
    let missing_prices = seen_model_ids
        .iter()
        .filter(|id| !covered.contains(id.as_str()) && !unpriced_ids.contains(id.as_str()))
        .cloned()
        .collect::<Vec<_>>();
    if !missing_prices.is_empty() {
        bail!(
            "OpenCode Go model ID table contains models without pricing rows: {}",
            missing_prices.join(", ")
        );
    }
    for id in ["qwen3.7-plus", "qwen3.6-plus"] {
        if covered.contains(id) {
            validate_token_tiers(&models, id, 256_000)?;
        }
    }
    if covered.contains("gpt-5.6-luna") {
        validate_token_tiers(&models, "gpt-5.6-luna", 272_000)?;
    }
    validate_time_windows(&models)?;

    let document_updated_at = parse_document_updated_at(html)?;
    let content_hash = format!("{:x}", Sha256::digest(html.as_bytes()));
    // A snapshot revision covers both the official document and the local
    // pricing policy. This prevents a policy update from colliding with an
    // older snapshot when the Go HTML itself is unchanged.
    let revision = revision_for_content_hash(&content_hash);
    apply_official_pricing_policy(&mut models, limits.window_month);
    sort_models(&mut models);

    Ok(PricingSnapshot {
        revision,
        activated_at: Utc::now().to_rfc3339(),
        document_updated_at,
        source_url: SOURCE_URL.to_string(),
        content_hash,
        limits,
        models,
        adjustment_policy_version: ADJUSTMENT_POLICY_VERSION.to_string(),
    })
}

fn validate_token_tiers(models: &[PricingModel], id: &str, boundary: i64) -> Result<()> {
    let tiers = models
        .iter()
        .filter(|model| model.model_id == id)
        .collect::<Vec<_>>();
    let label = format!("{}K", boundary / 1000);
    if tiers.len() != 2
        || !tiers
            .iter()
            .any(|tier| tier.max_input_tokens == Some(boundary))
        || !tiers
            .iter()
            .any(|tier| tier.min_input_tokens == Some(boundary + 1))
    {
        bail!("OpenCode Go {id} must contain complete {label} pricing tiers");
    }
    Ok(())
}

fn add_adjustments(models: &mut [PricingModel]) {
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

fn apply_official_pricing_policy(models: &mut [PricingModel], monthly_limit: f64) {
    apply_official_multipliers(models, monthly_limit);
    add_adjustments(models);
}

fn apply_official_multipliers(models: &mut [PricingModel], monthly_limit: f64) {
    for model in models.iter_mut() {
        model.quota_multiplier = monthly_limit / model.usage;
    }
}

fn revision_for_content_hash(content_hash: &str) -> String {
    let prefix = content_hash.chars().take(16).collect::<String>();
    format!("go-{prefix}-{ADJUSTMENT_POLICY_VERSION}")
}

fn unique_revision_for_content_hash(content_hash: &str) -> String {
    format!(
        "{}-{}",
        revision_for_content_hash(content_hash),
        uuid::Uuid::new_v4().simple()
    )
}

fn sort_models(models: &mut [PricingModel]) {
    models.sort_by(|left, right| {
        left.model_id
            .cmp(&right.model_id)
            .then(left.min_input_tokens.cmp(&right.min_input_tokens))
            .then(left.time_window.cmp(&right.time_window))
    });
}

fn select_priced_model<'a>(
    candidates: &[&'a PricingModel],
    at: DateTime<Utc>,
) -> Option<&'a PricingModel> {
    if candidates.is_empty() {
        return None;
    }
    let scheduled = candidates
        .iter()
        .any(|entry| entry.time_window != PricingTimeWindow::Always);
    if scheduled {
        let prefer = if is_official_peak_utc(at) {
            PricingTimeWindow::Peak
        } else {
            PricingTimeWindow::OffPeak
        };
        return candidates
            .iter()
            .copied()
            .find(|entry| entry.time_window == prefer)
            .or_else(|| {
                candidates
                    .iter()
                    .copied()
                    .find(|entry| entry.time_window == PricingTimeWindow::Peak)
            })
            .or_else(|| candidates.first().copied());
    }
    candidates
        .iter()
        .copied()
        .max_by_key(|entry| entry.model_id.len())
}

fn is_official_peak_utc(at: DateTime<Utc>) -> bool {
    // Official Go docs: DeepSeek Peak hours are 01:00-04:00 and 06:00-10:00 UTC.
    let minutes = at.hour() * 60 + at.minute();
    (60..240).contains(&minutes) || (360..600).contains(&minutes)
}

fn parse_time_window(name: &str) -> PricingTimeWindow {
    let lower = name.to_ascii_lowercase();
    if lower.contains("off-peak") || lower.contains("off peak") || lower.contains("offpeak") {
        return PricingTimeWindow::OffPeak;
    }
    if lower.contains("peak") {
        return PricingTimeWindow::Peak;
    }
    PricingTimeWindow::Always
}

fn validate_time_windows(models: &[PricingModel]) -> Result<()> {
    let mut windows_by_id: HashMap<String, HashSet<PricingTimeWindow>> = HashMap::new();
    for model in models {
        windows_by_id
            .entry(model.model_id.clone())
            .or_default()
            .insert(model.time_window);
    }
    for (id, windows) in windows_by_id {
        let scheduled = windows.contains(&PricingTimeWindow::Peak)
            || windows.contains(&PricingTimeWindow::OffPeak);
        if !scheduled {
            continue;
        }
        if windows.contains(&PricingTimeWindow::Always) {
            bail!("OpenCode Go {id} mixes scheduled and unscheduled pricing rows");
        }
        if !windows.contains(&PricingTimeWindow::Peak)
            || !windows.contains(&PricingTimeWindow::OffPeak)
        {
            bail!("OpenCode Go {id} must contain both Peak and Off-Peak pricing rows");
        }
    }
    Ok(())
}

fn canonical_display_name(name: &str) -> String {
    let base = name.split('(').next().unwrap_or(name);
    base.chars()
        .filter(|character| character.is_ascii_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

fn parse_token_tier(name: &str) -> Result<(Option<i64>, Option<i64>)> {
    // Official Go docs use ≤ / > token tiers (256K for Qwen, 272K for Luna).
    for boundary in [272_000_i64, 256_000, 200_000] {
        let label = format!("{}K", boundary / 1000);
        let label_lower = label.to_ascii_lowercase();
        if !(name.contains(&label) || name.contains(&label_lower)) {
            continue;
        }
        if name.contains('≤') || name.contains("<=") {
            return Ok((None, Some(boundary)));
        }
        if name.contains('>') {
            return Ok((Some(boundary + 1), None));
        }
        bail!("unrecognized token tier in {name}");
    }
    Ok((None, None))
}

fn is_placeholder_price(value: &str) -> bool {
    matches!(value.trim(), "-" | "—" | "–")
}

fn is_unpriced_promo_row(row: &[String]) -> bool {
    row.len() == 6 && row.iter().skip(1).all(|cell| is_placeholder_price(cell))
}

fn parse_dollar(value: &str, allow_dash: bool) -> Result<Option<f64>> {
    let value = value.trim();
    if allow_dash && matches!(value, "-" | "—" | "–") {
        return Ok(None);
    }
    let number = value
        .strip_prefix('$')
        .ok_or_else(|| anyhow!("expected USD value, got {value}"))?
        .replace(',', "");
    let parsed = number
        .parse::<f64>()
        .with_context(|| format!("invalid USD value {value}"))?;
    if !parsed.is_finite() || parsed < 0.0 {
        bail!("USD value must be finite and non-negative");
    }
    Ok(Some(parsed))
}

fn parse_limit(plain: &str, marker: &str) -> Result<f64> {
    let start = plain
        .find(marker)
        .ok_or_else(|| anyhow!("OpenCode Go page is missing {marker}"))?;
    let tail = &plain[start + marker.len()..];
    let dollar = tail
        .find('$')
        .ok_or_else(|| anyhow!("OpenCode Go page is missing USD value after {marker}"))?;
    let value = tail[dollar..]
        .split_whitespace()
        .next()
        .ok_or_else(|| anyhow!("OpenCode Go page is missing USD value after {marker}"))?;
    parse_dollar(
        value.trim_end_matches(|c: char| !c.is_ascii_digit() && c != '.'),
        false,
    )?
    .ok_or_else(|| anyhow!("OpenCode Go page is missing USD value after {marker}"))
}

fn parse_document_updated_at(html: &str) -> Result<String> {
    let marker = "title=\"Last updated:\"";
    let start = html
        .find(marker)
        .ok_or_else(|| anyhow!("OpenCode Go page is missing Last updated metadata"))?;
    let tail = &html[start..];
    let datetime = "datetime=\"";
    let value_start = tail
        .find(datetime)
        .ok_or_else(|| anyhow!("OpenCode Go page is missing Last updated datetime"))?
        + datetime.len();
    let value_end = tail[value_start..]
        .find('"')
        .ok_or_else(|| anyhow!("OpenCode Go Last updated datetime is malformed"))?
        + value_start;
    let value = &tail[value_start..value_end];
    chrono::DateTime::parse_from_rfc3339(value)
        .with_context(|| format!("invalid OpenCode Go Last updated datetime {value}"))?;
    Ok(value.to_string())
}

fn has_headers(table: &[Vec<String>], expected: &[&str]) -> bool {
    table.first().is_some_and(|row| {
        let actual = row
            .iter()
            .map(|cell| cell.trim().to_ascii_lowercase())
            .collect::<Vec<_>>();
        actual == expected
    })
}

fn extract_tables(html: &str) -> Result<Vec<Vec<Vec<String>>>> {
    let mut tables = Vec::new();
    let mut remainder = html;
    while let Some(start) = remainder.find("<table") {
        let table = &remainder[start..];
        let end = table
            .find("</table>")
            .ok_or_else(|| anyhow!("OpenCode Go page contains an unterminated table"))?;
        tables.push(extract_rows(&table[..end + "</table>".len()])?);
        remainder = &table[end + "</table>".len()..];
    }
    Ok(tables)
}

fn extract_rows(table: &str) -> Result<Vec<Vec<String>>> {
    let mut rows = Vec::new();
    let mut remainder = table;
    while let Some(start) = remainder.find("<tr") {
        let row = &remainder[start..];
        let end = row
            .find("</tr>")
            .ok_or_else(|| anyhow!("OpenCode Go page contains an unterminated row"))?;
        rows.push(extract_cells(&row[..end + "</tr>".len()])?);
        remainder = &row[end + "</tr>".len()..];
    }
    Ok(rows)
}

fn extract_cells(row: &str) -> Result<Vec<String>> {
    let mut cells = Vec::new();
    let mut cursor = 0;
    while cursor < row.len() {
        let th = row[cursor..].find("<th").map(|index| (index, "</th>"));
        let td = row[cursor..].find("<td").map(|index| (index, "</td>"));
        let Some((relative, end_tag)) = [th, td].into_iter().flatten().min_by_key(|item| item.0)
        else {
            break;
        };
        let start = cursor + relative;
        let content_start = row[start..]
            .find('>')
            .ok_or_else(|| anyhow!("OpenCode Go page contains a malformed table cell"))?
            + start
            + 1;
        let content_end = row[content_start..]
            .find(end_tag)
            .ok_or_else(|| anyhow!("OpenCode Go page contains an unterminated table cell"))?
            + content_start;
        cells.push(collapse_whitespace(&strip_tags(
            &row[content_start..content_end],
        )));
        cursor = content_end + end_tag.len();
    }
    Ok(cells)
}

fn strip_tags(input: &str) -> String {
    let mut output = String::with_capacity(input.len());
    let mut in_tag = false;
    let mut characters = input.chars().peekable();
    while let Some(character) = characters.next() {
        match character {
            '<' if characters.peek().is_some_and(|next| {
                next.is_ascii_alphabetic() || matches!(next, '/' | '!' | '?')
            }) =>
            {
                in_tag = true
            }
            '>' if in_tag => {
                in_tag = false;
                output.push(' ');
            }
            _ if !in_tag => output.push(character),
            _ => {}
        }
    }
    decode_entities(&output)
}

fn decode_entities(input: &str) -> String {
    let mut output = String::with_capacity(input.len());
    let mut remainder = input;
    while let Some(start) = remainder.find('&') {
        output.push_str(&remainder[..start]);
        let entity = &remainder[start..];
        let Some(end) = entity.find(';') else {
            output.push_str(entity);
            return output;
        };
        let code = &entity[1..end];
        let decoded = match code {
            "amp" => Some('&'),
            "lt" => Some('<'),
            "gt" => Some('>'),
            "quot" => Some('"'),
            "apos" | "#39" => Some('\''),
            "nbsp" => Some(' '),
            _ if code.starts_with("#x") => u32::from_str_radix(&code[2..], 16)
                .ok()
                .and_then(char::from_u32),
            _ if code.starts_with('#') => code[1..].parse::<u32>().ok().and_then(char::from_u32),
            _ => None,
        };
        if let Some(character) = decoded {
            output.push(character);
        } else {
            output.push_str(&entity[..=end]);
        }
        remainder = &entity[end + 1..];
    }
    output.push_str(remainder);
    output
}

fn collapse_whitespace(input: &str) -> String {
    input.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::{
        ProviderCostEstimate, ProviderCostState, ProviderPricingEvidence,
        ProviderPricingRefreshError, ProviderPricingValue, ProviderScopedPricingSnapshot,
        embedded_seed, ensure_current_adjustment_policy, ensure_seed_model_coverage,
        fetch_official_snapshot, fetch_provider_pricing_manual, latest_provider_pricing_snapshot,
        legacy_policy_needs_multiplier_repair, parse_official_html, provider_pricing_capability,
        quota_multiplier, store_provider_pricing_snapshot,
    };
    use chrono::{DateTime, Utc};

    use crate::db::Database;
    use crate::models::AppConfig;
    use crate::provider::{COMMAND_CODE_PROVIDER_ID, GOAT_OFFERING_ID};

    #[test]
    fn seed_coverage_backfills_missing_models_and_prices_them() {
        // A snapshot from a database created before the seed grew: no muse rows.
        let mut snapshot = embedded_seed();
        let previous_revision = snapshot.revision.clone();
        snapshot
            .models
            .retain(|entry| !entry.model_id.starts_with("muse-spark-1.2"));
        assert!(
            snapshot
                .estimate("muse-spark-1.2", 1_000, 100, 0, 0, None)
                .cost
                .is_none()
        );

        let repaired = ensure_seed_model_coverage(snapshot);
        assert_ne!(repaired.revision, previous_revision);
        let estimate = repaired.estimate("muse-spark-1.2", 1_000, 100, 0, 0, None);
        let cost = estimate.cost.expect("muse-spark-1.2 must be priced");
        // 1k uncached input @ $0.10 + 100 output @ $0.20 per M tokens.
        assert!((cost - (1_000.0 * 0.10 + 100.0 * 0.20) / 1_000_000.0).abs() < 1e-9);
        assert!(
            repaired
                .estimate("muse-spark-1.2-contributor", 1_000, 100, 0, 0, None)
                .cost
                .is_some()
        );
    }

    #[test]
    fn seed_coverage_never_overwrites_existing_rows() {
        // A snapshot whose muse-contributor row came from the official table
        // (or carries a user-edited multiplier) must survive the backfill.
        let mut snapshot = embedded_seed();
        snapshot
            .models
            .retain(|entry| entry.model_id != "muse-spark-1.2-contributor");
        let edited = super::PricingModel {
            model_id: "muse-spark-1.2-contributor".to_string(),
            display_name: "Muse Spark 1.2 Contributor".to_string(),
            input: 0.10,
            output: 0.20,
            cache_read: 0.002,
            cache_write: None,
            usage: 60.0,
            quota_multiplier: 2.5,
            min_input_tokens: None,
            max_input_tokens: None,
            time_window: super::PricingTimeWindow::Always,
            adjustments: Vec::new(),
        };
        snapshot.models.push(edited);

        let repaired = ensure_seed_model_coverage(snapshot);
        let row = repaired
            .models
            .iter()
            .find(|entry| entry.model_id == "muse-spark-1.2-contributor")
            .unwrap();
        assert_eq!(row.quota_multiplier, 2.5);
    }

    #[test]
    fn seed_coverage_is_noop_when_every_seed_model_is_present() {
        let snapshot = embedded_seed();
        let repaired = ensure_seed_model_coverage(snapshot.clone());
        assert_eq!(repaired.revision, snapshot.revision);
        assert_eq!(repaired.activated_at, snapshot.activated_at);
    }

    #[test]
    fn seed_coverage_does_not_revive_models_outside_the_muse_allowlist() {
        let mut snapshot = embedded_seed();
        snapshot.models.retain(|entry| entry.model_id != "grok-4.5");

        let repaired = ensure_seed_model_coverage(snapshot);

        assert!(
            repaired
                .models
                .iter()
                .all(|entry| entry.model_id != "grok-4.5")
        );
    }

    #[test]
    fn seed_uses_go_usage_as_quota_multiplier() {
        let snapshot = embedded_seed();
        let grok = snapshot
            .models
            .iter()
            .find(|entry| entry.model_id == "grok-4.5")
            .unwrap();
        let glm = snapshot
            .models
            .iter()
            .find(|entry| entry.model_id == "glm-5.2")
            .unwrap();
        assert_eq!(grok.quota_multiplier, 4.0);
        assert_eq!(glm.quota_multiplier, 1.0);
    }

    #[test]
    fn provider_quota_formula_uses_plan_limit_over_model_allowance() {
        assert_eq!(quota_multiplier(60.0, 15.0).unwrap(), 4.0);
        assert_eq!(quota_multiplier(60.0, 60.0).unwrap(), 1.0);
        assert!(quota_multiplier(60.0, 0.0).is_err());

        let estimate =
            ProviderCostEstimate::from_raw(2.0, Some(60.0), Some(15.0), Some(10.0)).unwrap();
        assert_eq!(estimate.raw_cost, Some(2.0));
        assert_eq!(estimate.quota_debit, Some(8.0));
        assert!((estimate.paid_cost.unwrap() - (4.0 / 3.0)).abs() < 1e-12);
        assert_eq!(estimate.cost_state, ProviderCostState::Priced);

        let unknown = ProviderCostEstimate::from_raw(2.0, None, None, None).unwrap();
        assert_eq!(unknown.raw_cost, Some(2.0));
        assert_eq!(unknown.quota_debit, None);
        assert_eq!(unknown.paid_cost, None);
        assert_eq!(unknown.cost_state, ProviderCostState::Unpriced);
    }

    #[test]
    fn zen_free_is_zero_in_every_cost_domain() {
        let estimate = ProviderCostEstimate::zen_free();
        assert_eq!(estimate.raw_cost, Some(0.0));
        assert_eq!(estimate.quota_debit, Some(0.0));
        assert_eq!(estimate.paid_cost, Some(0.0));
        assert_eq!(estimate.cost_state, ProviderCostState::Free);
    }

    #[test]
    fn provider_snapshot_round_trips_legacy_go_shape() {
        let legacy = embedded_seed();
        let typed = ProviderScopedPricingSnapshot::from_opencode_go(&legacy).unwrap();
        let record = typed.to_storage_record().unwrap();
        let loaded = ProviderScopedPricingSnapshot::from_storage_record(&record).unwrap();
        assert_eq!(loaded.provider_id(), "opencode");
        assert_eq!(loaded.offering_id(), "go");
        assert_eq!(loaded.revision(), legacy.revision);
        assert_eq!(loaded.evidence(), ProviderPricingEvidence::Verified);
        assert_eq!(loaded.values().len(), legacy.models.len());

        let legacy_record = crate::provider::ProviderPricingSnapshot {
            provider_id: "opencode".to_string(),
            offering_id: "go".to_string(),
            revision: legacy.revision.clone(),
            activated_at: legacy.activated_at.clone(),
            document_updated_at: Some(legacy.document_updated_at.clone()),
            source_url: legacy.source_url.clone(),
            content_hash: legacy.content_hash.clone(),
            snapshot_json: serde_json::to_string(&legacy).unwrap(),
        };
        let migrated = ProviderScopedPricingSnapshot::from_storage_record(&legacy_record).unwrap();
        assert_eq!(migrated.values().len(), legacy.models.len());
    }

    #[test]
    fn provider_snapshot_revision_is_append_only_in_v22_store() {
        let dir =
            std::env::temp_dir().join(format!("ocg-provider-pricing-{}", uuid::Uuid::new_v4()));
        let db = Database::open(dir.clone()).unwrap();
        let value = |name: &str, allowance: f64| {
            ProviderPricingValue::new(
                "captured-model",
                name,
                None,
                None,
                None,
                None,
                Some(60.0),
                Some(allowance),
                None,
                None,
                None,
                None,
                super::PricingTimeWindow::Always,
            )
            .unwrap()
        };
        let snapshot = |name: &str, allowance: f64| {
            ProviderScopedPricingSnapshot::new(
                COMMAND_CODE_PROVIDER_ID,
                GOAT_OFFERING_ID,
                "capture-1",
                "2030-01-01T00:00:00Z",
                None,
                "",
                "",
                ProviderPricingEvidence::Experimental,
                vec![value(name, allowance)],
            )
            .unwrap()
        };
        store_provider_pricing_snapshot(&db, &snapshot("first", 15.0)).unwrap();
        // Same provider/offering/revision is ignored, not overwritten.
        store_provider_pricing_snapshot(&db, &snapshot("second", 60.0)).unwrap();
        let loaded =
            latest_provider_pricing_snapshot(&db, COMMAND_CODE_PROVIDER_ID, GOAT_OFFERING_ID)
                .unwrap()
                .unwrap();
        assert_eq!(loaded.values()[0].display_name(), "first");
        assert_eq!(loaded.values()[0].quota_multiplier(), Some(4.0));
        drop(db);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[tokio::test]
    async fn goat_manual_pricing_refresh_is_explicitly_unavailable() {
        let capability =
            provider_pricing_capability(COMMAND_CODE_PROVIDER_ID, GOAT_OFFERING_ID).unwrap();
        assert_eq!(capability.evidence, ProviderPricingEvidence::Unavailable);
        assert!(capability.experimental);
        assert_eq!(capability.source_url, None);
        assert!(!capability.manual_refresh_available);
        let error = fetch_provider_pricing_manual(
            &AppConfig::default(),
            COMMAND_CODE_PROVIDER_ID,
            GOAT_OFFERING_ID,
        )
        .await
        .unwrap_err();
        assert_eq!(
            error,
            ProviderPricingRefreshError::ExperimentalContractUnavailable
        );
    }

    #[test]
    fn pro_usage_allowance_is_applied_after_the_official_table_rates() {
        let snapshot = embedded_seed();
        for (model_id, prompt, cached, completion, official_monthly_requests) in [
            ("deepseek-v4-pro", 82_750, 82_000, 290, 5_200.0),
            ("mimo-v2.5-pro", 86_790, 86_000, 305, 16_300.0),
        ] {
            let model = snapshot
                .models
                .iter()
                .find(|entry| entry.model_id == model_id)
                .unwrap();
            assert_eq!(model.usage, 15.0);
            assert_eq!(model.quota_multiplier, 4.0);

            let estimate = snapshot.estimate_at(
                model_id,
                prompt,
                completion,
                cached,
                0,
                None,
                DateTime::parse_from_rfc3339("2026-08-16T12:00:00Z")
                    .unwrap()
                    .with_timezone(&Utc),
            );
            let estimated_monthly_requests = snapshot.limits.window_month / estimate.cost.unwrap();
            assert!(
                (estimated_monthly_requests / official_monthly_requests - 1.0).abs() < 0.01,
                "{model_id}: {estimated_monthly_requests} != {official_monthly_requests}",
            );
            assert_eq!(estimate.quota_multiplier, Some(4.0));
        }

        let grok = snapshot
            .models
            .iter()
            .find(|entry| entry.model_id == "grok-4.5")
            .unwrap();
        assert_eq!(grok.usage, 15.0);
        assert_eq!(grok.quota_multiplier, 4.0);
        assert_eq!(
            snapshot.estimate("grok-4.5", 1_000_000, 0, 0, 0, None).cost,
            Some(8.0)
        );
    }

    #[test]
    fn policy_upgrade_repairs_persisted_pro_quota_multipliers() {
        let mut snapshot = embedded_seed();
        snapshot.adjustment_policy_version = "local-v2".to_string();
        for model in &mut snapshot.models {
            if matches!(model.model_id.as_str(), "deepseek-v4-pro" | "mimo-v2.5-pro") {
                model.quota_multiplier = 1.0;
            }
        }

        let upgraded = ensure_current_adjustment_policy(snapshot);
        for model_id in ["deepseek-v4-pro", "mimo-v2.5-pro"] {
            let model = upgraded
                .models
                .iter()
                .find(|entry| entry.model_id == model_id)
                .unwrap();
            assert_eq!(model.quota_multiplier, 4.0);
        }
    }

    #[test]
    fn legacy_snapshot_json_drops_old_price_multiplier_and_repairs_applied_multiplier() {
        let mut value = serde_json::to_value(embedded_seed()).unwrap();
        value["adjustment_policy_version"] = serde_json::Value::String("local-v2".into());
        for model in value["models"].as_array_mut().unwrap() {
            let object = model.as_object_mut().unwrap();
            if matches!(
                object.get("model_id").and_then(serde_json::Value::as_str),
                Some("deepseek-v4-pro" | "mimo-v2.5-pro")
            ) {
                object.insert("official_price_multiplier".into(), serde_json::json!(4.0));
                object.insert("quota_multiplier".into(), serde_json::json!(1.0));
            }
        }

        let persisted = serde_json::from_value(value).unwrap();
        let upgraded = ensure_current_adjustment_policy(persisted);
        for model_id in ["deepseek-v4-pro", "mimo-v2.5-pro"] {
            let model = upgraded
                .models
                .iter()
                .find(|entry| entry.model_id == model_id)
                .unwrap();
            assert_eq!(model.quota_multiplier, 4.0);
        }
        assert!(
            !serde_json::to_string(&upgraded)
                .unwrap()
                .contains("official_price_multiplier")
        );
    }

    #[test]
    fn editable_policy_versions_are_never_rebased() {
        assert!(legacy_policy_needs_multiplier_repair("local-v2"));
        assert!(!legacy_policy_needs_multiplier_repair("local-v3"));
        assert!(!legacy_policy_needs_multiplier_repair("local-v4"));
        assert!(!legacy_policy_needs_multiplier_repair("local-v99"));

        let mut snapshot = embedded_seed();
        snapshot.adjustment_policy_version = "local-v3".to_string();
        snapshot
            .models
            .iter_mut()
            .filter(|model| model.model_id == "qwen3.7-plus")
            .for_each(|model| model.quota_multiplier = 0.75);
        let upgraded = ensure_current_adjustment_policy(snapshot);
        assert!(
            upgraded
                .models
                .iter()
                .filter(|model| model.model_id == "qwen3.7-plus")
                .all(|model| model.quota_multiplier == 0.75)
        );
    }

    #[test]
    fn minimax_adjustments_follow_local_policy() {
        let snapshot = embedded_seed();
        let at_boundary = snapshot.estimate("minimax-m3", 512_000, 10, 0, 0, None);
        let over_boundary = snapshot.estimate("minimax-m3", 512_001, 10, 0, 0, None);
        assert!((over_boundary.local_adjustment_multiplier.unwrap() - 2.0).abs() < 1e-12);
        assert_eq!(at_boundary.local_adjustment_multiplier, Some(1.0));
        let priority = snapshot.estimate("minimax-m3", 1000, 10, 0, 0, Some("priority"));
        assert!((priority.local_adjustment_multiplier.unwrap() - 1.5).abs() < 1e-12);
        let combined = snapshot.estimate("minimax-m3", 512_001, 10, 0, 0, Some("priority"));
        assert!((combined.local_adjustment_multiplier.unwrap() - 3.0).abs() < 1e-12);
    }

    #[test]
    fn highspeed_only_doubles_input_and_output() {
        let snapshot = embedded_seed();
        let normal = snapshot
            .estimate("minimax-m2.7", 1000, 100, 400, 300, None)
            .cost
            .unwrap();
        let fast = snapshot
            .estimate("minimax-m2.7-highspeed", 1000, 100, 400, 300, None)
            .cost
            .unwrap();
        let expected = (300.0 * 0.60 + 100.0 * 2.40 + 400.0 * 0.06 + 300.0 * 0.375) / 1_000_000.0;
        assert!((fast - expected).abs() < 1e-12);
        assert!(fast < normal * 2.0);
    }

    #[test]
    fn unknown_model_is_unpriced() {
        let estimate = embedded_seed().estimate("future-model", 1000, 100, 0, 0, None);
        assert_eq!(estimate.cost, None);
        assert_eq!(estimate.cost_state, "unpriced");
        let prefixed = embedded_seed().estimate("provider-minimax-m3", 1000, 100, 0, 0, None);
        assert_eq!(prefixed.cost, None);
    }

    #[test]
    fn zen_free_models_do_not_enter_go_quota() {
        for model_id in [
            "mimo-v2.5-free",
            "hy3-free",
            "muse-spark-1.2-contributor-free",
        ] {
            let estimate = embedded_seed().estimate(model_id, 1000, 100, 0, 0, None);
            assert_eq!(estimate.cost, None, "{model_id}");
            assert_eq!(estimate.raw_cost_usd, Some(0.0), "{model_id}");
            assert_eq!(estimate.quota_debit, Some(0.0), "{model_id}");
            assert_eq!(estimate.effective_paid_cost_usd, Some(0.0), "{model_id}");
            assert_eq!(estimate.cost_state, "free", "{model_id}");
            assert_eq!(estimate.quota_multiplier, None, "{model_id}");
        }
        let paid = embedded_seed().estimate("deepseek-v4-flash", 1000, 100, 0, 0, None);
        assert_eq!(paid.cost_state, "priced");
        assert!(paid.cost.is_some());
        let go_named_free = embedded_seed().estimate("ox-alpha-free", 1000, 100, 0, 0, None);
        assert_eq!(go_named_free.cost_state, "unpriced");
        assert_ne!(go_named_free.cost_state, "free");
        let suffix_follows_zen_catalog_naming =
            embedded_seed().estimate("brand-new-promo-free", 1000, 100, 0, 0, None);
        assert_eq!(suffix_follows_zen_catalog_naming.cost_state, "free");
    }

    #[test]
    fn cache_write_dash_falls_back_to_new_input_price() {
        let estimate = embedded_seed().estimate("glm-5.2", 1000, 0, 0, 1000, None);
        assert!((estimate.cost.unwrap() - 0.0014).abs() < 1e-12);
    }

    #[test]
    fn parses_official_fixture() {
        let snapshot =
            parse_official_html(include_str!("../tests/fixtures/opencode-go.html")).unwrap();
        assert_eq!(snapshot.limits.window_5h, 12.0);
        assert_eq!(snapshot.limits.window_week, 30.0);
        assert_eq!(snapshot.limits.window_month, 60.0);
        assert_eq!(snapshot.models.len(), 25);
        assert!(
            snapshot
                .models
                .iter()
                .any(|entry| entry.model_id == "kimi-k3" && entry.quota_multiplier == 4.0)
        );
        for model_id in [
            "deepseek-v4-pro",
            "deepseek-v4-flash",
            "mimo-v2.5-pro",
            "gpt-5.6-luna",
        ] {
            let model = snapshot
                .models
                .iter()
                .find(|entry| entry.model_id == model_id)
                .unwrap();
            assert_eq!(model.quota_multiplier, 4.0);
        }
        assert_eq!(
            snapshot
                .models
                .iter()
                .filter(|entry| entry.model_id == "deepseek-v4-flash")
                .count(),
            2
        );
        assert!(
            snapshot
                .models
                .iter()
                .any(|entry| entry.model_id == "hy3" && entry.quota_multiplier == 1.0)
        );
        assert!(
            !snapshot
                .models
                .iter()
                .any(|entry| entry.model_id == "ox-alpha-free"),
            "dash-priced Go promos must not enter the USD snapshot"
        );
        let luna_tiers = snapshot
            .models
            .iter()
            .filter(|entry| entry.model_id == "gpt-5.6-luna")
            .count();
        assert_eq!(luna_tiers, 2);
    }

    #[test]
    fn deepseek_uses_utc_peak_and_off_peak_rows() {
        let snapshot = embedded_seed();
        let off_peak = DateTime::parse_from_rfc3339("2026-08-16T12:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let peak = DateTime::parse_from_rfc3339("2026-08-16T07:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let off = snapshot
            .estimate_at("deepseek-v4-flash", 1_000_000, 0, 0, 0, None, off_peak)
            .cost
            .unwrap();
        let on = snapshot
            .estimate_at("deepseek-v4-flash", 1_000_000, 0, 0, 0, None, peak)
            .cost
            .unwrap();
        assert!((off - 0.88).abs() < 1e-12);
        assert!((on - 1.76).abs() < 1e-12);
    }

    #[test]
    fn rejects_incomplete_peak_off_peak_pair() {
        let fixture = include_str!("../tests/fixtures/opencode-go.html").replace(
            "<tr><td>DeepSeek V4 Flash (Peak)</td><td>$0.44</td><td>$1.32</td><td>$0.014</td><td>-</td><td>$15</td></tr>",
            "",
        );
        assert!(
            parse_official_html(&fixture)
                .unwrap_err()
                .to_string()
                .contains("must contain both Peak and Off-Peak")
        );
    }

    #[test]
    fn rejects_model_id_without_a_matching_price_row() {
        let fixture = include_str!("../tests/fixtures/opencode-go.html");
        let incomplete = fixture.replace(
            "<tr><td>Grok 4.5</td><td>$2.00</td><td>$6.00</td><td>$0.30</td><td>-</td><td>$15</td></tr>",
            "",
        );
        assert!(
            parse_official_html(&incomplete)
                .unwrap_err()
                .to_string()
                .contains("model ID table contains models without pricing rows")
        );
    }

    #[test]
    fn accepts_official_model_removal_when_both_tables_still_match() {
        let fixture = include_str!("../tests/fixtures/opencode-go.html")
            .replace(
                "<tr><td>Grok 4.5</td><td>$2.00</td><td>$6.00</td><td>$0.30</td><td>-</td><td>$15</td></tr>",
                "",
            )
            .replace(
                "<tr><td>Grok 4.5</td><td>grok-4.5</td><td>x</td><td>x</td></tr>",
                "",
            );
        let snapshot = parse_official_html(&fixture).unwrap();
        assert!(
            snapshot
                .models
                .iter()
                .all(|model| model.model_id != "grok-4.5")
        );
    }

    #[test]
    fn qwen_tier_validation_is_conditional_on_the_model_being_present() {
        let fixture = include_str!("../tests/fixtures/opencode-go.html")
            .replace(
                "<tr><td>Qwen3.7 Plus (&#x2264; 256K tokens)</td><td>$0.40</td><td>$1.60</td><td>$0.04</td><td>$0.50</td><td>$60</td></tr>",
                "",
            )
            .replace(
                "<tr><td>Qwen3.7 Plus (&gt; 256K tokens)</td><td>$1.20</td><td>$4.80</td><td>$0.12</td><td>$1.50</td><td>$60</td></tr>",
                "",
            )
            .replace(
                "<tr><td>Qwen3.7 Plus</td><td>qwen3.7-plus</td><td>x</td><td>x</td></tr>",
                "",
            );
        let snapshot = parse_official_html(&fixture).unwrap();
        assert!(
            snapshot
                .models
                .iter()
                .all(|model| model.model_id != "qwen3.7-plus")
        );
    }

    #[test]
    fn rejects_structurally_valid_but_empty_catalog() {
        let fixture = r#"
            <p>5 hour limit — $12 of usage</p>
            <p>Weekly limit — $30 of usage</p>
            <p>Monthly limit — $60 of usage</p>
            <table><thead><tr><th>Model</th><th>Input</th><th>Output</th><th>Cached Read</th><th>Cached Write</th><th>Usage</th></tr></thead><tbody></tbody></table>
            <table><thead><tr><th>Model</th><th>Model ID</th><th>Endpoint</th><th>AI SDK Package</th></tr></thead><tbody></tbody></table>
            <time datetime="2026-07-17T15:53:00.000Z">Jul 17, 2026</time>
        "#;
        assert!(
            parse_official_html(fixture)
                .unwrap_err()
                .to_string()
                .contains("must not be empty")
        );
    }

    #[test]
    fn parsed_limit_and_price_changes_drive_dynamic_multiplier() {
        let fixture = include_str!("../tests/fixtures/opencode-go.html")
            .replace(
                "Monthly limit — $60 of usage",
                "Monthly limit — $90 of usage",
            )
            .replace(
                "<tr><td>Kimi K3</td><td>$3.00</td>",
                "<tr><td>Kimi K3</td><td>$3.50</td>",
            );
        let snapshot = parse_official_html(&fixture).unwrap();
        let kimi = snapshot
            .models
            .iter()
            .find(|model| model.model_id == "kimi-k3")
            .unwrap();
        assert_eq!(snapshot.limits.window_month, 90.0);
        assert_eq!(kimi.input, 3.5);
        assert_eq!(kimi.quota_multiplier, 6.0);
    }

    #[test]
    fn accepts_new_models_with_an_official_id_and_complete_prices() {
        let fixture = include_str!("../tests/fixtures/opencode-go.html")
            .replace("\r\n", "\n")
            .replace(
                "</tbody></table>\n<table><thead><tr><th>Model</th><th>Model ID</th>",
                "<tr><td>Future Model</td><td>$1.00</td><td>$2.00</td><td>$0.10</td><td>-</td><td>$60</td></tr></tbody></table>\n<table><thead><tr><th>Model</th><th>Model ID</th>",
            )
            .replace(
                "</tbody></table>\n<footer>",
                "<tr><td>Future Model</td><td>future-model</td><td>x</td><td>x</td></tr></tbody></table>\n<footer>",
            );
        let snapshot = parse_official_html(&fixture).unwrap();
        assert!(
            snapshot
                .models
                .iter()
                .any(|model| model.model_id == "future-model")
        );
    }

    #[test]
    fn rejects_missing_or_reordered_price_columns() {
        let fixture = include_str!("../tests/fixtures/opencode-go.html").replace(
            "<th>Input</th><th>Output</th>",
            "<th>Output</th><th>Input</th>",
        );
        assert!(
            parse_official_html(&fixture)
                .unwrap_err()
                .to_string()
                .contains("pricing table was not found")
        );
    }

    #[test]
    fn rejects_duplicate_price_rows() {
        let fixture = include_str!("../tests/fixtures/opencode-go.html").replace(
            "<tr><td>Grok 4.5</td><td>$2.00</td><td>$6.00</td><td>$0.30</td><td>-</td><td>$15</td></tr>",
            "<tr><td>Grok 4.5</td><td>$2.00</td><td>$6.00</td><td>$0.30</td><td>-</td><td>$15</td></tr><tr><td>Grok 4.5</td><td>$2.00</td><td>$6.00</td><td>$0.30</td><td>-</td><td>$15</td></tr>",
        );
        assert!(
            parse_official_html(&fixture)
                .unwrap_err()
                .to_string()
                .contains("duplicate row")
        );
    }

    #[tokio::test]
    #[ignore = "requires live access to opencode.ai"]
    async fn live_official_document_still_matches_the_parser() {
        let snapshot = fetch_official_snapshot(&crate::models::AppConfig::default())
            .await
            .unwrap();
        assert_eq!(snapshot.source_url, super::SOURCE_URL);
        assert!(snapshot.models.len() >= 18);
    }
}
