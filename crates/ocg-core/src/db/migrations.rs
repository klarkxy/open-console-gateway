//! Historical schema migrations (v27 through the current version).
//!
//! Sequenced by Database::open_internal. Bodies and ordering are unchanged;
//! this module does not open the database or own the connection.

use super::*;

fn access_keys_v27_ddl() -> String {
    format!(
        "
        CREATE TABLE IF NOT EXISTS access_keys (
            id TEXT PRIMARY KEY,
            name TEXT NOT NULL,
            key TEXT NOT NULL,
            is_primary INTEGER NOT NULL DEFAULT 0 CHECK (is_primary IN (0, 1)),
            enabled INTEGER NOT NULL DEFAULT 1 CHECK (enabled IN (0, 1)),
            deleted_at TEXT,
            created_at TEXT NOT NULL,
            CHECK (
                is_primary = 0 OR (
                    id = '{PRIMARY_KEY_ID}'
                    AND enabled = 1
                    AND deleted_at IS NULL
                    AND key <> ''
                )
            ),
            CHECK (
                id <> '{PRIMARY_KEY_ID}' OR is_primary = 1
            )
        );
        CREATE UNIQUE INDEX IF NOT EXISTS idx_access_keys_live_primary
            ON access_keys(is_primary) WHERE is_primary = 1 AND deleted_at IS NULL;
        CREATE UNIQUE INDEX IF NOT EXISTS idx_access_keys_active_key
            ON access_keys(key) WHERE deleted_at IS NULL AND key <> '';
        CREATE TRIGGER IF NOT EXISTS access_keys_protect_primary_delete
        BEFORE DELETE ON access_keys
        WHEN OLD.id = '{PRIMARY_KEY_ID}'
        BEGIN
            SELECT RAISE(ABORT, 'primary access key cannot be deleted');
        END;
        "
    )
}

fn mint_unique_primary_access_key(conn: &Connection) -> Result<String> {
    let mut taken = Vec::new();
    if table_exists(conn, "sub_gateway_keys")? {
        let mut stmt = conn
            .prepare("SELECT key FROM sub_gateway_keys WHERE deleted_at IS NULL AND key <> ''")?;
        taken = stmt
            .query_map([], |row| row.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
    }
    for _ in 0..64 {
        let left = uuid::Uuid::new_v4().simple().to_string();
        let right = uuid::Uuid::new_v4().simple().to_string();
        let candidate = format!("ocg-{}-{}", &left[..8], &right[..8]);
        if !taken.iter().any(|value| value == &candidate) {
            return Ok(candidate);
        }
    }
    anyhow::bail!("failed to mint a unique primary access key")
}

fn load_config_gateway_key(conn: &Connection) -> Result<Option<String>> {
    let Some(json) = conn
        .query_row(
            "SELECT value FROM settings WHERE key = 'config'",
            [],
            |row| row.get::<_, String>(0),
        )
        .optional()?
    else {
        return Ok(None);
    };
    let parsed: serde_json::Value = serde_json::from_str(&json).unwrap_or(serde_json::Value::Null);
    Ok(parsed
        .get("gateway_key")
        .and_then(|value| value.as_str())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string))
}

pub(super) fn sanitize_config_json_primary_key(json: &str) -> Result<(String, Option<String>)> {
    let mut value: serde_json::Value = serde_json::from_str(json)?;
    let primary = value
        .get("gateway_key")
        .and_then(|item| item.as_str())
        .map(str::trim)
        .filter(|item| !item.is_empty())
        .map(str::to_string);
    if let Some(object) = value.as_object_mut()
        && object.contains_key("gateway_key")
    {
        object.insert(
            "gateway_key".to_string(),
            serde_json::Value::String(String::new()),
        );
    }
    Ok((serde_json::to_string(&value)?, primary))
}

pub(super) fn upsert_primary_access_key_on(conn: &Connection, key: &str) -> Result<()> {
    let key = key.trim();
    anyhow::ensure!(!key.is_empty(), "primary access key cannot be empty");
    let now = Utc::now().to_rfc3339();
    conn.execute(
        "INSERT INTO access_keys (id, name, key, is_primary, enabled, deleted_at, created_at)
         VALUES (?1, ?2, ?3, 1, 1, NULL, ?4)
         ON CONFLICT(id) DO UPDATE SET
            key = excluded.key,
            name = excluded.name,
            is_primary = 1,
            enabled = 1,
            deleted_at = NULL",
        params![PRIMARY_KEY_ID, PRIMARY_KEY_NAME, key, now],
    )?;
    Ok(())
}

fn drop_column_if_exists(tx: &Transaction<'_>, table: &str, column: &str) -> Result<()> {
    if table_has_column(tx, table, column)? {
        tx.execute(&format!("ALTER TABLE {table} DROP COLUMN {column}"), [])?;
    }
    Ok(())
}

fn assert_v27_access_key_invariants(conn: &Connection) -> Result<()> {
    anyhow::ensure!(
        table_exists(conn, "access_keys")?,
        "v27 requires the access_keys table"
    );
    anyhow::ensure!(
        !table_exists(conn, "sub_gateway_keys")?,
        "v27 must drop sub_gateway_keys after copying into access_keys"
    );
    for column in USAGE_SYNC_ACCOUNT_COLUMNS {
        anyhow::ensure!(
            !table_has_column(conn, "accounts", column)?,
            "v27 must drop leftover accounts.{column}"
        );
    }
    let primary_count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM access_keys WHERE is_primary = 1 AND deleted_at IS NULL",
        [],
        |row| row.get(0),
    )?;
    anyhow::ensure!(
        primary_count == 1,
        "v27 requires exactly one live primary access key, found {primary_count}"
    );
    let (id, enabled, deleted_at, key): (String, i64, Option<String>, String) = conn.query_row(
        "SELECT id, enabled, deleted_at, key FROM access_keys WHERE is_primary = 1 LIMIT 1",
        [],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
    )?;
    anyhow::ensure!(
        id == PRIMARY_KEY_ID,
        "live primary access key id must be {PRIMARY_KEY_ID}, found {id}"
    );
    anyhow::ensure!(enabled == 1, "primary access key must stay enabled");
    anyhow::ensure!(
        deleted_at.is_none(),
        "primary access key must not be deleted"
    );
    anyhow::ensure!(
        !key.trim().is_empty(),
        "primary access key must be non-empty"
    );
    let active_non_primary: i64 = conn.query_row(
        "SELECT COUNT(*) FROM access_keys WHERE is_primary = 0 AND deleted_at IS NULL",
        [],
        |row| row.get(0),
    )?;
    anyhow::ensure!(
        active_non_primary <= MAX_ACTIVE_NON_PRIMARY_ACCESS_KEYS,
        "at most {MAX_ACTIVE_NON_PRIMARY_ACCESS_KEYS} active non-primary access keys are supported, found {active_non_primary}"
    );
    let duplicate_active: i64 = conn.query_row(
        "SELECT COUNT(*) FROM (
            SELECT key FROM access_keys
            WHERE deleted_at IS NULL AND key <> ''
            GROUP BY key HAVING COUNT(*) > 1
         )",
        [],
        |row| row.get(0),
    )?;
    anyhow::ensure!(
        duplicate_active == 0,
        "active access key values must be unique"
    );
    Ok(())
}

fn migrate_v27_body(tx: &Transaction<'_>) -> Result<()> {
    let account_count: i64 = if table_exists(tx, "accounts")? {
        tx.query_row("SELECT COUNT(*) FROM accounts", [], |row| row.get(0))?
    } else {
        0
    };
    let sub_count: i64 = if table_exists(tx, "sub_gateway_keys")? {
        tx.query_row("SELECT COUNT(*) FROM sub_gateway_keys", [], |row| {
            row.get(0)
        })?
    } else {
        0
    };
    if table_exists(tx, "sub_gateway_keys")? {
        let reserved: i64 = tx.query_row(
            "SELECT COUNT(*) FROM sub_gateway_keys WHERE id = ?1",
            [PRIMARY_KEY_ID],
            |row| row.get(0),
        )?;
        anyhow::ensure!(
            reserved == 0,
            "sub_gateway_keys must not reuse the fixed primary id {PRIMARY_KEY_ID}"
        );
    }

    tx.execute_batch(&access_keys_v27_ddl())?;

    let mut primary = load_config_gateway_key(tx)?.unwrap_or_default();
    if primary.is_empty() {
        primary = mint_unique_primary_access_key(tx)?;
    }
    upsert_primary_access_key_on(tx, &primary)?;

    if table_exists(tx, "sub_gateway_keys")? {
        tx.execute(
            "INSERT INTO access_keys (id, name, key, is_primary, enabled, deleted_at, created_at)
             SELECT id, name, key, 0, enabled, deleted_at, created_at
             FROM sub_gateway_keys",
            [],
        )?;
        tx.execute_batch("DROP TABLE IF EXISTS sub_gateway_keys;")?;
    }

    if let Some(json) = tx
        .query_row(
            "SELECT value FROM settings WHERE key = 'config'",
            [],
            |row| row.get::<_, String>(0),
        )
        .optional()?
    {
        let (sanitized, _) = sanitize_config_json_primary_key(&json)?;
        tx.execute(
            "INSERT INTO settings (key, value) VALUES ('config', ?1)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            [sanitized],
        )?;
    }

    for column in USAGE_SYNC_ACCOUNT_COLUMNS {
        drop_column_if_exists(tx, "accounts", column)?;
    }

    let access_count: i64 =
        tx.query_row("SELECT COUNT(*) FROM access_keys", [], |row| row.get(0))?;
    anyhow::ensure!(
        access_count == sub_count + 1,
        "v27 access_keys row count {access_count} must equal copied sub keys {sub_count} plus the primary row"
    );
    let migrated_accounts: i64 = if table_exists(tx, "accounts")? {
        tx.query_row("SELECT COUNT(*) FROM accounts", [], |row| row.get(0))?
    } else {
        0
    };
    anyhow::ensure!(
        migrated_accounts == account_count,
        "v27 must conserve account rows ({account_count} -> {migrated_accounts})"
    );

    super::lifecycle::sqlite_quick_check(tx)?;
    super::lifecycle::sqlite_foreign_key_check(tx)?;
    assert_v27_access_key_invariants(tx)?;
    Ok(())
}

struct ForeignKeysRestore<'a> {
    conn: &'a Connection,
    previous: i64,
}

impl Drop for ForeignKeysRestore<'_> {
    fn drop(&mut self) {
        if let Err(error) = self.conn.pragma_update(None, "foreign_keys", self.previous) {
            eprintln!(
                "warning: failed to restore PRAGMA foreign_keys={}: {error}",
                self.previous
            );
        }
    }
}

fn with_foreign_keys_off<T>(conn: &Connection, body: impl FnOnce() -> Result<T>) -> Result<T> {
    let previous: i64 = conn.query_row("PRAGMA foreign_keys", [], |row| row.get(0))?;
    conn.pragma_update(None, "foreign_keys", 0)?;
    let _restore = ForeignKeysRestore { conn, previous };
    body()
}

pub(super) fn migrate_to_v27(
    conn: &Connection,
    db_path: &Path,
    cipher: Option<&dyn KeyCipher>,
    is_fresh: bool,
) -> Result<()> {
    for _ in 0..V27_WRITER_RACE_RETRIES {
        let version = super::lifecycle::schema_version_on(conn)?;
        if version >= V27_SCHEMA_VERSION {
            return Ok(());
        }
        anyhow::ensure!(
            version == V26_SCHEMA_VERSION,
            "v27 requires a canonical schema v26 source, found {version}"
        );

        let data_version = super::lifecycle::sqlite_data_version(conn)?;
        v27_fault(V27MigrationFault::AfterDataVersionCapture)?;
        v27_fault(V27MigrationFault::BeforePreflight)?;
        super::lifecycle::sqlite_quick_check(conn)?;
        super::lifecycle::preflight_ciphertext_probes(conn, cipher)?;

        if !is_fresh {
            v27_fault(V27MigrationFault::BeforeBackup)?;
            super::lifecycle::create_pre_v3_backup(conn, db_path)?;
            v27_fault(V27MigrationFault::AfterBackup)?;
        }

        let migrated = with_foreign_keys_off(conn, || {
            let tx = Transaction::new_unchecked(conn, TransactionBehavior::Immediate)?;
            let version_locked = super::lifecycle::schema_version_on(&tx)?;
            if version_locked >= V27_SCHEMA_VERSION {
                tx.rollback()?;
                return Ok(true);
            }
            let data_version_locked = super::lifecycle::sqlite_data_version(&tx)?;
            if data_version_locked != data_version {
                tx.rollback()?;
                return Ok(false);
            }
            anyhow::ensure!(
                version_locked == V26_SCHEMA_VERSION,
                "v27 writer lock observed schema {version_locked}, expected {V26_SCHEMA_VERSION}"
            );
            migrate_v27_body(&tx)?;
            v27_fault(V27MigrationFault::BeforeSchemaVersion)?;
            tx.execute_batch(&format!(
                "INSERT OR REPLACE INTO schema_version (version) VALUES ({V27_SCHEMA_VERSION});"
            ))?;
            v27_fault(V27MigrationFault::BeforeCommit)?;
            tx.commit()?;
            Ok(true)
        })?;
        if migrated {
            return Ok(());
        }
    }
    anyhow::bail!(
        "v27 migration retried {V27_WRITER_RACE_RETRIES} times because a writer raced the pre-v3 backup"
    )
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum V27MigrationFault {
    BeforePreflight,
    BeforeBackup,
    AfterBackup,
    AfterDataVersionCapture,
    BeforeSchemaVersion,
    BeforeCommit,
}

fn v27_fault(point: V27MigrationFault) -> Result<()> {
    #[cfg(test)]
    {
        v27_test_hooks::inject(point)?;
    }
    let _ = point;
    Ok(())
}

pub(super) fn migrate_to_v28(conn: &Connection) -> Result<()> {
    let version = super::lifecycle::schema_version_on(conn)?;
    if version >= 28 {
        return Ok(());
    }
    anyhow::ensure!(
        version == V27_SCHEMA_VERSION,
        "v28 requires a canonical schema v27 source, found {version}"
    );
    let tx = conn.unchecked_transaction()?;
    ensure_column(
        &tx,
        "accounts",
        "goat_model_access",
        "TEXT NOT NULL DEFAULT 'goat'",
    )?;
    tx.execute(
        "UPDATE accounts SET goat_model_access = 'goat'
         WHERE goat_model_access NOT IN ('goat', 'all')",
        [],
    )?;
    tx.execute_batch("INSERT OR REPLACE INTO schema_version (version) VALUES (28);")?;
    tx.commit()?;
    Ok(())
}

pub(super) fn migrate_to_v29(conn: &Connection) -> Result<()> {
    let version = super::lifecycle::schema_version_on(conn)?;
    if version >= 29 {
        return Ok(());
    }
    anyhow::ensure!(
        version == 28,
        "v29 requires a canonical schema v28 source, found {version}"
    );
    let tx = conn.unchecked_transaction()?;
    // Mirror the child-table cleanup that delete_account performs so no rows
    // referencing deleted SCNet account ids survive the migration.
    tx.execute(
        "DELETE FROM quota_windows WHERE account_id IN (SELECT id FROM accounts WHERE provider_id = 'scnet')",
        [],
    )?;
    tx.execute(
        "DELETE FROM credit_balances WHERE account_id IN (SELECT id FROM accounts WHERE provider_id = 'scnet')",
        [],
    )?;
    tx.execute(
        "DELETE FROM provider_usage_sync_state WHERE account_id IN (SELECT id FROM accounts WHERE provider_id = 'scnet')",
        [],
    )?;
    tx.execute(
        "DELETE FROM account_custom_configs WHERE account_id IN (SELECT id FROM accounts WHERE provider_id = 'scnet')",
        [],
    )?;
    tx.execute(
        "DELETE FROM account_model_capabilities WHERE account_id IN (SELECT id FROM accounts WHERE provider_id = 'scnet')",
        [],
    )?;
    tx.execute(
        "DELETE FROM provider_contract_model_protocols WHERE scope_kind = 'provider' AND scope_id = 'scnet'",
        [],
    )?;
    tx.execute(
        "DELETE FROM provider_contract_scopes WHERE scope_kind = 'provider' AND scope_id = 'scnet'",
        [],
    )?;
    tx.execute(
        "DELETE FROM provider_pricing_snapshots WHERE provider_id = 'scnet'",
        [],
    )?;
    tx.execute(
        "DELETE FROM provider_model_catalogs WHERE provider_id = 'scnet'",
        [],
    )?;
    tx.execute(
        "UPDATE forward_logs SET provider_id = NULL, offering_id = NULL WHERE provider_id = 'scnet'",
        [],
    )?;
    tx.execute("DELETE FROM accounts WHERE provider_id = 'scnet'", [])?;
    tx.execute_batch(
        "DROP INDEX IF EXISTS idx_account_acknowledgements_account;
         DROP TABLE IF EXISTS account_acknowledgements;",
    )?;
    tx.execute_batch("INSERT OR REPLACE INTO schema_version (version) VALUES (29);")?;
    tx.commit()?;
    Ok(())
}

/// v30: Custom accounts become account-level multi-protocol. The single
/// `upstream_protocol` value is backfilled into a one-element JSON array in
/// the new `upstream_protocols` column, then the old column is dropped.
pub(super) fn migrate_to_v30(conn: &Connection) -> Result<()> {
    let version = super::lifecycle::schema_version_on(conn)?;
    if version >= 30 {
        return Ok(());
    }
    anyhow::ensure!(
        version == 29,
        "v30 requires a canonical schema v29 source, found {version}"
    );
    let tx = conn.unchecked_transaction()?;
    ensure_column(
        &tx,
        "account_custom_configs",
        "upstream_protocols",
        "TEXT NOT NULL DEFAULT '[]'",
    )?;
    if table_has_column(&tx, "account_custom_configs", "upstream_protocol")? {
        tx.execute(
            "UPDATE account_custom_configs SET upstream_protocols = json_array(upstream_protocol)",
            [],
        )?;
        tx.execute(
            "ALTER TABLE account_custom_configs DROP COLUMN upstream_protocol",
            [],
        )?;
    }
    tx.execute_batch("INSERT OR REPLACE INTO schema_version (version) VALUES (30);")?;
    tx.commit()?;
    Ok(())
}

/// v31: Per-model/per-protocol override table replaces scope-level protocol
/// switches. The `provider_contract_scopes` switch columns remain for backward
/// compatibility but are no longer read by effective contract derivation.
pub(super) fn migrate_to_v31(conn: &Connection) -> Result<()> {
    let version = super::lifecycle::schema_version_on(conn)?;
    if version >= 31 {
        return Ok(());
    }
    anyhow::ensure!(
        version == 30,
        "v31 requires a canonical schema v30 source, found {version}"
    );
    let tx = conn.unchecked_transaction()?;
    tx.execute_batch(
        "CREATE TABLE IF NOT EXISTS provider_contract_model_protocol_overrides (
            scope_kind TEXT NOT NULL,
            scope_id TEXT NOT NULL,
            model_id TEXT NOT NULL,
            protocol TEXT NOT NULL,
            state TEXT NOT NULL CHECK(state IN ('force_on','force_off')),
            updated_at TEXT NOT NULL,
            PRIMARY KEY(scope_kind, scope_id, model_id, protocol)
        );",
    )?;
    tx.execute_batch("INSERT OR REPLACE INTO schema_version (version) VALUES (31);")?;
    tx.commit()?;
    Ok(())
}

/// v32: Custom accounts bind one upstream protocol to one complete inference
/// endpoint. Historical multi-protocol rows are collapsed to the protocol that
/// the v31 runtime already preferred (Chat, then Responses, then Messages).
/// Migrated accounts are disabled and returned to pending verification so a
/// changed wire-auth rule is never activated silently.
pub(super) fn migrate_to_v32(conn: &Connection) -> Result<()> {
    let version = super::lifecycle::schema_version_on(conn)?;
    if version >= 32 {
        return Ok(());
    }
    anyhow::ensure!(
        version == 31,
        "v32 requires a canonical schema v31 source, found {version}"
    );
    // A few recovery/test paths can leave the schema marker behind after the
    // v32 table shape is already present. The actual v32 migration is atomic,
    // so recognizing the complete final shape is safe and keeps reopen
    // idempotent without trying to read removed v31 columns.
    // After v52 the accounts parent is gone; after v53 the leftover Custom
    // tables are gone. Rewind tests that only need later schema steps must
    // not resurrect those children with a FK to a missing parent.
    if !table_exists(conn, "account_custom_configs")? {
        conn.execute_batch("INSERT OR REPLACE INTO schema_version (version) VALUES (32);")?;
        return Ok(());
    }
    if table_has_column(conn, "account_custom_configs", "endpoint_url")?
        && table_has_column(conn, "account_custom_configs", "upstream_protocol")?
    {
        conn.execute_batch("INSERT OR REPLACE INTO schema_version (version) VALUES (32);")?;
        return Ok(());
    }
    if !table_has_column(conn, "account_custom_configs", "base_url")? {
        let row_count: i64 =
            conn.query_row("SELECT COUNT(*) FROM account_custom_configs", [], |row| {
                row.get(0)
            })?;
        anyhow::ensure!(
            row_count == 0,
            "cannot migrate nonempty Custom config table without base_url or endpoint_url"
        );
        let tx = conn.unchecked_transaction()?;
        tx.execute_batch(
            "DROP TABLE account_custom_configs;
             CREATE TABLE account_custom_configs (
                account_id TEXT PRIMARY KEY,
                endpoint_url TEXT NOT NULL,
                upstream_protocol TEXT NOT NULL,
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL,
                FOREIGN KEY (account_id) REFERENCES accounts(id) ON DELETE CASCADE
             );
             INSERT OR REPLACE INTO schema_version (version) VALUES (32);",
        )?;
        tx.commit()?;
        return Ok(());
    }
    let tx = conn.unchecked_transaction()?;
    tx.execute_batch(
        "CREATE TABLE account_custom_configs_v32 (
            account_id TEXT PRIMARY KEY,
            endpoint_url TEXT NOT NULL,
            upstream_protocol TEXT NOT NULL,
            created_at TEXT NOT NULL,
            updated_at TEXT NOT NULL,
            FOREIGN KEY (account_id) REFERENCES accounts(id) ON DELETE CASCADE
        );
        INSERT INTO account_custom_configs_v32 (
            account_id, endpoint_url, upstream_protocol, created_at, updated_at
        )
        SELECT
            account_id,
            rtrim(base_url, '/') || CASE
                WHEN EXISTS (SELECT 1 FROM json_each(upstream_protocols) WHERE value = 'chat_completions')
                    THEN '/chat/completions'
                WHEN EXISTS (SELECT 1 FROM json_each(upstream_protocols) WHERE value = 'responses')
                    THEN '/responses'
                ELSE '/messages'
            END,
            CASE
                WHEN EXISTS (SELECT 1 FROM json_each(upstream_protocols) WHERE value = 'chat_completions')
                    THEN 'chat_completions'
                WHEN EXISTS (SELECT 1 FROM json_each(upstream_protocols) WHERE value = 'responses')
                    THEN 'responses'
                ELSE 'messages'
            END,
            created_at,
            updated_at
        FROM account_custom_configs;

        DELETE FROM account_model_capabilities
         WHERE account_id IN (SELECT account_id FROM account_custom_configs_v32)
           AND protocol <> (
               SELECT upstream_protocol FROM account_custom_configs_v32 c
                WHERE c.account_id = account_model_capabilities.account_id
           );
        DELETE FROM provider_contract_model_protocols
         WHERE scope_kind = 'custom_endpoint'
           AND scope_id IN (SELECT account_id FROM account_custom_configs_v32)
           AND protocol <> (
               SELECT upstream_protocol FROM account_custom_configs_v32 c
                WHERE c.account_id = provider_contract_model_protocols.scope_id
           );
        DELETE FROM provider_contract_model_protocol_overrides
         WHERE scope_kind = 'custom_endpoint'
           AND scope_id IN (SELECT account_id FROM account_custom_configs_v32)
           AND protocol <> (
               SELECT upstream_protocol FROM account_custom_configs_v32 c
                WHERE c.account_id = provider_contract_model_protocol_overrides.scope_id
           );",
    )?;
    if table_has_column(&tx, "accounts", "enabled")?
        && table_has_column(&tx, "accounts", "verification_status")?
        && table_has_column(&tx, "accounts", "connection_verified_at")?
        && table_has_column(&tx, "accounts", "verification_error")?
    {
        tx.execute(
            "UPDATE accounts
                SET enabled = 0,
                    verification_status = 'pending',
                    connection_verified_at = NULL,
                    verification_error = NULL
              WHERE id IN (SELECT account_id FROM account_custom_configs_v32)",
            [],
        )?;
    }
    tx.execute_batch(
        "DROP TABLE account_custom_configs;
         ALTER TABLE account_custom_configs_v32 RENAME TO account_custom_configs;
         INSERT OR REPLACE INTO schema_version (version) VALUES (32);",
    )?;
    tx.commit()?;
    Ok(())
}

/// v33: Custom capabilities keep their client-facing identity in the existing
/// `model_id` column and gain the exact model ID sent upstream. Existing rows,
/// including non-Custom provider catalog rows, preserve their old behavior by
/// starting with identical public and upstream identities.
pub(super) fn migrate_to_v33(conn: &Connection) -> Result<()> {
    let version = super::lifecycle::schema_version_on(conn)?;
    if version >= 33 {
        return Ok(());
    }
    anyhow::ensure!(
        version == 32,
        "v33 requires a canonical schema v32 source, found {version}"
    );
    if !table_exists(conn, "account_model_capabilities")? {
        conn.execute_batch("INSERT OR REPLACE INTO schema_version (version) VALUES (33);")?;
        return Ok(());
    }
    let tx = conn.unchecked_transaction()?;
    if !table_has_column(&tx, "account_model_capabilities", "upstream_model")? {
        tx.execute_batch(
            "ALTER TABLE account_model_capabilities
                 ADD COLUMN upstream_model TEXT NOT NULL DEFAULT '';
             UPDATE account_model_capabilities
                SET upstream_model = model_id;",
        )?;
    } else {
        tx.execute(
            "UPDATE account_model_capabilities
                SET upstream_model = model_id
              WHERE upstream_model = ''",
            [],
        )?;
    }
    tx.execute_batch("INSERT OR REPLACE INTO schema_version (version) VALUES (33);")?;
    tx.commit()?;
    Ok(())
}

/// v34: one local CPA configuration row. Dropped in v55 after the singleton
/// maps onto the CPA destination + observer credential. The model snapshot
/// remains in `provider_model_catalogs`.
pub(super) fn migrate_to_v34(conn: &Connection) -> Result<()> {
    let version = super::lifecycle::schema_version_on(conn)?;
    if version >= 34 {
        return Ok(());
    }
    anyhow::ensure!(
        version == 33,
        "v34 requires a canonical schema v33 source, found {version}"
    );
    let tx = conn.unchecked_transaction()?;
    tx.execute_batch(
        "CREATE TABLE IF NOT EXISTS cpa_integration (
             id TEXT PRIMARY KEY CHECK (id = 'cpa'),
             account_id TEXT NOT NULL UNIQUE,
             base_url TEXT NOT NULL,
             management_key_cipher TEXT NOT NULL,
             updated_at TEXT NOT NULL,
             FOREIGN KEY (account_id) REFERENCES accounts(id) ON DELETE CASCADE
         );
         INSERT OR REPLACE INTO schema_version (version) VALUES (34);",
    )?;
    tx.commit()?;
    Ok(())
}

fn v34_pair_is_known(provider_id: &str, offering_id: &str) -> bool {
    V34_KNOWN_PROVIDER_OFFERING_PAIRS
        .iter()
        .any(|(provider, offering)| *provider == provider_id && *offering == offering_id)
}

fn preflight_v35_pairs(conn: &Connection, table: &str, provider_nullable: bool) -> Result<()> {
    if !table_exists(conn, table)? || !table_has_column(conn, table, "offering_id")? {
        return Ok(());
    }
    let mut stmt = conn.prepare(&format!(
        "SELECT DISTINCT provider_id, offering_id FROM {table}"
    ))?;
    let rows = stmt
        .query_map([], |row| {
            Ok((
                row.get::<_, Option<String>>(0)?,
                row.get::<_, Option<String>>(1)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    for (provider_id, offering_id) in rows {
        match (provider_id.as_deref(), offering_id.as_deref()) {
            (None, None) if provider_nullable => continue,
            (Some(provider), Some(offering)) if v34_pair_is_known(provider, offering) => continue,
            (provider, offering) => anyhow::bail!(
                "v35 preflight found unknown provider/offering pair in {table}: `{}`/`{}`",
                provider.unwrap_or("<null>"),
                offering.unwrap_or("<null>")
            ),
        }
    }
    Ok(())
}

fn preflight_v35_catalog_collisions(conn: &Connection) -> Result<()> {
    if !table_exists(conn, "provider_model_catalogs")?
        || !table_has_column(conn, "provider_model_catalogs", "offering_id")?
    {
        return Ok(());
    }
    let mut stmt = conn.prepare(
        "SELECT provider_id, COUNT(*) FROM provider_model_catalogs
         GROUP BY provider_id HAVING COUNT(*) > 1",
    )?;
    let collisions = stmt
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    anyhow::ensure!(
        collisions.is_empty(),
        "v35 preflight found provider_model_catalogs collisions collapsing onto provider_id: {collisions:?}"
    );
    Ok(())
}

fn preflight_v35_pricing_collisions(conn: &Connection) -> Result<()> {
    if !table_exists(conn, "provider_pricing_snapshots")?
        || !table_has_column(conn, "provider_pricing_snapshots", "offering_id")?
    {
        return Ok(());
    }
    let mut stmt = conn.prepare(
        "SELECT provider_id, revision, COUNT(*) FROM provider_pricing_snapshots
         GROUP BY provider_id, revision HAVING COUNT(*) > 1",
    )?;
    let collisions = stmt
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, i64>(2)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    anyhow::ensure!(
        collisions.is_empty(),
        "v35 preflight found provider_pricing_snapshots collisions collapsing onto (provider_id, revision): {collisions:?}"
    );
    Ok(())
}

fn preflight_v35_identity(conn: &Connection) -> Result<()> {
    super::lifecycle::sqlite_quick_check(conn)?;
    preflight_v35_pairs(conn, "accounts", false)?;
    preflight_v35_pairs(conn, "forward_logs", true)?;
    preflight_v35_pairs(conn, "provider_pricing_snapshots", false)?;
    preflight_v35_pairs(conn, "provider_model_catalogs", false)?;
    preflight_v35_catalog_collisions(conn)?;
    preflight_v35_pricing_collisions(conn)?;
    Ok(())
}

fn create_pre_v35_backup(conn: &Connection, db_path: &Path) -> Result<PathBuf> {
    create_pre_version_backup(
        conn,
        db_path,
        PRE_V35_BACKUP_FILE_PREFIX,
        V34_SCHEMA_VERSION,
    )
}

/// v42: same online snapshot pattern as v35, but the v41 source already
/// hosts the `dynamic_providers` / `dynamic_provider_models` pair that v42
/// renames.
fn create_pre_v42_backup(conn: &Connection, db_path: &Path) -> Result<PathBuf> {
    create_pre_version_backup(
        conn,
        db_path,
        PRE_V42_BACKUP_FILE_PREFIX,
        V41_SCHEMA_VERSION,
    )
}

fn create_pre_v48_backup(conn: &Connection, db_path: &Path) -> Result<PathBuf> {
    create_pre_version_backup(
        conn,
        db_path,
        PRE_V48_BACKUP_FILE_PREFIX,
        V47_SCHEMA_VERSION,
    )
}

pub(super) fn create_pre_v58_backup(conn: &Connection, db_path: &Path) -> Result<PathBuf> {
    create_pre_version_backup(
        conn,
        db_path,
        PRE_V58_BACKUP_FILE_PREFIX,
        V57_SCHEMA_VERSION,
    )
}

pub(super) fn create_pre_version_backup(
    conn: &Connection,
    db_path: &Path,
    prefix: &str,
    source_version: i32,
) -> Result<PathBuf> {
    for _ in 0..8 {
        let timestamp = Utc::now().format("%Y%m%dT%H%M%S%9fZ");
        let backup_path = db_path.with_file_name(format!("{prefix}{timestamp}.bak"));
        if backup_path.exists() {
            std::thread::sleep(std::time::Duration::from_millis(1));
            continue;
        }
        let backup_value = backup_path.to_string_lossy().into_owned();
        conn.execute("VACUUM main INTO ?1", [&backup_value])
            .with_context(|| {
                format!(
                    "failed to create {prefix} database backup {}",
                    backup_path.display()
                )
            })?;
        super::lifecycle::verify_schema_backup(&backup_path, prefix, source_version)?;
        super::lifecycle::write_backup_sha256_evidence(&backup_path)?;
        return Ok(backup_path);
    }
    anyhow::bail!("failed to allocate a unique {prefix} backup filename")
}

fn migrate_v35_body(tx: &Transaction<'_>) -> Result<()> {
    if table_exists(tx, "forward_logs")? {
        tx.execute_batch("DROP INDEX IF EXISTS idx_forward_logs_provider_offering;")?;
    }

    if table_exists(tx, "accounts")? && table_has_column(tx, "accounts", "offering_id")? {
        tx.execute_batch("ALTER TABLE accounts DROP COLUMN offering_id;")?;
    }

    if table_exists(tx, "forward_logs")? && table_has_column(tx, "forward_logs", "offering_id")? {
        tx.execute_batch("ALTER TABLE forward_logs DROP COLUMN offering_id;")?;
        tx.execute_batch(
            "CREATE INDEX IF NOT EXISTS idx_forward_logs_provider ON forward_logs(provider_id);",
        )?;
    }

    if table_exists(tx, "provider_model_catalogs")?
        && table_has_column(tx, "provider_model_catalogs", "offering_id")?
    {
        tx.execute_batch(
            "CREATE TABLE provider_model_catalogs_v35 (
                provider_id TEXT PRIMARY KEY,
                models_json TEXT NOT NULL,
                refreshed_at TEXT,
                source_url TEXT NOT NULL
            );",
        )?;
        tx.execute(
            "INSERT INTO provider_model_catalogs_v35
             (provider_id, models_json, refreshed_at, source_url)
             SELECT provider_id, models_json, refreshed_at, source_url
             FROM provider_model_catalogs",
            [],
        )?;
        tx.execute_batch(
            "DROP TABLE provider_model_catalogs;
             ALTER TABLE provider_model_catalogs_v35 RENAME TO provider_model_catalogs;",
        )?;
    }

    if table_exists(tx, "provider_pricing_snapshots")?
        && table_has_column(tx, "provider_pricing_snapshots", "offering_id")?
    {
        tx.execute_batch("DROP INDEX IF EXISTS idx_provider_pricing_active;")?;
        tx.execute_batch(
            "CREATE TABLE provider_pricing_snapshots_v35 (
                provider_id TEXT NOT NULL,
                revision TEXT NOT NULL,
                activated_at TEXT NOT NULL,
                document_updated_at TEXT,
                source_url TEXT NOT NULL,
                content_hash TEXT NOT NULL,
                snapshot_json TEXT NOT NULL,
                PRIMARY KEY (provider_id, revision)
            );",
        )?;
        tx.execute(
            "INSERT INTO provider_pricing_snapshots_v35
             (provider_id, revision, activated_at, document_updated_at,
              source_url, content_hash, snapshot_json)
             SELECT provider_id, revision, activated_at, document_updated_at,
                    source_url, content_hash, snapshot_json
             FROM provider_pricing_snapshots",
            [],
        )?;
        tx.execute_batch(
            "DROP TABLE provider_pricing_snapshots;
             ALTER TABLE provider_pricing_snapshots_v35 RENAME TO provider_pricing_snapshots;
             CREATE INDEX IF NOT EXISTS idx_provider_pricing_active
                 ON provider_pricing_snapshots(provider_id, activated_at DESC);",
        )?;
    }

    ensure_dynamic_provider_tables(tx)?;
    super::lifecycle::sqlite_quick_check(tx)?;
    super::lifecycle::sqlite_foreign_key_check(tx)?;
    Ok(())
}

pub(super) fn dynamic_tx_fault(point: &'static str) -> Result<()> {
    #[cfg(test)]
    {
        dynamic_provider_fault::inject(point)
    }
    #[cfg(not(test))]
    {
        let _ = point;
        Ok(())
    }
}

pub(super) fn list_dynamic_providers_on(conn: &Connection) -> Result<Vec<DynamicProviderRuntime>> {
    dynamic_store::list_dynamic_providers_on(conn)
}

pub(super) fn list_control_plane_dynamic_providers_on(
    conn: &Connection,
) -> Result<Vec<DynamicProviderRuntime>> {
    dynamic_store::list_control_plane_dynamic_providers_on(conn)
}

pub(super) fn get_dynamic_provider_on(
    conn: &Connection,
    provider_id: &str,
) -> Result<Option<DynamicProviderRuntime>> {
    dynamic_store::get_dynamic_provider_on(conn, provider_id)
}

pub(super) fn onboarding_draft_provider_ids_on(conn: &Connection) -> Result<HashSet<String>> {
    dynamic_store::onboarding_draft_provider_ids_on(conn)
}

pub(super) fn provider_is_onboarding_draft_on(
    conn: &Connection,
    provider_id: &str,
) -> Result<Option<bool>> {
    dynamic_store::provider_is_onboarding_draft_on(conn, provider_id)
}

/// Read a sealed builtin catalog row or a destination-backed dynamic
/// definition. Used by the public GET to surface the unified view.
pub(super) fn get_provider_definition_on(
    conn: &Connection,
    provider_id: &str,
) -> Result<Option<DynamicProviderRuntime>> {
    dynamic_store::get_provider_definition_on(conn, provider_id)
}

pub(super) fn find_dashboard_operation_on(
    conn: &Connection,
    operation_id: &str,
) -> Result<Option<DashboardOperationRow>> {
    conn.query_row(
        "SELECT operation_id, kind, payload_digest, result_json, created_at
         FROM dashboard_operations
         WHERE operation_id = ?1",
        [operation_id],
        |row| {
            Ok(DashboardOperationRow {
                operation_id: row.get(0)?,
                kind: row.get(1)?,
                payload_digest: row.get(2)?,
                result_json: row.get(3)?,
                created_at: row.get(4)?,
            })
        },
    )
    .optional()
    .map_err(Into::into)
}

pub(super) fn insert_dashboard_operation_on(
    conn: &Connection,
    operation: &NewDashboardOperation,
) -> Result<()> {
    let now = Utc::now();
    let cutoff = (now - Duration::days(DASHBOARD_OPERATION_PRUNE_DAYS)).to_rfc3339();
    conn.execute(
        "DELETE FROM dashboard_operations WHERE created_at < ?1",
        [&cutoff],
    )?;
    conn.execute(
        "INSERT INTO dashboard_operations
         (operation_id, kind, payload_digest, result_json, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5)",
        params![
            operation.operation_id,
            operation.kind,
            operation.payload_digest,
            operation.result_json,
            now.to_rfc3339(),
        ],
    )?;
    Ok(())
}

pub(super) fn insert_dynamic_provider_on(
    conn: &Connection,
    runtime: &DynamicProviderRuntime,
    onboarding_draft: bool,
) -> Result<()> {
    dynamic_store::insert_dynamic_provider_on(conn, runtime, onboarding_draft)
}

pub(super) fn count_accounts_for_provider_on(conn: &Connection, provider_id: &str) -> Result<i64> {
    account_store::count_accounts_for_provider_on(conn, provider_id)
}

pub(super) fn upsert_imported_dynamic_provider_on(
    conn: &Connection,
    runtime: &DynamicProviderRuntime,
    imported_account_ids: &HashSet<String>,
    onboarding_draft: bool,
) -> Result<()> {
    dynamic_store::upsert_imported_dynamic_provider_on(
        conn,
        runtime,
        imported_account_ids,
        onboarding_draft,
    )
}

pub(super) fn ensure_dynamic_singleton_accounts_on(conn: &Connection) -> Result<()> {
    for runtime in list_dynamic_providers_on(conn)? {
        if !runtime.auth_kind.is_singleton() {
            continue;
        }
        let count = count_accounts_for_provider_on(conn, &runtime.id)?;
        anyhow::ensure!(
            count <= 1,
            "no-auth provider `{}` requires a singleton account",
            runtime.id
        );
    }
    Ok(())
}

pub(super) fn ensure_dynamic_provider_tables(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS dynamic_providers (
            id TEXT PRIMARY KEY,
            name TEXT NOT NULL,
            endpoint_url TEXT NOT NULL,
            upstream_protocol TEXT NOT NULL,
            auth_kind TEXT NOT NULL,
            created_at TEXT NOT NULL,
            updated_at TEXT NOT NULL
         );
         CREATE TABLE IF NOT EXISTS dynamic_provider_models (
            provider_id TEXT NOT NULL,
            public_model TEXT NOT NULL,
            public_model_key TEXT NOT NULL,
            upstream_model TEXT NOT NULL,
            PRIMARY KEY (provider_id, public_model_key),
            FOREIGN KEY (provider_id) REFERENCES dynamic_providers(id)
         );
         CREATE INDEX IF NOT EXISTS idx_dynamic_provider_models_provider
            ON dynamic_provider_models(provider_id);",
    )?;
    Ok(())
}

/// Missing model overrides inherit the existing Provider defaults unchanged.
pub(super) fn migrate_to_v40(conn: &Connection) -> Result<()> {
    if super::lifecycle::schema_version_on(conn)? >= 40 {
        return Ok(());
    }
    let tx = conn.unchecked_transaction()?;
    let version = super::lifecycle::schema_version_on(&tx)?;
    if version >= 40 {
        return Ok(());
    }
    anyhow::ensure!(version == 39, "v40 requires schema v39");
    ensure_column(&tx, "dynamic_provider_models", "upstream_override", "TEXT")?;
    tx.execute_batch("INSERT OR REPLACE INTO schema_version(version) VALUES(40);")?;
    tx.commit()?;
    Ok(())
}

/// Model enablement and the selected preferred protocol have independent lifetimes.
pub(super) fn migrate_to_v41(conn: &Connection) -> Result<()> {
    if super::lifecycle::schema_version_on(conn)? >= 41 {
        return Ok(());
    }
    let tx = Transaction::new_unchecked(conn, TransactionBehavior::Immediate)?;
    let version = super::lifecycle::schema_version_on(&tx)?;
    if version >= 41 {
        return Ok(());
    }
    anyhow::ensure!(version == 40, "v41 requires schema v40");
    tx.execute_batch(
        "CREATE TABLE IF NOT EXISTS provider_model_protocol_preferences (
            provider_id TEXT NOT NULL CHECK(provider_id IN ('minimax', 'kimi')),
            model_id TEXT NOT NULL,
            protocol TEXT NOT NULL CHECK(protocol IN ('chat_completions', 'messages')),
            PRIMARY KEY(provider_id, model_id)
         );
         INSERT OR REPLACE INTO schema_version(version) VALUES(41);",
    )?;
    tx.commit()?;
    Ok(())
}

/// v42: unify the dynamic Provider table with the sealed builtin catalog.
/// The v41 source still carries `dynamic_providers` / `dynamic_provider_models`;
/// the rewrite creates `providers` / `provider_models` with `origin` and
/// `offering` columns and seeds the seven sealed adapters as `builtin` rows.
/// The data mirror for builtin rows never feeds routing — traffic keeps using
/// the sealed adapter code constants.
pub(super) fn migrate_to_v42(conn: &Connection, db_path: &Path, is_fresh: bool) -> Result<()> {
    // Read once before the writer lock: a concurrent migration can commit
    // between two reads, so checking `>= 42` and `== 41` separately is racy.
    let source_version = super::lifecycle::schema_version_on(conn)?;
    if source_version >= 42 {
        return Ok(());
    }
    anyhow::ensure!(
        source_version == V41_SCHEMA_VERSION,
        "v42 requires a canonical schema v41 source"
    );
    if !is_fresh {
        create_pre_v42_backup(conn, db_path)?;
    }
    super::lifecycle::sqlite_quick_check(conn)?;
    with_foreign_keys_off(conn, || {
        let tx = Transaction::new_unchecked(conn, TransactionBehavior::Immediate)?;
        let version_locked = super::lifecycle::schema_version_on(&tx)?;
        if version_locked >= 42 {
            tx.rollback()?;
            return Ok(());
        }
        anyhow::ensure!(
            version_locked == V41_SCHEMA_VERSION,
            "v42 writer lock observed schema {version_locked}, expected {V41_SCHEMA_VERSION}"
        );
        migrate_v42_body(&tx)?;
        tx.execute_batch("INSERT OR REPLACE INTO schema_version (version) VALUES (42);")?;
        super::lifecycle::sqlite_quick_check(&tx)?;
        super::lifecycle::sqlite_foreign_key_check(&tx)?;
        tx.commit()?;
        Ok(())
    })
}

/// v43: allow Responses as a stored preferred protocol, and restore Auto on
/// available CN sibling protocols that the exclusive radio force_off'd.
pub(super) fn migrate_to_v43(conn: &Connection) -> Result<()> {
    if super::lifecycle::schema_version_on(conn)? >= 43 {
        return Ok(());
    }
    let tx = Transaction::new_unchecked(conn, TransactionBehavior::Immediate)?;
    let version = super::lifecycle::schema_version_on(&tx)?;
    if version >= 43 {
        return Ok(());
    }
    anyhow::ensure!(version == 42, "v43 requires schema v42");
    if table_exists(&tx, "provider_model_protocol_preferences")? {
        tx.execute_batch(
            "CREATE TABLE provider_model_protocol_preferences_v43 AS
                 SELECT provider_id, model_id, protocol
                   FROM provider_model_protocol_preferences;
             DROP TABLE provider_model_protocol_preferences;
             CREATE TABLE provider_model_protocol_preferences (
                provider_id TEXT NOT NULL,
                model_id TEXT NOT NULL,
                protocol TEXT NOT NULL CHECK(protocol IN ('chat_completions', 'responses', 'messages')),
                PRIMARY KEY(provider_id, model_id)
             );
             INSERT INTO provider_model_protocol_preferences (provider_id, model_id, protocol)
             SELECT provider_id, model_id, protocol FROM provider_model_protocol_preferences_v43;
             DROP TABLE provider_model_protocol_preferences_v43;",
        )?;
    }
    if table_exists(&tx, "provider_contract_model_protocol_overrides")? {
        tx.execute(
            "DELETE FROM provider_contract_model_protocol_overrides
             WHERE state = 'force_off'
               AND scope_kind = 'provider'
               AND scope_id IN (?1, ?2)
               AND protocol IN ('chat_completions', 'messages')
               AND EXISTS (
                 SELECT 1
                 FROM provider_contract_model_protocol_overrides AS sibling
                 WHERE sibling.scope_kind = provider_contract_model_protocol_overrides.scope_kind
                   AND sibling.scope_id = provider_contract_model_protocol_overrides.scope_id
                   AND sibling.model_id = provider_contract_model_protocol_overrides.model_id
                   AND sibling.state = 'force_on'
                   AND sibling.protocol IN ('chat_completions', 'messages')
                   AND sibling.protocol != provider_contract_model_protocol_overrides.protocol
               )",
            params![MINIMAX_PROVIDER_ID, KIMI_PROVIDER_ID],
        )?;
    }
    tx.execute_batch("INSERT OR REPLACE INTO schema_version(version) VALUES (43);")?;
    tx.commit()?;
    Ok(())
}

/// v44: additive dashboard operation ledger for idempotent V4 writes.
/// Stores only a payload digest and a secret-free result — never the request body.
pub(super) fn migrate_to_v44(conn: &Connection) -> Result<()> {
    if super::lifecycle::schema_version_on(conn)? >= 44 {
        return Ok(());
    }
    let tx = Transaction::new_unchecked(conn, TransactionBehavior::Immediate)?;
    let version = super::lifecycle::schema_version_on(&tx)?;
    if version >= 44 {
        return Ok(());
    }
    anyhow::ensure!(version == 43, "v44 requires schema v43");
    tx.execute_batch(
        "CREATE TABLE IF NOT EXISTS dashboard_operations (
            operation_id TEXT PRIMARY KEY,
            kind TEXT NOT NULL,
            payload_digest TEXT NOT NULL,
            result_json TEXT NOT NULL,
            created_at TEXT NOT NULL
        );",
    )?;
    tx.execute_batch("INSERT OR REPLACE INTO schema_version(version) VALUES (44);")?;
    tx.commit()?;
    Ok(())
}

/// v47: persisted onboarding draft flag on the unified `providers` row.
/// Existing rows stay configured (`0`). Draft is never inferred from missing fields.
pub(super) fn migrate_to_v47(conn: &Connection) -> Result<()> {
    if super::lifecycle::schema_version_on(conn)? >= 47 {
        return Ok(());
    }
    let tx = Transaction::new_unchecked(conn, TransactionBehavior::Immediate)?;
    let version = super::lifecycle::schema_version_on(&tx)?;
    if version >= 47 {
        return Ok(());
    }
    anyhow::ensure!(version == 46, "v47 requires schema v46");
    if table_exists(&tx, "providers")? {
        ensure_column(
            &tx,
            "providers",
            "onboarding_draft",
            "INTEGER NOT NULL DEFAULT 0",
        )?;
    }
    tx.execute_batch("INSERT OR REPLACE INTO schema_version(version) VALUES (47);")?;
    tx.commit()?;
    Ok(())
}

fn retired_dynamic_table_row_count(conn: &Connection, table: &str) -> Result<Option<i64>> {
    if !table_exists(conn, table)? {
        return Ok(None);
    }
    Ok(Some(conn.query_row(
        &format!("SELECT COUNT(*) FROM {table}"),
        [],
        |row| row.get(0),
    )?))
}

fn ensure_retired_dynamic_tables_empty(conn: &Connection) -> Result<()> {
    let mut nonempty = Vec::new();
    for table in ["dynamic_providers", "dynamic_provider_models"] {
        if let Some(count) = retired_dynamic_table_row_count(conn, table)?
            && count > 0
        {
            nonempty.push(format!("{table} ({count} rows)"));
        }
    }
    anyhow::ensure!(
        nonempty.is_empty(),
        "v48 found nonempty leftover {}; refusing to drop or claim schema 48",
        nonempty.join(" and ")
    );
    Ok(())
}

/// v48: drop inert protocol-switch and free-alias columns, and empty leftover
/// `dynamic_providers` / `dynamic_provider_models` tables from the v42 reopen bug.
/// Nonempty leftovers fail closed and keep schema 47.
pub(super) fn migrate_to_v48(conn: &Connection, db_path: &Path, is_fresh: bool) -> Result<()> {
    let source_version = super::lifecycle::schema_version_on(conn)?;
    if source_version >= 48 {
        return Ok(());
    }
    anyhow::ensure!(
        source_version == V47_SCHEMA_VERSION,
        "v48 requires a canonical schema v47 source"
    );
    ensure_retired_dynamic_tables_empty(conn)?;
    if !is_fresh {
        create_pre_v48_backup(conn, db_path)?;
    }
    let tx = Transaction::new_unchecked(conn, TransactionBehavior::Immediate)?;
    let version_locked = super::lifecycle::schema_version_on(&tx)?;
    if version_locked >= 48 {
        tx.rollback()?;
        return Ok(());
    }
    anyhow::ensure!(
        version_locked == V47_SCHEMA_VERSION,
        "v48 writer lock observed schema {version_locked}, expected {V47_SCHEMA_VERSION}"
    );
    ensure_retired_dynamic_tables_empty(&tx)?;
    migrate_v48_body(&tx)?;
    tx.execute_batch("INSERT OR REPLACE INTO schema_version (version) VALUES (48);")?;
    tx.commit()?;
    Ok(())
}

fn migrate_v48_body(tx: &Transaction<'_>) -> Result<()> {
    for table in ["dynamic_provider_models", "dynamic_providers"] {
        if table_exists(tx, table)? {
            tx.execute(&format!("DROP TABLE {table}"), [])?;
        }
    }
    for column in [
        "chat_completions_enabled",
        "responses_enabled",
        "messages_enabled",
    ] {
        drop_column_if_exists(tx, "provider_contract_scopes", column)?;
    }
    drop_column_if_exists(tx, "accounts", "free_alias_enabled")?;
    Ok(())
}

/// v49: unpublished public model names hidden from `GET /v1/models`.
/// Missing names stay published. Additive; no pre-migration backup.
pub(super) fn migrate_to_v49(conn: &Connection) -> Result<()> {
    if super::lifecycle::schema_version_on(conn)? >= 49 {
        return Ok(());
    }
    let tx = Transaction::new_unchecked(conn, TransactionBehavior::Immediate)?;
    let version = super::lifecycle::schema_version_on(&tx)?;
    if version >= 49 {
        return Ok(());
    }
    anyhow::ensure!(version == 48, "v49 requires schema v48");
    tx.execute_batch(
        "CREATE TABLE IF NOT EXISTS unpublished_public_models (
            public_model TEXT PRIMARY KEY,
            updated_at TEXT NOT NULL
        );",
    )?;
    tx.execute_batch("INSERT OR REPLACE INTO schema_version(version) VALUES (49);")?;
    tx.commit()?;
    Ok(())
}

/// v50: destination/credential shadow of `project()`. Additive; no Key
/// material; no second quota-pool tables; no `observations`.
pub(super) fn migrate_to_v50(conn: &Connection) -> Result<()> {
    if super::lifecycle::schema_version_on(conn)? >= 50 {
        return Ok(());
    }
    let tx = Transaction::new_unchecked(conn, TransactionBehavior::Immediate)?;
    let version = super::lifecycle::schema_version_on(&tx)?;
    if version >= 50 {
        return Ok(());
    }
    anyhow::ensure!(version == 49, "v50 requires schema v49");
    tx.execute_batch(
        "CREATE TABLE IF NOT EXISTS destinations (
            id TEXT PRIMARY KEY,
            legacy_kind TEXT NOT NULL CHECK(legacy_kind IN ('builtin','dynamic','custom_account','platform_parent')),
            legacy_id TEXT NOT NULL,
            adapter TEXT NOT NULL,
            name TEXT NOT NULL,
            brand_family TEXT,
            base_url TEXT,
            protocols_json TEXT NOT NULL,
            auth_scheme TEXT NOT NULL,
            capabilities_json TEXT NOT NULL,
            plan_json TEXT,
            max_credentials INTEGER,
            observer_credential_id TEXT,
            enabled INTEGER NOT NULL,
            UNIQUE(legacy_kind, legacy_id)
        );
        CREATE TABLE IF NOT EXISTS destination_models (
            destination_id TEXT NOT NULL,
            public_model TEXT NOT NULL,
            public_model_key TEXT NOT NULL,
            upstream_model TEXT NOT NULL,
            protocols_json TEXT NOT NULL,
            preferred TEXT,
            enabled INTEGER NOT NULL,
            PRIMARY KEY (destination_id, public_model_key)
        );
        CREATE TABLE IF NOT EXISTS credentials (
            id TEXT PRIMARY KEY,
            legacy_account_id TEXT NOT NULL UNIQUE,
            destination_id TEXT NOT NULL,
            name TEXT NOT NULL,
            notes TEXT,
            has_secret INTEGER NOT NULL,
            enabled INTEGER NOT NULL,
            routing_rank INTEGER NOT NULL,
            scope_json TEXT NOT NULL,
            auth_state TEXT NOT NULL,
            last_error TEXT,
            cooldown_generic_until TEXT,
            cooldown_5h_until TEXT,
            cooldown_week_until TEXT,
            cooldown_month_until TEXT,
            cooldown_free_until TEXT,
            quota_pool_id TEXT,
            onboarding_json TEXT,
            purchase_date TEXT,
            username TEXT,
            referral_code TEXT,
            cooldown_until TEXT,
            created_at TEXT,
            updated_at TEXT,
            auth_error TEXT,
            account_type TEXT,
            setup_step TEXT,
            provider_id TEXT,
            credential_kind TEXT,
            quota_scope TEXT,
            identity_id TEXT,
            verification_status TEXT,
            connection_verified_at TEXT,
            verification_error TEXT,
            usage_5h_window_started_at TEXT,
            usage_5h_window_cost_offset REAL NOT NULL DEFAULT 0,
            usage_week_window_started_at TEXT,
            usage_week_window_cost_offset REAL NOT NULL DEFAULT 0,
            usage_month_window_cost_offset REAL NOT NULL DEFAULT 0
        );
        CREATE TABLE IF NOT EXISTS credential_grants (
            credential_id TEXT NOT NULL,
            kind TEXT NOT NULL CHECK(kind IN ('endpoint_id','origin')),
            value TEXT NOT NULL,
            PRIMARY KEY (credential_id, kind, value)
        );",
    )?;
    tx.execute_batch("INSERT OR REPLACE INTO schema_version(version) VALUES (50);")?;
    tx.commit()?;
    Ok(())
}

/// v51: credentials become the secret store. Copy Key/password ciphertext
/// from `accounts` via `legacy_account_id`. Additive; no pre-migration backup.
/// The `accounts` table remains until remaining readers move off it.
pub(super) fn migrate_to_v51(conn: &Connection) -> Result<()> {
    if super::lifecycle::schema_version_on(conn)? >= 51 {
        return Ok(());
    }
    let tx = Transaction::new_unchecked(conn, TransactionBehavior::Immediate)?;
    let version = super::lifecycle::schema_version_on(&tx)?;
    if version >= 51 {
        return Ok(());
    }
    anyhow::ensure!(version == 50, "v51 requires schema v50");
    if !table_has_column(&tx, "credentials", "key_cipher")? {
        tx.execute_batch("ALTER TABLE credentials ADD COLUMN key_cipher TEXT NOT NULL DEFAULT ''")?;
    }
    if !table_has_column(&tx, "credentials", "password_cipher")? {
        tx.execute_batch("ALTER TABLE credentials ADD COLUMN password_cipher TEXT")?;
    }
    if table_exists(&tx, "accounts")? {
        tx.execute_batch(
            "UPDATE credentials
             SET key_cipher = COALESCE(
                    (SELECT key_cipher FROM accounts WHERE accounts.id = credentials.legacy_account_id),
                    key_cipher
                 ),
                 password_cipher = COALESCE(
                    (SELECT password_cipher FROM accounts WHERE accounts.id = credentials.legacy_account_id),
                    password_cipher
                 )
             WHERE EXISTS (
                SELECT 1 FROM accounts WHERE accounts.id = credentials.legacy_account_id
             );",
        )?;
    }
    tx.execute_batch("INSERT OR REPLACE INTO schema_version(version) VALUES (51);")?;
    tx.commit()?;
    Ok(())
}

/// v52: credentials become the account-row store. Copy remaining Account
/// fields from `accounts`, rebuild the destination shadow while that table
/// still exists, refuse the drop if any account lacks a credential row, then
/// `DROP TABLE accounts`. Child-table FKs that pointed at `accounts` are
/// rewritten in place. Does not drop providers (those drop in v56), platform
/// leftovers (those drop in v54), CPA (that drops in v55), or identity
/// satellites.
pub(super) fn migrate_to_v52(db: &Database) -> Result<()> {
    if super::lifecycle::schema_version_on(&db.conn)? >= 52 {
        if !account_store::accounts_table_exists(&db.conn)? {
            with_foreign_keys_off(&db.conn, || {
                account_store::drop_accounts_foreign_keys(&db.conn)?;
                Ok(())
            })?;
        }
        return Ok(());
    }
    account_store::ensure_v52_credential_columns(&db.conn)?;
    if account_store::accounts_table_exists(&db.conn)? {
        let _ = crate::destination_projection::replace_persisted_on(db)?;
        account_store::backfill_credential_account_columns(&db.conn)?;
        account_store::assert_credential_totality(&db.conn)?;
        with_foreign_keys_off(&db.conn, || {
            account_store::drop_accounts_foreign_keys(&db.conn)?;
            account_store::drop_accounts_table(&db.conn)?;
            Ok(())
        })?;
    } else {
        with_foreign_keys_off(&db.conn, || {
            account_store::drop_accounts_foreign_keys(&db.conn)?;
            Ok(())
        })?;
    }
    db.conn
        .execute_batch("INSERT OR REPLACE INTO schema_version(version) VALUES (52);")?;
    Ok(())
}

/// v53: destinations + destination_models become the Custom HTTP store.
/// Backfill Custom destinations from leftover custom tables when those rows
/// can map, refuse if an unlinked leftover config/capability cannot map,
/// then `DROP TABLE account_custom_configs` and
/// `DROP TABLE account_model_capabilities`. Linked platform Keys stay on
/// leftover `platform_*` tables until v54. Does not drop providers (those
/// drop in v56), CPA (that drops in v55), or identity satellites.
pub(super) fn migrate_to_v53(conn: &Connection) -> Result<()> {
    if super::lifecycle::schema_version_on(conn)? >= 53 {
        return Ok(());
    }
    anyhow::ensure!(
        super::lifecycle::schema_version_on(conn)? == 52,
        "v53 requires schema v52"
    );
    custom_store::migrate_v53_backfill_and_drop(conn)?;
    conn.execute_batch("INSERT OR REPLACE INTO schema_version(version) VALUES (53);")?;
    Ok(())
}

/// v54: destinations + credentials become the platform parent/link store.
/// Backfill leftover parents/links when those rows can map, refuse if a
/// leftover parent/link cannot map, then `DROP TABLE platform_links` and
/// `DROP TABLE platform_accounts`. Does not drop providers (those drop in
/// v56), CPA (that drops in v55), or identity satellites.
pub(super) fn migrate_to_v54(conn: &Connection) -> Result<()> {
    if super::lifecycle::schema_version_on(conn)? >= 54 {
        platform::ensure_v54_columns(conn)?;
        return Ok(());
    }
    anyhow::ensure!(
        super::lifecycle::schema_version_on(conn)? == 53,
        "v54 requires schema v53"
    );
    platform::migrate_v54_backfill_and_drop(conn)?;
    conn.execute_batch("INSERT OR REPLACE INTO schema_version(version) VALUES (54);")?;
    Ok(())
}

/// v55: destinations + credentials become the CPA integration store.
/// Backfill leftover `cpa_integration` when that row can map, refuse if it
/// cannot, then `DROP TABLE cpa_integration`. Does not invent a CPA
/// destination when leftover is absent. Does not drop providers (those drop
/// in v56), identity satellites, or `provider_model_catalogs`.
pub(super) fn migrate_to_v55(conn: &Connection) -> Result<()> {
    if super::lifecycle::schema_version_on(conn)? >= 55 {
        conn.execute_batch("DROP TABLE IF EXISTS cpa_integration;")?;
        return Ok(());
    }
    anyhow::ensure!(
        super::lifecycle::schema_version_on(conn)? == 54,
        "v55 requires schema v54"
    );
    cpa::migrate_v55_backfill_and_drop(conn)?;
    conn.execute_batch("INSERT OR REPLACE INTO schema_version(version) VALUES (55);")?;
    Ok(())
}

/// v56: destinations + destination_models become the user-defined Provider
/// store. Backfill leftover `origin IN ('preset','custom')` rows when they
/// can map, refuse empty keyed HTTP URLs or unknown adapters, then
/// `DROP TABLE provider_models` and `DROP TABLE providers`. Builtin seed
/// rows stay sealed catalog. Does not drop identity satellites,
/// `quota_pools`, `provider_model_catalogs`, or `dashboard_operations`.
pub(super) fn migrate_to_v56(conn: &Connection) -> Result<()> {
    if super::lifecycle::schema_version_on(conn)? >= 56 {
        conn.execute_batch(
            "DROP TABLE IF EXISTS provider_models;
             DROP TABLE IF EXISTS providers;",
        )?;
        dynamic_store::ensure_v56_columns(conn)?;
        return Ok(());
    }
    anyhow::ensure!(
        super::lifecycle::schema_version_on(conn)? == 55,
        "v56 requires schema v55"
    );
    dynamic_store::migrate_v56_backfill_and_drop(conn)?;
    conn.execute_batch("INSERT OR REPLACE INTO schema_version(version) VALUES (56);")?;
    Ok(())
}

/// v57: credentials + `credential_grants` become the identity / binding /
/// onboarding / subscription store. Copy leftover satellites when they can
/// map, refuse unmappable leftovers, then drop `upstream_identities`,
/// `credential_state`, `credential_bindings`, `legacy_identity_map`,
/// `onboarding_tasks`, and `subscription_records`. Keeps `quota_pools` /
/// `quota_pool_members`. Does not invent identities on leftover-absent DBs.
pub(super) fn migrate_to_v57(conn: &Connection) -> Result<()> {
    if super::lifecycle::schema_version_on(conn)? >= 57 {
        identity_v57::drop_leftover_identity_tables(conn)?;
        identity_v57::ensure_v57_columns(conn)?;
        return Ok(());
    }
    anyhow::ensure!(
        super::lifecycle::schema_version_on(conn)? == 56,
        "v57 requires schema v56"
    );
    identity_v57::migrate_v57_backfill_and_drop(conn)?;
    conn.execute_batch("INSERT OR REPLACE INTO schema_version(version) VALUES (57);")?;
    Ok(())
}

/// v58: Custom HTTP configuration becomes connection-owned. Existing Custom
/// destination and credential IDs stay in place; the old account id remains
/// only as the destination's legacy compatibility anchor. Multiple inference
/// credentials may now reference that destination. Model-name resolution is
/// persisted explicitly so legacy public-name-only behavior survives while
/// normal user-defined HTTP connections keep unique upstream-id lookup.
pub(super) fn migrate_to_v58(
    conn: &Connection,
    db_path: &Path,
    is_fresh: bool,
    backup_created: bool,
) -> Result<()> {
    let source_version = super::lifecycle::schema_version_on(conn)?;
    if source_version >= 58 {
        anyhow::ensure!(
            table_has_column(conn, "destinations", "model_resolution")?,
            "schema v58 is missing destinations.model_resolution"
        );
        return Ok(());
    }
    anyhow::ensure!(
        source_version == V57_SCHEMA_VERSION,
        "v58 requires schema v57"
    );
    if !is_fresh && !backup_created {
        create_pre_v58_backup(conn, db_path)?;
    }
    let tx = Transaction::new_unchecked(conn, TransactionBehavior::Immediate)?;
    let locked_version = super::lifecycle::schema_version_on(&tx)?;
    if locked_version >= 58 {
        return Ok(());
    }
    anyhow::ensure!(
        locked_version == V57_SCHEMA_VERSION,
        "v58 writer lock observed schema {locked_version}, expected {V57_SCHEMA_VERSION}"
    );
    ensure_v58_destination_column(&tx)?;
    tx.execute_batch(
        "UPDATE destinations
         SET model_resolution = CASE legacy_kind
             WHEN 'dynamic' THEN 'public_and_upstream'
             WHEN 'custom_account' THEN 'public_only'
             WHEN 'platform_parent' THEN 'public_only'
             ELSE 'adapter_defined'
         END;
         UPDATE destinations
         SET max_credentials = NULL
         WHERE legacy_kind = 'custom_account';
         INSERT OR REPLACE INTO schema_version(version) VALUES (58);",
    )?;
    tx.commit()?;
    Ok(())
}

/// Transitional compatibility for v52+ projection writers compiled with the
/// v58 Destination shape. The schema version remains unchanged; v58 performs
/// the authoritative backfill and version bump after the verified backup.
pub(super) fn ensure_v58_destination_column(conn: &Connection) -> Result<()> {
    if table_exists(conn, "destinations")?
        && !table_has_column(conn, "destinations", "model_resolution")?
    {
        conn.execute_batch(
            "ALTER TABLE destinations ADD COLUMN model_resolution TEXT NOT NULL DEFAULT 'adapter_defined';",
        )?;
    }
    Ok(())
}

/// Keep historical endpoint-grant identities as data. Platform Keys used a
/// per-credential connection identity, while shared HTTP Keys used the owner's
/// identity; neither distinction belongs in runtime route selection.
pub(super) fn migrate_to_v59(
    db: &Database,
    db_path: &Path,
    is_fresh: bool,
    backup_created: bool,
) -> Result<()> {
    let conn = &db.conn;
    if super::lifecycle::schema_version_on(conn)? >= 59 {
        anyhow::ensure!(
            table_has_column(conn, "credentials", "authorization_connection_id")?,
            "schema v59 is missing credentials.authorization_connection_id"
        );
        return Ok(());
    }
    if !is_fresh && !backup_created {
        create_pre_version_backup(conn, db_path, PRE_V59_BACKUP_FILE_PREFIX, 58)?;
    }
    let tx = Transaction::new_unchecked(conn, TransactionBehavior::Immediate)?;
    if !table_has_column(&tx, "credentials", "authorization_connection_id")? {
        tx.execute_batch("ALTER TABLE credentials ADD COLUMN authorization_connection_id TEXT;")?;
    }
    identity::backfill_authorization_connections_on(&tx)?;
    destination_store::migrate_custom_protocol_controls(db)?;
    tx.execute_batch("INSERT OR REPLACE INTO schema_version(version) VALUES (59);")?;
    tx.commit()?;
    Ok(())
}

/// v60: per-Key confirmed quota exhaustion and recovery wait. Additive; no
/// pre-migration backup. Ordinary cooldown columns are unchanged.
pub(super) fn migrate_to_v60(conn: &Connection) -> Result<()> {
    if super::lifecycle::schema_version_on(conn)? >= 60 {
        quota_recovery::ensure_column(conn)?;
        return Ok(());
    }
    anyhow::ensure!(
        super::lifecycle::schema_version_on(conn)? == 59,
        "v60 requires schema v59"
    );
    quota_recovery::ensure_column(conn)?;
    conn.execute_batch("INSERT OR REPLACE INTO schema_version(version) VALUES (60);")?;
    Ok(())
}

/// v61: local encrypted Step Plan observation sessions. Inference Keys and
/// routing state are unchanged; the nullable column needs no table rewrite.
pub(super) fn migrate_to_v61(conn: &Connection) -> Result<()> {
    if super::lifecycle::schema_version_on(conn)? >= 61 {
        let tx = conn.unchecked_transaction()?;
        ensure_column(&tx, "credentials", "stepfun_usage_json", "TEXT")?;
        tx.commit()?;
        return Ok(());
    }
    anyhow::ensure!(
        super::lifecycle::schema_version_on(conn)? == 60,
        "v61 requires schema v60"
    );
    let tx = conn.unchecked_transaction()?;
    ensure_column(&tx, "credentials", "stepfun_usage_json", "TEXT")?;
    tx.execute_batch("INSERT OR REPLACE INTO schema_version(version) VALUES (61);")?;
    tx.commit()?;
    Ok(())
}

/// v62: durable per-credential credit meters and per-request settlement receipts.
/// Historical v61 console sessions are retired; inference credentials are unchanged.
pub(super) fn migrate_to_v62(conn: &Connection) -> Result<()> {
    let tx = Transaction::new_unchecked(conn, TransactionBehavior::Immediate)?;
    let version = super::lifecycle::schema_version_on(&tx)?;
    anyhow::ensure!(version >= 61, "v62 requires schema v61");
    ensure_column(&tx, "credentials", "credit_meter_json", "TEXT")?;
    ensure_column(&tx, "forward_logs", "credit_receipt_json", "TEXT")?;
    tx.execute_batch("CREATE INDEX IF NOT EXISTS forward_logs_pending_credits
        ON forward_logs(json_extract(credit_receipt_json,'$.attempt.credentialId'),
                        json_extract(credit_receipt_json,'$.attempt.meterId'))
        WHERE credit_receipt_json IS NOT NULL AND json_extract(credit_receipt_json,'$.phase')='pending';")?;
    if version == 61 {
        tx.execute_batch(
            "UPDATE credentials SET stepfun_usage_json = NULL;
             INSERT OR REPLACE INTO schema_version(version) VALUES (62);",
        )?;
    }
    tx.commit()?;
    Ok(())
}

pub(super) fn migrate_to_v63(conn: &Connection) -> Result<()> {
    let version = super::lifecycle::schema_version_on(conn)?;
    anyhow::ensure!(version >= 62, "v63 requires schema v62");
    let tx = conn.unchecked_transaction()?;
    http_routes::ensure_storage_on(&tx)?;
    if version == 62 {
        tx.execute_batch("INSERT OR REPLACE INTO schema_version(version) VALUES (63);")?;
    }
    tx.commit()?;
    Ok(())
}

pub(super) fn migrate_to_v64(conn: &Connection) -> Result<()> {
    let version = super::lifecycle::schema_version_on(conn)?;
    if version >= 64 {
        return Ok(());
    }
    anyhow::ensure!(version == 63, "v64 requires schema v63");
    let tx = Transaction::new_unchecked(conn, TransactionBehavior::Immediate)?;
    dynamic_store::migrate_preset_public_model_leaves(&tx)?;
    tx.execute_batch("INSERT OR REPLACE INTO schema_version(version) VALUES (64);")?;
    tx.commit()?;
    Ok(())
}

fn migrate_v42_body(tx: &Transaction<'_>) -> Result<()> {
    let now = Utc::now().to_rfc3339();
    let v41_dynamic_providers_exists = table_exists(tx, "dynamic_providers")?;
    let v41_dynamic_models_exists = table_exists(tx, "dynamic_provider_models")?;
    let v41_preferences_exists = table_exists(tx, "provider_model_protocol_preferences")?;

    anyhow::ensure!(
        !table_exists(tx, "providers")? && !table_exists(tx, "provider_models")?,
        "v42 requires a canonical v41 source without unified provider tables"
    );
    anyhow::ensure!(
        !table_exists(tx, "provider_model_protocol_preferences_v42")?,
        "v42 requires a canonical v41 source without leftover provider_model_protocol_preferences_v42"
    );

    tx.execute_batch(
        "CREATE TABLE providers_new (
            id TEXT PRIMARY KEY,
            origin TEXT NOT NULL CHECK(origin IN ('builtin','preset','custom')),
            adapter_kind TEXT NOT NULL,
            name TEXT NOT NULL,
            endpoint_url TEXT,
            upstream_protocol TEXT,
            auth_kind TEXT,
            preset_id TEXT,
            offering TEXT NOT NULL DEFAULT 'api' CHECK(offering IN ('plan','api')),
            display_family TEXT,
            endpoint_per_account INTEGER NOT NULL DEFAULT 0,
            created_at TEXT NOT NULL,
            updated_at TEXT NOT NULL
         );
         CREATE TABLE provider_models_new (
            provider_id TEXT NOT NULL,
            public_model TEXT NOT NULL,
            public_model_key TEXT NOT NULL,
            upstream_model TEXT NOT NULL,
            upstream_override TEXT,
            PRIMARY KEY (provider_id, public_model_key),
            FOREIGN KEY (provider_id) REFERENCES providers(id) ON DELETE CASCADE
         );",
    )?;

    if v41_dynamic_providers_exists {
        copy_dynamic_providers_to_providers_new_v42(tx)?;
    }

    seed_builtin_providers_v42(tx, &now)?;

    if v41_dynamic_providers_exists {
        tx.execute_batch("DROP TABLE dynamic_providers;")?;
    }
    tx.execute_batch("ALTER TABLE providers_new RENAME TO providers;")?;

    if v41_dynamic_models_exists {
        tx.execute(
            "INSERT INTO provider_models_new
                (provider_id, public_model, public_model_key, upstream_model, upstream_override)
             SELECT provider_id, public_model, public_model_key, upstream_model, upstream_override
               FROM dynamic_provider_models",
            [],
        )?;
    }
    if v41_dynamic_models_exists {
        tx.execute_batch("DROP TABLE dynamic_provider_models;")?;
    }
    tx.execute_batch(
        "ALTER TABLE provider_models_new RENAME TO provider_models;
         CREATE INDEX IF NOT EXISTS idx_provider_models_provider
            ON provider_models(provider_id);",
    )?;

    if v41_preferences_exists {
        tx.execute_batch(
            "CREATE TABLE provider_model_protocol_preferences_v42 AS
                 SELECT provider_id, model_id, protocol
                   FROM provider_model_protocol_preferences;
             DROP TABLE provider_model_protocol_preferences;",
        )?;
        tx.execute_batch(
            "CREATE TABLE provider_model_protocol_preferences (
                provider_id TEXT NOT NULL,
                model_id TEXT NOT NULL,
                protocol TEXT NOT NULL CHECK(protocol IN ('chat_completions', 'messages')),
                PRIMARY KEY(provider_id, model_id)
             );
             INSERT INTO provider_model_protocol_preferences (provider_id, model_id, protocol)
             SELECT provider_id, model_id, protocol FROM provider_model_protocol_preferences_v42;
             DROP TABLE provider_model_protocol_preferences_v42;",
        )?;
    } else {
        tx.execute_batch(
            "CREATE TABLE provider_model_protocol_preferences (
                provider_id TEXT NOT NULL,
                model_id TEXT NOT NULL,
                protocol TEXT NOT NULL CHECK(protocol IN ('chat_completions', 'messages')),
                PRIMARY KEY(provider_id, model_id)
             );",
        )?;
    }

    Ok(())
}

fn copy_dynamic_providers_to_providers_new_v42(tx: &Transaction<'_>) -> Result<()> {
    let mut stmt = tx.prepare(
        "SELECT id, name, endpoint_url, upstream_protocol, auth_kind,
                preset_id, created_at, updated_at
           FROM dynamic_providers",
    )?;
    let rows = stmt.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, String>(3)?,
            row.get::<_, String>(4)?,
            row.get::<_, Option<String>>(5)?,
            row.get::<_, String>(6)?,
            row.get::<_, String>(7)?,
        ))
    })?;
    let mut collected = Vec::new();
    for row in rows {
        collected.push(row?);
    }
    drop(stmt);
    for (id, name, endpoint_url, upstream_protocol, auth_kind, preset_id, created_at, updated_at) in
        collected
    {
        let origin = if preset_id.is_some() {
            "preset"
        } else {
            "custom"
        };
        let offering = preset_id
            .as_deref()
            .map(ocg_domain::provider::preset_offering)
            .unwrap_or("api");
        tx.execute(
            "INSERT INTO providers_new
                (id, origin, adapter_kind, name, endpoint_url, upstream_protocol,
                 auth_kind, preset_id, offering, display_family, endpoint_per_account,
                 created_at, updated_at)
             VALUES (?1, ?2, 'configurable_http', ?3, ?4, ?5, ?6, ?7, ?8, NULL, 0, ?9, ?10)",
            params![
                id,
                origin,
                name,
                endpoint_url,
                upstream_protocol,
                auth_kind,
                preset_id,
                offering,
                created_at,
                updated_at,
            ],
        )?;
    }
    Ok(())
}

fn seed_builtin_providers_v42(tx: &Transaction<'_>, now: &str) -> Result<()> {
    use ocg_domain::provider::ProviderAdapterKind;
    type BuiltinProviderSeed = (
        &'static str,
        ProviderAdapterKind,
        &'static str,
        &'static str,
        &'static str,
        &'static str,
        &'static str,
        &'static str,
        i32,
    );
    let seeds: [BuiltinProviderSeed; 7] = [
        (
            OPENCODE_PROVIDER_ID,
            ProviderAdapterKind::OpenCodeGo,
            "OpenCode Go",
            OPENCODE_GO_BASE_URL,
            "chat_completions",
            "bearer",
            "plan",
            "OpenCode",
            0,
        ),
        (
            OPENCODE_ZEN_FREE_PROVIDER_ID,
            ProviderAdapterKind::ZenFree,
            "OpenCode Zen Free",
            OPENCODE_ZEN_BASE_URL,
            "chat_completions",
            "none",
            "api",
            "OpenCode",
            0,
        ),
        (
            COMMAND_CODE_PROVIDER_ID,
            ProviderAdapterKind::CommandCodeGoat,
            "Command Code GOAT",
            COMMAND_CODE_GOAT_BASE_URL,
            "chat_completions",
            "bearer",
            "plan",
            "Command Code",
            0,
        ),
        (
            MINIMAX_PROVIDER_ID,
            ProviderAdapterKind::MiniMaxCn,
            "MiniMax CN Token Plan",
            MINIMAX_CN_BASE_URL,
            "chat_completions",
            "bearer",
            "plan",
            "MiniMax",
            0,
        ),
        (
            KIMI_PROVIDER_ID,
            ProviderAdapterKind::KimiCn,
            "Kimi Code CN",
            KIMI_CN_BASE_URL,
            "chat_completions",
            "bearer",
            "plan",
            "Kimi",
            0,
        ),
        (
            OLLAMA_PROVIDER_ID,
            ProviderAdapterKind::OllamaCloud,
            "Ollama Cloud",
            OLLAMA_CLOUD_BASE_URL,
            "chat_completions",
            "bearer",
            "plan",
            "Ollama",
            0,
        ),
        (
            CUSTOM_PROVIDER_ID,
            ProviderAdapterKind::ConfigurableHttp,
            "Custom API",
            "",
            "",
            "bearer",
            "api",
            "Custom",
            1,
        ),
    ];
    for (
        id,
        kind,
        name,
        endpoint_url,
        upstream_protocol,
        auth_kind,
        offering,
        display_family,
        endpoint_per_account,
    ) in seeds
    {
        let endpoint_url = if endpoint_url.is_empty() {
            None
        } else {
            Some(endpoint_url)
        };
        let upstream_protocol = if upstream_protocol.is_empty() {
            None
        } else {
            Some(upstream_protocol)
        };
        tx.execute(
            "INSERT INTO providers_new
                (id, origin, adapter_kind, name, endpoint_url, upstream_protocol,
                 auth_kind, preset_id, offering, display_family, endpoint_per_account,
                 created_at, updated_at)
             VALUES (?1, 'builtin', ?2, ?3, ?4, ?5, ?6, NULL, ?7, ?8, ?9, ?10, ?10)",
            params![
                id,
                kind.as_str(),
                name,
                endpoint_url,
                upstream_protocol,
                auth_kind,
                offering,
                display_family,
                endpoint_per_account,
                now,
            ],
        )?;
    }
    Ok(())
}

/// v39 preserves template provenance independently of routing configuration.
pub(super) fn migrate_to_v39(conn: &Connection) -> Result<()> {
    if super::lifecycle::schema_version_on(conn)? >= 39 {
        return Ok(());
    }
    let tx = conn.unchecked_transaction()?;
    let version = super::lifecycle::schema_version_on(&tx)?;
    if version >= 39 {
        return Ok(());
    }
    anyhow::ensure!(version == 38, "v39 requires schema v38");
    ensure_column(&tx, "dynamic_providers", "preset_id", "TEXT")?;
    tx.execute_batch("INSERT OR REPLACE INTO schema_version(version) VALUES(39);")?;
    tx.commit()?;
    Ok(())
}

/// v35: collapse provider/offering identity onto provider_id. Historical
/// v1–v34 migrations keep the offering_id column name; this rewrite is the
/// first schema that omits it.
pub(super) fn migrate_to_v35(conn: &Connection, db_path: &Path, is_fresh: bool) -> Result<()> {
    let version = super::lifecycle::schema_version_on(conn)?;
    if version >= 35 {
        return Ok(());
    }
    anyhow::ensure!(
        version == V34_SCHEMA_VERSION,
        "v35 requires a canonical schema v34 source, found {version}"
    );
    if !is_fresh {
        create_pre_v35_backup(conn, db_path)?;
    }
    preflight_v35_identity(conn)?;
    with_foreign_keys_off(conn, || {
        let tx = Transaction::new_unchecked(conn, TransactionBehavior::Immediate)?;
        let version_locked = super::lifecycle::schema_version_on(&tx)?;
        if version_locked >= 35 {
            tx.rollback()?;
            return Ok(());
        }
        anyhow::ensure!(
            version_locked == V34_SCHEMA_VERSION,
            "v35 writer lock observed schema {version_locked}, expected {V34_SCHEMA_VERSION}"
        );
        preflight_v35_identity(&tx)?;
        migrate_v35_body(&tx)?;
        tx.execute_batch("INSERT OR REPLACE INTO schema_version (version) VALUES (35);")?;
        tx.commit()?;
        Ok(())
    })
}

/// v36: additive Ollama Cloud Cookie-usage state. One row per account holds
/// the obfuscated browser-session Cookie and the last-good sanitized snapshot.
/// Failures update status columns and never clear the snapshot. The row
/// cascades with the account.
pub(super) fn migrate_to_v36(conn: &Connection) -> Result<()> {
    let version = super::lifecycle::schema_version_on(conn)?;
    if version >= 36 {
        return Ok(());
    }
    anyhow::ensure!(
        version == 35,
        "v36 requires a canonical schema v35 source, found {version}"
    );
    let tx = conn.unchecked_transaction()?;
    tx.execute_batch(
        "CREATE TABLE IF NOT EXISTS ollama_cloud_usage_state (
            account_id TEXT PRIMARY KEY,
            cookie_cipher TEXT,
            status TEXT NOT NULL DEFAULT 'unconfigured',
            snapshot TEXT,
            last_error TEXT,
            last_success_at TEXT,
            last_attempt_at TEXT,
            next_eligible_at TEXT,
            failure_streak INTEGER NOT NULL DEFAULT 0,
            FOREIGN KEY (account_id) REFERENCES accounts(id) ON DELETE CASCADE
        );
        INSERT OR REPLACE INTO schema_version (version) VALUES (36);",
    )?;
    tx.commit()?;
    Ok(())
}

/// v37: drop the unreleased Cookie-usage scrape table and add the account
/// billing-tier side table. Existing Ollama accounts stay routeable with no
/// row (unconfigured). Account Keys and logs are untouched.
pub(super) fn migrate_to_v37(conn: &Connection) -> Result<()> {
    let version = super::lifecycle::schema_version_on(conn)?;
    if version >= 37 {
        return Ok(());
    }
    anyhow::ensure!(
        version == 36,
        "v37 requires a canonical schema v36 source, found {version}"
    );
    let tx = conn.unchecked_transaction()?;
    tx.execute_batch(
        "DROP TABLE IF EXISTS ollama_cloud_usage_state;
        CREATE TABLE IF NOT EXISTS ollama_cloud_billing (
            account_id TEXT PRIMARY KEY,
            billing_tier TEXT NOT NULL CHECK (billing_tier IN ('pro', 'max', 'team')),
            FOREIGN KEY (account_id) REFERENCES accounts(id) ON DELETE CASCADE
        );
        INSERT OR REPLACE INTO schema_version (version) VALUES (37);",
    )?;
    tx.commit()?;
    Ok(())
}

pub(super) fn migrate_baseline(db: &Database) -> Result<()> {
    db.conn.execute(
        "CREATE TABLE IF NOT EXISTS schema_version (
            version INTEGER PRIMARY KEY
        )",
        [],
    )?;

    let tx = db.conn.unchecked_transaction()?;
    let mut version: i32 = tx
        .query_row(
            "SELECT version FROM schema_version ORDER BY version DESC LIMIT 1",
            [],
            |row| row.get(0),
        )
        .unwrap_or(0);
    anyhow::ensure!(
        version <= CURRENT_SCHEMA_VERSION,
        "database schema version {version} is newer than this build supports ({CURRENT_SCHEMA_VERSION}); restore a matching data directory and encryption key"
    );

    // 修复：v1.4.2 -> v1.5.0 升级时，旧 v9 migration（HEAD 固定窗口）只添加了
    // usage_*_window_* 列，没有添加 upstream v9 的 cost_state 等 forward_logs 列。
    // 检测 cost_state 列是否存在，不存在则把 version 回退到 8，让 v9/v10/v11
    // 重跑（v9/v11 已改成幂等，不会因列已存在而报错）。
    let has_cost_state = {
        let mut stmt = tx.prepare("PRAGMA table_info(forward_logs)")?;
        let columns = stmt.query_map([], |row| row.get::<_, String>(1))?;
        columns
            .collect::<rusqlite::Result<Vec<_>>>()?
            .iter()
            .any(|existing| existing == "cost_state")
    };
    if !has_cost_state && version >= 9 {
        version = 8;
    }

    if version < 1 {
        tx.execute_batch(
            "CREATE TABLE IF NOT EXISTS accounts (
                id TEXT PRIMARY KEY,
                name TEXT NOT NULL,
                key_cipher TEXT NOT NULL,
                enabled INTEGER NOT NULL DEFAULT 1,
                referral_code TEXT,
                recharge_date TEXT NOT NULL,
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL
            );
            CREATE TABLE IF NOT EXISTS settings (
                key TEXT PRIMARY KEY,
                value TEXT NOT NULL
            );
            CREATE TABLE IF NOT EXISTS gateway_logs (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                level TEXT NOT NULL,
                category TEXT NOT NULL,
                message TEXT NOT NULL,
                created_at TEXT NOT NULL
            );
            CREATE TABLE IF NOT EXISTS forward_logs (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                timestamp TEXT NOT NULL,
                model TEXT NOT NULL,
                account_id TEXT NOT NULL,
                account_name TEXT NOT NULL,
                status TEXT NOT NULL,
                http_status INTEGER,
                prompt_tokens INTEGER NOT NULL DEFAULT 0,
                completion_tokens INTEGER NOT NULL DEFAULT 0,
                cached_tokens INTEGER NOT NULL DEFAULT 0,
                cost REAL NOT NULL DEFAULT 0,
                error_message TEXT
            );
            CREATE INDEX IF NOT EXISTS idx_forward_logs_time ON forward_logs(timestamp);
            CREATE INDEX IF NOT EXISTS idx_forward_logs_account ON forward_logs(account_id);
            INSERT OR REPLACE INTO schema_version (version) VALUES (1);
        ",
        )?;
    }

    if version < 2 {
        // v2: per-account rate-limit cooldown (parsed from upstream 429 body).
        // Two nullable columns; no new table — account count is tiny, avoids a JOIN.
        tx.execute_batch(
            "ALTER TABLE accounts ADD COLUMN cooldown_until TEXT;
            ALTER TABLE accounts ADD COLUMN last_error TEXT;
            INSERT OR REPLACE INTO schema_version (version) VALUES (2);",
        )?;
    }

    if version < 3 {
        tx.execute_batch(
            "ALTER TABLE accounts ADD COLUMN username TEXT;
            ALTER TABLE accounts ADD COLUMN password_cipher TEXT;
            INSERT OR REPLACE INTO schema_version (version) VALUES (3);",
        )?;
    }

    if version < 4 {
        tx.execute_batch(
            "ALTER TABLE accounts ADD COLUMN usage_5h_baseline_percent REAL CHECK (usage_5h_baseline_percent BETWEEN 0 AND 100);
            ALTER TABLE accounts ADD COLUMN usage_5h_anchor_success_cost REAL CHECK (usage_5h_anchor_success_cost >= 0);
            ALTER TABLE accounts ADD COLUMN usage_week_baseline_percent REAL CHECK (usage_week_baseline_percent BETWEEN 0 AND 100);
            ALTER TABLE accounts ADD COLUMN usage_week_anchor_success_cost REAL CHECK (usage_week_anchor_success_cost >= 0);
            ALTER TABLE accounts ADD COLUMN usage_month_baseline_percent REAL CHECK (usage_month_baseline_percent BETWEEN 0 AND 100);
            ALTER TABLE accounts ADD COLUMN usage_month_anchor_success_cost REAL CHECK (usage_month_anchor_success_cost >= 0);
            INSERT OR REPLACE INTO schema_version (version) VALUES (4);",
        )?;
    }

    if version < 5 {
        tx.execute(
            "ALTER TABLE accounts ADD COLUMN sort_order INTEGER NOT NULL DEFAULT 0",
            [],
        )?;

        let accounts = {
            let mut stmt = tx.prepare(
                "SELECT id, recharge_date, created_at
                 FROM accounts
                 ORDER BY created_at ASC, id ASC",
            )?;
            let rows = stmt.query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, Option<String>>(1)?,
                    row.get::<_, String>(2)?,
                ))
            })?;
            rows.collect::<rusqlite::Result<Vec<_>>>()?
        };

        for (sort_order, (id, recharge_date, created_at)) in accounts.into_iter().enumerate() {
            let purchase_date = match recharge_date {
                Some(value) if normalize_purchase_date(&value).is_ok() => value,
                _ => migration_fallback_purchase_date(&created_at)?,
            };
            tx.execute(
                "UPDATE accounts
                 SET recharge_date = ?1, sort_order = ?2
                 WHERE id = ?3",
                params![purchase_date, sort_order as i64, id],
            )?;
        }

        tx.execute(
            "INSERT OR REPLACE INTO schema_version (version) VALUES (5)",
            [],
        )?;
    }

    if version < 6 {
        tx.execute_batch(
            "CREATE INDEX IF NOT EXISTS idx_forward_logs_model ON forward_logs(model);
            CREATE INDEX IF NOT EXISTS idx_forward_logs_status ON forward_logs(status);
            INSERT OR REPLACE INTO schema_version (version) VALUES (6)",
        )?;
    }

    if version < 7 {
        for column in [
            "cooldown_generic_until",
            "cooldown_5h_until",
            "cooldown_week_until",
            "cooldown_month_until",
            // compute_cooldown_until also reads free; ensure before recompute.
            "cooldown_free_until",
        ] {
            ensure_column(&tx, "accounts", column, "TEXT")?;
        }
        tx.execute_batch(
            "CREATE INDEX IF NOT EXISTS idx_forward_logs_model ON forward_logs(model);
            CREATE INDEX IF NOT EXISTS idx_forward_logs_status ON forward_logs(status);
            CREATE INDEX IF NOT EXISTS idx_forward_logs_time_instant
                ON forward_logs(julianday(timestamp));
            UPDATE accounts
            SET cooldown_generic_until = COALESCE(cooldown_generic_until, CASE
                    WHEN lower(COALESCE(last_error, '')) LIKE '%5-hour usage limit%'
                      OR lower(COALESCE(last_error, '')) LIKE '%5 hour usage limit%'
                      OR lower(COALESCE(last_error, '')) LIKE '%weekly usage limit%'
                      OR lower(COALESCE(last_error, '')) LIKE '%monthly usage limit%'
                    THEN NULL ELSE cooldown_until END),
                cooldown_5h_until = COALESCE(cooldown_5h_until, CASE
                    WHEN lower(COALESCE(last_error, '')) LIKE '%5-hour usage limit%'
                      OR lower(COALESCE(last_error, '')) LIKE '%5 hour usage limit%'
                    THEN cooldown_until ELSE NULL END),
                cooldown_week_until = COALESCE(cooldown_week_until, CASE
                    WHEN lower(COALESCE(last_error, '')) LIKE '%weekly usage limit%'
                    THEN cooldown_until ELSE NULL END),
                cooldown_month_until = COALESCE(cooldown_month_until, CASE
                    WHEN lower(COALESCE(last_error, '')) LIKE '%monthly usage limit%'
                    THEN cooldown_until ELSE NULL END)
            WHERE cooldown_until IS NOT NULL;",
        )?;

        let account_ids = {
            let mut stmt = tx.prepare("SELECT id FROM accounts")?;
            let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
            rows.collect::<rusqlite::Result<Vec<_>>>()?
        };
        let now = Utc::now().to_rfc3339();
        for id in account_ids {
            let cooldown = super::usage_store::compute_cooldown_until(&tx, &id, &now)?;
            tx.execute(
                "UPDATE accounts SET cooldown_until = ?2 WHERE id = ?1",
                params![id, cooldown],
            )?;
        }
        tx.execute(
            "INSERT OR REPLACE INTO schema_version (version) VALUES (7)",
            [],
        )?;
    }

    if version < 8 {
        // Older binaries can still write NULL or otherwise invalid purchase dates after the
        // v5 backfill has already run. Repair those rows so current account reads stay valid.
        let accounts = {
            let mut stmt = tx.prepare(
                "SELECT id, recharge_date, created_at
                 FROM accounts",
            )?;
            let rows = stmt.query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, Option<String>>(1)?,
                    row.get::<_, String>(2)?,
                ))
            })?;
            rows.collect::<rusqlite::Result<Vec<_>>>()?
        };

        for (id, recharge_date, created_at) in accounts {
            let needs_repair = match recharge_date.as_deref() {
                Some(value) => normalize_purchase_date(value).is_err(),
                None => true,
            };
            if needs_repair {
                let purchase_date = migration_fallback_purchase_date(&created_at)?;
                tx.execute(
                    "UPDATE accounts SET recharge_date = ?1 WHERE id = ?2",
                    params![purchase_date, id],
                )?;
            }
        }

        tx.execute(
            "INSERT OR REPLACE INTO schema_version (version) VALUES (8)",
            [],
        )?;
    }

    if version < 9 {
        tx.execute_batch(
            "CREATE TABLE IF NOT EXISTS pricing_snapshots (
                revision TEXT PRIMARY KEY,
                activated_at TEXT NOT NULL,
                document_updated_at TEXT NOT NULL,
                source_url TEXT NOT NULL,
                content_hash TEXT NOT NULL,
                snapshot_json TEXT NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_pricing_snapshots_activated
                ON pricing_snapshots(activated_at DESC);",
        )?;
        ensure_column(&tx, "forward_logs", "pricing_revision_id", "TEXT")?;
        ensure_column(&tx, "forward_logs", "quota_multiplier", "REAL")?;
        ensure_column(&tx, "forward_logs", "local_adjustment_multiplier", "REAL")?;
        ensure_column(
            &tx,
            "forward_logs",
            "cache_creation_tokens",
            "INTEGER NOT NULL DEFAULT 0",
        )?;
        ensure_column(&tx, "forward_logs", "service_tier", "TEXT")?;
        ensure_column(
            &tx,
            "forward_logs",
            "cost_state",
            "TEXT NOT NULL DEFAULT 'not_applicable'",
        )?;
        tx.execute_batch(
            "UPDATE forward_logs SET cost_state = CASE
                WHEN status = 'success' THEN 'legacy_estimate'
                WHEN status = 'error' AND cost > 0 THEN 'legacy_estimate'
                WHEN status = 'success_no_usage' THEN 'usage_missing'
                WHEN status = 'success_unpriced' THEN 'unpriced'
                WHEN status = 'outcome_unknown' THEN 'outcome_unknown'
                ELSE 'not_applicable'
            END;
            INSERT OR REPLACE INTO schema_version (version) VALUES (9);",
        )?;
    }

    if version < 10 {
        // Repair databases that already ran the original v9 migration, which
        // classified charged response-conversion failures as not applicable.
        tx.execute_batch(
            "UPDATE forward_logs
             SET cost_state = 'legacy_estimate'
             WHERE status = 'error'
               AND cost > 0
               AND cost_state = 'not_applicable';
             INSERT OR REPLACE INTO schema_version (version) VALUES (10);",
        )?;
    }

    if version < 11 {
        // v11: 用固定窗口替代滚动窗口 + baseline 机制。
        // 5h/周窗口记一条"窗口起点时间戳"和"起点用量偏移"（手动校准用）。
        // 月窗口无新列：起点 = purchase_date 00:00，终点 = purchase_expires_on(purchase_date) 00:00。
        // 旧的 6 个 baseline 列保留不读不写，避免 DROP COLUMN 迁移风险。
        ensure_column(&tx, "accounts", "usage_5h_window_started_at", "TEXT")?;
        ensure_column(
            &tx,
            "accounts",
            "usage_5h_window_cost_offset",
            "REAL NOT NULL DEFAULT 0 CHECK (usage_5h_window_cost_offset >= 0)",
        )?;
        ensure_column(&tx, "accounts", "usage_week_window_started_at", "TEXT")?;
        ensure_column(
            &tx,
            "accounts",
            "usage_week_window_cost_offset",
            "REAL NOT NULL DEFAULT 0 CHECK (usage_week_window_cost_offset >= 0)",
        )?;
        tx.execute(
            "INSERT OR REPLACE INTO schema_version (version) VALUES (11)",
            [],
        )?;
    }

    if version < 12 {
        // v12:
        // - 重建 accounts 表去掉 usage_5h/week_window_cost_offset 的 CHECK (>= 0) 约束。
        //   SQLite 不支持 ALTER TABLE DROP CONSTRAINT，必须 rename + create + copy + drop。
        //   允许手动校准时 offset 为负数（target_cost < actual_cost 的情况），避免向左拉
        //   滑块时锁死在实际 cost 对应的百分比（Bug 1.5）。
        // - 新增 usage_month_window_cost_offset 列（无 CHECK），支持月窗口手动校准。
        let needs_rebuild: bool = {
            let sql: String = tx
                .query_row(
                    "SELECT sql FROM sqlite_master WHERE type='table' AND name='accounts'",
                    [],
                    |row| row.get(0),
                )
                .unwrap_or_default();
            sql.contains("usage_5h_window_cost_offset >= 0")
                || sql.contains("usage_week_window_cost_offset >= 0")
                || !sql.contains("usage_month_window_cost_offset")
        };
        if needs_rebuild {
            tx.execute_batch("PRAGMA foreign_keys=OFF;")?;
            tx.execute_batch("ALTER TABLE accounts RENAME TO accounts_v11_backup;")?;
            tx.execute_batch(
                "CREATE TABLE accounts (
                    id TEXT PRIMARY KEY,
                    name TEXT NOT NULL,
                    username TEXT,
                    password_cipher TEXT,
                    key_cipher TEXT NOT NULL,
                    enabled INTEGER NOT NULL DEFAULT 1,
                    referral_code TEXT,
                    recharge_date TEXT NOT NULL,
                    cooldown_until TEXT,
                    cooldown_generic_until TEXT,
                    cooldown_5h_until TEXT,
                    cooldown_week_until TEXT,
                    cooldown_month_until TEXT,
                    last_error TEXT,
                    usage_5h_baseline_percent REAL,
                    usage_5h_anchor_success_cost REAL,
                    usage_week_baseline_percent REAL,
                    usage_week_anchor_success_cost REAL,
                    usage_month_baseline_percent REAL,
                    usage_month_anchor_success_cost REAL,
                    sort_order INTEGER NOT NULL DEFAULT 0,
                    usage_5h_window_started_at TEXT,
                    usage_5h_window_cost_offset REAL NOT NULL DEFAULT 0,
                    usage_week_window_started_at TEXT,
                    usage_week_window_cost_offset REAL NOT NULL DEFAULT 0,
                    usage_month_window_cost_offset REAL NOT NULL DEFAULT 0,
                    created_at TEXT NOT NULL,
                    updated_at TEXT NOT NULL
                );",
            )?;
            // accounts_v11_backup 不含 usage_month_window_cost_offset 列，用字面量 0
            // 填充（NOT NULL DEFAULT 0 列拒绝显式 NULL，所以不能写 NULL）。
            tx.execute_batch(
                "INSERT INTO accounts (
                    id, name, username, password_cipher, key_cipher, enabled, referral_code,
                    recharge_date, cooldown_until, cooldown_generic_until, cooldown_5h_until,
                    cooldown_week_until, cooldown_month_until, last_error,
                    usage_5h_baseline_percent, usage_5h_anchor_success_cost,
                    usage_week_baseline_percent, usage_week_anchor_success_cost,
                    usage_month_baseline_percent, usage_month_anchor_success_cost,
                    sort_order, usage_5h_window_started_at, usage_5h_window_cost_offset,
                    usage_week_window_started_at, usage_week_window_cost_offset,
                    usage_month_window_cost_offset, created_at, updated_at
                )
                SELECT
                    id, name, username, password_cipher, key_cipher, enabled, referral_code,
                    recharge_date, cooldown_until, cooldown_generic_until, cooldown_5h_until,
                    cooldown_week_until, cooldown_month_until, last_error,
                    usage_5h_baseline_percent, usage_5h_anchor_success_cost,
                    usage_week_baseline_percent, usage_week_anchor_success_cost,
                    usage_month_baseline_percent, usage_month_anchor_success_cost,
                    sort_order, usage_5h_window_started_at, usage_5h_window_cost_offset,
                    usage_week_window_started_at, usage_week_window_cost_offset,
                    0, created_at, updated_at
                FROM accounts_v11_backup;
                DROP TABLE accounts_v11_backup;
                PRAGMA foreign_keys=ON;",
            )?;
        } else {
            // 已重建过的库只需补 usage_month_window_cost_offset 列。
            ensure_column(
                &tx,
                "accounts",
                "usage_month_window_cost_offset",
                "REAL NOT NULL DEFAULT 0",
            )?;
        }
        tx.execute(
            "INSERT OR REPLACE INTO schema_version (version) VALUES (12)",
            [],
        )?;
    }

    if version < 13 {
        // v13 preserves manual calibrations from the old rolling-window
        // baseline model. Anchor fixed windows at the migration instant so
        // already-counted logs are not charged twice, then let new logs
        // accumulate normally from that point onward.
        let limits = tx
            .query_row(
                "SELECT snapshot_json FROM pricing_snapshots
                 ORDER BY activated_at DESC LIMIT 1",
                [],
                |row| row.get::<_, String>(0),
            )
            .optional()?
            .map(|json| serde_json::from_str::<PricingSnapshot>(&json))
            .transpose()?
            .map(|snapshot| snapshot.limits)
            .unwrap_or(SEED_LIMITS);
        migrate_legacy_usage_baselines(&tx, &limits, Utc::now())?;
        tx.execute(
            "INSERT OR REPLACE INTO schema_version (version) VALUES (13)",
            [],
        )?;
    }

    if version < 14 {
        // Some early development databases (and their migration fixtures) did not
        // yet contain the optional runtime log table. Recreate its stable base shape
        // before adding diagnostic columns so upgrades remain repairable.
        tx.execute_batch(
            "CREATE TABLE IF NOT EXISTS gateway_logs (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                level TEXT NOT NULL,
                category TEXT NOT NULL,
                message TEXT NOT NULL,
                created_at TEXT NOT NULL
            );",
        )?;
        for (table, columns) in [
            (
                "forward_logs",
                [
                    ("request_id", "TEXT"),
                    ("attempt", "INTEGER"),
                    ("error_source", "TEXT"),
                    ("error_stage", "TEXT"),
                    ("duration_ms", "INTEGER"),
                    ("diagnostic_json", "TEXT"),
                ],
            ),
            (
                "gateway_logs",
                [
                    ("request_id", "TEXT"),
                    ("attempt", "INTEGER"),
                    ("error_source", "TEXT"),
                    ("error_stage", "TEXT"),
                    ("duration_ms", "INTEGER"),
                    ("diagnostic_json", "TEXT"),
                ],
            ),
        ] {
            for (column, definition) in columns {
                ensure_column(&tx, table, column, definition)?;
            }
        }
        tx.execute_batch(
            "CREATE INDEX IF NOT EXISTS idx_forward_logs_request_id
                ON forward_logs(request_id);
             CREATE INDEX IF NOT EXISTS idx_gateway_logs_request_id
                ON gateway_logs(request_id);
             INSERT OR REPLACE INTO schema_version (version) VALUES (14);",
        )?;
    }

    if version < 15 {
        // A 401 is account-specific and safe to fail over, but unlike a
        // quota cooldown it has no trustworthy reset time. Persist it in a
        // separate slot so routing can exclude the account without
        // conflating auth failure with a manual disable or rate limit.
        ensure_column(&tx, "accounts", "auth_error", "TEXT")?;
        tx.execute(
            "INSERT OR REPLACE INTO schema_version (version) VALUES (15)",
            [],
        )?;
    }

    if version < 16 {
        // v16 introduces resumable managed-account onboarding. Existing
        // accounts remain immediately routable as imported keys.
        ensure_column(
            &tx,
            "accounts",
            "account_type",
            "TEXT NOT NULL DEFAULT 'key' CHECK (account_type IN ('key', 'managed'))",
        )?;
        ensure_column(
            &tx,
            "accounts",
            "setup_step",
            "TEXT NOT NULL DEFAULT 'ready' CHECK (setup_step IN ('google_account', 'opencode_registration', 'payment', 'key_verification', 'ready'))",
        )?;
        tx.execute_batch(
            "UPDATE accounts
             SET account_type = 'key'
             WHERE account_type IS NULL OR account_type NOT IN ('key', 'managed');
             UPDATE accounts
             SET setup_step = 'ready'
             WHERE setup_step IS NULL OR setup_step NOT IN ('google_account', 'opencode_registration', 'payment', 'key_verification', 'ready');
             INSERT OR REPLACE INTO schema_version (version) VALUES (16);",
        )?;
    }

    // v17: independent Zen free-model promo cooldown window.
    if version < 17 {
        ensure_column(&tx, "accounts", "cooldown_free_until", "TEXT")?;
        tx.execute_batch("INSERT OR REPLACE INTO schema_version (version) VALUES (17);")?;
    }

    // v18 (upstream v1.6.3): optional account notes.
    if version < 18 {
        ensure_column(&tx, "accounts", "notes", "TEXT")?;
        tx.execute_batch("INSERT OR REPLACE INTO schema_version (version) VALUES (18);")?;
    }

    // v19: client gateway key attribution on forward logs. Nullable columns
    // keep old binaries (which select explicit column names) downgrade-safe;
    // historical NULL rows mean "unattributed" until the startup backfill
    // attributes them to the fixed primary key id (PRIMARY_KEY_ID).
    if version < 19 {
        ensure_column(&tx, "forward_logs", "client_key_id", "TEXT")?;
        ensure_column(&tx, "forward_logs", "client_key_name", "TEXT")?;
        tx.execute_batch(
            "CREATE INDEX IF NOT EXISTS idx_forward_logs_client_key
                ON forward_logs(client_key_id);
             INSERT OR REPLACE INTO schema_version (version) VALUES (19);",
        )?;
    }

    // v20: sub gateway keys live in their own table, owned exclusively by
    // the key lifecycle API. Old single-key binaries never read or rewrite
    // it, so sub keys survive downgrade round trips unchanged. The partial
    // unique index only backstops uniqueness among non-deleted sub keys;
    // the primary key lives in the legacy config scalar and cross-tier
    // collision checks are enforced at the API layer.
    if version < 20 {
        tx.execute_batch(
            "CREATE TABLE IF NOT EXISTS sub_gateway_keys (
                id TEXT PRIMARY KEY,
                name TEXT NOT NULL,
                key TEXT NOT NULL,
                enabled INTEGER NOT NULL DEFAULT 1,
                deleted_at TEXT,
                created_at TEXT NOT NULL
            );
             CREATE UNIQUE INDEX IF NOT EXISTS idx_sub_gateway_keys_key
                ON sub_gateway_keys(key) WHERE deleted_at IS NULL AND key <> '';
             INSERT OR REPLACE INTO schema_version (version) VALUES (20);",
        )?;
    }

    // v21: official Go usage sync metadata. Columns live on accounts so
    // deleting an account drops scheduler state with it. Defaults keep
    // pre-v21 rows inert until the adaptive scheduler first touches them.
    if version < 21 {
        ensure_column(&tx, "accounts", "usage_sync_last_success_at", "TEXT")?;
        ensure_column(&tx, "accounts", "usage_sync_last_attempt_at", "TEXT")?;
        ensure_column(&tx, "accounts", "usage_sync_next_eligible_at", "TEXT")?;
        ensure_column(
            &tx,
            "accounts",
            "usage_sync_failure_streak",
            "INTEGER NOT NULL DEFAULT 0",
        )?;
        ensure_column(&tx, "accounts", "usage_sync_last_expedited_at", "TEXT")?;
        tx.execute_batch("INSERT OR REPLACE INTO schema_version (version) VALUES (21);")?;
    }

    if version < 22 {
        // Several old development fixtures omitted the stable settings
        // table despite reporting a later schema. Repair it before reading
        // the legacy free-routing config.
        tx.execute_batch(
            "CREATE TABLE IF NOT EXISTS settings (
                key TEXT PRIMARY KEY,
                value TEXT NOT NULL
            );",
        )?;
        // v22: provider/offering bindings become explicit and immutable for
        // generic account updates. Additive columns keep old binaries able
        // to read their legacy projection during a rollback.
        ensure_column(
            &tx,
            "accounts",
            "provider_id",
            "TEXT NOT NULL DEFAULT 'opencode'",
        )?;
        ensure_column(&tx, "accounts", "offering_id", "TEXT NOT NULL DEFAULT 'go'")?;
        ensure_column(
            &tx,
            "accounts",
            "credential_kind",
            "TEXT NOT NULL DEFAULT 'api_key' CHECK (credential_kind IN ('api_key', 'none'))",
        )?;
        ensure_column(
            &tx,
            "accounts",
            "quota_scope",
            "TEXT NOT NULL DEFAULT 'key' CHECK (quota_scope IN ('key', 'egress-ip'))",
        )?;
        ensure_column(
            &tx,
            "accounts",
            "free_alias_enabled",
            "INTEGER NOT NULL DEFAULT 0",
        )?;

        for (column, definition) in [
            ("route_account_id", "TEXT"),
            ("provider_id", "TEXT"),
            ("offering_id", "TEXT"),
            ("credential_account_id", "TEXT"),
            ("raw_cost_usd", "REAL"),
            ("quota_debit", "REAL"),
            ("effective_paid_cost_usd", "REAL"),
        ] {
            ensure_column(&tx, "forward_logs", column, definition)?;
        }

        tx.execute_batch(
            "CREATE TABLE IF NOT EXISTS quota_windows (
                account_id TEXT NOT NULL,
                window_kind TEXT NOT NULL,
                used REAL NOT NULL DEFAULT 0,
                limit_value REAL,
                started_at TEXT,
                resets_at TEXT,
                calibration_offset REAL NOT NULL DEFAULT 0,
                unit TEXT NOT NULL,
                source TEXT NOT NULL,
                observed_at TEXT,
                updated_at TEXT NOT NULL,
                PRIMARY KEY (account_id, window_kind),
                FOREIGN KEY (account_id) REFERENCES accounts(id) ON DELETE CASCADE
            );
            CREATE TABLE IF NOT EXISTS credit_balances (
                account_id TEXT NOT NULL,
                balance_kind TEXT NOT NULL,
                amount REAL NOT NULL,
                unit TEXT NOT NULL,
                source TEXT NOT NULL,
                observed_at TEXT,
                updated_at TEXT NOT NULL,
                PRIMARY KEY (account_id, balance_kind),
                FOREIGN KEY (account_id) REFERENCES accounts(id) ON DELETE CASCADE
            );
            CREATE TABLE IF NOT EXISTS provider_pricing_snapshots (
                provider_id TEXT NOT NULL,
                offering_id TEXT NOT NULL,
                revision TEXT NOT NULL,
                activated_at TEXT NOT NULL,
                document_updated_at TEXT,
                source_url TEXT NOT NULL,
                content_hash TEXT NOT NULL,
                snapshot_json TEXT NOT NULL,
                PRIMARY KEY (provider_id, offering_id, revision)
            );
            CREATE INDEX IF NOT EXISTS idx_provider_pricing_active
                ON provider_pricing_snapshots(provider_id, offering_id, activated_at DESC);
            CREATE TABLE IF NOT EXISTS provider_usage_sync_state (
                account_id TEXT PRIMARY KEY,
                last_success_at TEXT,
                last_attempt_at TEXT,
                next_eligible_at TEXT,
                failure_streak INTEGER NOT NULL DEFAULT 0,
                last_expedited_at TEXT,
                FOREIGN KEY (account_id) REFERENCES accounts(id) ON DELETE CASCADE
            );
            CREATE INDEX IF NOT EXISTS idx_forward_logs_route_account
                ON forward_logs(route_account_id);
            CREATE INDEX IF NOT EXISTS idx_forward_logs_provider_offering
                ON forward_logs(provider_id, offering_id);",
        )?;

        let migrated_at = Utc::now();
        let migrated_at_rfc = migrated_at.to_rfc3339();
        let mut legacy_free_cooldown: Option<String> = None;
        let mut normal_account_ids = Vec::new();
        let mut supports_account_backfill = true;
        for column in [
            "name",
            "key_cipher",
            "enabled",
            "recharge_date",
            "sort_order",
            "cooldown_until",
            "cooldown_free_until",
            "usage_5h_window_started_at",
            "usage_5h_window_cost_offset",
            "usage_week_window_started_at",
            "usage_week_window_cost_offset",
            "usage_month_window_cost_offset",
            "created_at",
            "updated_at",
        ] {
            if !table_has_column(&tx, "accounts", column)? {
                supports_account_backfill = false;
                break;
            }
        }
        if supports_account_backfill {
            let reserved_binding = tx
                .query_row(
                    "SELECT provider_id, offering_id, credential_kind, quota_scope
                 FROM accounts WHERE id = ?1",
                    [ZEN_FREE_ACCOUNT_ID],
                    |row| {
                        Ok((
                            row.get::<_, String>(0)?,
                            row.get::<_, String>(1)?,
                            row.get::<_, String>(2)?,
                            row.get::<_, String>(3)?,
                        ))
                    },
                )
                .optional()?;
            if let Some((provider, offering, credential, scope)) = reserved_binding {
                anyhow::ensure!(
                    provider == OPENCODE_ZEN_FREE_PROVIDER_ID
                        && offering == V34_OFFERING_ANONYMOUS_FREE
                        && credential == CredentialKind::None.as_str()
                        && scope == QuotaScope::EgressIp.as_str(),
                    "reserved Zen Free account id {ZEN_FREE_ACCOUNT_ID} is already used by a different account"
                );
            }

            let free_mode = tx
                .query_row(
                    "SELECT value FROM settings WHERE key = 'config'",
                    [],
                    |row| row.get::<_, String>(0),
                )
                .optional()?
                .and_then(|json| serde_json::from_str::<serde_json::Value>(&json).ok())
                .and_then(|config| {
                    config
                        .get("free_model_routing")
                        .and_then(|value| value.as_str())
                        .map(str::to_owned)
                })
                .unwrap_or_else(|| "explicit".to_string());
            let zen_enabled = free_mode != "deny";

            legacy_free_cooldown = tx.query_row(
                "SELECT MAX(value) FROM (
                SELECT cooldown_free_until AS value FROM accounts
                WHERE cooldown_free_until IS NOT NULL
                UNION ALL
                SELECT value FROM settings WHERE key = ?1
             )",
                [FREE_CHANNEL_COOLDOWN_SETTING],
                |row| row.get(0),
            )?;
            let purchase_date = local_today();
            tx.execute(
                "INSERT OR IGNORE INTO accounts (
                id, provider_id, offering_id, credential_kind, quota_scope,
                free_alias_enabled, name, key_cipher, enabled, recharge_date,
                sort_order, cooldown_until, cooldown_free_until, account_type,
                setup_step, created_at, updated_at
             ) VALUES (
                ?1, ?2, ?3, ?4, ?5, ?6, ?7, '', ?8, ?9,
                (SELECT COALESCE(MAX(sort_order), -1) + 1 FROM accounts),
                ?10, ?10, 'key', 'ready', ?11, ?11
             )",
                params![
                    ZEN_FREE_ACCOUNT_ID,
                    OPENCODE_ZEN_FREE_PROVIDER_ID,
                    V34_OFFERING_ANONYMOUS_FREE,
                    CredentialKind::None.as_str(),
                    QuotaScope::EgressIp.as_str(),
                    0,
                    ZEN_FREE_ACCOUNT_NAME,
                    zen_enabled as i32,
                    purchase_date,
                    legacy_free_cooldown,
                    migrated_at_rfc,
                ],
            )?;
            // Repair a prior interrupted development migration while retaining
            // the user's chosen sort order for the singleton row.
            tx.execute(
                "UPDATE accounts SET
                provider_id = ?2, offering_id = ?3, credential_kind = ?4,
                quota_scope = ?5, free_alias_enabled = ?6, name = ?7,
                key_cipher = '', enabled = ?8, cooldown_free_until = ?9,
                cooldown_until = ?9, account_type = 'key', setup_step = 'ready',
                updated_at = ?10
             WHERE id = ?1",
                params![
                    ZEN_FREE_ACCOUNT_ID,
                    OPENCODE_ZEN_FREE_PROVIDER_ID,
                    V34_OFFERING_ANONYMOUS_FREE,
                    CredentialKind::None.as_str(),
                    QuotaScope::EgressIp.as_str(),
                    0,
                    ZEN_FREE_ACCOUNT_NAME,
                    zen_enabled as i32,
                    legacy_free_cooldown,
                    migrated_at_rfc,
                ],
            )?;
            if let Some(until) = legacy_free_cooldown.as_deref() {
                super::usage_store::upsert_free_channel_cooldown(&tx, until)?;
            }
            tx.execute(
                "UPDATE accounts SET cooldown_free_until = NULL
             WHERE id <> ?1",
                [ZEN_FREE_ACCOUNT_ID],
            )?;
            normal_account_ids = {
                let mut stmt = tx.prepare("SELECT id FROM accounts WHERE id <> ?1")?;
                stmt.query_map([ZEN_FREE_ACCOUNT_ID], |row| row.get::<_, String>(0))?
                    .collect::<rusqlite::Result<Vec<_>>>()?
            };
            for id in &normal_account_ids {
                let cooldown =
                    super::usage_store::compute_cooldown_until(&tx, id, &migrated_at_rfc)?;
                tx.execute(
                    "UPDATE accounts SET cooldown_until = ?2 WHERE id = ?1",
                    params![id, cooldown],
                )?;
            }
        }

        // Route attribution is snapshotted independently from the
        // credential-bearing account. Historical monetary cost remains in
        // `cost`; the new three cost columns intentionally stay NULL.
        if table_has_column(&tx, "forward_logs", "account_id")?
            && table_has_column(&tx, "forward_logs", "cost_state")?
        {
            tx.execute(
                "UPDATE forward_logs SET
                route_account_id = CASE WHEN cost_state = 'free' THEN ?1 ELSE account_id END,
                provider_id = CASE WHEN cost_state = 'free' THEN ?2 ELSE ?3 END,
                offering_id = CASE WHEN cost_state = 'free' THEN ?4 ELSE ?5 END,
                credential_account_id = account_id
             WHERE route_account_id IS NULL
                OR provider_id IS NULL
                OR offering_id IS NULL
                OR credential_account_id IS NULL",
                params![
                    ZEN_FREE_ACCOUNT_ID,
                    OPENCODE_ZEN_FREE_PROVIDER_ID,
                    OPENCODE_PROVIDER_ID,
                    V34_OFFERING_ANONYMOUS_FREE,
                    V34_OFFERING_GO,
                ],
            )?;
        }

        if table_exists(&tx, "pricing_snapshots")? {
            tx.execute(
                "INSERT OR IGNORE INTO provider_pricing_snapshots (
                provider_id, offering_id, revision, activated_at,
                document_updated_at, source_url, content_hash, snapshot_json
             ) SELECT ?1, ?2, revision, activated_at, document_updated_at,
                      source_url, content_hash, snapshot_json
               FROM pricing_snapshots",
                params![OPENCODE_PROVIDER_ID, V34_OFFERING_GO],
            )?;
        }
        tx.execute(
            "INSERT OR IGNORE INTO provider_usage_sync_state (
                account_id, last_success_at, last_attempt_at, next_eligible_at,
                failure_streak, last_expedited_at
             ) SELECT id, usage_sync_last_success_at, usage_sync_last_attempt_at,
                      usage_sync_next_eligible_at, usage_sync_failure_streak,
                      usage_sync_last_expedited_at
               FROM accounts WHERE id <> ?1",
            [ZEN_FREE_ACCOUNT_ID],
        )?;

        let limits = if table_exists(&tx, "pricing_snapshots")? {
            tx.query_row(
                "SELECT snapshot_json FROM pricing_snapshots
                 ORDER BY activated_at DESC LIMIT 1",
                [],
                |row| row.get::<_, String>(0),
            )
            .optional()?
            .map(|json| serde_json::from_str::<PricingSnapshot>(&json))
            .transpose()?
            .map(|snapshot| snapshot.limits)
            .unwrap_or(SEED_LIMITS)
        } else {
            SEED_LIMITS
        };
        for account_id in &normal_account_ids {
            let usage = super::usage_store::account_usage_with_limits_on(
                &tx,
                account_id,
                &limits,
                migrated_at,
            )?;
            let (started_5h, offset_5h, started_week, offset_week, offset_month, purchase) = tx
                .query_row(
                    "SELECT usage_5h_window_started_at, usage_5h_window_cost_offset,
                            usage_week_window_started_at, usage_week_window_cost_offset,
                            usage_month_window_cost_offset, recharge_date
                     FROM accounts WHERE id = ?1",
                    [account_id],
                    |row| {
                        Ok((
                            row.get::<_, Option<String>>(0)?,
                            row.get::<_, f64>(1)?,
                            row.get::<_, Option<String>>(2)?,
                            row.get::<_, f64>(3)?,
                            row.get::<_, f64>(4)?,
                            row.get::<_, String>(5)?,
                        ))
                    },
                )?;
            let month_started = super::usage_store::month_window_start_utc(&purchase)?.to_rfc3339();
            for (kind, used, limit, started, resets, offset) in [
                (
                    QUOTA_WINDOW_FIVE_HOURS,
                    usage.window_5h,
                    limits.window_5h,
                    started_5h,
                    usage.resets_in_5h,
                    offset_5h,
                ),
                (
                    QUOTA_WINDOW_WEEK,
                    usage.window_week,
                    limits.window_week,
                    started_week,
                    usage.resets_in_week,
                    offset_week,
                ),
                (
                    QUOTA_WINDOW_MONTH,
                    usage.window_month,
                    limits.window_month,
                    Some(month_started),
                    usage.resets_in_month,
                    offset_month,
                ),
            ] {
                tx.execute(
                    "INSERT OR IGNORE INTO quota_windows (
                        account_id, window_kind, used, limit_value, started_at,
                        resets_at, calibration_offset, unit, source, observed_at,
                        updated_at
                     ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 'usd',
                               'migration-v22', NULL, ?8)",
                    params![
                        account_id,
                        kind,
                        used,
                        limit,
                        started,
                        resets.map(|value| value.to_rfc3339()),
                        offset,
                        migrated_at_rfc,
                    ],
                )?;
            }
        }
        if supports_account_backfill {
            tx.execute(
                "INSERT OR IGNORE INTO quota_windows (
                account_id, window_kind, used, limit_value, started_at,
                resets_at, calibration_offset, unit, source, observed_at,
                updated_at
             ) VALUES (?1, ?2, 0, NULL, NULL, ?3, 0, 'request',
                       'migration-v22', NULL, ?4)",
                params![
                    ZEN_FREE_ACCOUNT_ID,
                    QUOTA_WINDOW_FREE,
                    legacy_free_cooldown,
                    migrated_at_rfc,
                ],
            )?;
        }

        tx.execute_batch("INSERT OR REPLACE INTO schema_version (version) VALUES (22);")?;
    }

    if version < 23 {
        ensure_column(
            &tx,
            "accounts",
            "verification_status",
            "TEXT NOT NULL DEFAULT 'not_required'",
        )?;
        ensure_column(&tx, "accounts", "connection_verified_at", "TEXT")?;
        ensure_column(&tx, "accounts", "verification_error", "TEXT")?;
        for (column, definition) in [
            ("requested_model", "TEXT"),
            ("resolved_alias", "TEXT"),
            ("upstream_model", "TEXT"),
            ("native_cost_value", "REAL"),
            ("native_cost_unit", "TEXT"),
            ("native_cost_currency", "TEXT"),
        ] {
            ensure_column(&tx, "forward_logs", column, definition)?;
        }
        tx.execute_batch(
            "CREATE TABLE IF NOT EXISTS account_custom_configs (
                account_id TEXT PRIMARY KEY,
                base_url TEXT NOT NULL,
                upstream_protocols TEXT NOT NULL,
                auth_scheme TEXT NOT NULL,
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL,
                FOREIGN KEY (account_id) REFERENCES accounts(id) ON DELETE CASCADE
            );
            CREATE TABLE IF NOT EXISTS account_model_capabilities (
                account_id TEXT NOT NULL,
                model_id TEXT NOT NULL,
                protocol TEXT NOT NULL,
                verified_at TEXT,
                source TEXT NOT NULL DEFAULT 'manual',
                PRIMARY KEY (account_id, model_id, protocol),
                FOREIGN KEY (account_id) REFERENCES accounts(id) ON DELETE CASCADE
            );
            CREATE INDEX IF NOT EXISTS idx_account_model_capabilities_account
                ON account_model_capabilities(account_id);",
        )?;

        if table_has_column(&tx, "accounts", "provider_id")?
            && table_has_column(&tx, "accounts", "offering_id")?
        {
            if table_has_column(&tx, "accounts", "enabled")? {
                tx.execute(
                    "UPDATE accounts SET verification_status = 'pending', verification_error = NULL,
                            enabled = 0
                     WHERE provider_id = ?1 AND offering_id = ?2",
                    params![COMMAND_CODE_PROVIDER_ID, V34_OFFERING_GOAT],
                )?;
            } else {
                tx.execute(
                    "UPDATE accounts SET verification_status = 'pending', verification_error = NULL
                     WHERE provider_id = ?1 AND offering_id = ?2",
                    params![COMMAND_CODE_PROVIDER_ID, V34_OFFERING_GOAT],
                )?;
            }
            tx.execute(
                "UPDATE accounts SET verification_status = 'not_required', verification_error = NULL
                 WHERE NOT (provider_id = ?1 AND offering_id = ?2)
                   AND (verification_status IS NULL OR verification_status = 'not_required')",
                params![COMMAND_CODE_PROVIDER_ID, V34_OFFERING_GOAT],
            )?;
        }

        if table_has_column(&tx, "forward_logs", "model")? {
            tx.execute(
                "UPDATE forward_logs SET
                    requested_model = COALESCE(requested_model, model),
                    upstream_model = COALESCE(upstream_model, model),
                    native_cost_value = COALESCE(
                        native_cost_value,
                        raw_cost_usd,
                        CASE WHEN cost_state IN ('priced', 'legacy_estimate', 'free')
                             THEN cost ELSE NULL END
                    ),
                    native_cost_unit = COALESCE(
                        native_cost_unit,
                        CASE WHEN cost_state IN ('priced', 'legacy_estimate', 'free')
                             THEN 'usd' ELSE NULL END
                    ),
                    native_cost_currency = COALESCE(
                        native_cost_currency,
                        CASE WHEN cost_state IN ('priced', 'legacy_estimate', 'free')
                             THEN 'USD' ELSE NULL END
                    )
                 WHERE requested_model IS NULL
                    OR upstream_model IS NULL
                    OR native_cost_value IS NULL
                    OR native_cost_unit IS NULL
                    OR native_cost_currency IS NULL",
                [],
            )?;
        }

        tx.execute_batch("INSERT OR REPLACE INTO schema_version (version) VALUES (23);")?;
    }

    // v24: route leg label (`auto`/`proxy`/`direct`) for every forward
    // attempt. Rows written before this change keep the empty default,
    // honestly marking "not recorded" instead of guessing a leg.
    if version < 24 {
        ensure_column(&tx, "forward_logs", "route", "TEXT NOT NULL DEFAULT ''")?;
        tx.execute_batch("INSERT OR REPLACE INTO schema_version (version) VALUES (24);")?;
    }

    // v25: last successful provider model-catalog snapshots. Zen Free
    // refreshes replace this row atomically only after validation/filtering.
    if version < 25 {
        tx.execute_batch(
            "CREATE TABLE IF NOT EXISTS provider_model_catalogs (
                provider_id TEXT NOT NULL,
                offering_id TEXT NOT NULL,
                models_json TEXT NOT NULL,
                refreshed_at TEXT,
                source_url TEXT NOT NULL,
                PRIMARY KEY (provider_id, offering_id)
            );
            INSERT OR REPLACE INTO schema_version (version) VALUES (25);",
        )?;
    }

    // v26: scope-level effective contracts (catalog snapshots, protocol
    // evidence, Chat/Responses/Messages switches). Additive only; v25 Zen
    // catalog rows are projected into the Zen provider scope.
    if version < 26 {
        tx.execute_batch(PROVIDER_CONTRACT_V26_DDL)?;
        backfill_v26_zen_provider_scope(&tx)?;
        tx.execute_batch("INSERT OR REPLACE INTO schema_version (version) VALUES (26);")?;
    }

    // Unreleased #43 drafts numbered client-key columns as v18 and the
    // sub-key table as v19, so those databases already report version
    // >= 18 and skip the notes gate above. ensure_column is idempotent
    // on released v1.6.3 libraries and on fresh installs. After v52 the
    // accounts table is gone; skip these backstops.
    if table_exists(&tx, "accounts")? {
        ensure_column(&tx, "accounts", "notes", "TEXT")?;
        // Idempotent backstop for v21 columns when an unreleased draft already
        // reported a higher schema_version number without these fields. v27
        // drops these leftovers in favor of `provider_usage_sync_state`; never
        // resurrect them on a v27+ database.
        if version < V27_SCHEMA_VERSION {
            ensure_column(&tx, "accounts", "usage_sync_last_success_at", "TEXT")?;
            ensure_column(&tx, "accounts", "usage_sync_last_attempt_at", "TEXT")?;
            ensure_column(&tx, "accounts", "usage_sync_next_eligible_at", "TEXT")?;
            ensure_column(
                &tx,
                "accounts",
                "usage_sync_failure_streak",
                "INTEGER NOT NULL DEFAULT 0",
            )?;
            ensure_column(&tx, "accounts", "usage_sync_last_expedited_at", "TEXT")?;
        }
        ensure_column(
            &tx,
            "accounts",
            "verification_status",
            "TEXT NOT NULL DEFAULT 'not_required'",
        )?;
        ensure_column(&tx, "accounts", "connection_verified_at", "TEXT")?;
        ensure_column(&tx, "accounts", "verification_error", "TEXT")?;
    }
    for (column, definition) in [
        ("requested_model", "TEXT"),
        ("resolved_alias", "TEXT"),
        ("upstream_model", "TEXT"),
        ("native_cost_value", "REAL"),
        ("native_cost_unit", "TEXT"),
        ("native_cost_currency", "TEXT"),
    ] {
        ensure_column(&tx, "forward_logs", column, definition)?;
    }
    // Historical leftover Custom tables referenced `accounts`. After v52
    // that parent is gone, so do not recreate the children on rewind.
    // v32/v33 treat a missing leftover table as already-migrated.
    if version < 53 && table_exists(&tx, "accounts")? {
        tx.execute_batch(
            "CREATE TABLE IF NOT EXISTS account_custom_configs (
                account_id TEXT PRIMARY KEY,
                base_url TEXT NOT NULL,
                upstream_protocols TEXT NOT NULL,
                auth_scheme TEXT NOT NULL,
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL,
                FOREIGN KEY (account_id) REFERENCES accounts(id) ON DELETE CASCADE
            );
            CREATE TABLE IF NOT EXISTS account_model_capabilities (
                account_id TEXT NOT NULL,
                model_id TEXT NOT NULL,
                protocol TEXT NOT NULL,
                verified_at TEXT,
                source TEXT NOT NULL DEFAULT 'manual',
                PRIMARY KEY (account_id, model_id, protocol),
                FOREIGN KEY (account_id) REFERENCES accounts(id) ON DELETE CASCADE
            );",
        )?;
    }
    tx.execute_batch(PROVIDER_CONTRACT_V26_DDL)?;
    // Fail-closed leftovers for every catalogued-but-unroutable offering,
    // including already-v23 verified rows. Sparse pre-v22 fixtures may still
    // lack `enabled` even after additive column backstops, so skip rather
    // than fail the open. Go/Zen and unknown pairs are not in this set.
    disable_unroutable_catalog_accounts(&tx)?;
    // Command Code's public model directory is not Key verification.
    // Normalize historical GOAT verification states to the single current
    // account semantic; inference remains the actual Key-auth boundary.
    if table_has_column(&tx, "accounts", "provider_id")?
        && table_has_column(&tx, "accounts", "verification_status")?
    {
        tx.execute(
            "UPDATE accounts
             SET verification_status = 'not_required',
                 connection_verified_at = NULL,
                 verification_error = NULL
             WHERE provider_id = ?1",
            params![COMMAND_CODE_PROVIDER_ID],
        )?;
    }
    if table_has_column(&tx, "credentials", "provider_id")?
        && table_has_column(&tx, "credentials", "verification_status")?
    {
        tx.execute(
            "UPDATE credentials
             SET verification_status = 'not_required',
                 connection_verified_at = NULL,
                 verification_error = NULL
             WHERE provider_id = ?1",
            params![COMMAND_CODE_PROVIDER_ID],
        )?;
    }

    // Detailed diagnostics are intentionally short-lived. Keep the base log row,
    // stable request id, source, stage, and original compact error indefinitely.
    // Timestamps are stored as to_rfc3339 strings, so a precomputed cutoff keeps
    // the comparison index-friendly instead of calling julianday() per row.
    let diagnostic_cutoff = (Utc::now() - Duration::days(30)).to_rfc3339();
    tx.execute(
        "UPDATE forward_logs SET diagnostic_json = NULL
         WHERE diagnostic_json IS NOT NULL
           AND timestamp < ?1",
        params![diagnostic_cutoff],
    )?;
    tx.execute(
        "UPDATE gateway_logs SET diagnostic_json = NULL
         WHERE diagnostic_json IS NOT NULL
           AND created_at < ?1",
        params![diagnostic_cutoff],
    )?;

    // v17 originally stored the IP-shared free cooldown only on the account
    // that observed it. Backfill an active legacy value into a durable global
    // setting on every open, without adding another schema migration.
    let now_rfc = Utc::now().to_rfc3339();
    let legacy_free_cooldown: Option<String> = if table_exists(&tx, "accounts")? {
        tx.query_row(
            "SELECT MAX(cooldown_free_until)
             FROM accounts
             WHERE cooldown_free_until IS NOT NULL
               AND cooldown_free_until > ?1",
            params![now_rfc],
            |row| row.get(0),
        )?
    } else if table_exists(&tx, "credentials")? {
        tx.query_row(
            "SELECT MAX(cooldown_free_until)
             FROM credentials
             WHERE cooldown_free_until IS NOT NULL
               AND cooldown_free_until > ?1",
            params![now_rfc],
            |row| row.get(0),
        )?
    } else {
        None
    };
    if let Some(until) = legacy_free_cooldown {
        super::usage_store::upsert_free_channel_cooldown(&tx, &until)?;
    }

    tx.commit()?;
    Ok(())
}
fn backfill_v26_zen_provider_scope(tx: &Transaction<'_>) -> Result<()> {
    if !table_exists(tx, "provider_model_catalogs")? {
        return Ok(());
    }
    tx.execute(
        "INSERT INTO provider_contract_scopes (
            scope_kind, scope_id, catalog_models_json, catalog_refreshed_at,
            catalog_source, catalog_source_url,
            chat_completions_enabled, responses_enabled, messages_enabled,
            revision, updated_at
         )
         SELECT
            'provider',
            provider_id,
            models_json,
            refreshed_at,
            ?1,
            source_url,
            1, 1, 1, 1,
            COALESCE(refreshed_at, datetime('now'))
         FROM provider_model_catalogs
         WHERE provider_id = ?2
         ON CONFLICT(scope_kind, scope_id) DO NOTHING",
        params![CATALOG_SOURCE_OFFICIAL_ZEN, OPENCODE_ZEN_FREE_PROVIDER_ID],
    )?;
    Ok(())
}
fn migrate_legacy_usage_baselines(
    tx: &rusqlite::Transaction<'_>,
    limits: &PricingLimits,
    now: DateTime<Utc>,
) -> Result<()> {
    type LegacyUsageRow = (
        String,
        Option<f64>,
        Option<f64>,
        Option<f64>,
        Option<f64>,
        Option<f64>,
        Option<f64>,
        String,
    );

    let accounts = {
        let mut stmt = tx.prepare(
            "SELECT id,
                    usage_5h_baseline_percent, usage_5h_anchor_success_cost,
                    usage_week_baseline_percent, usage_week_anchor_success_cost,
                    usage_month_baseline_percent, usage_month_anchor_success_cost,
                    recharge_date
             FROM accounts
             WHERE usage_5h_baseline_percent IS NOT NULL
                OR usage_week_baseline_percent IS NOT NULL
                OR usage_month_baseline_percent IS NOT NULL",
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
            ))
        })?
        .collect::<rusqlite::Result<Vec<LegacyUsageRow>>>()?
    };
    let now_string = now.to_rfc3339();

    for (
        id,
        percent_5h,
        anchor_5h,
        percent_week,
        anchor_week,
        percent_month,
        anchor_month,
        purchase_date,
    ) in accounts
    {
        let total_cost: f64 = tx.query_row(
            "SELECT COALESCE(SUM(cost), 0) FROM forward_logs
             WHERE account_id = ?1
               AND cost_state IN ('priced', 'legacy_estimate')",
            [&id],
            |row| row.get(0),
        )?;
        let migrated_5h = percent_5h.zip(anchor_5h).map(|baseline| {
            super::usage_store::effective_usage(0.0, Some(baseline), total_cost, limits.window_5h)
        });
        let migrated_week = percent_week.zip(anchor_week).map(|baseline| {
            super::usage_store::effective_usage(0.0, Some(baseline), total_cost, limits.window_week)
        });
        let migrated_month = match percent_month.zip(anchor_month) {
            Some(baseline) => {
                let month_start =
                    super::usage_store::month_window_start_utc(&purchase_date)?.to_rfc3339();
                let actual_month_cost: f64 = tx.query_row(
                    "SELECT COALESCE(SUM(cost), 0) FROM forward_logs
                     WHERE account_id = ?1
                       AND cost_state IN ('priced', 'legacy_estimate')
                       AND timestamp >= ?2",
                    params![&id, month_start],
                    |row| row.get(0),
                )?;
                Some(
                    super::usage_store::effective_usage(
                        0.0,
                        Some(baseline),
                        total_cost,
                        limits.window_month,
                    ) - actual_month_cost,
                )
            }
            None => None,
        };

        tx.execute(
            "UPDATE accounts SET
                usage_5h_window_started_at = CASE WHEN ?2 IS NULL THEN usage_5h_window_started_at ELSE ?1 END,
                usage_5h_window_cost_offset = COALESCE(?2, usage_5h_window_cost_offset),
                usage_week_window_started_at = CASE WHEN ?3 IS NULL THEN usage_week_window_started_at ELSE ?1 END,
                usage_week_window_cost_offset = COALESCE(?3, usage_week_window_cost_offset),
                usage_month_window_cost_offset = COALESCE(?4, usage_month_window_cost_offset),
                usage_5h_baseline_percent = NULL,
                usage_5h_anchor_success_cost = NULL,
                usage_week_baseline_percent = NULL,
                usage_week_anchor_success_cost = NULL,
                usage_month_baseline_percent = NULL,
                usage_month_anchor_success_cost = NULL
             WHERE id = ?5",
            params![
                &now_string,
                migrated_5h,
                migrated_week,
                migrated_month,
                &id
            ],
        )?;
    }
    Ok(())
}
/// Idempotent open/startup backstop: leftover `enabled=1` rows for every
/// catalog plan with `routable=false` are forced off. Providers come from
/// [`BUILTIN_PROVIDERS`]; Go, Zen, and unknown provider rows are skipped.
fn disable_unroutable_catalog_accounts(tx: &Transaction<'_>) -> Result<()> {
    let accounts = table_has_column(tx, "accounts", "provider_id")?
        && table_has_column(tx, "accounts", "enabled")?;
    let credentials = table_has_column(tx, "credentials", "provider_id")?
        && table_has_column(tx, "credentials", "enabled")?;
    if !accounts && !credentials {
        return Ok(());
    }
    for plan in BUILTIN_PROVIDERS.iter().filter(|plan| !plan.routable) {
        if accounts {
            tx.execute(
                "UPDATE accounts SET enabled = 0
                 WHERE provider_id = ?1 AND enabled <> 0",
                params![plan.provider_id],
            )?;
        }
        if credentials {
            tx.execute(
                "UPDATE credentials SET enabled = 0
                 WHERE provider_id = ?1 AND enabled <> 0",
                params![plan.provider_id],
            )?;
        }
    }
    Ok(())
}
pub(super) fn migration_fallback_purchase_date(created_at: &str) -> Result<String> {
    let created_at = DateTime::parse_from_rfc3339(created_at).map_err(|error| {
        anyhow::anyhow!(
            "invalid account created_at {created_at:?} while repairing purchase date: {error}"
        )
    })?;
    Ok(created_at
        .with_timezone(&Utc)
        .date_naive()
        .format("%Y-%m-%d")
        .to_string())
}
