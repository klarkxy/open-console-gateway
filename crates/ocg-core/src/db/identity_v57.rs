//! Schema v57: fold leftover identity satellites onto credentials +
//! `credential_grants`, then drop the leftover tables.
//!
//! `quota_pools` / `quota_pool_members` stay. `legacy_identity_map` lookups
//! become reserved-id helpers / `credentials.identity_id` / destination
//! legacy ids. Fresh databases do not invent identities.

use super::*;
use ocg_domain::credential::{
    identity_id_for_platform_account, onboarding_task_id_for_legacy_account,
};
use std::collections::HashSet;

pub(crate) const IDENTITY_LEFTOVER_TABLES: &[&str] = &[
    "upstream_identities",
    "credential_state",
    "credential_bindings",
    "legacy_identity_map",
    "onboarding_tasks",
    "subscription_records",
];

pub(crate) const V57_CREDENTIAL_COLUMNS: &[(&str, &str)] = &[
    ("identity_confidence", "TEXT"),
    ("authority_site", "TEXT"),
    ("authority_subject", "TEXT"),
    ("identity_enabled", "INTEGER"),
    ("identity_label", "TEXT"),
    ("identity_notes", "TEXT"),
    ("credential_version", "INTEGER"),
    ("auth_state_version", "INTEGER"),
    ("rotated_at", "TEXT"),
    ("binding_id", "TEXT"),
    ("binding_enabled", "INTEGER"),
    ("subscription_source", "TEXT"),
    ("subscription_expires_on", "TEXT"),
    ("grants_initialized", "INTEGER"),
];

const GRANT_ENDPOINT: &str = "endpoint_id";
const GRANT_ORIGIN: &str = "origin";

pub(crate) fn leftover_identity_tables_present(conn: &Connection) -> Result<bool> {
    table_exists(conn, "upstream_identities")
}

pub(crate) fn identity_facts_on_credentials(conn: &Connection) -> Result<bool> {
    table_has_column(conn, "credentials", "binding_id")
}

pub(crate) fn drop_leftover_identity_tables(conn: &Connection) -> Result<()> {
    let sql = IDENTITY_LEFTOVER_TABLES
        .iter()
        .rev()
        .map(|table| format!("DROP TABLE IF EXISTS {table};"))
        .collect::<Vec<_>>()
        .join("\n");
    conn.execute_batch(&sql)?;
    Ok(())
}

/// v57: credentials + `credential_grants` become the identity / binding /
/// onboarding / subscription store. Copy leftover satellites when they can
/// map, refuse unmappable leftovers, then drop the six leftover tables.
/// Does not drop `quota_pools`, `quota_pool_members`, `credential_grants`,
/// `provider_model_catalogs`, or `dashboard_operations`.
pub(crate) fn migrate_v57_backfill_and_drop(conn: &Connection) -> Result<()> {
    ensure_v57_columns(conn)?;
    if leftover_identity_tables_present(conn)? {
        assert_leftover_identity_totality(conn)?;
        copy_leftover_identity_satellites(conn)?;
    }
    drop_leftover_identity_tables(conn)?;
    Ok(())
}

pub(crate) fn ensure_v57_columns(conn: &Connection) -> Result<()> {
    if !table_exists(conn, "credentials")? {
        return Ok(());
    }
    for (column, definition) in V57_CREDENTIAL_COLUMNS {
        if !table_has_column(conn, "credentials", column)? {
            conn.execute(
                &format!("ALTER TABLE credentials ADD COLUMN {column} {definition}"),
                [],
            )?;
        }
    }
    Ok(())
}

fn credential_legacy_ids(conn: &Connection) -> Result<HashSet<String>> {
    if !table_exists(conn, "credentials")? {
        return Ok(HashSet::new());
    }
    let mut stmt = conn.prepare("SELECT legacy_account_id FROM credentials")?;
    let rows = stmt
        .query_map([], |row| row.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows.into_iter().collect())
}

fn credential_identity_ids(conn: &Connection) -> Result<HashSet<String>> {
    if !table_exists(conn, "credentials")? {
        return Ok(HashSet::new());
    }
    let mut stmt =
        conn.prepare("SELECT identity_id FROM credentials WHERE identity_id IS NOT NULL")?;
    let rows = stmt
        .query_map([], |row| row.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows.into_iter().collect())
}

fn platform_destination_identity_ids(conn: &Connection) -> Result<HashSet<String>> {
    if !table_exists(conn, "destinations")? {
        return Ok(HashSet::new());
    }
    let mut stmt =
        conn.prepare("SELECT legacy_id FROM destinations WHERE legacy_kind = 'platform_parent'")?;
    let rows = stmt
        .query_map([], |row| row.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows
        .into_iter()
        .map(|id| identity_id_for_platform_account(&id).to_string())
        .collect())
}

fn leftover_account_ids(conn: &Connection, table: &str, column: &str) -> Result<Vec<String>> {
    if !table_exists(conn, table)? {
        return Ok(Vec::new());
    }
    let mut stmt = conn.prepare(&format!("SELECT DISTINCT {column} FROM {table}"))?;
    let rows = stmt
        .query_map([], |row| row.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

fn assert_leftover_identity_totality(conn: &Connection) -> Result<()> {
    let credential_ids = credential_legacy_ids(conn)?;
    let mapped_identities = {
        let mut ids = credential_identity_ids(conn)?;
        ids.extend(platform_destination_identity_ids(conn)?);
        ids
    };

    if table_exists(conn, "upstream_identities")? {
        let mut stmt = conn.prepare("SELECT id FROM upstream_identities")?;
        let leftover_identities = stmt
            .query_map([], |row| row.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        for id in leftover_identities {
            anyhow::ensure!(
                mapped_identities.contains(&id),
                "v57 refuses leftover identity `{id}`: no reconstructible credential or platform destination"
            );
        }
    }

    for (table, column) in [
        ("credential_state", "account_id"),
        ("credential_bindings", "account_id"),
        ("onboarding_tasks", "account_id"),
        ("subscription_records", "account_id"),
    ] {
        for account_id in leftover_account_ids(conn, table, column)? {
            anyhow::ensure!(
                credential_ids.contains(&account_id),
                "v57 refuses leftover {table} for `{account_id}`: no reconstructible credential"
            );
        }
    }
    Ok(())
}

fn copy_leftover_identity_satellites(conn: &Connection) -> Result<()> {
    copy_leftover_identities(conn)?;
    copy_leftover_credential_state(conn)?;
    copy_leftover_bindings_and_grants(conn)?;
    copy_leftover_onboarding(conn)?;
    copy_leftover_subscriptions(conn)?;
    Ok(())
}

fn copy_leftover_identities(conn: &Connection) -> Result<()> {
    if !table_exists(conn, "upstream_identities")? {
        return Ok(());
    }
    let mut stmt = conn.prepare(
        "SELECT id, label, identity_confidence, authority_site, authority_subject,
                enabled, notes
         FROM upstream_identities",
    )?;
    let rows = stmt
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, Option<String>>(3)?,
                row.get::<_, Option<String>>(4)?,
                row.get::<_, i32>(5)?,
                row.get::<_, Option<String>>(6)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    drop(stmt);
    for (id, label, confidence, site, subject, enabled, notes) in rows {
        conn.execute(
            "UPDATE credentials SET
                identity_label = COALESCE(identity_label, ?2),
                identity_confidence = COALESCE(identity_confidence, ?3),
                authority_site = COALESCE(authority_site, ?4),
                authority_subject = COALESCE(authority_subject, ?5),
                identity_enabled = COALESCE(identity_enabled, ?6),
                identity_notes = COALESCE(identity_notes, ?7)
             WHERE identity_id = ?1",
            params![id, label, confidence, site, subject, enabled, notes],
        )?;
    }
    Ok(())
}

fn copy_leftover_credential_state(conn: &Connection) -> Result<()> {
    if !table_exists(conn, "credential_state")? {
        return Ok(());
    }
    let mut stmt = conn.prepare(
        "SELECT account_id, credential_id, version, auth_state_version, rotated_at
         FROM credential_state",
    )?;
    let rows = stmt
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, i64>(3)?,
                row.get::<_, Option<String>>(4)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    drop(stmt);
    for (account_id, leftover_credential_id, version, auth_state_version, rotated_at) in rows {
        let current_id: String = conn.query_row(
            "SELECT id FROM credentials WHERE legacy_account_id = ?1",
            [&account_id],
            |row| row.get(0),
        )?;
        if leftover_credential_id != current_id {
            let taken: i64 = conn.query_row(
                "SELECT COUNT(*) FROM credentials WHERE id = ?1 AND legacy_account_id <> ?2",
                params![leftover_credential_id, account_id],
                |row| row.get(0),
            )?;
            anyhow::ensure!(
                taken == 0,
                "v57 refuses leftover credential_state for `{account_id}`: credential id `{leftover_credential_id}` already owned"
            );
            conn.execute(
                "UPDATE credential_grants SET credential_id = ?2 WHERE credential_id = ?1",
                params![current_id, leftover_credential_id],
            )?;
            conn.execute(
                "UPDATE destinations SET observer_credential_id = ?2
                 WHERE observer_credential_id = ?1",
                params![current_id, leftover_credential_id],
            )?;
            conn.execute(
                "UPDATE credentials SET id = ?2 WHERE id = ?1",
                params![current_id, leftover_credential_id],
            )?;
        }
        conn.execute(
            "UPDATE credentials SET
                credential_version = COALESCE(credential_version, ?2),
                auth_state_version = COALESCE(auth_state_version, ?3),
                rotated_at = COALESCE(rotated_at, ?4)
             WHERE legacy_account_id = ?1",
            params![account_id, version, auth_state_version, rotated_at],
        )?;
    }
    Ok(())
}

fn copy_leftover_bindings_and_grants(conn: &Connection) -> Result<()> {
    if !table_exists(conn, "credential_bindings")? {
        return Ok(());
    }
    let has_grant_cols = table_has_column(conn, "credential_bindings", "allowed_endpoint_ids")?
        && table_has_column(conn, "credential_bindings", "allowed_origins")?;
    let sql = if has_grant_cols {
        "SELECT account_id, id, enabled, model_scope, allowed_endpoint_ids, allowed_origins
         FROM credential_bindings"
    } else {
        "SELECT account_id, id, enabled, model_scope, NULL, NULL FROM credential_bindings"
    };
    let mut stmt = conn.prepare(sql)?;
    let rows = stmt
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, i32>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, Option<String>>(4)?,
                row.get::<_, Option<String>>(5)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    drop(stmt);
    for (account_id, binding_id, enabled, model_scope, ids, origins) in rows {
        conn.execute(
            "UPDATE credentials SET
                binding_id = COALESCE(binding_id, ?2),
                binding_enabled = COALESCE(binding_enabled, ?3),
                scope_json = CASE
                    WHEN scope_json IS NULL OR scope_json = '' THEN ?4
                    ELSE scope_json
                END
             WHERE legacy_account_id = ?1",
            params![account_id, binding_id, enabled, model_scope],
        )?;
        let credential_id: String = conn.query_row(
            "SELECT id FROM credentials WHERE legacy_account_id = ?1",
            [&account_id],
            |row| row.get(0),
        )?;
        let has_grants: bool = conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM credential_grants WHERE credential_id = ?1)",
            [&credential_id],
            |row| row.get(0),
        )?;
        let leftover_initialized = ids.is_some() || origins.is_some();
        if leftover_initialized {
            conn.execute(
                "UPDATE credentials SET grants_initialized = 1 WHERE legacy_account_id = ?1",
                [&account_id],
            )?;
        }
        if has_grants {
            continue;
        }
        if leftover_initialized {
            insert_grant_list(conn, &credential_id, GRANT_ENDPOINT, ids.as_deref())?;
            insert_grant_list(conn, &credential_id, GRANT_ORIGIN, origins.as_deref())?;
        }
    }
    Ok(())
}

fn insert_grant_list(
    conn: &Connection,
    credential_id: &str,
    kind: &str,
    raw: Option<&str>,
) -> Result<()> {
    let values = raw
        .and_then(|value| serde_json::from_str::<Vec<String>>(value).ok())
        .unwrap_or_default();
    for value in values {
        conn.execute(
            "INSERT OR IGNORE INTO credential_grants (credential_id, kind, value)
             VALUES (?1, ?2, ?3)",
            params![credential_id, kind, value],
        )?;
    }
    Ok(())
}

fn copy_leftover_onboarding(conn: &Connection) -> Result<()> {
    if !table_exists(conn, "onboarding_tasks")? {
        return Ok(());
    }
    let mut stmt =
        conn.prepare("SELECT account_id, id, kind, step, state FROM onboarding_tasks")?;
    let rows = stmt
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    drop(stmt);
    for (account_id, id, kind, step, state) in rows {
        let json = serde_json::json!({
            "id": id,
            "kind": kind,
            "step": step,
            "state": state,
        })
        .to_string();
        conn.execute(
            "UPDATE credentials SET onboarding_json = COALESCE(onboarding_json, ?2)
             WHERE legacy_account_id = ?1",
            params![account_id, json],
        )?;
    }
    Ok(())
}

fn copy_leftover_subscriptions(conn: &Connection) -> Result<()> {
    if !table_exists(conn, "subscription_records")? {
        return Ok(());
    }
    let mut stmt = conn.prepare(
        "SELECT account_id, source, purchase_date, expires_on FROM subscription_records",
    )?;
    let rows = stmt
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    drop(stmt);
    for (account_id, source, purchase_date, expires_on) in rows {
        conn.execute(
            "UPDATE credentials SET
                subscription_source = COALESCE(subscription_source, ?2),
                purchase_date = CASE
                    WHEN purchase_date IS NULL OR purchase_date = '' THEN ?3
                    ELSE purchase_date
                END,
                subscription_expires_on = COALESCE(subscription_expires_on, ?4)
             WHERE legacy_account_id = ?1",
            params![account_id, source, purchase_date, expires_on],
        )?;
    }
    Ok(())
}

pub(crate) fn stored_onboarding_from_json(
    account_id: &str,
    raw: Option<&str>,
    setup_step: &str,
) -> Option<super::identity::StoredOnboarding> {
    if let Some(raw) = raw.filter(|value| !value.is_empty()) {
        if let Ok(value) = serde_json::from_str::<serde_json::Value>(raw) {
            let kind = value
                .get("kind")
                .and_then(|item| item.as_str())
                .unwrap_or("managed_registration")
                .to_string();
            let state = value
                .get("state")
                .and_then(|item| item.as_str())
                .unwrap_or("in_progress")
                .to_string();
            let step = value
                .get("step")
                .and_then(|item| item.as_str())
                .unwrap_or(setup_step)
                .to_string();
            let id = value
                .get("id")
                .and_then(|item| item.as_str())
                .map(ToOwned::to_owned)
                .unwrap_or_else(|| onboarding_task_id_for_legacy_account(account_id).to_string());
            return Some(super::identity::StoredOnboarding {
                id,
                kind,
                step,
                state,
            });
        }
    }
    None
}

pub(crate) fn leftover_identity_tables_ddl() -> &'static str {
    "CREATE TABLE IF NOT EXISTS upstream_identities (
            id TEXT PRIMARY KEY,
            label TEXT NOT NULL,
            identity_confidence TEXT NOT NULL,
            authority_site TEXT,
            authority_subject TEXT,
            enabled INTEGER NOT NULL DEFAULT 1,
            notes TEXT,
            created_at TEXT NOT NULL,
            updated_at TEXT NOT NULL
        );
        CREATE TABLE IF NOT EXISTS credential_state (
            account_id TEXT PRIMARY KEY,
            credential_id TEXT NOT NULL UNIQUE,
            version INTEGER NOT NULL DEFAULT 1,
            auth_state_version INTEGER NOT NULL DEFAULT 1,
            rotated_at TEXT
        );
        CREATE TABLE IF NOT EXISTS credential_bindings (
            id TEXT PRIMARY KEY,
            account_id TEXT NOT NULL,
            connection_legacy_kind TEXT NOT NULL,
            connection_legacy_id TEXT NOT NULL,
            model_scope TEXT NOT NULL,
            enabled INTEGER NOT NULL DEFAULT 1,
            created_at TEXT NOT NULL,
            updated_at TEXT NOT NULL,
            allowed_endpoint_ids TEXT,
            allowed_origins TEXT
        );
        CREATE TABLE IF NOT EXISTS legacy_identity_map (
            legacy_kind TEXT NOT NULL,
            legacy_id TEXT NOT NULL,
            new_kind TEXT NOT NULL,
            new_id TEXT NOT NULL,
            migration_version INTEGER NOT NULL,
            PRIMARY KEY (legacy_kind, legacy_id, new_kind)
        );
        CREATE TABLE IF NOT EXISTS onboarding_tasks (
            id TEXT PRIMARY KEY,
            account_id TEXT NOT NULL,
            kind TEXT NOT NULL,
            step TEXT NOT NULL,
            state TEXT NOT NULL,
            created_at TEXT NOT NULL,
            updated_at TEXT NOT NULL
        );
        CREATE TABLE IF NOT EXISTS subscription_records (
            account_id TEXT PRIMARY KEY,
            source TEXT NOT NULL,
            purchase_date TEXT NOT NULL,
            expires_on TEXT NOT NULL,
            recorded_at TEXT NOT NULL
        );"
}

/// Test helper: copy credential identity facts onto leftover satellite tables
/// and mark schema v56 so reopen exercises the v57 copy+drop path.
pub(crate) fn rewind_identity_satellites_to_v56(conn: &Connection) -> Result<()> {
    let now = Utc::now().to_rfc3339();
    conn.execute_batch(leftover_identity_tables_ddl())?;
    let rows: Vec<(
        String,
        String,
        String,
        String,
        Option<String>,
        Option<String>,
        Option<String>,
        Option<String>,
        Option<i64>,
        Option<i64>,
        Option<String>,
        Option<String>,
        Option<i32>,
        Option<String>,
        Option<String>,
        Option<String>,
        Option<String>,
        Option<String>,
        Option<String>,
        Option<String>,
    )> = {
        let mut stmt = conn.prepare(
            "SELECT legacy_account_id, id, name, COALESCE(identity_id, ''),
                    identity_label, identity_confidence, authority_site, authority_subject,
                    credential_version, auth_state_version, rotated_at, binding_id,
                    binding_enabled, scope_json, onboarding_json, subscription_source,
                    subscription_expires_on, purchase_date, account_type, setup_step
             FROM credentials
             WHERE COALESCE(credential_purpose, 'inference') = 'inference'",
        )?;
        stmt.query_map([], |row| {
            Ok((
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get(3)?,
                row.get(4)?,
                row.get(5)?,
                row.get(6)?,
                row.get(7)?,
                row.get(8)?,
                row.get(9)?,
                row.get(10)?,
                row.get(11)?,
                row.get(12)?,
                row.get(13)?,
                row.get(14)?,
                row.get(15)?,
                row.get(16)?,
                row.get(17)?,
                row.get(18)?,
                row.get(19)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?
    };
    for (
        account_id,
        credential_id,
        name,
        identity_id,
        identity_label,
        identity_confidence,
        authority_site,
        authority_subject,
        credential_version,
        auth_state_version,
        rotated_at,
        binding_id,
        binding_enabled,
        scope_json,
        onboarding_json,
        subscription_source,
        subscription_expires_on,
        purchase_date,
        _account_type,
        setup_step,
    ) in rows
    {
        if identity_id.is_empty() {
            continue;
        }
        conn.execute(
            "INSERT OR IGNORE INTO upstream_identities (
                id, label, identity_confidence, authority_site, authority_subject,
                enabled, notes, created_at, updated_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, 1, NULL, ?6, ?6)",
            params![
                identity_id,
                identity_label.unwrap_or(name),
                identity_confidence.unwrap_or_else(|| "opaque".into()),
                authority_site,
                authority_subject,
                now,
            ],
        )?;
        conn.execute(
            "INSERT OR REPLACE INTO credential_state (
                account_id, credential_id, version, auth_state_version, rotated_at
             ) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                account_id,
                credential_id,
                credential_version.unwrap_or(1),
                auth_state_version.unwrap_or(1),
                rotated_at,
            ],
        )?;
        if let Some(binding_id) = binding_id {
            let grants: Vec<(String, String)> = {
                let mut grant_stmt = conn.prepare(
                    "SELECT kind, value FROM credential_grants
                     WHERE credential_id = ?1
                     ORDER BY rowid",
                )?;
                grant_stmt
                    .query_map([&credential_id], |row| {
                        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
                    })?
                    .collect::<rusqlite::Result<Vec<_>>>()?
            };
            let ids: Vec<String> = grants
                .iter()
                .filter(|(kind, _)| kind == "endpoint_id")
                .map(|(_, value)| value.clone())
                .collect();
            let origins: Vec<String> = grants
                .iter()
                .filter(|(kind, _)| kind == "origin")
                .map(|(_, value)| value.clone())
                .collect();
            conn.execute(
                "INSERT OR REPLACE INTO credential_bindings (
                    id, account_id, connection_legacy_kind, connection_legacy_id,
                    model_scope, enabled, created_at, updated_at,
                    allowed_endpoint_ids, allowed_origins
                 ) VALUES (?1, ?2, 'account', ?3, ?4, ?5, ?6, ?6, ?7, ?8)",
                params![
                    binding_id,
                    account_id,
                    account_id,
                    scope_json.unwrap_or_else(|| "{\"kind\":\"all\"}".into()),
                    binding_enabled.unwrap_or(1),
                    now,
                    serde_json::to_string(&ids)?,
                    serde_json::to_string(&origins)?,
                ],
            )?;
        }
        if let Some(raw) = onboarding_json.filter(|value| !value.is_empty()) {
            if let Ok(value) = serde_json::from_str::<serde_json::Value>(&raw) {
                conn.execute(
                    "INSERT OR REPLACE INTO onboarding_tasks (
                        id, account_id, kind, step, state, created_at, updated_at
                     ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?6)",
                    params![
                        value
                            .get("id")
                            .and_then(|item| item.as_str())
                            .unwrap_or("onboarding"),
                        account_id,
                        value
                            .get("kind")
                            .and_then(|item| item.as_str())
                            .unwrap_or("managed_registration"),
                        value
                            .get("step")
                            .and_then(|item| item.as_str())
                            .unwrap_or(setup_step.as_deref().unwrap_or("ready")),
                        value
                            .get("state")
                            .and_then(|item| item.as_str())
                            .unwrap_or("in_progress"),
                        now,
                    ],
                )?;
            }
        }
        if let Some(source) = subscription_source.filter(|value| !value.is_empty()) {
            conn.execute(
                "INSERT OR REPLACE INTO subscription_records (
                    account_id, source, purchase_date, expires_on, recorded_at
                 ) VALUES (?1, ?2, ?3, ?4, ?5)",
                params![
                    account_id,
                    source,
                    purchase_date.unwrap_or_default(),
                    subscription_expires_on.unwrap_or_else(|| "2026-01-01".into()),
                    now,
                ],
            )?;
        }
    }
    // Force v57 to copy leftover satellites rather than keep already-written
    // credential columns. Keep identity_id so leftover identities stay mappable.
    conn.execute_batch(
        "DELETE FROM credential_grants;
         UPDATE credentials SET
            identity_label = NULL,
            identity_confidence = NULL,
            authority_site = NULL,
            authority_subject = NULL,
            identity_enabled = NULL,
            identity_notes = NULL,
            credential_version = NULL,
            auth_state_version = NULL,
            rotated_at = NULL,
            binding_id = NULL,
            binding_enabled = NULL,
            subscription_source = NULL,
            subscription_expires_on = NULL,
            onboarding_json = NULL,
            grants_initialized = NULL;
         DELETE FROM schema_version;
         INSERT INTO schema_version(version) VALUES (56);",
    )?;
    Ok(())
}
