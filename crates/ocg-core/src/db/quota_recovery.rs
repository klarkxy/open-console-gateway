//! Per-credential quota recovery persistence. No pool fan-out.

use super::table_has_column;
use crate::quota_recovery::{PersistedQuotaRecovery, QuotaEpisode};
use crate::routing_snapshot::ExecutionCredential;
use anyhow::Result;
use chrono::{DateTime, Utc};
use ocg_gateway::quota::QuotaEvidence;
use rusqlite::{Connection, OptionalExtension, params};
use std::collections::HashMap;

pub(crate) fn ensure_column(conn: &Connection) -> Result<()> {
    if !table_has_column(conn, "credentials", "quota_recovery_json")? {
        conn.execute_batch("ALTER TABLE credentials ADD COLUMN quota_recovery_json TEXT;")?;
    }
    Ok(())
}

pub(crate) fn load_on(
    conn: &Connection,
    credential_id: &str,
) -> Result<Option<PersistedQuotaRecovery>> {
    if !table_has_column(conn, "credentials", "quota_recovery_json")? {
        return Ok(None);
    }
    let json: Option<String> = conn
        .query_row(
            "SELECT quota_recovery_json FROM credentials WHERE id = ?1",
            [credential_id],
            |row| row.get(0),
        )
        .optional()?
        .flatten();
    parse_json(json)
}

#[allow(clippy::type_complexity)]
pub(crate) fn load_for_legacy_on(
    conn: &Connection,
    account_id: &str,
) -> Result<Option<(String, u64, String, Option<PersistedQuotaRecovery>)>> {
    if !table_has_column(conn, "credentials", "quota_recovery_json")? {
        return Ok(None);
    }
    let row: Option<(String, i64, String, Option<String>)> = conn
        .query_row(
            "SELECT id, COALESCE(credential_version, 1), key_cipher, quota_recovery_json
             FROM credentials
             WHERE legacy_account_id = ?1 AND COALESCE(credential_purpose, 'inference') = 'inference'",
            [account_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .optional()?;
    let Some((id, version, key_cipher, json)) = row else {
        return Ok(None);
    };
    Ok(Some((id, version as u64, key_cipher, parse_json(json)?)))
}

pub(crate) struct QuotaRecoveryRow {
    pub credential_version: u64,
    pub key_cipher: String,
    pub recovery: PersistedQuotaRecovery,
}

#[allow(dead_code)]
pub(crate) fn load_all_on(conn: &Connection) -> Result<HashMap<String, PersistedQuotaRecovery>> {
    Ok(load_all_identified_on(conn)?
        .into_iter()
        .map(|(id, row)| (id, row.recovery))
        .collect())
}

pub(crate) fn load_all_identified_on(
    conn: &Connection,
) -> Result<HashMap<String, QuotaRecoveryRow>> {
    let mut map = HashMap::new();
    if !table_has_column(conn, "credentials", "quota_recovery_json")? {
        return Ok(map);
    }
    let mut stmt = conn.prepare(
        "SELECT id, COALESCE(credential_version, 1), key_cipher, quota_recovery_json
         FROM credentials
         WHERE quota_recovery_json IS NOT NULL
           AND COALESCE(credential_purpose, 'inference') = 'inference'",
    )?;
    let rows = stmt.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, i64>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, Option<String>>(3)?,
        ))
    })?;
    for row in rows {
        let (id, version, key_cipher, json) = row?;
        if let Some(recovery) = parse_json(json)? {
            map.insert(
                id,
                QuotaRecoveryRow {
                    credential_version: version as u64,
                    key_cipher,
                    recovery,
                },
            );
        }
    }
    Ok(map)
}

pub(crate) fn save_on(
    conn: &Connection,
    episode: &QuotaEpisode,
    recovery: &PersistedQuotaRecovery,
) -> Result<bool> {
    if !table_has_column(conn, "credentials", "quota_recovery_json")? {
        return Ok(false);
    }
    let json = serde_json::to_string(recovery)?;
    let updated = conn.execute(
        "UPDATE credentials
         SET quota_recovery_json = ?2, updated_at = ?3
         WHERE id = ?1
           AND COALESCE(credential_version, 1) = ?4
           AND key_cipher = ?5",
        params![
            episode.credential_id,
            json,
            Utc::now().to_rfc3339(),
            episode.credential_version as i64,
            episode.key_cipher,
        ],
    )?;
    Ok(updated > 0)
}

pub(crate) fn record_evidence_on(
    conn: &Connection,
    credential: &ExecutionCredential,
    evidence: &QuotaEvidence,
    episode: Option<&QuotaEpisode>,
    now: DateTime<Utc>,
) -> Result<bool> {
    let previous = load_on(conn, &credential.credential_id)?;
    let next = PersistedQuotaRecovery::from_evidence(previous.as_ref(), evidence, now, episode);
    let episode = episode.cloned().unwrap_or(QuotaEpisode {
        credential_id: credential.credential_id.clone(),
        account_id: credential.id.clone(),
        credential_version: credential.credential_version,
        epoch: next.epoch,
        key_cipher: credential.key_cipher.clone(),
    });
    save_on(conn, &episode, &next)
}

/// Called under the usage coordinator's captured identity check and DB lock.
/// A complete official snapshot replaces previous quota evidence, including
/// healthy windows; the separate short 429 backoff is deliberately untouched.
pub(crate) fn reconcile_official_go_usage(
    conn: &Connection,
    account_id: &str,
    snapshot: &crate::go_usage::GoUsageSnapshot,
    now: DateTime<Utc>,
) -> Result<()> {
    let evidence = crate::usage_sync::official_go_quota_evidence(snapshot, now);
    let Some((credential_id, version, key_cipher, previous)) =
        load_for_legacy_on(conn, account_id)?
    else {
        return Ok(());
    };
    let mut next = None;
    for item in evidence {
        next = Some(PersistedQuotaRecovery::from_evidence(
            next.as_ref(),
            &item,
            now,
            None,
        ));
    }
    let Some(mut next) = next else {
        return clear_for_account_on(conn, account_id);
    };
    next.epoch = previous
        .as_ref()
        .map_or(1, |row| row.epoch.saturating_add(1));
    save_on(
        conn,
        &QuotaEpisode {
            credential_id,
            account_id: account_id.into(),
            credential_version: version,
            epoch: next.epoch,
            key_cipher,
        },
        &next,
    )?;
    Ok(())
}

pub(crate) fn release_nonquota_trial_on(
    conn: &Connection,
    episode: &QuotaEpisode,
    now: DateTime<Utc>,
) -> Result<bool> {
    let Some(current) = load_on(conn, &episode.credential_id)? else {
        return Ok(false);
    };
    if current.epoch != episode.epoch {
        return Ok(false);
    }
    let next = current.with_nonquota_trial_failure(now);
    save_on(conn, episode, &next)
}

pub(crate) fn clear_matching_on(conn: &Connection, episode: &QuotaEpisode) -> Result<bool> {
    if !table_has_column(conn, "credentials", "quota_recovery_json")? {
        return Ok(false);
    }
    let current = load_on(conn, &episode.credential_id)?;
    let Some(current) = current else {
        return Ok(false);
    };
    if current.epoch != episode.epoch {
        return Ok(false);
    }
    let updated = conn.execute(
        "UPDATE credentials
         SET quota_recovery_json = NULL, updated_at = ?2
         WHERE id = ?1
           AND COALESCE(credential_version, 1) = ?3
           AND key_cipher = ?4",
        params![
            episode.credential_id,
            Utc::now().to_rfc3339(),
            episode.credential_version as i64,
            episode.key_cipher,
        ],
    )?;
    Ok(updated > 0)
}

pub(crate) fn clear_for_account_on(conn: &Connection, account_id: &str) -> Result<()> {
    if !table_has_column(conn, "credentials", "quota_recovery_json")? {
        return Ok(());
    }
    conn.execute(
        "UPDATE credentials SET quota_recovery_json = NULL
         WHERE legacy_account_id = ?1",
        [account_id],
    )?;
    Ok(())
}

pub(crate) fn snapshot_on(conn: &Connection) -> Result<HashMap<String, Option<String>>> {
    let mut map = HashMap::new();
    if !table_has_column(conn, "credentials", "quota_recovery_json")? {
        return Ok(map);
    }
    let mut stmt = conn.prepare(
        "SELECT id, quota_recovery_json FROM credentials WHERE quota_recovery_json IS NOT NULL",
    )?;
    let rows = stmt.query_map([], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?))
    })?;
    for row in rows {
        let (id, json) = row?;
        map.insert(id, json);
    }
    Ok(map)
}

pub(crate) fn restore_on(conn: &Connection, rows: &HashMap<String, Option<String>>) -> Result<()> {
    if rows.is_empty() || !table_has_column(conn, "credentials", "quota_recovery_json")? {
        return Ok(());
    }
    for (id, json) in rows {
        conn.execute(
            "UPDATE credentials SET quota_recovery_json = ?2 WHERE id = ?1",
            params![id, json],
        )?;
    }
    Ok(())
}

fn parse_json(json: Option<String>) -> Result<Option<PersistedQuotaRecovery>> {
    let Some(json) = json.filter(|value| !value.trim().is_empty()) else {
        return Ok(None);
    };
    Ok(Some(serde_json::from_str(&json)?))
}

#[cfg(test)]
mod tests;
