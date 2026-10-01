//! Stored official price-sheet checks. Parse and fetch are not in this module.
//! `seed` remains so crate tests can build a reviewed DeepSeek or Zhipu sheet.
use super::*;
use anyhow::{Result, ensure};
use std::collections::BTreeSet;

#[cfg(test)]
pub(crate) fn seed(kind: OfficialApiKind) -> OfficialPriceSheet {
    let at = DateTime::parse_from_rfc3339("2026-09-17T00:00:00Z")
        .expect("constant date")
        .with_timezone(&Utc);
    let mut rows = Vec::new();
    match kind {
        OfficialApiKind::Deepseek => {
            for (model, input, output, cache) in [
                ("deepseek-flash", 0.3, 1.2, 0.006),
                ("deepseek-v4-flash", 0.3, 1.2, 0.006),
                ("deepseek-v4-flash-vision-exp", 0.3, 1.2, 0.006),
                ("deepseek-v4-pro", 1.32, 3.96, 0.044),
            ] {
                for (period, factor) in [("peak", 1.0), ("off_peak", 0.5)] {
                    rows.push(row(
                        model,
                        "USD",
                        period,
                        input * factor,
                        output * factor,
                        Some(cache * factor),
                    ));
                }
            }
        }
        OfficialApiKind::Zhipu => {
            for (model, input, output, cache) in [
                ("glm-5.3", 8.0, 28.0, 2.0),
                ("glm-5.3-flash", 0.8, 2.8, 0.23),
                ("glm-5.2", 8.0, 28.0, 2.0),
                ("glm-4.7-flashx", 0.5, 3.0, 0.1),
                ("glm-4.7-flash", 0.0, 0.0, 0.0),
            ] {
                rows.push(row(model, "CNY", "all", input, output, Some(cache)));
            }
        }
    }
    finish(kind, rows, at).expect("reviewed seed is valid")
}

#[cfg(test)]
fn row(
    model: &str,
    currency: &str,
    period: &str,
    input: f64,
    output: f64,
    cache: Option<f64>,
) -> OfficialPriceRow {
    OfficialPriceRow {
        model: model.into(),
        currency: currency.into(),
        period: period.into(),
        input_per_million: input,
        output_per_million: output,
        cache_read_per_million: cache,
    }
}

#[cfg(test)]
fn finish(
    kind: OfficialApiKind,
    rows: Vec<OfficialPriceRow>,
    now: DateTime<Utc>,
) -> Result<OfficialPriceSheet> {
    ensure!(
        !rows.is_empty() && rows.len() <= 256,
        "official price rows are missing or too numerous"
    );
    let encoded = serde_json::to_vec(&(now, &rows))?;
    let revision = format!(
        "official-api:{}:{}",
        kind.id(),
        hex::encode(Sha256::digest(encoded))
    );
    let sheet = OfficialPriceSheet {
        kind,
        revision,
        source_url: kind.pricing_url().into(),
        observed_at: now,
        valid_until: now + chrono::Duration::days(PRICE_MAX_AGE_DAYS),
        rows,
    };
    validate(&sheet)?;
    Ok(sheet)
}

pub(crate) fn validate(sheet: &OfficialPriceSheet) -> Result<()> {
    let revision = format!(
        "official-api:{}:{}",
        sheet.kind.id(),
        hex::encode(Sha256::digest(serde_json::to_vec(&(
            sheet.observed_at,
            &sheet.rows
        ))?))
    );
    ensure!(
        sheet.source_url == sheet.kind.pricing_url() && sheet.revision == revision,
        "invalid price source or revision"
    );
    ensure!(
        sheet.valid_until > sheet.observed_at
            && sheet.valid_until - sheet.observed_at <= chrono::Duration::days(PRICE_MAX_AGE_DAYS),
        "invalid price lifetime"
    );
    ensure!(
        !sheet.rows.is_empty() && sheet.rows.len() <= 256,
        "invalid price count"
    );
    let mut identities = BTreeSet::new();
    for r in &sheet.rows {
        ensure!(
            r.model.len() <= 128
                && !r.model.is_empty()
                && r.model
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.')),
            "invalid price model"
        );
        ensure!(
            r.currency == sheet.kind.currency(),
            "price currency mismatch"
        );
        ensure!(
            match sheet.kind {
                OfficialApiKind::Deepseek => matches!(r.period.as_str(), "peak" | "off_peak"),
                OfficialApiKind::Zhipu => r.period == "all",
            },
            "invalid price period"
        );
        ensure!(
            identities.insert((&r.model, &r.period)),
            "duplicate price row"
        );
        for v in [
            Some(r.input_per_million),
            Some(r.output_per_million),
            r.cache_read_per_million,
        ]
        .into_iter()
        .flatten()
        {
            ensure!(
                v.is_finite() && (0.0..=100_000.0).contains(&v),
                "invalid price amount"
            );
        }
    }
    if sheet.kind == OfficialApiKind::Deepseek {
        for r in &sheet.rows {
            ensure!(
                sheet
                    .rows
                    .iter()
                    .filter(|other| other.model == r.model)
                    .count()
                    == 2,
                "incomplete peak/off-peak pair"
            );
        }
    }
    Ok(())
}
