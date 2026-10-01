//! Historical pricing snapshots, and the approved-host HTML fetch used by
//! protocol baselines.
//!
//! Live price parsers, refresh, multipliers, and cost estimates are not in
//! this module. `from_storage_record` still decodes a stored row, including
//! the v22 OpenCode Go snapshot JSON, and does not write that row back.

use anyhow::{Context, Result, anyhow, bail};
use chrono::{DateTime, Utc};
use futures_util::StreamExt;
use reqwest::redirect::{Attempt, Policy};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::time::Duration;

use crate::db::Database;
use crate::kernel::ids::OPENCODE_PROVIDER_ID;

pub use crate::kernel::ids::normalize_model_name;
use crate::kernel::pricing::ProviderPricingValueWire;
pub use crate::kernel::pricing::{
    PricingAdjustment, PricingEstimate, PricingLimits, PricingModel, PricingSnapshot,
    PricingTimeWindow, ProviderPricingEvidence, ProviderPricingSnapshot, ProviderPricingValue,
    SEED_LIMITS, SOURCE_URL,
};

pub const GOAT_SOURCE_URL: &str = "https://commandcode.ai/docs/plans/goat";
const MAX_DOCUMENT_BYTES: usize = 2 * 1024 * 1024;
const MAX_APPROVED_HOST_REDIRECTS: usize = 5;

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

    /// Decode a v22 OpenCode Go snapshot value. The stored JSON is not rewritten.
    fn from_opencode_go(snapshot: &PricingSnapshot) -> Result<Self> {
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

fn approved_https_host(url: &reqwest::Url, host: &str) -> bool {
    url.scheme() == "https"
        && url.host_str() == Some(host)
        && url.port_or_known_default() == Some(443)
}

fn same_approved_host_redirect(
    attempt: Attempt<'_>,
    host: &str,
    label: &str,
) -> reqwest::redirect::Action {
    if attempt.previous().len() >= MAX_APPROVED_HOST_REDIRECTS {
        return attempt.error(format!("too many {label} redirects"));
    }
    if approved_https_host(attempt.url(), host) {
        attempt.follow()
    } else {
        attempt.error(format!("{label} redirect left the approved HTTPS host"))
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
) -> Result<Option<ProviderScopedPricingSnapshot>> {
    db.latest_provider_pricing_snapshot(provider_id)?
        .as_ref()
        .map(ProviderScopedPricingSnapshot::from_storage_record)
        .transpose()
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
