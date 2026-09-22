use anyhow::{Context, Result, anyhow, bail};
use chrono::{DateTime, Datelike, Timelike, Utc};
use futures_util::StreamExt;
use reqwest::redirect::{Attempt, Policy};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::fmt;
use std::time::Duration;

use crate::db::Database;
use crate::kernel::ids::{
    COMMAND_CODE_PROVIDER_ID, OLLAMA_CLOUD_PRICING_URL, OLLAMA_PROVIDER_ID, OPENCODE_PROVIDER_ID,
    OPENCODE_ZEN_FREE_PROVIDER_ID, is_free_model,
};
use ocg_domain::billing::{BillingTokens, TokenRates, convert_charge, token_charge};

pub use crate::kernel::ids::normalize_model_name;
use crate::kernel::pricing::ProviderPricingValueWire;
pub use crate::kernel::pricing::{
    PricingAdjustment, PricingEstimate, PricingLimits, PricingModel, PricingSnapshot,
    PricingTimeWindow, ProviderCostEstimate, ProviderCostState, ProviderPricingCapability,
    ProviderPricingEvidence, ProviderPricingSnapshot, ProviderPricingValue, SEED_LIMITS,
    SOURCE_URL, provider_pricing_capability, quota_multiplier, seed_snapshot,
};

const SOURCE_HOST: &str = "opencode.ai";
pub const GOAT_SOURCE_URL: &str = "https://commandcode.ai/docs/plans/goat";
const GOAT_SOURCE_HOST: &str = "commandcode.ai";
pub const OLLAMA_SOURCE_URL: &str = OLLAMA_CLOUD_PRICING_URL;
const OLLAMA_SOURCE_HOST: &str = "ollama.com";
const MAX_DOCUMENT_BYTES: usize = 2 * 1024 * 1024;
const ADJUSTMENT_POLICY_VERSION: &str = "local-v4";

/// Typed, append-only value stored inside `provider_pricing_snapshots`.
/// Fields are private so a loaded snapshot cannot be mutated in place; a new
/// official observation receives a new revision.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct ProviderScopedPricingSnapshot {
    provider_id: String,

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

        revision: impl Into<String>,
        activated_at: impl Into<String>,
        document_updated_at: Option<String>,
        source_url: impl Into<String>,
        content_hash: impl Into<String>,
        evidence: ProviderPricingEvidence,
        values: Vec<ProviderPricingValue>,
    ) -> Result<Self> {
        let provider_id = provider_id.into();
        let revision = revision.into();
        let activated_at = activated_at.into();
        let source_url = source_url.into();
        let content_hash = content_hash.into();
        if [
            provider_id.as_str(),
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

    pub fn revision(&self) -> &str {
        &self.revision
    }

    pub fn evidence(&self) -> ProviderPricingEvidence {
        self.evidence
    }

    pub fn values(&self) -> &[ProviderPricingValue] {
        &self.values
    }

    pub fn activated_at(&self) -> &str {
        &self.activated_at
    }

    pub fn document_updated_at(&self) -> Option<&str> {
        self.document_updated_at.as_deref()
    }

    pub fn source_url(&self) -> &str {
        &self.source_url
    }

    pub fn content_hash(&self) -> &str {
        &self.content_hash
    }

    #[allow(clippy::too_many_arguments)]
    pub fn estimate(
        &self,
        model: &str,
        prompt: i64,
        completion: i64,
        cached: i64,
        cache_creation: i64,
        at: DateTime<Utc>,
    ) -> PricingEstimate {
        let tokens = BillingTokens::clamped(prompt, completion, cached, cache_creation);
        let preferred_time_window = if is_provider_peak_utc(&self.provider_id, at) {
            PricingTimeWindow::Peak
        } else {
            PricingTimeWindow::OffPeak
        };
        let Some(value) = select_provider_pricing_value(
            &self.values,
            model,
            tokens.input,
            preferred_time_window,
            &self.provider_id,
        ) else {
            return PricingEstimate::unpriced(&self.revision);
        };

        let rates = (
            value.input_per_million(),
            value.output_per_million(),
            value.cache_read_per_million(),
        );
        if matches!(rates, (None, None, None)) && value.cache_write_per_million().is_none() {
            return PricingEstimate::free(&self.revision);
        }
        let (Some(input), Some(output), ..) = rates else {
            return PricingEstimate::unpriced(&self.revision);
        };
        // A model without a published cache rate bills cached traffic at the
        // input rate, matching how cache writes already fall back.
        let cache_read = value.cache_read_per_million().unwrap_or(input);
        let cache_write = value.cache_write_per_million().unwrap_or(input);
        let Some(raw_cost) = token_charge(
            tokens,
            TokenRates::per_million(input, output, Some(cache_read), Some(cache_write)),
        ) else {
            return PricingEstimate::unpriced(&self.revision);
        };
        let estimate = ProviderCostEstimate::from_raw_with_multiplier(
            raw_cost,
            value.quota_multiplier(),
            value.paid_plan_price(),
            value.plan_limit(),
        )
        .expect("validated provider pricing must remain calculable");
        if estimate.cost_state != ProviderCostState::Priced {
            return PricingEstimate::unpriced(&self.revision);
        }
        PricingEstimate {
            raw_cost_usd: estimate.raw_cost,
            quota_debit: estimate.quota_debit,
            effective_paid_cost_usd: estimate.paid_cost,
            cost: estimate.quota_debit,
            pricing_revision_id: Some(self.revision.clone()),
            quota_multiplier: value.quota_multiplier(),
            local_adjustment_multiplier: Some(1.0),
            cost_state: "priced",
        }
    }

    pub fn to_storage_record(&self) -> Result<ProviderPricingSnapshot> {
        Ok(ProviderPricingSnapshot {
            provider_id: self.provider_id.clone(),

            revision: self.revision.clone(),
            activated_at: self.activated_at.clone(),
            document_updated_at: self.document_updated_at.clone(),
            source_url: self.source_url.clone(),
            content_hash: self.content_hash.clone(),
            snapshot_json: serde_json::to_string(self)?,
        })
    }

    pub fn from_storage_record(record: &ProviderPricingSnapshot) -> Result<Self> {
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
        if record.provider_id == OPENCODE_PROVIDER_ID {
            let legacy: PricingSnapshot = serde_json::from_str(&record.snapshot_json)
                .context("invalid provider pricing snapshot JSON")?;
            let snapshot = Self::from_opencode_go(&legacy)?;
            snapshot.ensure_matches_record(record)?;
            return Ok(snapshot);
        }
        bail!(
            "provider pricing snapshot `{}/{}` has an unsupported value schema",
            record.provider_id,
            record.revision
        )
    }

    fn ensure_matches_record(&self, record: &ProviderPricingSnapshot) -> Result<()> {
        if self.provider_id != record.provider_id
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
    UnknownProvider,
    NotApplicable,
    FetchFailed,
}

impl fmt::Display for ProviderPricingRefreshError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownProvider => f.write_str("unknown provider pricing offering"),
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
) -> std::result::Result<ProviderScopedPricingSnapshot, ProviderPricingRefreshError> {
    match provider_id {
        OPENCODE_PROVIDER_ID => {
            let snapshot = fetch_official_snapshot(config)
                .await
                .map_err(|_| ProviderPricingRefreshError::FetchFailed)?;
            ProviderScopedPricingSnapshot::from_opencode_go(&snapshot)
                .map_err(|_| ProviderPricingRefreshError::FetchFailed)
        }
        COMMAND_CODE_PROVIDER_ID => fetch_goat_pricing_snapshot(config)
            .await
            .map_err(|_| ProviderPricingRefreshError::FetchFailed),
        OLLAMA_PROVIDER_ID => fetch_ollama_pricing_snapshot(config)
            .await
            .map_err(|_| ProviderPricingRefreshError::FetchFailed),
        OPENCODE_ZEN_FREE_PROVIDER_ID => Err(ProviderPricingRefreshError::NotApplicable),
        _ => Err(ProviderPricingRefreshError::UnknownProvider),
    }
}

pub async fn fetch_goat_pricing_snapshot(
    config: &crate::models::AppConfig,
) -> Result<ProviderScopedPricingSnapshot> {
    let html = fetch_approved_host_html(
        config,
        GOAT_SOURCE_URL,
        GOAT_SOURCE_HOST,
        "Command Code GOAT pricing",
    )
    .await?;
    parse_goat_html(&html)
}

pub async fn fetch_ollama_pricing_snapshot(
    config: &crate::models::AppConfig,
) -> Result<ProviderScopedPricingSnapshot> {
    let html = fetch_approved_host_html(
        config,
        OLLAMA_SOURCE_URL,
        OLLAMA_SOURCE_HOST,
        "Ollama Cloud pricing",
    )
    .await?;
    parse_ollama_html(&html)
}

pub(crate) async fn fetch_approved_host_html(
    config: &crate::models::AppConfig,
    url: &str,
    host: &'static str,
    label: &'static str,
) -> Result<String> {
    let client = crate::http_client::configured_builder(config)?
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(20))
        .redirect(Policy::custom(move |attempt: Attempt<'_>| {
            same_approved_host_redirect(attempt, host, label)
        }))
        .build()
        .with_context(|| format!("build {label} client"))?;
    let response = client
        .get(url)
        .header(
            reqwest::header::USER_AGENT,
            concat!("OpenConsoleGateway/", env!("CARGO_PKG_VERSION")),
        )
        .send()
        .await
        .with_context(|| format!("fetch {label} page"))?
        .error_for_status()
        .with_context(|| format!("{label} page returned an error"))?;
    if response
        .content_length()
        .is_some_and(|length| length > MAX_DOCUMENT_BYTES as u64)
    {
        bail!("{label} page exceeds 2 MiB");
    }
    let mut bytes = Vec::new();
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.with_context(|| format!("read {label} page"))?;
        if bytes.len() + chunk.len() > MAX_DOCUMENT_BYTES {
            bail!("{label} page exceeds 2 MiB");
        }
        bytes.extend_from_slice(&chunk);
    }
    String::from_utf8(bytes).with_context(|| format!("{label} page is not UTF-8"))
}

fn same_approved_host_redirect(
    attempt: Attempt<'_>,
    host: &str,
    label: &str,
) -> reqwest::redirect::Action {
    if attempt.previous().len() >= 5 {
        return attempt.error(format!("too many {label} redirects"));
    }
    let url = attempt.url();
    if url.scheme() == "https"
        && url.host_str() == Some(host)
        && url.port_or_known_default() == Some(443)
    {
        attempt.follow()
    } else {
        attempt.error(format!("{label} redirect left the approved HTTPS host"))
    }
}

pub fn parse_goat_html(html: &str) -> Result<ProviderScopedPricingSnapshot> {
    let plain = collapse_whitespace(&strip_tags(html));
    let included_count = parse_count_before(&plain, "All plans")?;
    let monthly_price = parse_first_dollar_after(&plain, "for $")?;
    let window_5h = parse_first_dollar_after(&plain, "5-hour limit - $")?;
    let window_week = parse_first_dollar_after(&plain, "Weekly limit - $")?;
    let window_month = parse_first_dollar_after(&plain, "Monthly limit - $")?;
    if included_count == 0
        || monthly_price <= 0.0
        || window_5h <= 0.0
        || window_week <= 0.0
        || window_month <= 0.0
    {
        bail!("Command Code GOAT plan summary is invalid");
    }

    let tables = extract_tables(html)?;
    let rates = tables
        .iter()
        .find(|table| {
            has_headers(
                table,
                &[
                    "model",
                    "context",
                    "intelligence",
                    "tok/s",
                    "input",
                    "output",
                    "cache read",
                    "cache write",
                    "caps",
                ],
            )
        })
        .ok_or_else(|| {
            let headers = tables
                .iter()
                .filter_map(|table| table.first())
                .map(|row| row.join(" | "))
                .collect::<Vec<_>>()
                .join("; ");
            anyhow!("Command Code GOAT model pricing table was not found; headers: {headers}")
        })?;

    let mut allowances = HashMap::<String, f64>::new();
    for table in tables.iter().filter(|table| {
        has_headers(
            table,
            &[
                "model",
                "input",
                "output",
                "cache read",
                "cache write",
                "monthly credits",
            ],
        )
    }) {
        for row in table.iter().skip(1) {
            if row.len() != 6 {
                bail!("Command Code GOAT monthly-credit table contains an incomplete row");
            }
            let name = clean_goat_model_name(&row[0]);
            let allowance = parse_goat_money(&row[5])?
                .ok_or_else(|| anyhow!("{name} is missing a monthly allowance"))?;
            if allowances
                .insert(canonical_display_name(&name), allowance)
                .is_some()
            {
                bail!("Command Code GOAT monthly-credit table contains duplicate model {name}");
            }
        }
    }

    let older = parse_older_goat_models(&plain);
    let mut values = Vec::new();
    let mut seen = HashSet::new();
    for row in rates.iter().skip(1) {
        if row.len() != 9 {
            bail!("Command Code GOAT model pricing table contains an incomplete row");
        }
        let base_name = clean_goat_model_name(&row[0]);
        let free_variant = goat_model_has_free_badge(&row[0]);
        let display_name = if free_variant {
            format!("{base_name} Free")
        } else {
            base_name.clone()
        };
        let identity = canonical_display_name(&display_name);
        if identity.is_empty() || !seen.insert(identity) {
            bail!("Command Code GOAT model pricing table contains an invalid or duplicate model");
        }
        let input = parse_goat_money(&row[4])?;
        let output = parse_goat_money(&row[5])?;
        let cache_read = parse_goat_money(&row[6])?;
        let cache_write = parse_goat_money(&row[7])?;
        let allowance_identity = canonical_display_name(&base_name);
        let allowance = (!free_variant)
            .then(|| {
                allowances.get(&allowance_identity).copied().or_else(|| {
                    older
                        .contains(&allowance_identity)
                        .then_some(window_month.min(20.0))
                })
            })
            .flatten();
        let model_id = goat_reference_model_id(&display_name);
        if row[0].contains("Off-peak shown") {
            let peak_input = parse_goat_peak_money(html, &base_name, "input")?;
            let peak_output = parse_goat_peak_money(html, &base_name, "output")?;
            let peak_cache_read = parse_goat_peak_money(html, &base_name, "cache read")?;
            let peak_cache_write = if cache_write.is_some() {
                Some(parse_goat_peak_money(html, &base_name, "cache write")?)
            } else {
                None
            };
            values.push(ProviderPricingValue::new(
                model_id.clone(),
                display_name.clone(),
                input,
                output,
                cache_read,
                cache_write,
                Some(window_month),
                allowance,
                Some(monthly_price),
                Some("USD".to_string()),
                None,
                None,
                PricingTimeWindow::OffPeak,
            )?);
            values.push(ProviderPricingValue::new(
                model_id,
                display_name,
                Some(peak_input),
                Some(peak_output),
                Some(peak_cache_read),
                peak_cache_write,
                Some(window_month),
                allowance,
                Some(monthly_price),
                Some("USD".to_string()),
                None,
                None,
                PricingTimeWindow::Peak,
            )?);
        } else {
            values.push(ProviderPricingValue::new(
                model_id,
                display_name,
                input,
                output,
                cache_read,
                cache_write,
                Some(window_month),
                allowance,
                Some(monthly_price),
                Some("USD".to_string()),
                None,
                None,
                PricingTimeWindow::Always,
            )?);
        }
    }
    if seen.len() != included_count {
        bail!(
            "Command Code GOAT declared {included_count} included models but parsed {} rows",
            seen.len()
        );
    }
    let priced_models = values
        .iter()
        .filter(|value| value.input_per_million().is_some())
        .count();
    let priced_allowances = values
        .iter()
        .filter(|value| value.input_per_million().is_some())
        .filter(|value| value.model_allowance().is_some())
        .count();
    if priced_allowances != priced_models {
        bail!("Command Code GOAT monthly allowances are incomplete");
    }
    values.sort_by(|left, right| left.display_name().cmp(right.display_name()));
    let content_hash = format!("{:x}", Sha256::digest(html.as_bytes()));
    let revision = format!("goat-{}", content_hash.chars().take(16).collect::<String>());
    ProviderScopedPricingSnapshot::new(
        COMMAND_CODE_PROVIDER_ID,
        revision,
        Utc::now().to_rfc3339(),
        None,
        GOAT_SOURCE_URL,
        content_hash,
        ProviderPricingEvidence::Verified,
        values,
    )
}

pub fn parse_ollama_html(html: &str) -> Result<ProviderScopedPricingSnapshot> {
    let tables = extract_tables(html)?;
    let rates = tables
        .iter()
        .find(|table| has_headers(table, &["model", "input", "cached input", "output"]))
        .ok_or_else(|| {
            let headers = tables
                .iter()
                .filter_map(|table| table.first())
                .map(|row| row.join(" | "))
                .collect::<Vec<_>>()
                .join("; ");
            anyhow!("Ollama Cloud model pricing table was not found; headers: {headers}")
        })?;
    let mut values = Vec::new();
    let mut seen = HashSet::new();
    for row in rates.iter().skip(1) {
        if row.len() != 4 {
            bail!("Ollama Cloud model pricing table contains an incomplete row");
        }
        let model_id = row[0].trim().to_string();
        if model_id.is_empty() || !seen.insert(canonical_display_name(&model_id)) {
            bail!("Ollama Cloud model pricing table contains an invalid or duplicate model");
        }
        let input = parse_ollama_money(&row[1])?;
        let cached = parse_ollama_cached(&row[2])?;
        let output = parse_ollama_money(&row[3])?;
        values.push(
            ProviderPricingValue::new(
                model_id.clone(),
                model_id,
                Some(input),
                Some(output),
                cached,
                None,
                None,
                None,
                None,
                Some("USD".to_string()),
                None,
                None,
                PricingTimeWindow::Always,
            )?
            .with_quota_multiplier(1.0)?,
        );
    }
    if values.is_empty() {
        bail!("Ollama Cloud model pricing table is empty");
    }
    values.sort_by(|left, right| left.model_id().cmp(right.model_id()));
    let content_hash = format!("{:x}", Sha256::digest(html.as_bytes()));
    let revision = format!(
        "ollama-{}",
        content_hash.chars().take(16).collect::<String>()
    );
    ProviderScopedPricingSnapshot::new(
        OLLAMA_PROVIDER_ID,
        revision,
        Utc::now().to_rfc3339(),
        None,
        OLLAMA_SOURCE_URL,
        content_hash,
        ProviderPricingEvidence::Verified,
        values,
    )
}

fn parse_ollama_money(value: &str) -> Result<f64> {
    parse_dollar(value.trim(), false)?
        .ok_or_else(|| anyhow!("Ollama Cloud pricing cell is missing a USD value: {value}"))
}

fn parse_ollama_cached(value: &str) -> Result<Option<f64>> {
    parse_dollar(value.trim(), true)
}

fn parse_count_before(plain: &str, marker: &str) -> Result<usize> {
    let index = plain
        .find(marker)
        .ok_or_else(|| anyhow!("Command Code GOAT page is missing {marker}"))?;
    plain[..index]
        .split_whitespace()
        .next_back()
        .ok_or_else(|| anyhow!("Command Code GOAT page is missing its included-model count"))?
        .parse::<usize>()
        .context("invalid Command Code GOAT included-model count")
}

fn parse_first_dollar_after(plain: &str, marker: &str) -> Result<f64> {
    let start = plain
        .find(marker)
        .ok_or_else(|| anyhow!("Command Code GOAT page is missing {marker}"))?;
    let tail = &plain[start + marker.len() - 1..];
    let token = dollar_numeric_token(tail)
        .ok_or_else(|| anyhow!("Command Code GOAT page is missing a USD value after {marker}"))?;
    parse_dollar(token, false)?
        .ok_or_else(|| anyhow!("Command Code GOAT page is missing a USD value after {marker}"))
}

fn clean_goat_model_name(value: &str) -> String {
    let mut value = value.trim().to_string();
    for marker in ["Off-peak shown", "-98%", "-99%", "-50%", "Free"] {
        if let Some(index) = value.find(marker) {
            value.truncate(index);
        }
    }
    value.trim().to_string()
}

fn goat_model_has_free_badge(value: &str) -> bool {
    let value = value.trim();
    value.contains(" Free") || value.contains("FreeEnds") || value.ends_with("Free")
}

fn parse_goat_money(value: &str) -> Result<Option<f64>> {
    let value = value.trim();
    if value.eq_ignore_ascii_case("free") || matches!(value, "-" | "—" | "–") {
        return Ok(None);
    }
    let dollars = value
        .match_indices('$')
        .map(|(index, _)| index)
        .collect::<Vec<_>>();
    let start = dollars
        .last()
        .copied()
        .ok_or_else(|| anyhow!("expected GOAT USD value, got {value}"))?;
    let token = dollar_numeric_token(&value[start..])
        .ok_or_else(|| anyhow!("expected GOAT USD value, got {value}"))?;
    parse_dollar(token, false)
}

fn parse_goat_peak_money(html: &str, display_name: &str, price_kind: &str) -> Result<f64> {
    let marker = format!(r#"aria-label="{display_name} {price_kind}: $"#);
    parse_first_dollar_after(html, &marker).with_context(|| {
        format!("Command Code GOAT {display_name} is missing its peak {price_kind} price")
    })
}

fn parse_older_goat_models(plain: &str) -> HashSet<String> {
    let marker = "Older models also available";
    let Some(start) = plain.find(marker) else {
        return HashSet::new();
    };
    let tail = plain[start + marker.len()..].trim_start_matches(|character: char| {
        character.is_whitespace() || matches!(character, '-' | '—' | ':')
    });
    let end = tail.find("all at").unwrap_or(tail.len());
    tail[..end]
        .trim_end_matches(|character: char| {
            character.is_whitespace() || matches!(character, '-' | '—')
        })
        .replace(", and ", ", ")
        .replace(" and ", ", ")
        .split(',')
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(canonical_display_name)
        .collect()
}

fn dollar_numeric_token(value: &str) -> Option<&str> {
    let rest = value.strip_prefix('$')?;
    let length = rest
        .char_indices()
        .take_while(|(_, character)| character.is_ascii_digit() || matches!(character, '.' | ','))
        .map(|(index, character)| index + character.len_utf8())
        .last()?;
    Some(&value[..length + 1])
}

fn goat_reference_model_id(display_name: &str) -> String {
    let mut result = String::new();
    let mut dash = false;
    for character in display_name.chars().flat_map(char::to_lowercase) {
        if character.is_ascii_alphanumeric() {
            result.push(character);
            dash = false;
        } else if !dash && !result.is_empty() {
            result.push('-');
            dash = true;
        }
    }
    result.trim_end_matches('-').to_string()
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
) -> Result<Option<ProviderScopedPricingSnapshot>> {
    db.latest_provider_pricing_snapshot(provider_id)?
        .as_ref()
        .map(ProviderScopedPricingSnapshot::from_storage_record)
        .transpose()
}

/// Validate and apply provider-local multiplier overrides. The returned
/// snapshot is append-only and keeps the verified official rates/evidence;
/// only the applied quota multiplier and activation identity change.
pub(crate) fn prepare_provider_multiplier_update(
    active: &ProviderScopedPricingSnapshot,
    writes: &[(String, f64)],
) -> std::result::Result<Option<ProviderScopedPricingSnapshot>, String> {
    if writes.is_empty() {
        return Err("at least one multiplier is required".to_string());
    }
    let editable_models = active
        .values
        .iter()
        .filter(|value| value.quota_multiplier.is_some())
        .map(|value| value.model_id.as_str())
        .collect::<HashSet<_>>();
    let mut requested = BTreeMap::new();
    for (model_id, multiplier) in writes {
        let model_id = model_id.trim();
        if model_id.is_empty() || !editable_models.contains(model_id) {
            return Err(format!(
                "unknown or unpriced provider pricing model `{model_id}`"
            ));
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
    for value in &mut snapshot.values {
        if let Some(multiplier) = requested.get(&value.model_id)
            && value.quota_multiplier != Some(*multiplier)
        {
            value.quota_multiplier = Some(*multiplier);
            changed = true;
        }
    }
    if !changed {
        return Ok(None);
    }

    let mut revision_input = active.revision.clone();
    for (model_id, multiplier) in requested {
        revision_input.push('|');
        revision_input.push_str(&model_id);
        revision_input.push('=');
        revision_input.push_str(&multiplier.to_string());
    }
    let digest = format!("{:x}", Sha256::digest(revision_input.as_bytes()));
    snapshot.revision = format!("local-{}", &digest[..16]);
    snapshot.activated_at = Utc::now().to_rfc3339();
    Ok(Some(snapshot))
}

pub(crate) fn provider_multiplier_deltas(
    current: &ProviderScopedPricingSnapshot,
    official: &ProviderScopedPricingSnapshot,
) -> Vec<PricingMultiplierDelta> {
    let current = current
        .values
        .iter()
        .filter_map(|value| Some((value.model_id.clone(), value.quota_multiplier?)))
        .collect::<BTreeMap<_, _>>();
    official
        .values
        .iter()
        .filter_map(|value| {
            let official_multiplier = value.quota_multiplier?;
            let current_multiplier = *current.get(&value.model_id)?;
            (current_multiplier != official_multiplier).then(|| PricingMultiplierDelta {
                model_id: value.model_id.clone(),
                current_multiplier,
                official_multiplier,
            })
        })
        .collect()
}

pub(crate) fn merge_current_provider_multipliers(
    current: &ProviderScopedPricingSnapshot,
    candidate: &mut ProviderScopedPricingSnapshot,
) {
    let current = current
        .values
        .iter()
        .filter_map(|value| Some((value.model_id.as_str(), value.quota_multiplier?)))
        .collect::<HashMap<_, _>>();
    let mut changed = false;
    for value in &mut candidate.values {
        if let Some(multiplier) = current.get(value.model_id.as_str())
            && value.quota_multiplier != Some(*multiplier)
        {
            value.quota_multiplier = Some(*multiplier);
            changed = true;
        }
    }
    if changed {
        let mut revision_input = candidate.revision.clone();
        for value in &candidate.values {
            if let Some(multiplier) = value.quota_multiplier {
                revision_input.push('|');
                revision_input.push_str(&value.model_id);
                revision_input.push('=');
                revision_input.push_str(&multiplier.to_string());
            }
        }
        let digest = format!("{:x}", Sha256::digest(revision_input.as_bytes()));
        candidate.revision = format!("local-{}", &digest[..16]);
        candidate.activated_at = Utc::now().to_rfc3339();
    }
}

pub(crate) fn provider_pricing_semantically_equal(
    left: &ProviderScopedPricingSnapshot,
    right: &ProviderScopedPricingSnapshot,
) -> bool {
    left.provider_id == right.provider_id
        && left.document_updated_at == right.document_updated_at
        && left.source_url == right.source_url
        && left.content_hash == right.content_hash
        && left.evidence == right.evidence
        && left.values == right.values
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
        let tokens = BillingTokens::clamped(prompt, completion, cached, cache_creation);
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
                    .is_none_or(|minimum| tokens.input >= minimum)
                    && entry
                        .max_input_tokens
                        .is_none_or(|maximum| tokens.input <= maximum)
            })
            .collect::<Vec<_>>();
        let selected = select_priced_model(&candidates, at);
        let Some(price) = selected else {
            return PricingEstimate::unpriced(&self.revision);
        };

        // A '-' in the official Cached Write column means there is no separate
        // cache-write price. Cache creation is still new input, so it uses input.
        let cache_write = price.cache_write.unwrap_or(price.input);
        let Some(base) = token_charge(
            tokens,
            TokenRates::per_million(
                price.input,
                price.output,
                Some(price.cache_read),
                Some(cache_write),
            ),
        ) else {
            return PricingEstimate::unpriced(&self.revision);
        };

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
            if tokens.input > 512_000 {
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
        let Some(adjusted) = token_charge(
            tokens,
            TokenRates::per_million(
                adjusted_input,
                adjusted_output,
                Some(adjusted_cache_read),
                Some(adjusted_cache_write),
            ),
        ) else {
            return PricingEstimate::unpriced(&self.revision);
        };
        let local_adjustment_multiplier = if base > 0.0 { adjusted / base } else { 1.0 };

        let Some(quota_debit) = convert_charge(adjusted, price.quota_multiplier) else {
            return PricingEstimate::unpriced(&self.revision);
        };
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

/// Dashboard confirmation policy for an official pricing refresh. Kept in
/// one place so multiplier merge / confirmation matching stays identical.
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
        window_5h: parse_limit(&plain, &["5-hour limit", "5 hour limit"])?,
        window_week: parse_limit(&plain, &["Weekly limit"])?,
        window_month: parse_limit(&plain, &["Monthly limit"])?,
    };
    if limits.window_5h <= 0.0 || limits.window_week <= 0.0 || limits.window_month <= 0.0 {
        bail!("OpenCode Go usage limits must be positive");
    }
    let tables = extract_tables(html)?;
    let pricing_table = tables
        .iter()
        .find(|table| is_go_pricing_table(table))
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
        let usage = parse_usage_allowance(&row[5], &display_name)?;
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
    if covered.contains("grok-4.6") {
        validate_token_tiers(&models, "grok-4.6", 200_000)?;
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

fn select_provider_pricing_value<'a>(
    values: &'a [ProviderPricingValue],
    model: &str,
    prompt_tokens: i64,
    preferred_time_window: PricingTimeWindow,
    provider_id: &str,
) -> Option<&'a ProviderPricingValue> {
    let requested = provider_model_identities(model);
    let exact = values
        .iter()
        .filter(|value| {
            provider_value_identities(value)
                .iter()
                .any(|id| requested.contains(id))
        })
        .collect::<Vec<_>>();
    let matched = if !exact.is_empty() {
        exact
    } else if provider_id == OLLAMA_PROVIDER_ID {
        ollama_runtime_tag_matches(values, model)
    } else {
        let suffix = values
            .iter()
            .filter(|value| {
                provider_value_identities(value).iter().any(|value_id| {
                    requested.iter().any(|requested_id| {
                        value_id.len() >= 3
                            && requested_id.len() >= 3
                            && (value_id.ends_with(requested_id)
                                || requested_id.ends_with(value_id))
                    })
                })
            })
            .collect::<Vec<_>>();
        let model_groups = suffix
            .iter()
            .map(|value| canonical_display_name(value.display_name()))
            .collect::<HashSet<_>>();
        if model_groups.len() != 1 {
            return None;
        }
        suffix
    };
    let candidates = matched
        .into_iter()
        .filter(|value| {
            value
                .min_input_tokens()
                .is_none_or(|minimum| prompt_tokens >= minimum)
                && value
                    .max_input_tokens()
                    .is_none_or(|maximum| prompt_tokens <= maximum)
        })
        .collect::<Vec<_>>();
    select_provider_time_window(&candidates, preferred_time_window)
}

fn ollama_runtime_tag_matches<'a>(
    values: &'a [ProviderPricingValue],
    model: &str,
) -> Vec<&'a ProviderPricingValue> {
    let Some((stem, tag)) = model.rsplit_once(':') else {
        return Vec::new();
    };
    if stem.is_empty() || tag.is_empty() {
        return Vec::new();
    }
    let requested = provider_model_identities(stem);
    let matched = values
        .iter()
        .filter(|value| {
            provider_value_identities(value)
                .iter()
                .any(|id| requested.contains(id))
        })
        .collect::<Vec<_>>();
    if matched.len() == 1 {
        matched
    } else {
        Vec::new()
    }
}

fn provider_model_identities(model: &str) -> HashSet<String> {
    let mut identities = HashSet::new();
    identities.insert(canonical_display_name(model));
    if let Some(leaf) = model.trim().rsplit('/').next() {
        identities.insert(canonical_display_name(leaf));
    }
    identities.retain(|identity| !identity.is_empty());
    identities
}

fn provider_value_identities(value: &ProviderPricingValue) -> HashSet<String> {
    let mut identities = provider_model_identities(value.model_id());
    identities.insert(canonical_display_name(value.display_name()));
    identities.retain(|identity| !identity.is_empty());
    identities
}

fn select_provider_time_window<'a>(
    candidates: &[&'a ProviderPricingValue],
    preferred_time_window: PricingTimeWindow,
) -> Option<&'a ProviderPricingValue> {
    let scheduled = candidates
        .iter()
        .any(|entry| entry.time_window() != PricingTimeWindow::Always);
    if scheduled {
        return candidates
            .iter()
            .copied()
            .find(|entry| entry.time_window() == preferred_time_window)
            .or_else(|| {
                candidates
                    .iter()
                    .copied()
                    .find(|entry| entry.time_window() == PricingTimeWindow::Always)
            });
    }
    candidates.first().copied()
}

fn is_provider_peak_utc(provider_id: &str, at: DateTime<Utc>) -> bool {
    if provider_id == COMMAND_CODE_PROVIDER_ID {
        let minutes = at.hour() * 60 + at.minute();
        return (60..240).contains(&minutes) || (360..600).contains(&minutes);
    }
    provider_id == OPENCODE_PROVIDER_ID && is_official_peak_utc(at)
}

fn is_official_peak_utc(at: DateTime<Utc>) -> bool {
    // Official Go docs: DeepSeek Peak hours are 01:00-04:00 and 06:00-10:00 UTC,
    // Monday through Friday. Saturday and Sunday are off-peak all day.
    //
    // The weekday is read in UTC rather than on DeepSeek's Beijing clock. That is
    // sound only because both windows end at 10:00 UTC, well before 16:00 UTC --
    // the instant a UTC date and a Beijing date begin to disagree. If a window
    // ever extends past 16:00 UTC this needs to move to Asia/Shanghai.
    if at.weekday().number_from_monday() > 5 {
        return false;
    }
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
    // Official Go docs use ≤ / > token tiers (200K for Grok 4.6, 256K for Qwen,
    // 272K for Luna).
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

fn is_go_pricing_table(table: &[Vec<String>]) -> bool {
    // Official docs renamed the last column from Usage to Monthly limit.
    // Both headers describe the same per-model monthly allowance.
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
    ) || has_headers(
        table,
        &[
            "model",
            "input",
            "output",
            "cached read",
            "cached write",
            "monthly limit",
        ],
    )
}

fn parse_usage_allowance(value: &str, display_name: &str) -> Result<f64> {
    // Promo cells publish the base and current limits together, e.g.
    // "$15 $60 4x · Ends Sep 20". The larger dollar amount is the current
    // advertised monthly allowance used for the official multiplier.
    let usage = dollar_amounts(value)
        .into_iter()
        .max_by(|left, right| left.total_cmp(right))
        .ok_or_else(|| anyhow!("{display_name} is missing Usage"))?;
    if usage <= 0.0 {
        bail!("{display_name} Usage must be positive");
    }
    Ok(usage)
}

fn dollar_amounts(value: &str) -> Vec<f64> {
    let mut amounts = Vec::new();
    let mut remainder = value;
    while let Some(start) = remainder.find('$') {
        remainder = &remainder[start + 1..];
        let mut number = String::new();
        let mut consumed = 0;
        for character in remainder.chars() {
            if character.is_ascii_digit() || character == '.' {
                number.push(character);
                consumed += character.len_utf8();
            } else if character == ',' && !number.is_empty() {
                consumed += character.len_utf8();
            } else {
                break;
            }
        }
        remainder = remainder.get(consumed..).unwrap_or("");
        if let Ok(parsed) = number.parse::<f64>()
            && parsed.is_finite()
            && parsed >= 0.0
        {
            amounts.push(parsed);
        }
    }
    amounts
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

fn parse_limit(plain: &str, markers: &[&str]) -> Result<f64> {
    for marker in markers {
        if let Ok(value) = parse_limit_after(plain, marker) {
            return Ok(value);
        }
    }
    bail!(
        "OpenCode Go page is missing {}",
        markers.first().copied().unwrap_or("usage limit")
    )
}

fn parse_limit_after(plain: &str, marker: &str) -> Result<f64> {
    let start = plain
        .find(marker)
        .ok_or_else(|| anyhow!("OpenCode Go page is missing {marker}"))?;
    let tail = &plain[start + marker.len()..];
    let dollar = tail
        .find('$')
        .ok_or_else(|| anyhow!("OpenCode Go page is missing USD value after {marker}"))?;
    let after_dollar = &tail[dollar..];
    let value = after_dollar
        .split_whitespace()
        .next()
        .ok_or_else(|| anyhow!("OpenCode Go page is missing USD value after {marker}"))?;
    let parsed = parse_dollar(
        value.trim_end_matches(|c: char| !c.is_ascii_digit() && c != '.'),
        false,
    )?
    .ok_or_else(|| anyhow!("OpenCode Go page is missing USD value after {marker}"))?;
    // The worked example is "5-hour limit — $12 of usage". Requiring that
    // phrase keeps the table's "Monthly limit" header from being read as the
    // account-level window when it appears first in a future page layout.
    let after_value = after_dollar
        .get(value.len()..)
        .unwrap_or("")
        .trim_start()
        .to_ascii_lowercase();
    if !after_value.starts_with("of usage") {
        bail!("OpenCode Go page is missing USD value after {marker}");
    }
    Ok(parsed)
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

pub(crate) fn has_headers(table: &[Vec<String>], expected: &[&str]) -> bool {
    table.first().is_some_and(|row| {
        let actual = row
            .iter()
            .map(|cell| {
                cell.trim()
                    .trim_end_matches('↕')
                    .trim()
                    .to_ascii_lowercase()
            })
            .collect::<Vec<_>>();
        actual == expected
    })
}

pub(crate) fn extract_tables(html: &str) -> Result<Vec<Vec<Vec<String>>>> {
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

pub(crate) fn strip_tags(input: &str) -> String {
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

pub(crate) fn collapse_whitespace(input: &str) -> String {
    input.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests;
