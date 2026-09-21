//! Personal-account balances and idempotent request receipts. Callers own transactions.

use anyhow::{Result, ensure};
use chrono::{DateTime, Utc};
use ocg_domain::billing::BillingTokens;
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};

use crate::billing::{CreditAttempt, CreditMeterState};
use crate::billing_types::{
    CreditBalanceCorrection, CreditBucket, CreditConfiguration, CreditMeterView,
    PortableCreditMeter,
};

struct Binding {
    credential_id: String,
    destination_id: String,
    endpoint: String,
    json: Option<String>,
}

fn binding_on(conn: &Connection, account_id: &str) -> Result<Option<Binding>> {
    Ok(conn
        .query_row(
            "SELECT c.id, c.destination_id, d.base_url, c.credit_meter_json
         FROM credentials c JOIN destinations d ON d.id=c.destination_id
         WHERE c.legacy_account_id=?1 AND d.adapter='http'
           AND d.legacy_kind IN ('custom_account','dynamic')
           AND COALESCE(c.credential_purpose,'inference')='inference'",
            [account_id],
            |row| {
                Ok(Binding {
                    credential_id: row.get(0)?,
                    destination_id: row.get(1)?,
                    endpoint: row.get(2)?,
                    json: row.get(3)?,
                })
            },
        )
        .optional()?)
}

pub(crate) fn load_on(conn: &Connection, account_id: &str) -> Result<Option<CreditMeterState>> {
    let Some(binding) = binding_on(conn, account_id)? else {
        return Ok(None);
    };
    let Some(raw) = binding.json else {
        return Ok(None);
    };
    ensure!(raw.len() <= 512 * 1024, "credit meter is too large");
    let state: CreditMeterState = serde_json::from_str(&raw)?;
    state.validate()?;
    Ok((state.credential_id == binding.credential_id
        && state.destination_id == binding.destination_id
        && state.endpoint == binding.endpoint)
        .then_some(state))
}

pub(crate) fn save_on(conn: &Connection, state: &CreditMeterState) -> Result<()> {
    state.validate()?;
    let raw = serde_json::to_string(state)?;
    ensure!(raw.len() <= 512 * 1024, "credit meter is too large");
    let count = conn.execute(
        "UPDATE credentials SET credit_meter_json=?1 WHERE id=?2 AND destination_id=?3
         AND EXISTS (SELECT 1 FROM destinations d WHERE d.id=?3 AND d.base_url=?4)",
        params![
            raw,
            state.credential_id,
            state.destination_id,
            state.endpoint
        ],
    )?;
    ensure!(count == 1, "credit account binding changed");
    Ok(())
}

pub(crate) fn configure_on(
    conn: &Connection,
    account_id: &str,
    configuration: CreditConfiguration,
    initial_buckets: Option<Vec<CreditBucket>>,
    now: DateTime<Utc>,
) -> Result<()> {
    let binding = binding_on(conn, account_id)?
        .ok_or_else(|| anyhow::anyhow!("account cannot use personal credits"))?;
    let state = if let Some(mut state) = load_on(conn, account_id)? {
        ensure!(
            initial_buckets.is_none(),
            "existing balances must be calibrated separately"
        );
        state.advance(now)?;
        state.configure(configuration, now)?;
        state
    } else {
        let buckets = initial_buckets
            .ok_or_else(|| anyhow::anyhow!("initial credit buckets are required"))?;
        let mut state = CreditMeterState::new(
            uuid::Uuid::new_v4().to_string(),
            binding.credential_id,
            binding.destination_id,
            binding.endpoint,
            configuration,
            buckets,
            now,
        )?;
        state.advance(now)?;
        state
    };
    save_on(conn, &state)
}

pub(crate) fn calibrate_on(
    conn: &Connection,
    account_id: &str,
    balances: &[CreditBalanceCorrection],
    now: DateTime<Utc>,
) -> Result<()> {
    let mut state = required_on(conn, account_id)?;
    ensure!(
        pending_count_on(conn, &state.credential_id, &state.meter_id)? == 0,
        "wait for pending requests before calibrating credits"
    );
    state.advance(now)?;
    state.calibrate(balances, now)?;
    save_on(conn, &state)
}

pub(crate) fn grant_on(
    conn: &Connection,
    account_id: &str,
    label: String,
    amount: f64,
    expires_at: Option<DateTime<Utc>>,
    now: DateTime<Utc>,
) -> Result<()> {
    let mut state = required_on(conn, account_id)?;
    state.add_grant(label, amount, expires_at, now)?;
    save_on(conn, &state)
}

pub(crate) fn disable_on(conn: &Connection, account_id: &str, _now: DateTime<Utc>) -> Result<()> {
    conn.execute(
        "UPDATE credentials SET credit_meter_json=NULL WHERE legacy_account_id=?1",
        [account_id],
    )?;
    Ok(())
}

fn required_on(conn: &Connection, account_id: &str) -> Result<CreditMeterState> {
    load_on(conn, account_id)?.ok_or_else(|| anyhow::anyhow!("credit meter is not configured"))
}

pub(crate) fn read_view_on(
    conn: &Connection,
    account_id: &str,
    now: DateTime<Utc>,
) -> Result<Option<CreditMeterView>> {
    let Some(mut state) = load_on(conn, account_id)? else {
        return Ok(None);
    };
    state.advance(now)?;
    let pending = pending_count_on(conn, &state.credential_id, &state.meter_id)?;
    Ok(Some(state.project(now, pending)))
}

pub(crate) fn capture_on(
    conn: &Connection,
    account_id: &str,
    endpoint: &str,
    model: &str,
    at: DateTime<Utc>,
) -> Result<Option<CreditAttempt>> {
    let Some(state) = load_on(conn, account_id)? else {
        return Ok(None);
    };
    if state.endpoint != endpoint {
        return Ok(None);
    }
    Ok(Some(state.capture_attempt(
        account_id.to_string(),
        model,
        at,
    )))
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Receipt {
    attempt: CreditAttempt,
    phase: String,
    amount: Option<f64>,
    uncertain: bool,
    finished_at: Option<DateTime<Utc>>,
}

pub(crate) fn attach_attempt_on(
    conn: &Connection,
    log_id: i64,
    attempt: &CreditAttempt,
) -> Result<()> {
    let receipt = Receipt {
        attempt: attempt.clone(),
        phase: "pending".into(),
        amount: None,
        uncertain: false,
        finished_at: None,
    };
    conn.execute("UPDATE forward_logs SET credit_receipt_json=?1 WHERE id=?2 AND account_id=?3 AND credit_receipt_json IS NULL",
        params![serde_json::to_string(&receipt)?, log_id, attempt.account_id])?;
    Ok(())
}

fn receipt_on(conn: &Connection, log_id: i64) -> Result<Option<(String, Receipt)>> {
    let row: Option<(String, String)> = conn.query_row(
        "SELECT account_id, credit_receipt_json FROM forward_logs WHERE id=?1 AND credit_receipt_json IS NOT NULL",
        [log_id], |row| Ok((row.get(0)?, row.get(1)?)),
    ).optional()?;
    row.map(|(id, raw)| Ok((id, serde_json::from_str(&raw)?)))
        .transpose()
}

pub(crate) fn settlement_finished_on(conn: &Connection, log_id: i64) -> Result<bool> {
    Ok(receipt_on(conn, log_id)?.is_none_or(|(_, receipt)| receipt.phase != "pending"))
}

pub(crate) fn pending_count_on(
    conn: &Connection,
    credential_id: &str,
    meter_id: &str,
) -> Result<u64> {
    Ok(conn.query_row(
        "SELECT COUNT(*) FROM forward_logs WHERE credit_receipt_json IS NOT NULL
         AND json_extract(credit_receipt_json,'$.phase')='pending'
         AND json_extract(credit_receipt_json,'$.attempt.credentialId')=?1
         AND json_extract(credit_receipt_json,'$.attempt.meterId')=?2",
        params![credential_id, meter_id],
        |row| row.get(0),
    )?)
}

pub(crate) fn settle_on(
    conn: &Connection,
    log_id: i64,
    attempt: &CreditAttempt,
    tokens: BillingTokens,
    status: &str,
    at: DateTime<Utc>,
) -> Result<()> {
    if status == "streaming" {
        return Ok(());
    }
    let Some((account_id, mut receipt)) = receipt_on(conn, log_id)? else {
        return Ok(());
    };
    if receipt.phase != "pending" || receipt.attempt != *attempt || account_id != attempt.account_id
    {
        return Ok(());
    }
    let mut state = load_on(conn, &account_id)?;
    let matching = state.as_ref().is_some_and(|state| {
        state.meter_id == attempt.meter_id
            && state.credential_id == attempt.credential_id
            && state.destination_id == attempt.destination_id
            && state.endpoint == attempt.endpoint
    });
    receipt.phase = if matching { "settled" } else { "ignored" }.into();
    receipt.finished_at = Some(at);
    if matching {
        let state = state.as_mut().unwrap();
        let rejected = matches!(status, "error" | "client_error");
        let amount = if status.starts_with("success") && status != "success_no_usage" {
            attempt.charge(tokens)
        } else {
            None
        };
        if let Some(amount) = amount {
            state.deduct(amount, at)?;
            receipt.amount = Some(amount);
        } else if rejected {
            receipt.amount = Some(0.0);
        } else {
            state.unpriced_requests = state
                .unpriced_requests
                .checked_add(1)
                .ok_or_else(|| anyhow::anyhow!("unpriced request count overflow"))?;
            receipt.uncertain = true;
        }
        save_on(conn, state)?;
    }
    conn.execute("UPDATE forward_logs SET credit_receipt_json=?1, native_cost_value=?2,
        native_cost_unit=CASE WHEN ?2 IS NULL THEN NULL ELSE 'credits' END, native_cost_currency=NULL WHERE id=?3",
        params![serde_json::to_string(&receipt)?, receipt.amount, log_id])?;
    Ok(())
}

/// Cold startup only, under the directory's exclusive lifetime lock and a
/// transaction: interrupted requests become explicit uncertainty exactly once.
pub(crate) fn recover_pending_on(conn: &Connection, now: DateTime<Utc>) -> Result<()> {
    let has_pending: bool = conn.query_row("SELECT EXISTS(SELECT 1 FROM forward_logs WHERE credit_receipt_json IS NOT NULL AND json_extract(credit_receipt_json,'$.phase')='pending')", [], |row| row.get(0))?;
    if !has_pending {
        return Ok(());
    }
    let mut statement = conn.prepare("SELECT id FROM forward_logs WHERE credit_receipt_json IS NOT NULL AND json_extract(credit_receipt_json,'$.phase')='pending'")?;
    let ids = statement
        .query_map([], |row| row.get::<_, i64>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    drop(statement);
    for id in ids {
        if let Some((_, receipt)) = receipt_on(conn, id)? {
            settle_on(
                conn,
                id,
                &receipt.attempt,
                BillingTokens::new(0, 0, 0, 0),
                "outcome_unknown",
                now,
            )?;
        }
    }
    Ok(())
}

pub(crate) fn export_on(
    conn: &Connection,
    account_id: &str,
    now: DateTime<Utc>,
) -> Result<Option<PortableCreditMeter>> {
    let Some(mut state) = load_on(conn, account_id)? else {
        return Ok(None);
    };
    state.advance(now)?;
    let pending = pending_count_on(conn, &state.credential_id, &state.meter_id)?;
    Ok(Some(PortableCreditMeter {
        configuration: state.configuration,
        buckets: state.buckets,
        spent_since_calibration: state.spent_since_calibration,
        overdrawn: state.overdrawn,
        unpriced_requests: state.unpriced_requests.saturating_add(pending),
        last_calibration_at: state.last_calibration_at,
        created_at: state.created_at,
        monthly_cursor: state.monthly_cursor,
        exported_at: now,
    }))
}

pub(crate) fn import_on(
    conn: &Connection,
    account_id: &str,
    portable: &PortableCreditMeter,
    now: DateTime<Utc>,
) -> Result<()> {
    let binding = binding_on(conn, account_id)?
        .ok_or_else(|| anyhow::anyhow!("account cannot import credits"))?;
    if binding.json.is_some() {
        return Ok(());
    }
    let mut state = CreditMeterState::new(
        uuid::Uuid::new_v4().to_string(),
        binding.credential_id,
        binding.destination_id,
        binding.endpoint,
        portable.configuration.clone(),
        portable.buckets.clone(),
        portable.created_at,
    )?;
    state.spent_since_calibration = portable.spent_since_calibration;
    state.overdrawn = portable.overdrawn;
    state.unpriced_requests = portable.unpriced_requests;
    state.last_calibration_at = portable.last_calibration_at;
    state.monthly_cursor = portable.monthly_cursor;
    state.validate()?;
    state.advance(now)?;
    save_on(conn, &state)
}

#[cfg(test)]
mod tests;
