use crate::kernel::ids::{PRIMARY_KEY_ID, PRIMARY_KEY_NAME};
use crate::kernel::pricing::{PricingLimits, PricingSnapshot, SEED_LIMITS};
use crate::models::*;
use crate::provider::*;
use crate::provider_contracts::{
    CATALOG_SOURCE_OFFICIAL_ZEN, ContractEvidenceSource, ContractScope, PersistedContracts,
    PersistedModelProtocol, PersistedScopeRow, ProbeResultKind, ProtocolSwitches,
    SCOPE_KIND_CUSTOM_ENDPOINT,
};
use anyhow::{Context, Result};
use chrono::{DateTime, Duration, Local, NaiveDate, TimeZone, Utc};
use rusqlite::{
    Connection, OpenFlags, OptionalExtension, Row, Transaction, params, params_from_iter,
    types::{Type, Value},
};
use std::{
    collections::HashSet,
    fmt,
    path::{Path, PathBuf},
};

pub struct Database {
    conn: Connection,
}

/// Settings key holding the forward-log client-key backfill watermark
/// (max processed rowid), or `BACKFILL_DONE` once complete.
pub const BACKFILL_SETTING_KEY: &str = "backfill_forward_logs_client_key";
pub const BACKFILL_DONE: &str = "done";
/// Rows per backfill transaction; tuned so one chunk holds the connection
/// for only tens of milliseconds on local SQLite.
pub const FORWARD_LOG_BACKFILL_CHUNK_ROWS: i64 = 50_000;
/// Pause between backfill chunks so concurrent request logging wins the lock.
pub const FORWARD_LOG_BACKFILL_CHUNK_PAUSE: std::time::Duration =
    std::time::Duration::from_millis(10);
/// Durable, egress-IP-wide Zen free-channel cooldown.
///
/// This must not be tied only to an account row: disabling or deleting the key
/// that observed the 429 does not restore the shared upstream quota.
pub const FREE_CHANNEL_COOLDOWN_SETTING: &str = "free_channel_cooldown_until";
/// One-time, non-overwriting SQLite snapshot taken before an existing pre-v22
/// database receives any migration writes on its way to v22.
pub const PRE_V22_BACKUP_FILE_PREFIX: &str = "data.sqlite.pre-v22.";
/// One-time, non-overwriting SQLite snapshot taken before an existing pre-v23
/// database receives any migration writes on its way to v23.
pub const PRE_V23_BACKUP_FILE_PREFIX: &str = "data.sqlite.pre-v23.";
/// Highest schema this binary can open or migrate. Newer databases fail closed.
pub const CURRENT_SCHEMA_VERSION: i32 = 26;

const PROVIDER_CONTRACT_V26_DDL: &str = "
    CREATE TABLE IF NOT EXISTS provider_contract_scopes (
        scope_kind TEXT NOT NULL,
        scope_id TEXT NOT NULL,
        catalog_models_json TEXT NOT NULL DEFAULT '[]',
        catalog_refreshed_at TEXT,
        catalog_source TEXT NOT NULL DEFAULT '',
        catalog_source_url TEXT NOT NULL DEFAULT '',
        chat_completions_enabled INTEGER NOT NULL DEFAULT 1,
        responses_enabled INTEGER NOT NULL DEFAULT 1,
        messages_enabled INTEGER NOT NULL DEFAULT 1,
        revision INTEGER NOT NULL DEFAULT 1,
        updated_at TEXT NOT NULL,
        PRIMARY KEY (scope_kind, scope_id)
    );
    CREATE TABLE IF NOT EXISTS provider_contract_model_protocols (
        scope_kind TEXT NOT NULL,
        scope_id TEXT NOT NULL,
        model_id TEXT NOT NULL,
        protocol TEXT NOT NULL,
        source TEXT NOT NULL,
        verified_at TEXT,
        observed_at TEXT,
        last_probe_result TEXT,
        last_probe_at TEXT,
        last_probe_error TEXT,
        PRIMARY KEY (scope_kind, scope_id, model_id, protocol)
    );
    CREATE INDEX IF NOT EXISTS idx_provider_contract_model_protocols_scope
        ON provider_contract_model_protocols(scope_kind, scope_id);
";

pub struct ForwardLogQueryOptions<'a> {
    pub limit: i64,
    pub offset: i64,
    pub status: Option<&'a str>,
    pub account_id: Option<&'a str>,
    pub provider_id: Option<&'a str>,
    pub offering_id: Option<&'a str>,
    pub route_account_id: Option<&'a str>,
    pub credential_account_id: Option<&'a str>,
    pub model: Option<&'a str>,
    pub request_id: Option<&'a str>,
    pub start_time: Option<&'a str>,
    pub end_time: Option<&'a str>,
    pub sort_by: Option<&'a str>,
    pub sort_order: Option<&'a str>,
    /// Filter by the gateway key that authenticated the request;
    /// `UNATTRIBUTED_KEY_FILTER` selects rows without a client key.
    pub key_id: Option<&'a str>,
}

pub struct ForwardLogDiagnosticUpdate<'a> {
    pub error_source: &'a str,
    pub error_stage: &'a str,
    pub duration_ms: i64,
    pub diagnostic_json: &'a str,
}

/// Official or test-provided values used to atomically calibrate all three
/// Go usage windows. Monthly remaining minutes stay derived from purchase date.
#[derive(Debug, Clone, PartialEq)]
pub struct AccountUsageCalibrationSnapshot {
    pub rolling_percent: f64,
    pub weekly_percent: f64,
    pub monthly_percent: f64,
    pub rolling_resets_in_minutes: i64,
    pub weekly_resets_in_minutes: i64,
}

/// Metadata committed in the same transaction as an official usage snapshot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AccountUsageSyncSuccessMetadata {
    pub now: DateTime<Utc>,
    pub next_eligible_at: DateTime<Utc>,
    pub mark_expedited: bool,
}

/// Persisted official-usage sync metadata for one account (schema v21).
/// Never stores plaintext keys or upstream bodies.
pub type AccountUsageSyncState = ProviderUsageSyncState;

#[derive(Debug)]
pub enum ReorderAccountsError {
    DuplicateAccountId,
    AccountSetMismatch,
    Database(rusqlite::Error),
}

impl fmt::Display for ReorderAccountsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::DuplicateAccountId => f.write_str("account_ids must not contain duplicates"),
            Self::AccountSetMismatch => {
                f.write_str("account set changed; reload the account list and retry")
            }
            Self::Database(error) => write!(f, "failed to reorder accounts: {error}"),
        }
    }
}

impl std::error::Error for ReorderAccountsError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Database(error) => Some(error),
            Self::DuplicateAccountId | Self::AccountSetMismatch => None,
        }
    }
}

impl From<rusqlite::Error> for ReorderAccountsError {
    fn from(error: rusqlite::Error) -> Self {
        Self::Database(error)
    }
}

/// 幂等地为指定表添加列。若列已存在则跳过，避免 v1.4.2 -> v1.5.0 升级时
/// 旧 v9 migration（HEAD 固定窗口）和 upstream v9（cost_state）冲突导致的
/// "duplicate column" 错误。
fn ensure_column(
    tx: &rusqlite::Transaction<'_>,
    table: &str,
    column: &str,
    definition: &str,
) -> Result<()> {
    let exists = {
        let mut stmt = tx.prepare(&format!("PRAGMA table_info({table})"))?;
        let columns = stmt.query_map([], |row| row.get::<_, String>(1))?;
        columns
            .collect::<rusqlite::Result<Vec<_>>>()?
            .iter()
            .any(|existing| existing == column)
    };
    if !exists {
        tx.execute(
            &format!("ALTER TABLE {table} ADD COLUMN {column} {definition}"),
            [],
        )?;
    }
    Ok(())
}

fn table_exists(conn: &Connection, table: &str) -> Result<bool> {
    Ok(conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1)",
        [table],
        |row| row.get::<_, i64>(0),
    )? != 0)
}

fn table_has_column(conn: &Connection, table: &str, column: &str) -> Result<bool> {
    if !table_exists(conn, table)? {
        return Ok(false);
    }
    let mut stmt = conn.prepare(&format!("PRAGMA table_info({table})"))?;
    let columns = stmt.query_map([], |row| row.get::<_, String>(1))?;
    Ok(columns
        .collect::<rusqlite::Result<Vec<_>>>()?
        .iter()
        .any(|existing| existing == column))
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

fn parse_rfc3339_opt(
    value: Option<String>,
    column: usize,
) -> rusqlite::Result<Option<DateTime<Utc>>> {
    value
        .map(|text| parse_rfc3339_column(text, column))
        .transpose()
}

fn parse_rfc3339_column(value: String, column: usize) -> rusqlite::Result<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(&value)
        .map(|value| value.with_timezone(&Utc))
        .map_err(|error| {
            rusqlite::Error::FromSqlConversionFailure(column, Type::Text, Box::new(error))
        })
}

fn scope_from_row(kind: &str, id: &str) -> rusqlite::Result<ContractScope> {
    ContractScope::parse(kind, id).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(
            0,
            Type::Text,
            Box::new(std::io::Error::other(error)),
        )
    })
}

fn persist_scope_from_row(row: &Row<'_>) -> rusqlite::Result<PersistedScopeRow> {
    let kind: String = row.get(0)?;
    let id: String = row.get(1)?;
    let models_json: String = row.get(2)?;
    let models: Vec<String> = serde_json::from_str(&models_json).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(2, Type::Text, Box::new(error))
    })?;
    Ok(PersistedScopeRow {
        scope: scope_from_row(&kind, &id)?,
        catalog_models: models,
        catalog_refreshed_at: parse_rfc3339_opt(row.get(3)?, 3)?,
        catalog_source: row.get(4)?,
        catalog_source_url: row.get(5)?,
        switches: ProtocolSwitches {
            chat_completions: row.get::<_, i64>(6)? != 0,
            responses: row.get::<_, i64>(7)? != 0,
            messages: row.get::<_, i64>(8)? != 0,
        },
        revision: row.get::<_, i64>(9)? as u64,
        updated_at: parse_rfc3339_column(row.get(10)?, 10)?,
    })
}

fn persist_evidence_from_row(row: &Row<'_>) -> rusqlite::Result<PersistedModelProtocol> {
    let kind: String = row.get(0)?;
    let id: String = row.get(1)?;
    let protocol_value: String = row.get(3)?;
    let protocol = UpstreamProtocolKind::try_from(protocol_value.as_str()).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(
            3,
            Type::Text,
            Box::new(std::io::Error::other(error.to_string())),
        )
    })?;
    let source_value: String = row.get(4)?;
    let source = ContractEvidenceSource::try_from(source_value.as_str()).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(
            4,
            Type::Text,
            Box::new(std::io::Error::other(error)),
        )
    })?;
    let last_probe: Option<String> = row.get(7)?;
    let last_probe_result = last_probe
        .map(|value| {
            ProbeResultKind::try_from(value.as_str()).map_err(|error| {
                rusqlite::Error::FromSqlConversionFailure(
                    7,
                    Type::Text,
                    Box::new(std::io::Error::other(error)),
                )
            })
        })
        .transpose()?;
    Ok(PersistedModelProtocol {
        scope: scope_from_row(&kind, &id)?,
        model_id: row.get(2)?,
        protocol,
        source,
        verified_at: parse_rfc3339_opt(row.get(5)?, 5)?,
        observed_at: parse_rfc3339_opt(row.get(6)?, 6)?,
        last_probe_result,
        last_probe_at: parse_rfc3339_opt(row.get(8)?, 8)?,
        last_probe_error: row.get(9)?,
    })
}

fn load_scope_on(conn: &Connection, scope: &ContractScope) -> Result<Option<PersistedScopeRow>> {
    conn.query_row(
        "SELECT scope_kind, scope_id, catalog_models_json, catalog_refreshed_at,
                catalog_source, catalog_source_url,
                chat_completions_enabled, responses_enabled, messages_enabled,
                revision, updated_at
         FROM provider_contract_scopes
         WHERE scope_kind = ?1 AND scope_id = ?2",
        params![scope.kind_str(), scope.id()],
        persist_scope_from_row,
    )
    .optional()
    .map_err(Into::into)
}

fn ensure_contract_scope_row(
    conn: &Connection,
    scope: &ContractScope,
    now: DateTime<Utc>,
) -> Result<()> {
    conn.execute(
        "INSERT INTO provider_contract_scopes (
            scope_kind, scope_id, catalog_models_json, catalog_refreshed_at,
            catalog_source, catalog_source_url,
            chat_completions_enabled, responses_enabled, messages_enabled,
            revision, updated_at
         ) VALUES (?1, ?2, '[]', NULL, '', '', 1, 1, 1, 1, ?3)
         ON CONFLICT(scope_kind, scope_id) DO NOTHING",
        params![scope.kind_str(), scope.id(), now.to_rfc3339()],
    )?;
    Ok(())
}

fn upsert_contract_catalog_on(
    conn: &Connection,
    scope: &ContractScope,
    models: &[String],
    refreshed_at: Option<DateTime<Utc>>,
    source: &str,
    source_url: &str,
    now: DateTime<Utc>,
) -> Result<()> {
    let models_json = serde_json::to_string(models)?;
    conn.execute(
        "INSERT INTO provider_contract_scopes (
            scope_kind, scope_id, catalog_models_json, catalog_refreshed_at,
            catalog_source, catalog_source_url,
            chat_completions_enabled, responses_enabled, messages_enabled,
            revision, updated_at
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, 1, 1, 1, 1, ?7)
         ON CONFLICT(scope_kind, scope_id) DO UPDATE SET
            catalog_models_json = excluded.catalog_models_json,
            catalog_refreshed_at = excluded.catalog_refreshed_at,
            catalog_source = excluded.catalog_source,
            catalog_source_url = excluded.catalog_source_url,
            revision = provider_contract_scopes.revision + 1,
            updated_at = excluded.updated_at",
        params![
            scope.kind_str(),
            scope.id(),
            models_json,
            refreshed_at.map(|value| value.to_rfc3339()),
            source,
            source_url,
            now.to_rfc3339(),
        ],
    )?;
    Ok(())
}

fn upsert_model_protocol_row_on(conn: &Connection, row: &PersistedModelProtocol) -> Result<()> {
    conn.execute(
        "INSERT INTO provider_contract_model_protocols (
            scope_kind, scope_id, model_id, protocol, source, verified_at,
            observed_at, last_probe_result, last_probe_at, last_probe_error
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
         ON CONFLICT(scope_kind, scope_id, model_id, protocol) DO UPDATE SET
            source = excluded.source,
            verified_at = excluded.verified_at,
            observed_at = excluded.observed_at,
            last_probe_result = excluded.last_probe_result,
            last_probe_at = excluded.last_probe_at,
            last_probe_error = excluded.last_probe_error",
        params![
            row.scope.kind_str(),
            row.scope.id(),
            row.model_id,
            row.protocol.as_str(),
            row.source.as_str(),
            row.verified_at.map(|value| value.to_rfc3339()),
            row.observed_at.map(|value| value.to_rfc3339()),
            row.last_probe_result.map(|value| value.as_str()),
            row.last_probe_at.map(|value| value.to_rfc3339()),
            row.last_probe_error,
        ],
    )?;
    Ok(())
}

fn bump_scope_revision_on(
    conn: &Connection,
    scope: &ContractScope,
    now: DateTime<Utc>,
) -> Result<u64> {
    ensure_contract_scope_row(conn, scope, now)?;
    conn.execute(
        "UPDATE provider_contract_scopes
         SET revision = revision + 1, updated_at = ?3
         WHERE scope_kind = ?1 AND scope_id = ?2",
        params![scope.kind_str(), scope.id(), now.to_rfc3339()],
    )?;
    let revision = conn.query_row(
        "SELECT revision FROM provider_contract_scopes
         WHERE scope_kind = ?1 AND scope_id = ?2",
        params![scope.kind_str(), scope.id()],
        |row| row.get::<_, i64>(0),
    )?;
    Ok(revision as u64)
}

fn schema_version_on(conn: &Connection) -> Result<i32> {
    if !table_exists(conn, "schema_version")? {
        return Ok(0);
    }
    Ok(conn
        .query_row(
            "SELECT version FROM schema_version ORDER BY version DESC LIMIT 1",
            [],
            |row| row.get(0),
        )
        .unwrap_or(0))
}

fn verify_schema_backup(path: &Path, prefix: &str, source_version: i32) -> Result<()> {
    let backup = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .with_context(|| format!("failed to open {prefix} backup {}", path.display()))?;
    let version = schema_version_on(&backup)?;
    anyhow::ensure!(
        version == source_version,
        "refusing to reuse invalid {prefix} backup {} (expected schema version {source_version}, found {version})",
        path.display()
    );
    Ok(())
}

fn ensure_schema_backup(
    conn: &Connection,
    db_path: &Path,
    prefix: &str,
    source_version: i32,
) -> Result<()> {
    let data_dir = db_path
        .parent()
        .ok_or_else(|| anyhow::anyhow!("database path has no parent: {}", db_path.display()))?;
    let mut existing_backups = std::fs::read_dir(data_dir)?
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path())
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with(prefix) && name.ends_with(".bak"))
        })
        .collect::<Vec<_>>();
    existing_backups.sort();
    if let Some(valid) = existing_backups
        .iter()
        .rev()
        .find(|path| verify_schema_backup(path, prefix, source_version).is_ok())
    {
        return verify_schema_backup(valid, prefix, source_version);
    }

    let timestamp = Utc::now().format("%Y%m%dT%H%M%S%9fZ");
    let backup_path = db_path.with_file_name(format!("{prefix}{timestamp}.bak"));

    // VACUUM INTO is SQLite's consistent online snapshot mechanism: unlike a
    // raw file copy it also includes committed pages still resident in WAL.
    // SQLite refuses to overwrite an existing target, preserving the first
    // rollback point across retries.
    let backup_value = backup_path.to_string_lossy().into_owned();
    match conn.execute("VACUUM main INTO ?1", [&backup_value]) {
        Ok(_) => verify_schema_backup(&backup_path, prefix, source_version),
        Err(error) if backup_path.exists() => {
            verify_schema_backup(&backup_path, prefix, source_version).with_context(|| {
                format!("{prefix} backup appeared concurrently after SQLite reported: {error}")
            })
        }
        Err(error) => Err(error).with_context(|| {
            format!(
                "failed to create {prefix} database backup {}",
                backup_path.display()
            )
        }),
    }
}

fn has_unversioned_legacy_tables(conn: &Connection) -> Result<bool> {
    Ok(table_exists(conn, "accounts")?
        || table_exists(conn, "settings")?
        || table_exists(conn, "forward_logs")?)
}

fn ensure_pre_v22_backup(conn: &Connection, db_path: &Path) -> Result<()> {
    let source_version = schema_version_on(conn)?;
    let unversioned_legacy = source_version == 0 && has_unversioned_legacy_tables(conn)?;
    if !(1..22).contains(&source_version) && !unversioned_legacy {
        return Ok(());
    }
    ensure_schema_backup(conn, db_path, PRE_V22_BACKUP_FILE_PREFIX, source_version)
}

fn ensure_pre_v23_backup(conn: &Connection, db_path: &Path) -> Result<()> {
    let source_version = schema_version_on(conn)?;
    let unversioned_legacy = source_version == 0 && has_unversioned_legacy_tables(conn)?;
    if !(1..23).contains(&source_version) && !unversioned_legacy {
        return Ok(());
    }
    ensure_schema_backup(conn, db_path, PRE_V23_BACKUP_FILE_PREFIX, source_version)
}

fn insert_account_row(
    conn: &Connection,
    account: &Account,
    purchase_date: &str,
    verification_status: ConnectionVerificationStatus,
) -> Result<()> {
    conn.execute(
        "INSERT INTO accounts (id, name, username, password_cipher, key_cipher, enabled, referral_code, recharge_date, sort_order, cooldown_until, cooldown_generic_until, cooldown_5h_until, cooldown_week_until, cooldown_month_until, cooldown_free_until, last_error, auth_error, account_type, setup_step, notes, created_at, updated_at, provider_id, offering_id, credential_kind, quota_scope, verification_status, connection_verified_at, verification_error)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, (SELECT COALESCE(MAX(sort_order), -1) + 1 FROM accounts), ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21, ?22, ?23, ?24, ?25, ?26, NULL, NULL)",
        params![
            account.id,
            account.name,
            account.username,
            account.password_cipher,
            account.key_cipher,
            account.enabled as i32,
            account.referral_code,
            purchase_date,
            account.cooldown_until.map(|t| t.to_rfc3339()),
            account.cooldown_generic_until.map(|t| t.to_rfc3339()),
            account.cooldown_5h_until.map(|t| t.to_rfc3339()),
            account.cooldown_week_until.map(|t| t.to_rfc3339()),
            account.cooldown_month_until.map(|t| t.to_rfc3339()),
            account.cooldown_free_until.map(|t| t.to_rfc3339()),
            account.last_error,
            account.auth_error,
            account.account_type.as_str(),
            account.setup_step.as_str(),
            account.notes,
            account.created_at.to_rfc3339(),
            account.updated_at.to_rfc3339(),
            account.provider_id,
            account.offering_id,
            account.credential_kind.as_str(),
            account.quota_scope.as_str(),
            verification_status.as_str(),
        ],
    )?;
    conn.execute(
        "INSERT INTO provider_usage_sync_state (
            account_id, last_success_at, last_attempt_at, next_eligible_at,
            failure_streak, last_expedited_at
         ) VALUES (?1, NULL, NULL, NULL, 0, NULL)",
        [&account.id],
    )?;
    Ok(())
}

fn persist_account_custom_config_on(
    conn: &Connection,
    account_id: &str,
    input: &AccountCustomConfigInput,
    allow_protocol_auth_change: bool,
) -> Result<()> {
    let base_url = validate_custom_base_url(&input.base_url)?;
    let now = Utc::now().to_rfc3339();
    let existing = conn
        .query_row(
            "SELECT upstream_protocol, auth_scheme FROM account_custom_configs WHERE account_id = ?1",
            [account_id],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
        )
        .optional()?;
    if let Some((protocol, auth_scheme)) = existing {
        anyhow::ensure!(
            allow_protocol_auth_change
                || (protocol == input.upstream_protocol.as_str()
                    && auth_scheme == input.auth_scheme.as_str()),
            "Custom protocol and auth scheme cannot be changed after create"
        );
        conn.execute(
            "UPDATE account_custom_configs
             SET base_url = ?2, upstream_protocol = ?3, auth_scheme = ?4, updated_at = ?5
             WHERE account_id = ?1",
            params![
                account_id,
                base_url,
                input.upstream_protocol.as_str(),
                input.auth_scheme.as_str(),
                now,
            ],
        )?;
    } else {
        conn.execute(
            "INSERT INTO account_custom_configs (
                account_id, base_url, upstream_protocol, auth_scheme, created_at, updated_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?5)",
            params![
                account_id,
                base_url,
                input.upstream_protocol.as_str(),
                input.auth_scheme.as_str(),
                now,
            ],
        )?;
    }
    mark_required_verification_stale_on(conn, account_id)?;
    Ok(())
}

/// Re-open a verification-required account as a disabled pending draft.
/// Go (`not_required`) rows are left untouched.
fn mark_required_verification_stale_on(conn: &Connection, account_id: &str) -> Result<()> {
    conn.execute(
        "UPDATE accounts
         SET verification_status = 'pending',
             connection_verified_at = NULL,
             verification_error = NULL,
             enabled = 0,
             updated_at = ?2
         WHERE id = ?1 AND verification_status <> 'not_required'",
        params![account_id, Utc::now().to_rfc3339()],
    )?;
    Ok(())
}

fn persist_account_model_capabilities_on(
    conn: &Connection,
    account_id: &str,
    capabilities: &[AccountModelCapabilityInput],
) -> Result<()> {
    if !capabilities.is_empty() {
        let expected_protocol: String = conn
            .query_row(
                "SELECT upstream_protocol FROM account_custom_configs WHERE account_id = ?1",
                [account_id],
                |row| row.get(0),
            )
            .optional()?
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "Custom model capabilities require a persisted custom_config.upstream_protocol"
                )
            })?;
        for capability in capabilities {
            anyhow::ensure!(
                capability.protocol.as_str() == expected_protocol,
                "model capability protocol must match account custom_config.upstream_protocol"
            );
        }
    }
    conn.execute(
        "DELETE FROM account_model_capabilities WHERE account_id = ?1",
        [account_id],
    )?;
    let mut seen = HashSet::new();
    for capability in capabilities {
        let model_id = validate_custom_model_id(&capability.model_id)?;
        let source = capability
            .source
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .unwrap_or("manual");
        let key = (model_id.clone(), capability.protocol.as_str().to_string());
        anyhow::ensure!(
            seen.insert(key),
            "duplicate model capability `{model_id}` / {}",
            capability.protocol.as_str()
        );
        conn.execute(
            "INSERT INTO account_model_capabilities (
                account_id, model_id, protocol, verified_at, source
             ) VALUES (?1, ?2, ?3, NULL, ?4)",
            params![account_id, model_id, capability.protocol.as_str(), source],
        )?;
    }
    mark_required_verification_stale_on(conn, account_id)?;
    Ok(())
}

fn persist_account_acknowledgement_on(
    conn: &Connection,
    account_id: &str,
    notice: PlanRiskNotice,
    accepted_at: DateTime<Utc>,
) -> Result<()> {
    let hash = notice.content_hash();
    conn.execute(
        "INSERT INTO account_acknowledgements (
            account_id, acknowledgement_id, version, content_hash, accepted_at
         ) VALUES (?1, ?2, ?3, ?4, ?5)
         ON CONFLICT(account_id, acknowledgement_id) DO UPDATE SET
            version = excluded.version,
            content_hash = excluded.content_hash,
            accepted_at = excluded.accepted_at",
        params![
            account_id,
            notice.acknowledgement_id,
            notice.version,
            hash,
            accepted_at.to_rfc3339(),
        ],
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
        let migrated_5h = percent_5h
            .zip(anchor_5h)
            .map(|baseline| effective_usage(0.0, Some(baseline), total_cost, limits.window_5h));
        let migrated_week = percent_week
            .zip(anchor_week)
            .map(|baseline| effective_usage(0.0, Some(baseline), total_cost, limits.window_week));
        let migrated_month = match percent_month.zip(anchor_month) {
            Some(baseline) => {
                let month_start = month_window_start_utc(&purchase_date)?.to_rfc3339();
                let actual_month_cost: f64 = tx.query_row(
                    "SELECT COALESCE(SUM(cost), 0) FROM forward_logs
                     WHERE account_id = ?1
                       AND cost_state IN ('priced', 'legacy_estimate')
                       AND timestamp >= ?2",
                    params![&id, month_start],
                    |row| row.get(0),
                )?;
                Some(
                    effective_usage(0.0, Some(baseline), total_cost, limits.window_month)
                        - actual_month_cost,
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
/// catalog plan with `routable=false` are forced off. Pairs come from
/// [`BUILTIN_PLANS`]; Go, Zen, and unknown provider/offering rows are skipped.
fn disable_unroutable_catalog_accounts(tx: &Transaction<'_>) -> Result<()> {
    if !(table_has_column(tx, "accounts", "provider_id")?
        && table_has_column(tx, "accounts", "offering_id")?
        && table_has_column(tx, "accounts", "enabled")?)
    {
        return Ok(());
    }
    for plan in BUILTIN_PLANS.iter().filter(|plan| !plan.routable) {
        tx.execute(
            "UPDATE accounts SET enabled = 0
             WHERE provider_id = ?1 AND offering_id = ?2 AND enabled <> 0",
            params![plan.offering.provider_id, plan.offering.offering_id],
        )?;
    }
    Ok(())
}

impl Database {
    pub fn open(data_dir: PathBuf) -> Result<Self> {
        std::fs::create_dir_all(&data_dir)?;
        let db_path = data_dir.join("data.sqlite");
        let conn = Connection::open(&db_path)?;
        conn.pragma_update(None, "busy_timeout", 5000)?;
        let existing_version = schema_version_on(&conn)?;
        anyhow::ensure!(
            existing_version <= CURRENT_SCHEMA_VERSION,
            "database schema version {existing_version} is newer than this build supports ({CURRENT_SCHEMA_VERSION}); restore a matching data directory and encryption key"
        );
        ensure_pre_v22_backup(&conn, &db_path)?;
        ensure_pre_v23_backup(&conn, &db_path)?;
        // WAL keeps request-path log writes off the rollback-journal FULL fsync;
        // must be set outside any transaction, hence before migrate().
        let _journal_mode: String =
            conn.query_row("PRAGMA journal_mode = WAL", [], |row| row.get(0))?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        let db = Self { conn };
        db.migrate()?;
        Ok(db)
    }

    fn migrate(&self) -> Result<()> {
        self.conn.execute(
            "CREATE TABLE IF NOT EXISTS schema_version (
                version INTEGER PRIMARY KEY
            )",
            [],
        )?;

        let tx = self.conn.unchecked_transaction()?;
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
                let cooldown = Self::compute_cooldown_until(&tx, &id, &now)?;
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
                            && offering == ANONYMOUS_FREE_OFFERING_ID
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
                        ANONYMOUS_FREE_OFFERING_ID,
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
                        ANONYMOUS_FREE_OFFERING_ID,
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
                    Self::upsert_free_channel_cooldown(&tx, until)?;
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
                    let cooldown = Self::compute_cooldown_until(&tx, id, &migrated_at_rfc)?;
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
                        ANONYMOUS_FREE_OFFERING_ID,
                        GO_OFFERING_ID,
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
                    params![OPENCODE_PROVIDER_ID, GO_OFFERING_ID],
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
                let usage = account_usage_with_limits_on(&tx, account_id, &limits, migrated_at)?;
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
                let month_started = month_window_start_utc(&purchase)?.to_rfc3339();
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
                    upstream_protocol TEXT NOT NULL,
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
                CREATE TABLE IF NOT EXISTS account_acknowledgements (
                    account_id TEXT NOT NULL,
                    acknowledgement_id TEXT NOT NULL,
                    version TEXT NOT NULL,
                    content_hash TEXT NOT NULL,
                    accepted_at TEXT NOT NULL,
                    PRIMARY KEY (account_id, acknowledgement_id),
                    FOREIGN KEY (account_id) REFERENCES accounts(id) ON DELETE CASCADE
                );
                CREATE INDEX IF NOT EXISTS idx_account_model_capabilities_account
                    ON account_model_capabilities(account_id);
                CREATE INDEX IF NOT EXISTS idx_account_acknowledgements_account
                    ON account_acknowledgements(account_id);",
            )?;

            if table_has_column(&tx, "accounts", "provider_id")?
                && table_has_column(&tx, "accounts", "offering_id")?
            {
                if table_has_column(&tx, "accounts", "enabled")? {
                    tx.execute(
                        "UPDATE accounts SET verification_status = 'pending', verification_error = NULL,
                                enabled = 0
                         WHERE provider_id = ?1 AND offering_id = ?2",
                        params![COMMAND_CODE_PROVIDER_ID, GOAT_OFFERING_ID],
                    )?;
                } else {
                    tx.execute(
                        "UPDATE accounts SET verification_status = 'pending', verification_error = NULL
                         WHERE provider_id = ?1 AND offering_id = ?2",
                        params![COMMAND_CODE_PROVIDER_ID, GOAT_OFFERING_ID],
                    )?;
                }
                tx.execute(
                    "UPDATE accounts SET verification_status = 'not_required', verification_error = NULL
                     WHERE NOT (provider_id = ?1 AND offering_id = ?2)
                       AND (verification_status IS NULL OR verification_status = 'not_required')",
                    params![COMMAND_CODE_PROVIDER_ID, GOAT_OFFERING_ID],
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
        // on released v1.6.3 libraries and on fresh installs.
        ensure_column(&tx, "accounts", "notes", "TEXT")?;
        // Idempotent backstop for v21 columns when an unreleased draft already
        // reported a higher schema_version number without these fields.
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
                upstream_protocol TEXT NOT NULL,
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
            CREATE TABLE IF NOT EXISTS account_acknowledgements (
                account_id TEXT NOT NULL,
                acknowledgement_id TEXT NOT NULL,
                version TEXT NOT NULL,
                content_hash TEXT NOT NULL,
                accepted_at TEXT NOT NULL,
                PRIMARY KEY (account_id, acknowledgement_id),
                FOREIGN KEY (account_id) REFERENCES accounts(id) ON DELETE CASCADE
            );",
        )?;
        tx.execute_batch(PROVIDER_CONTRACT_V26_DDL)?;
        // Fail-closed leftovers for every catalogued-but-unroutable offering,
        // including already-v23 verified rows. Sparse pre-v22 fixtures may still
        // lack `enabled` even after additive column backstops, so skip rather
        // than fail the open. Go/Zen and unknown pairs are not in this set.
        disable_unroutable_catalog_accounts(&tx)?;
        // Unverified GOAT rows from the first v23 contract slice stay pending.
        // Verified leftovers keep their verification snapshot; enablement is
        // handled above.
        if table_has_column(&tx, "accounts", "provider_id")?
            && table_has_column(&tx, "accounts", "offering_id")?
            && table_has_column(&tx, "accounts", "verification_status")?
        {
            tx.execute(
                "UPDATE accounts SET verification_status = 'pending', verification_error = NULL
                 WHERE provider_id = ?1 AND offering_id = ?2
                   AND verification_status <> 'verified'",
                params![COMMAND_CODE_PROVIDER_ID, GOAT_OFFERING_ID],
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
        let legacy_free_cooldown: Option<String> = tx.query_row(
            "SELECT MAX(cooldown_free_until)
             FROM accounts
             WHERE cooldown_free_until IS NOT NULL
               AND cooldown_free_until > ?1",
            params![Utc::now().to_rfc3339()],
            |row| row.get(0),
        )?;
        if let Some(until) = legacy_free_cooldown {
            Self::upsert_free_channel_cooldown(&tx, &until)?;
        }

        // `free_alias_enabled` was a development-era projection of the former
        // Deny/Explicit/Prefer policy. Zen Free is now enabled or disabled as a
        // normal ordered provider account; keep the legacy column inert without
        // rewriting timestamps or requiring a destructive table rebuild.
        tx.execute(
            "UPDATE accounts SET free_alias_enabled = 0
             WHERE free_alias_enabled <> 0",
            [],
        )?;

        tx.commit()?;
        Ok(())
    }

    pub fn insert_pricing_snapshot(&self, snapshot: &PricingSnapshot) -> Result<()> {
        let snapshot_json = serde_json::to_string(snapshot)?;
        let tx = self.conn.unchecked_transaction()?;
        tx.execute(
            "INSERT OR IGNORE INTO pricing_snapshots
             (revision, activated_at, document_updated_at, source_url, content_hash, snapshot_json)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                snapshot.revision,
                snapshot.activated_at,
                snapshot.document_updated_at,
                snapshot.source_url,
                snapshot.content_hash,
                snapshot_json,
            ],
        )?;
        tx.execute(
            "INSERT OR IGNORE INTO provider_pricing_snapshots
             (provider_id, offering_id, revision, activated_at, document_updated_at,
              source_url, content_hash, snapshot_json)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                OPENCODE_PROVIDER_ID,
                GO_OFFERING_ID,
                snapshot.revision,
                snapshot.activated_at,
                snapshot.document_updated_at,
                snapshot.source_url,
                snapshot.content_hash,
                snapshot_json,
            ],
        )?;
        tx.commit()?;
        Ok(())
    }

    pub fn zen_free_model_catalog(
        &self,
    ) -> Result<Option<crate::kernel::zen::ZenFreeModelCatalog>> {
        self.conn
            .query_row(
                "SELECT models_json, refreshed_at, source_url
                 FROM provider_model_catalogs
                 WHERE provider_id = ?1 AND offering_id = ?2",
                params![OPENCODE_ZEN_FREE_PROVIDER_ID, ANONYMOUS_FREE_OFFERING_ID],
                |row| {
                    let models_json: String = row.get(0)?;
                    let refreshed_at: Option<String> = row.get(1)?;
                    let models = serde_json::from_str(&models_json).map_err(|error| {
                        rusqlite::Error::FromSqlConversionFailure(0, Type::Text, Box::new(error))
                    })?;
                    let refreshed_at = refreshed_at
                        .map(|value| {
                            DateTime::parse_from_rfc3339(&value)
                                .map(|value| value.with_timezone(&Utc))
                                .map_err(|error| {
                                    rusqlite::Error::FromSqlConversionFailure(
                                        1,
                                        Type::Text,
                                        Box::new(error),
                                    )
                                })
                        })
                        .transpose()?;
                    Ok(crate::kernel::zen::ZenFreeModelCatalog {
                        models,
                        refreshed_at,
                        source_url: row.get(2)?,
                    })
                },
            )
            .optional()
            .map_err(Into::into)
    }

    pub fn set_zen_free_model_catalog(
        &self,
        catalog: &crate::kernel::zen::ZenFreeModelCatalog,
    ) -> Result<()> {
        let models_json = serde_json::to_string(&catalog.models)?;
        let refreshed_at = catalog.refreshed_at.map(|value| value.to_rfc3339());
        let tx = self.conn.unchecked_transaction()?;
        tx.execute(
            "INSERT INTO provider_model_catalogs
             (provider_id, offering_id, models_json, refreshed_at, source_url)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(provider_id, offering_id) DO UPDATE SET
                 models_json = excluded.models_json,
                 refreshed_at = excluded.refreshed_at,
                 source_url = excluded.source_url",
            params![
                OPENCODE_ZEN_FREE_PROVIDER_ID,
                ANONYMOUS_FREE_OFFERING_ID,
                models_json,
                refreshed_at,
                catalog.source_url,
            ],
        )?;
        upsert_contract_catalog_on(
            &tx,
            &ContractScope::provider(OPENCODE_ZEN_FREE_PROVIDER_ID),
            &catalog.models,
            catalog.refreshed_at,
            CATALOG_SOURCE_OFFICIAL_ZEN,
            &catalog.source_url,
            Utc::now(),
        )?;
        tx.commit()?;
        Ok(())
    }

    pub fn load_persisted_contracts(&self) -> Result<PersistedContracts> {
        let mut persisted = PersistedContracts::default();
        {
            let mut stmt = self.conn.prepare(
                "SELECT scope_kind, scope_id, catalog_models_json, catalog_refreshed_at,
                        catalog_source, catalog_source_url,
                        chat_completions_enabled, responses_enabled, messages_enabled,
                        revision, updated_at
                 FROM provider_contract_scopes",
            )?;
            let rows = stmt.query_map([], persist_scope_from_row)?;
            for row in rows {
                let row = row?;
                persisted.scopes.insert(row.scope.clone(), row);
            }
        }
        {
            let mut stmt = self.conn.prepare(
                "SELECT scope_kind, scope_id, model_id, protocol, source, verified_at,
                        observed_at, last_probe_result, last_probe_at, last_probe_error
                 FROM provider_contract_model_protocols",
            )?;
            let rows = stmt.query_map([], persist_evidence_from_row)?;
            for row in rows {
                let row = row?;
                persisted
                    .evidence
                    .entry(row.scope.clone())
                    .or_default()
                    .push(row);
            }
        }
        Ok(persisted)
    }

    pub fn load_persisted_scope(&self, scope: &ContractScope) -> Result<Option<PersistedScopeRow>> {
        self.conn
            .query_row(
                "SELECT scope_kind, scope_id, catalog_models_json, catalog_refreshed_at,
                        catalog_source, catalog_source_url,
                        chat_completions_enabled, responses_enabled, messages_enabled,
                        revision, updated_at
                 FROM provider_contract_scopes
                 WHERE scope_kind = ?1 AND scope_id = ?2",
                params![scope.kind_str(), scope.id()],
                persist_scope_from_row,
            )
            .optional()
            .map_err(Into::into)
    }

    pub fn set_contract_catalog(
        &self,
        scope: &ContractScope,
        models: &[String],
        refreshed_at: Option<DateTime<Utc>>,
        source: &str,
        source_url: &str,
        now: DateTime<Utc>,
    ) -> Result<PersistedScopeRow> {
        let tx = self.conn.unchecked_transaction()?;
        upsert_contract_catalog_on(&tx, scope, models, refreshed_at, source, source_url, now)?;
        let row = load_scope_on(&tx, scope)?
            .ok_or_else(|| anyhow::anyhow!("contract scope was not persisted"))?;
        tx.commit()?;
        Ok(row)
    }

    pub fn set_protocol_switch(
        &self,
        scope: &ContractScope,
        protocol: UpstreamProtocolKind,
        enabled: bool,
        now: DateTime<Utc>,
    ) -> Result<PersistedScopeRow> {
        let tx = self.conn.unchecked_transaction()?;
        ensure_contract_scope_row(&tx, scope, now)?;
        let mut current = load_scope_on(&tx, scope)?
            .ok_or_else(|| anyhow::anyhow!("contract scope was not persisted"))?;
        current.switches.set(protocol, enabled);
        current.revision = current.revision.saturating_add(1);
        current.updated_at = now;
        tx.execute(
            "UPDATE provider_contract_scopes SET
                chat_completions_enabled = ?3,
                responses_enabled = ?4,
                messages_enabled = ?5,
                revision = ?6,
                updated_at = ?7
             WHERE scope_kind = ?1 AND scope_id = ?2",
            params![
                scope.kind_str(),
                scope.id(),
                current.switches.chat_completions as i64,
                current.switches.responses as i64,
                current.switches.messages as i64,
                current.revision as i64,
                current.updated_at.to_rfc3339(),
            ],
        )?;
        tx.commit()?;
        Ok(current)
    }

    pub fn load_model_protocol(
        &self,
        scope: &ContractScope,
        model_id: &str,
        protocol: UpstreamProtocolKind,
    ) -> Result<Option<PersistedModelProtocol>> {
        self.conn
            .query_row(
                "SELECT scope_kind, scope_id, model_id, protocol, source, verified_at,
                        observed_at, last_probe_result, last_probe_at, last_probe_error
                 FROM provider_contract_model_protocols
                 WHERE scope_kind = ?1 AND scope_id = ?2 AND model_id = ?3 AND protocol = ?4",
                params![scope.kind_str(), scope.id(), model_id, protocol.as_str()],
                persist_evidence_from_row,
            )
            .optional()
            .map_err(Into::into)
    }

    pub fn upsert_model_protocol(&self, row: &PersistedModelProtocol) -> Result<PersistedScopeRow> {
        self.upsert_model_protocols(std::slice::from_ref(row))
    }

    /// Persist a nonempty set of protocol observations and advance the nested
    /// contract-scope revision exactly once, in a single SQLite transaction.
    ///
    /// All rows must share one [`ContractScope`]. Mixed scopes are rejected
    /// before any write so a caller cannot commit a partial batch.
    pub fn upsert_model_protocols(
        &self,
        rows: &[PersistedModelProtocol],
    ) -> Result<PersistedScopeRow> {
        let Some((first, rest)) = rows.split_first() else {
            anyhow::bail!("protocol observation batch must be nonempty");
        };
        if let Some(other) = rest.iter().find(|row| row.scope != first.scope) {
            anyhow::bail!(
                "protocol observations mix contract scopes `{}:{}` and `{}:{}`",
                first.scope.kind_str(),
                first.scope.id(),
                other.scope.kind_str(),
                other.scope.id()
            );
        }
        let tx = self.conn.unchecked_transaction()?;
        for row in rows {
            upsert_model_protocol_row_on(&tx, row)?;
        }
        let now = rows
            .iter()
            .rev()
            .find_map(|row| row.observed_at)
            .unwrap_or_else(Utc::now);
        bump_scope_revision_on(&tx, &first.scope, now)?;
        let scope = load_scope_on(&tx, &first.scope)?
            .ok_or_else(|| anyhow::anyhow!("contract scope was not persisted"))?;
        tx.commit()?;
        Ok(scope)
    }

    pub fn latest_pricing_snapshot(&self) -> Result<Option<PricingSnapshot>> {
        let snapshot_json = self
            .conn
            .query_row(
                "SELECT snapshot_json FROM pricing_snapshots
                 ORDER BY datetime(activated_at) DESC, rowid DESC LIMIT 1",
                [],
                |row| row.get::<_, String>(0),
            )
            .optional()?;
        snapshot_json
            .map(|json| serde_json::from_str(&json).map_err(Into::into))
            .transpose()
    }

    pub fn insert_provider_pricing_snapshot(
        &self,
        snapshot: &ProviderPricingSnapshot,
    ) -> Result<()> {
        anyhow::ensure!(
            builtin_offering(&snapshot.provider_id, &snapshot.offering_id).is_some(),
            "unknown provider offering `{}/{}`",
            snapshot.provider_id,
            snapshot.offering_id
        );
        self.conn.execute(
            "INSERT OR IGNORE INTO provider_pricing_snapshots
             (provider_id, offering_id, revision, activated_at, document_updated_at,
              source_url, content_hash, snapshot_json)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                snapshot.provider_id,
                snapshot.offering_id,
                snapshot.revision,
                snapshot.activated_at,
                snapshot.document_updated_at,
                snapshot.source_url,
                snapshot.content_hash,
                snapshot.snapshot_json,
            ],
        )?;
        Ok(())
    }

    pub fn latest_provider_pricing_snapshot(
        &self,
        provider_id: &str,
        offering_id: &str,
    ) -> Result<Option<ProviderPricingSnapshot>> {
        self.conn
            .query_row(
                "SELECT provider_id, offering_id, revision, activated_at,
                        document_updated_at, source_url, content_hash, snapshot_json
                 FROM provider_pricing_snapshots
                 WHERE provider_id = ?1 AND offering_id = ?2
                 ORDER BY activated_at DESC, rowid DESC LIMIT 1",
                params![provider_id, offering_id],
                |row| {
                    Ok(ProviderPricingSnapshot {
                        provider_id: row.get(0)?,
                        offering_id: row.get(1)?,
                        revision: row.get(2)?,
                        activated_at: row.get(3)?,
                        document_updated_at: row.get(4)?,
                        source_url: row.get(5)?,
                        content_hash: row.get(6)?,
                        snapshot_json: row.get(7)?,
                    })
                },
            )
            .optional()
            .map_err(Into::into)
    }

    // Accounts
    pub fn create_account(&self, account: &Account) -> Result<()> {
        anyhow::ensure!(
            account.id != ZEN_FREE_ACCOUNT_ID,
            "Zen Free is database-owned and cannot be created through the generic account API"
        );
        account.validate_provider_binding()?;
        ensure_enabled_offering_is_routable(
            &account.provider_id,
            &account.offering_id,
            account.enabled,
        )?;
        let purchase_date = if account.purchase_date.trim().is_empty() {
            local_today()
        } else {
            normalize_purchase_date(&account.purchase_date)?
        };
        let verification_status = builtin_plan(&account.provider_id, &account.offering_id)
            .map(default_verification_status)
            .unwrap_or(ConnectionVerificationStatus::NotRequired);
        let tx = self.conn.unchecked_transaction()?;
        insert_account_row(&tx, account, &purchase_date, verification_status)?;
        tx.commit()?;
        Ok(())
    }

    /// Persist the account row together with Custom config/capabilities and a
    /// required SCNet acknowledgement in one SQLite transaction. A crash or
    /// constraint failure leaves no orphan account and does not rely on
    /// compensating deletes.
    pub fn create_account_with_contract(
        &self,
        account: &Account,
        custom_config: Option<&AccountCustomConfigInput>,
        capabilities: &[AccountModelCapabilityInput],
        risk_notice: Option<PlanRiskNotice>,
    ) -> Result<()> {
        anyhow::ensure!(
            account.id != ZEN_FREE_ACCOUNT_ID,
            "Zen Free is database-owned and cannot be created through the generic account API"
        );
        account.validate_provider_binding()?;
        ensure_enabled_offering_is_routable(
            &account.provider_id,
            &account.offering_id,
            account.enabled,
        )?;
        let plan = builtin_plan(&account.provider_id, &account.offering_id)
            .ok_or_else(|| anyhow::anyhow!("unknown provider offering"))?;
        if plan_requires_custom_config(plan) {
            anyhow::ensure!(
                custom_config.is_some(),
                "Custom API accounts require a base URL, upstream protocol, and auth scheme"
            );
            anyhow::ensure!(
                !capabilities.is_empty(),
                "Custom API accounts require at least one model capability"
            );
        } else {
            anyhow::ensure!(
                custom_config.is_none(),
                "custom config is only available for Custom API accounts"
            );
            anyhow::ensure!(
                capabilities.is_empty(),
                "model capabilities are only available for Custom API accounts"
            );
        }
        if let Some(required) = plan.risk_notice {
            let accepted = risk_notice.is_some_and(|notice| {
                notice.acknowledgement_id == required.acknowledgement_id
                    && notice.version == required.version
            });
            anyhow::ensure!(
                accepted,
                "this Plan requires a matching versioned risk acknowledgement before create"
            );
        }
        let purchase_date = if account.purchase_date.trim().is_empty() {
            local_today()
        } else {
            normalize_purchase_date(&account.purchase_date)?
        };
        let verification_status = default_verification_status(plan);
        let tx = self.conn.unchecked_transaction()?;
        insert_account_row(&tx, account, &purchase_date, verification_status)?;
        if let Some(config) = custom_config {
            persist_account_custom_config_on(&tx, &account.id, config, true)?;
        }
        if !capabilities.is_empty() {
            persist_account_model_capabilities_on(&tx, &account.id, capabilities)?;
        }
        if let Some(notice) = plan.risk_notice {
            persist_account_acknowledgement_on(&tx, &account.id, notice, Utc::now())?;
        }
        tx.commit()?;
        Ok(())
    }

    pub fn update_account(
        &self,
        id: &str,
        update: &AccountUpdate,
        key_cipher: Option<&str>,
        password_cipher: Option<&str>,
    ) -> Result<()> {
        let existing = self
            .get_account(id)?
            .ok_or_else(|| anyhow::anyhow!("account not found"))?;
        if existing.is_zen_free() {
            anyhow::bail!("Zen Free settings must use the dedicated provider-settings operation");
        }
        let name = update.name.as_ref().unwrap_or(&existing.name);
        let username = match &update.username {
            Some(s) if s.is_empty() => None,
            Some(s) => Some(s.clone()),
            None => existing.username.clone(),
        };
        let requested_enabled = update.enabled.unwrap_or(existing.enabled);
        let referral_code = match &update.referral_code {
            Some(s) if s.is_empty() => None,        // explicitly cleared
            Some(s) => Some(s.clone()),             // set to new value
            None => existing.referral_code.clone(), // not provided, keep existing
        };
        let purchase_date = match &update.purchase_date {
            Some(value) => normalize_purchase_date(value)?,
            None => existing.purchase_date.clone(),
        };
        let purchase_date_changed = purchase_date != existing.purchase_date;
        let notes = match &update.notes {
            Some(s) if s.is_empty() => None,
            Some(s) => Some(s.clone()),
            None => existing.notes.clone(),
        };
        let key = key_cipher.unwrap_or(&existing.key_cipher);
        let password = match password_cipher {
            Some("") => None,
            Some(s) => Some(s.to_string()),
            None => existing.password_cipher.clone(),
        };
        let key_replaced = key_cipher.is_some();
        let requires_verification = builtin_plan(&existing.provider_id, &existing.offering_id)
            .is_some_and(|plan| plan.verification_policy == VerificationPolicy::Required);
        // Gate the value that will actually persist. Verification-required key
        // replacement still forces enabled=0 in SQL; that write is not an
        // enablement of an unroutable Plan.
        let enabled = if key_replaced && requires_verification {
            false
        } else {
            requested_enabled
        };
        ensure_enabled_offering_is_routable(&existing.provider_id, &existing.offering_id, enabled)?;

        let tx = self.conn.unchecked_transaction()?;
        tx.execute(
            "UPDATE accounts SET name = ?1, username = ?2, password_cipher = ?3, key_cipher = ?4,
             enabled = CASE WHEN ?10 AND ?13 THEN 0 ELSE ?5 END, referral_code = ?6, recharge_date = ?7, notes = ?8,
             usage_month_window_cost_offset = CASE WHEN ?9 THEN 0 ELSE usage_month_window_cost_offset END,
             auth_error = CASE WHEN ?10 THEN NULL ELSE auth_error END,
             verification_status = CASE WHEN ?10 AND ?13 THEN 'pending' ELSE verification_status END,
             connection_verified_at = CASE WHEN ?10 AND ?13 THEN NULL ELSE connection_verified_at END,
             verification_error = CASE WHEN ?10 AND ?13 THEN NULL ELSE verification_error END,
             updated_at = ?11 WHERE id = ?12",
            params![
                name,
                username,
                password,
                key,
                enabled as i32,
                referral_code,
                purchase_date,
                notes,
                purchase_date_changed,
                key_replaced,
                Utc::now().to_rfc3339(),
                id,
                requires_verification,
            ],
        )?;
        tx.commit()?;
        Ok(())
    }

    pub fn set_config(&self, config_json: &str) -> Result<()> {
        self.conn.execute(
            "INSERT INTO settings (key, value) VALUES ('config', ?1)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            [config_json],
        )?;
        Ok(())
    }

    /// The Zen Free singleton has one canonical user setting: enabled.
    /// The retired `free_alias_enabled` column is forced to zero for rollback
    /// compatibility but no longer participates in runtime behavior.
    pub fn set_zen_free_enabled(&self, enabled: bool) -> Result<()> {
        ensure_enabled_offering_is_routable(
            OPENCODE_ZEN_FREE_PROVIDER_ID,
            ANONYMOUS_FREE_OFFERING_ID,
            enabled,
        )?;
        let changed = self.conn.execute(
            "UPDATE accounts SET enabled = ?2, free_alias_enabled = 0, updated_at = ?3
             WHERE id = ?1 AND provider_id = ?4 AND offering_id = ?5",
            params![
                ZEN_FREE_ACCOUNT_ID,
                enabled as i32,
                Utc::now().to_rfc3339(),
                OPENCODE_ZEN_FREE_PROVIDER_ID,
                ANONYMOUS_FREE_OFFERING_ID,
            ],
        )?;
        anyhow::ensure!(changed == 1, "Zen Free singleton is missing");
        Ok(())
    }

    pub fn delete_account(&mut self, id: &str) -> Result<()> {
        anyhow::ensure!(
            id != ZEN_FREE_ACCOUNT_ID,
            "Zen Free is a built-in singleton and cannot be deleted"
        );
        let tx = self.conn.transaction()?;
        tx.execute("DELETE FROM quota_windows WHERE account_id = ?1", [id])?;
        tx.execute("DELETE FROM credit_balances WHERE account_id = ?1", [id])?;
        tx.execute(
            "DELETE FROM provider_usage_sync_state WHERE account_id = ?1",
            [id],
        )?;
        tx.execute(
            "DELETE FROM account_custom_configs WHERE account_id = ?1",
            [id],
        )?;
        tx.execute(
            "DELETE FROM account_model_capabilities WHERE account_id = ?1",
            [id],
        )?;
        tx.execute(
            "DELETE FROM account_acknowledgements WHERE account_id = ?1",
            [id],
        )?;
        tx.execute(
            "DELETE FROM provider_contract_model_protocols
             WHERE scope_kind = ?1 AND scope_id = ?2",
            params![SCOPE_KIND_CUSTOM_ENDPOINT, id],
        )?;
        tx.execute(
            "DELETE FROM provider_contract_scopes
             WHERE scope_kind = ?1 AND scope_id = ?2",
            params![SCOPE_KIND_CUSTOM_ENDPOINT, id],
        )?;
        tx.execute("DELETE FROM accounts WHERE id = ?1", [id])?;
        tx.commit()?;
        Ok(())
    }

    pub fn schema_version(&self) -> Result<i32> {
        let version = self
            .conn
            .query_row(
                "SELECT version FROM schema_version ORDER BY version DESC LIMIT 1",
                [],
                |row| row.get(0),
            )
            .unwrap_or(0);
        Ok(version)
    }

    pub fn account_verification_state(
        &self,
        account_id: &str,
    ) -> Result<Option<AccountVerificationState>> {
        self.conn
            .query_row(
                "SELECT id, verification_status, connection_verified_at, verification_error
                 FROM accounts WHERE id = ?1",
                [account_id],
                account_verification_from_row,
            )
            .optional()
            .map_err(Into::into)
    }

    pub fn load_account_contract(&self, account_id: &str) -> Result<AccountContractState> {
        let Some(verification) = self.account_verification_state(account_id)? else {
            return Ok(AccountContractState::default());
        };
        Ok(AccountContractState {
            verification,
            custom_config: self.account_custom_config(account_id)?,
            model_capabilities: self.list_account_model_capabilities(account_id)?,
            acknowledgements: self.list_account_acknowledgements(account_id)?,
        })
    }

    pub fn set_account_verification(
        &self,
        account_id: &str,
        status: ConnectionVerificationStatus,
        verified_at: Option<DateTime<Utc>>,
        error: Option<&str>,
    ) -> Result<bool> {
        let changed = self.conn.execute(
            "UPDATE accounts
             SET verification_status = ?2,
                 connection_verified_at = ?3,
                 verification_error = ?4,
                 updated_at = ?5
             WHERE id = ?1",
            params![
                account_id,
                status.as_str(),
                verified_at.map(|value| value.to_rfc3339()),
                error,
                Utc::now().to_rfc3339(),
            ],
        )?;
        Ok(changed == 1)
    }

    /// Snapshot the Custom verification contract, including the raw account
    /// revision token and encrypted key identity. `None` if the account row is
    /// gone. Missing config is `Ok(None)` only when the account itself is gone;
    /// a row without config returns an error so callers fail closed.
    pub fn capture_custom_verification_contract(
        &self,
        account_id: &str,
    ) -> Result<Option<crate::custom::CustomVerificationContract>> {
        let Some((updated_at, key_cipher, _status)) =
            self.custom_verification_row_identity(account_id)?
        else {
            return Ok(None);
        };
        let config = self.account_custom_config(account_id)?.ok_or_else(|| {
            anyhow::anyhow!(
                "Custom API accounts require a persisted base URL, protocol, and auth scheme"
            )
        })?;
        let capabilities = self.list_account_model_capabilities_declared(account_id)?;
        Ok(Some(crate::custom::CustomVerificationContract::from_parts(
            account_id,
            updated_at,
            key_cipher,
            &config,
            &capabilities,
        )))
    }

    /// Commit a Custom probe only when the captured contract and unverified
    /// state still match. Returns `false` for key/config/capability/delete/
    /// concurrent-verification races without writing.
    pub fn commit_custom_verification_if_contract_matches(
        &self,
        contract: &crate::custom::CustomVerificationContract,
        status: ConnectionVerificationStatus,
        verified_at: Option<DateTime<Utc>>,
        error: Option<&str>,
    ) -> Result<bool> {
        let tx = self.conn.unchecked_transaction()?;
        if !custom_verification_contract_still_matches_on(&tx, contract)? {
            return Ok(false);
        }
        let changed = tx.execute(
            "UPDATE accounts
             SET verification_status = ?2,
                 connection_verified_at = ?3,
                 verification_error = ?4,
                 updated_at = ?5
             WHERE id = ?1
               AND verification_status IN ('pending', 'failed')
               AND updated_at = ?6
               AND key_cipher = ?7",
            params![
                contract.account_id,
                status.as_str(),
                verified_at.map(|value| value.to_rfc3339()),
                error,
                Utc::now().to_rfc3339(),
                contract.account_updated_at,
                contract.key_cipher,
            ],
        )?;
        if changed != 1 {
            return Ok(false);
        }
        tx.commit()?;
        Ok(true)
    }

    fn custom_verification_row_identity(
        &self,
        account_id: &str,
    ) -> Result<Option<(String, String, String)>> {
        self.conn
            .query_row(
                "SELECT updated_at, key_cipher, verification_status
                 FROM accounts WHERE id = ?1",
                [account_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()
            .map_err(Into::into)
    }

    pub fn account_custom_config(&self, account_id: &str) -> Result<Option<AccountCustomConfig>> {
        self.conn
            .query_row(
                "SELECT account_id, base_url, upstream_protocol, auth_scheme, created_at, updated_at
                 FROM account_custom_configs WHERE account_id = ?1",
                [account_id],
                account_custom_config_from_row,
            )
            .optional()
            .map_err(Into::into)
    }

    pub fn upsert_account_custom_config(
        &self,
        account_id: &str,
        input: &AccountCustomConfigInput,
        allow_protocol_auth_change: bool,
    ) -> Result<AccountCustomConfig> {
        self.commit_account_custom_config(account_id, input, allow_protocol_auth_change)?;
        self.account_custom_config(account_id)?
            .ok_or_else(|| anyhow::anyhow!("custom config was not persisted"))
    }

    /// Persist and commit a Custom account config without performing a
    /// fallible post-commit read. Callers that publish an external revision
    /// can therefore advance it immediately after this method returns `Ok`.
    pub(crate) fn commit_account_custom_config(
        &self,
        account_id: &str,
        input: &AccountCustomConfigInput,
        allow_protocol_auth_change: bool,
    ) -> Result<()> {
        anyhow::ensure!(self.get_account(account_id)?.is_some(), "account not found");
        let tx = self.conn.unchecked_transaction()?;
        persist_account_custom_config_on(&tx, account_id, input, allow_protocol_auth_change)?;
        tx.commit()?;
        Ok(())
    }

    pub fn list_account_model_capabilities(
        &self,
        account_id: &str,
    ) -> Result<Vec<AccountModelCapability>> {
        let mut stmt = self.conn.prepare(
            "SELECT account_id, model_id, protocol, verified_at, source
             FROM account_model_capabilities
             WHERE account_id = ?1
             ORDER BY model_id ASC, protocol ASC",
        )?;
        let rows = stmt.query_map([account_id], account_model_capability_from_row)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    pub fn list_account_model_capabilities_declared(
        &self,
        account_id: &str,
    ) -> Result<Vec<AccountModelCapability>> {
        let mut stmt = self.conn.prepare(
            "SELECT account_id, model_id, protocol, verified_at, source
             FROM account_model_capabilities
             WHERE account_id = ?1
             ORDER BY rowid ASC",
        )?;
        let rows = stmt.query_map([account_id], account_model_capability_from_row)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    /// Custom accounts in saved account order, with config and declared capabilities.
    pub fn list_custom_account_runtimes(&self) -> Result<Vec<crate::custom::CustomAccountRuntime>> {
        let mut stmt = self.conn.prepare(
            "SELECT a.id, a.enabled, a.verification_status, a.setup_step, a.key_cipher,
                    c.account_id, c.base_url, c.upstream_protocol, c.auth_scheme,
                    c.created_at, c.updated_at
             FROM accounts a
             INNER JOIN account_custom_configs c ON c.account_id = a.id
             WHERE a.provider_id = ?1 AND a.offering_id = ?2
             ORDER BY a.sort_order ASC, a.created_at ASC, a.id ASC",
        )?;
        let rows = stmt.query_map(params![CUSTOM_PROVIDER_ID, CUSTOM_API_OFFERING_ID], |row| {
            let account_id: String = row.get(0)?;
            let enabled = row.get::<_, i32>(1)? != 0;
            let status_value = row.get::<_, String>(2)?;
            let verification_status = ConnectionVerificationStatus::try_from(status_value.as_str())
                .map_err(|error| {
                    rusqlite::Error::FromSqlConversionFailure(2, Type::Text, Box::new(error))
                })?;
            let setup_value = row.get::<_, String>(3)?;
            let setup_step = AccountSetupStep::try_from(setup_value.as_str()).map_err(|error| {
                rusqlite::Error::FromSqlConversionFailure(
                    3,
                    Type::Text,
                    Box::new(std::io::Error::other(error)),
                )
            })?;
            let key_cipher: String = row.get(4)?;
            let config = AccountCustomConfig {
                account_id: row.get(5)?,
                base_url: row.get(6)?,
                upstream_protocol: {
                    let value = row.get::<_, String>(7)?;
                    UpstreamProtocolKind::try_from(value.as_str()).map_err(|error| {
                        rusqlite::Error::FromSqlConversionFailure(7, Type::Text, Box::new(error))
                    })?
                },
                auth_scheme: {
                    let value = row.get::<_, String>(8)?;
                    UpstreamAuthScheme::try_from(value.as_str()).map_err(|error| {
                        rusqlite::Error::FromSqlConversionFailure(8, Type::Text, Box::new(error))
                    })?
                },
                created_at: parse_datetime(row.get::<_, String>(9)?),
                updated_at: parse_datetime(row.get::<_, String>(10)?),
            };
            Ok((
                account_id,
                enabled,
                verification_status,
                setup_step.is_ready(),
                !key_cipher.is_empty(),
                config,
            ))
        })?;
        let mut runtimes = Vec::new();
        for row in rows {
            let (account_id, enabled, verification_status, setup_ready, has_key, config) = row?;
            let capabilities = self.list_account_model_capabilities_declared(&account_id)?;
            runtimes.push(crate::custom::CustomAccountRuntime {
                account_id,
                enabled,
                verification_status,
                setup_ready,
                has_key,
                config,
                capabilities,
            });
        }
        Ok(runtimes)
    }

    pub fn replace_account_model_capabilities(
        &self,
        account_id: &str,
        capabilities: &[AccountModelCapabilityInput],
    ) -> Result<Vec<AccountModelCapability>> {
        self.commit_account_model_capabilities(account_id, capabilities)?;
        self.list_account_model_capabilities(account_id)
    }

    /// Persist and commit declared model capabilities without performing a
    /// fallible post-commit read. See [`Self::commit_account_custom_config`].
    pub(crate) fn commit_account_model_capabilities(
        &self,
        account_id: &str,
        capabilities: &[AccountModelCapabilityInput],
    ) -> Result<()> {
        anyhow::ensure!(self.get_account(account_id)?.is_some(), "account not found");
        let tx = self.conn.unchecked_transaction()?;
        persist_account_model_capabilities_on(&tx, account_id, capabilities)?;
        tx.commit()?;
        Ok(())
    }

    pub fn list_account_acknowledgements(
        &self,
        account_id: &str,
    ) -> Result<Vec<AccountAcknowledgement>> {
        let mut stmt = self.conn.prepare(
            "SELECT account_id, acknowledgement_id, version, content_hash, accepted_at
             FROM account_acknowledgements
             WHERE account_id = ?1
             ORDER BY acknowledgement_id ASC",
        )?;
        let rows = stmt.query_map([account_id], account_acknowledgement_from_row)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    pub fn record_account_acknowledgement(
        &self,
        account_id: &str,
        notice: PlanRiskNotice,
        accepted_at: DateTime<Utc>,
    ) -> Result<AccountAcknowledgement> {
        self.commit_account_acknowledgement(account_id, notice, accepted_at)?;
        self.list_account_acknowledgements(account_id)?
            .into_iter()
            .find(|row| row.acknowledgement_id == notice.acknowledgement_id)
            .ok_or_else(|| anyhow::anyhow!("acknowledgement was not persisted"))
    }

    /// Persist and commit a risk acknowledgement without performing a
    /// fallible post-commit read. See [`Self::commit_account_custom_config`].
    pub(crate) fn commit_account_acknowledgement(
        &self,
        account_id: &str,
        notice: PlanRiskNotice,
        accepted_at: DateTime<Utc>,
    ) -> Result<()> {
        anyhow::ensure!(self.get_account(account_id)?.is_some(), "account not found");
        let tx = self.conn.unchecked_transaction()?;
        persist_account_acknowledgement_on(&tx, account_id, notice, accepted_at)?;
        tx.commit()?;
        Ok(())
    }

    pub fn account_has_acknowledgement(
        &self,
        account_id: &str,
        notice: PlanRiskNotice,
    ) -> Result<bool> {
        let hash = notice.content_hash();
        let found = self.conn.query_row(
            "SELECT EXISTS(
                SELECT 1 FROM account_acknowledgements
                 WHERE account_id = ?1
                   AND acknowledgement_id = ?2
                   AND version = ?3
                   AND content_hash = ?4
             )",
            params![account_id, notice.acknowledgement_id, notice.version, hash],
            |row| row.get::<_, i64>(0),
        )?;
        Ok(found != 0)
    }

    pub fn forward_log_native_attribution(
        &self,
        id: i64,
    ) -> Result<Option<ForwardLogNativeAttribution>> {
        self.conn
            .query_row(
                "SELECT requested_model, resolved_alias, upstream_model,
                        native_cost_value, native_cost_unit, native_cost_currency
                 FROM forward_logs WHERE id = ?1",
                [id],
                forward_log_native_from_row,
            )
            .optional()
            .map_err(Into::into)
    }

    pub fn set_forward_log_native_attribution(
        &self,
        id: i64,
        attribution: &ForwardLogNativeAttribution,
    ) -> Result<bool> {
        let changed = self.conn.execute(
            "UPDATE forward_logs SET
                requested_model = ?2,
                resolved_alias = ?3,
                upstream_model = ?4,
                native_cost_value = ?5,
                native_cost_unit = ?6,
                native_cost_currency = ?7
             WHERE id = ?1",
            params![
                id,
                attribution.requested_model,
                attribution.resolved_alias,
                attribution.upstream_model,
                attribution.native_cost_value,
                attribution.native_cost_unit,
                attribution.native_cost_currency,
            ],
        )?;
        Ok(changed == 1)
    }

    pub fn query_forward_log_native_attributions(
        &self,
        ids: &[i64],
    ) -> Result<std::collections::HashMap<i64, ForwardLogNativeAttribution>> {
        let mut map = std::collections::HashMap::new();
        if ids.is_empty() {
            return Ok(map);
        }
        let mut stmt = self.conn.prepare(
            "SELECT id, requested_model, resolved_alias, upstream_model,
                    native_cost_value, native_cost_unit, native_cost_currency
             FROM forward_logs WHERE id = ?1",
        )?;
        for id in ids {
            if let Some(attribution) = stmt
                .query_row([id], |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        ForwardLogNativeAttribution {
                            requested_model: row.get(1)?,
                            resolved_alias: row.get(2)?,
                            upstream_model: row.get(3)?,
                            native_cost_value: row.get(4)?,
                            native_cost_unit: row.get(5)?,
                            native_cost_currency: row.get(6)?,
                        },
                    ))
                })
                .optional()?
            {
                map.insert(attribution.0, attribution.1);
            }
        }
        Ok(map)
    }

    pub fn upsert_quota_window(&self, window: &QuotaWindow) -> Result<()> {
        anyhow::ensure!(
            self.get_account(&window.account_id)?.is_some(),
            "account not found"
        );
        self.conn.execute(
            "INSERT INTO quota_windows (
                account_id, window_kind, used, limit_value, started_at, resets_at,
                calibration_offset, unit, source, observed_at, updated_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)
             ON CONFLICT(account_id, window_kind) DO UPDATE SET
                used = excluded.used,
                limit_value = excluded.limit_value,
                started_at = excluded.started_at,
                resets_at = excluded.resets_at,
                calibration_offset = excluded.calibration_offset,
                unit = excluded.unit,
                source = excluded.source,
                observed_at = excluded.observed_at,
                updated_at = excluded.updated_at",
            params![
                window.account_id,
                window.window_kind,
                window.used,
                window.limit_value,
                window.started_at.map(|value| value.to_rfc3339()),
                window.resets_at.map(|value| value.to_rfc3339()),
                window.calibration_offset,
                window.unit,
                window.source,
                window.observed_at.map(|value| value.to_rfc3339()),
                window.updated_at.to_rfc3339(),
            ],
        )?;
        Ok(())
    }

    pub fn list_quota_windows(&self, account_id: &str) -> Result<Vec<QuotaWindow>> {
        let mut stmt = self.conn.prepare(
            "SELECT account_id, window_kind, used, limit_value, started_at,
                    resets_at, calibration_offset, unit, source, observed_at, updated_at
             FROM quota_windows WHERE account_id = ?1 ORDER BY window_kind ASC",
        )?;
        let rows = stmt.query_map([account_id], |row| {
            Ok(QuotaWindow {
                account_id: row.get(0)?,
                window_kind: row.get(1)?,
                used: row.get(2)?,
                limit_value: row.get(3)?,
                started_at: row.get::<_, Option<String>>(4)?.map(parse_datetime),
                resets_at: row.get::<_, Option<String>>(5)?.map(parse_datetime),
                calibration_offset: row.get(6)?,
                unit: row.get(7)?,
                source: row.get(8)?,
                observed_at: row.get::<_, Option<String>>(9)?.map(parse_datetime),
                updated_at: parse_datetime(row.get::<_, String>(10)?),
            })
        })?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(Into::into)
    }

    pub fn upsert_credit_balance(&self, balance: &CreditBalance) -> Result<()> {
        anyhow::ensure!(
            self.get_account(&balance.account_id)?.is_some(),
            "account not found"
        );
        self.conn.execute(
            "INSERT INTO credit_balances (
                account_id, balance_kind, amount, unit, source, observed_at, updated_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
             ON CONFLICT(account_id, balance_kind) DO UPDATE SET
                amount = excluded.amount,
                unit = excluded.unit,
                source = excluded.source,
                observed_at = excluded.observed_at,
                updated_at = excluded.updated_at",
            params![
                balance.account_id,
                balance.balance_kind,
                balance.amount,
                balance.unit,
                balance.source,
                balance.observed_at.map(|value| value.to_rfc3339()),
                balance.updated_at.to_rfc3339(),
            ],
        )?;
        Ok(())
    }

    pub fn list_credit_balances(&self, account_id: &str) -> Result<Vec<CreditBalance>> {
        let mut stmt = self.conn.prepare(
            "SELECT account_id, balance_kind, amount, unit, source, observed_at, updated_at
             FROM credit_balances WHERE account_id = ?1 ORDER BY balance_kind ASC",
        )?;
        let rows = stmt.query_map([account_id], |row| {
            Ok(CreditBalance {
                account_id: row.get(0)?,
                balance_kind: row.get(1)?,
                amount: row.get(2)?,
                unit: row.get(3)?,
                source: row.get(4)?,
                observed_at: row.get::<_, Option<String>>(5)?.map(parse_datetime),
                updated_at: parse_datetime(row.get::<_, String>(6)?),
            })
        })?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(Into::into)
    }

    pub fn account_usage_sync_state(
        &self,
        account_id: &str,
    ) -> Result<Option<AccountUsageSyncState>> {
        let row = self
            .conn
            .query_row(
                "SELECT last_success_at, last_attempt_at, next_eligible_at,
                        failure_streak, last_expedited_at
                 FROM provider_usage_sync_state WHERE account_id = ?1",
                [account_id],
                |row| {
                    Ok(AccountUsageSyncState {
                        account_id: account_id.to_string(),
                        last_success_at: row.get::<_, Option<String>>(0)?.map(parse_datetime),
                        last_attempt_at: row.get::<_, Option<String>>(1)?.map(parse_datetime),
                        next_eligible_at: row.get::<_, Option<String>>(2)?.map(parse_datetime),
                        failure_streak: row.get::<_, i64>(3)?,
                        last_expedited_at: row.get::<_, Option<String>>(4)?.map(parse_datetime),
                    })
                },
            )
            .optional()?;
        Ok(row)
    }

    /// Pull `next_eligible_at` earlier when `proposal` is sooner.
    ///
    /// When `respect_failure_backoff` is true and the account is in a failure
    /// streak, the existing next-eligible floor is left untouched so threshold,
    /// cadence, and reset logic cannot defeat the backoff ladder. Callers that
    /// intentionally override (real inference 429) pass false.
    pub fn pull_account_usage_sync_next_eligible(
        &self,
        account_id: &str,
        proposal: DateTime<Utc>,
        respect_failure_backoff: bool,
    ) -> Result<()> {
        let current = self.account_usage_sync_state(account_id)?;
        let Some(current) = current else {
            // Account gone — nothing to schedule.
            return Ok(());
        };
        if respect_failure_backoff && current.failure_streak > 0 {
            return Ok(());
        }
        let next = match current.next_eligible_at {
            Some(existing) => existing.min(proposal),
            None => proposal,
        };
        self.conn.execute(
            "UPDATE provider_usage_sync_state
             SET next_eligible_at = ?1
             WHERE account_id = ?2",
            params![next.to_rfc3339(), account_id],
        )?;
        Ok(())
    }

    pub fn record_account_usage_sync_success(
        &self,
        account_id: &str,
        now: DateTime<Utc>,
        next_eligible_at: DateTime<Utc>,
        mark_expedited: bool,
    ) -> Result<()> {
        record_account_usage_sync_success_on(
            &self.conn,
            account_id,
            AccountUsageSyncSuccessMetadata {
                now,
                next_eligible_at,
                mark_expedited,
            },
        )?;
        Ok(())
    }

    pub fn record_account_usage_sync_failure(
        &self,
        account_id: &str,
        now: DateTime<Utc>,
        failure_streak: i64,
        next_eligible_at: DateTime<Utc>,
    ) -> Result<()> {
        // Never clear last_success_at on failure.
        self.conn.execute(
            "UPDATE provider_usage_sync_state
             SET last_attempt_at = ?1,
                 next_eligible_at = ?2,
                 failure_streak = ?3
             WHERE account_id = ?4",
            params![
                now.to_rfc3339(),
                next_eligible_at.to_rfc3339(),
                failure_streak,
                account_id
            ],
        )?;
        Ok(())
    }

    /// Touch only the manual-throttle timestamp without changing success,
    /// streak, or next-eligible fields (e.g. post-network CAS conflicts).
    pub fn touch_account_usage_sync_attempt(
        &self,
        account_id: &str,
        now: DateTime<Utc>,
    ) -> Result<()> {
        self.conn.execute(
            "UPDATE provider_usage_sync_state
             SET last_attempt_at = ?1
             WHERE account_id = ?2",
            params![now.to_rfc3339(), account_id],
        )?;
        Ok(())
    }

    /// True when the account has at least one successful, possibly
    /// Go-quota-consuming forward log at or after `since` (active cadence).
    /// Uses `julianday` so lexicographic RFC3339 edge cases cannot mis-order,
    /// and `EXISTS` so the scan can stop early. Zen free successes are excluded.
    pub fn account_has_local_activity_since(
        &self,
        account_id: &str,
        since: DateTime<Utc>,
    ) -> Result<bool> {
        let exists: i64 = self.conn.query_row(
            "SELECT EXISTS(
                SELECT 1 FROM forward_logs
                WHERE account_id = ?1
                  AND status IN ('success', 'success_no_usage', 'success_unpriced')
                  AND cost_state IN ('priced', 'legacy_estimate', 'unpriced', 'usage_missing')
                  AND julianday(timestamp) >= julianday(?2)
                LIMIT 1
             )",
            params![account_id, since.to_rfc3339()],
            |row| row.get(0),
        )?;
        Ok(exists != 0)
    }

    pub fn get_account(&self, id: &str) -> Result<Option<Account>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, name, username, password_cipher, key_cipher, enabled, referral_code, recharge_date, cooldown_until, cooldown_generic_until, cooldown_5h_until, cooldown_week_until, cooldown_month_until, cooldown_free_until, last_error, created_at, updated_at, auth_error, account_type, setup_step, notes, provider_id, offering_id, credential_kind, quota_scope FROM accounts WHERE id = ?1"
        )?;
        let account = stmt.query_row([id], account_from_row).optional()?;
        Ok(account)
    }

    pub fn list_accounts(&self) -> Result<Vec<Account>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, name, username, password_cipher, key_cipher, enabled, referral_code, recharge_date, cooldown_until, cooldown_generic_until, cooldown_5h_until, cooldown_week_until, cooldown_month_until, cooldown_free_until, last_error, created_at, updated_at, auth_error, account_type, setup_step, notes, provider_id, offering_id, credential_kind, quota_scope FROM accounts ORDER BY sort_order ASC, created_at ASC, id ASC"
        )?;
        let rows = stmt.query_map([], account_from_row)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(|e| e.into())
    }

    /// Advance a managed-account onboarding step with an optimistic current-step
    /// guard. The caller is responsible for validating that `to` is the only
    /// legal successor of `from`.
    pub fn advance_managed_setup(
        &self,
        id: &str,
        from: AccountSetupStep,
        to: AccountSetupStep,
    ) -> Result<bool> {
        let confirmed_purchase_date = (from == AccountSetupStep::Payment
            && to == AccountSetupStep::KeyVerification)
            .then(local_today);
        let changed = self.conn.execute(
            "UPDATE accounts
             SET setup_step = ?1, enabled = 0,
                 recharge_date = CASE WHEN ?5 IS NULL THEN recharge_date ELSE ?5 END,
                 usage_month_window_cost_offset = CASE WHEN ?5 IS NULL
                     THEN usage_month_window_cost_offset ELSE 0 END,
                 updated_at = ?2
             WHERE id = ?3 AND account_type = 'managed' AND setup_step = ?4",
            params![
                to.as_str(),
                Utc::now().to_rfc3339(),
                id,
                from.as_str(),
                confirmed_purchase_date,
            ],
        )?;
        Ok(changed == 1)
    }

    /// Persist a candidate key while keeping the account isolated from routing.
    pub fn save_managed_key_for_verification(&self, id: &str, key_cipher: &str) -> Result<bool> {
        let changed = self.conn.execute(
            "UPDATE accounts
             SET key_cipher = ?1, enabled = 0, auth_error = NULL, last_error = NULL,
                 updated_at = ?2
             WHERE id = ?3 AND account_type = 'managed' AND setup_step = 'key_verification'",
            params![key_cipher, Utc::now().to_rfc3339(), id],
        )?;
        Ok(changed == 1)
    }

    /// Make a verified managed account routable only if the tested encrypted key
    /// is still the one stored in the row.
    pub fn complete_managed_setup_if_key_matches(
        &self,
        id: &str,
        expected_key_cipher: &str,
    ) -> Result<bool> {
        let Some(account) = self.get_account(id)? else {
            return Ok(false);
        };
        ensure_enabled_offering_is_routable(&account.provider_id, &account.offering_id, true)?;
        let changed = self.conn.execute(
            "UPDATE accounts
             SET setup_step = 'ready', enabled = 1, auth_error = NULL, updated_at = ?1
             WHERE id = ?2 AND account_type = 'managed'
               AND setup_step = 'key_verification' AND key_cipher = ?3",
            params![Utc::now().to_rfc3339(), id, expected_key_cipher],
        )?;
        Ok(changed == 1)
    }

    /// Reset only an unfinished managed onboarding. Ready accounts keep their key
    /// when their browser profile is reset.
    pub fn reset_pending_managed_setup(&self, id: &str) -> Result<bool> {
        let changed = self.conn.execute(
            "UPDATE accounts
             SET setup_step = 'google_account', key_cipher = '', enabled = 0,
                 auth_error = NULL, last_error = NULL, cooldown_until = NULL,
                 cooldown_generic_until = NULL, cooldown_5h_until = NULL,
                 cooldown_week_until = NULL, cooldown_month_until = NULL, cooldown_free_until = NULL,
                 updated_at = ?1
             WHERE id = ?2 AND account_type = 'managed' AND setup_step <> 'ready'",
            params![Utc::now().to_rfc3339(), id],
        )?;
        Ok(changed == 1)
    }

    pub fn reorder_accounts(
        &self,
        account_ids: &[String],
    ) -> std::result::Result<(), ReorderAccountsError> {
        let tx = self.conn.unchecked_transaction()?;
        let mut requested_ids = HashSet::with_capacity(account_ids.len());
        if account_ids
            .iter()
            .any(|id| !requested_ids.insert(id.as_str()))
        {
            return Err(ReorderAccountsError::DuplicateAccountId);
        }

        let current_ids = {
            let mut stmt = tx.prepare("SELECT id FROM accounts")?;
            let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
            rows.collect::<rusqlite::Result<Vec<_>>>()?
        };
        if current_ids.len() != account_ids.len()
            || current_ids
                .iter()
                .any(|id| !requested_ids.contains(id.as_str()))
        {
            return Err(ReorderAccountsError::AccountSetMismatch);
        }

        for (sort_order, id) in account_ids.iter().enumerate() {
            tx.execute(
                "UPDATE accounts SET sort_order = ?1 WHERE id = ?2",
                params![sort_order as i64, id],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    // Settings
    pub fn set_setting(&self, key: &str, value: &str) -> Result<()> {
        self.conn.execute(
            "INSERT OR REPLACE INTO settings (key, value) VALUES (?1, ?2)",
            [key, value],
        )?;
        Ok(())
    }

    pub fn get_setting(&self, key: &str) -> Result<Option<String>> {
        self.conn
            .query_row("SELECT value FROM settings WHERE key = ?1", [key], |row| {
                row.get(0)
            })
            .optional()
            .map_err(|e| e.into())
    }

    /// Return the active egress-IP-wide Zen free cooldown, if any.
    pub fn free_channel_cooldown_until(&self) -> Result<Option<DateTime<Utc>>> {
        self.free_channel_cooldown_until_at(Utc::now())
    }

    fn free_channel_cooldown_until_at(&self, now: DateTime<Utc>) -> Result<Option<DateTime<Utc>>> {
        let Some(value) = self.get_setting(FREE_CHANNEL_COOLDOWN_SETTING)? else {
            return Ok(None);
        };
        let until = DateTime::parse_from_rfc3339(&value)
            .map(|value| value.with_timezone(&Utc))
            .map_err(|error| {
                anyhow::anyhow!(
                    "invalid {FREE_CHANNEL_COOLDOWN_SETTING} setting {value:?}: {error}"
                )
            })?;
        Ok((until > now).then_some(until))
    }

    // Logging
    pub fn log_gateway(&self, level: &str, category: &str, message: &str) -> Result<()> {
        self.log_gateway_diagnostic(level, category, message, None, None, None, None, None, None)
    }

    #[allow(clippy::too_many_arguments)]
    pub fn log_gateway_diagnostic(
        &self,
        level: &str,
        category: &str,
        message: &str,
        request_id: Option<&str>,
        attempt: Option<i64>,
        error_source: Option<&str>,
        error_stage: Option<&str>,
        duration_ms: Option<i64>,
        diagnostic_json: Option<&str>,
    ) -> Result<()> {
        self.conn.execute(
            "INSERT INTO gateway_logs
             (level, category, message, created_at, request_id, attempt,
              error_source, error_stage, duration_ms, diagnostic_json)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            params![
                level,
                category,
                message,
                Utc::now().to_rfc3339(),
                request_id,
                attempt,
                error_source,
                error_stage,
                duration_ms,
                diagnostic_json,
            ],
        )?;
        Ok(())
    }

    /// Insert many forward_logs rows in one transaction (bulk seeding).
    /// Test-only helper: production writes go through `log_forward`.
    pub fn log_forward_batch(&self, logs: &[ForwardLog]) -> Result<()> {
        let tx = self.conn.unchecked_transaction()?;
        for log in logs {
            tx.execute(
                "INSERT INTO forward_logs
                 (timestamp, model, account_id, account_name, client_key_id, client_key_name,
                  route_account_id, provider_id, offering_id, credential_account_id,
                  status, http_status, prompt_tokens, completion_tokens, cached_tokens,
                  cache_creation_tokens, cost, cost_state, raw_cost_usd, quota_debit,
                  effective_paid_cost_usd)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12,
                         0, 0, 0, 0, 0, 'legacy_estimate', ?13, ?14, ?15)",
                params![
                    log.timestamp.to_rfc3339(),
                    log.model,
                    log.account_id,
                    log.account_name,
                    log.client_key_id,
                    log.client_key_name,
                    log.route_account_id,
                    log.provider_id,
                    log.offering_id,
                    log.credential_account_id,
                    log.status,
                    log.http_status,
                    log.raw_cost_usd,
                    log.quota_debit,
                    log.effective_paid_cost_usd,
                ],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    /// Insert a forward_logs row. Returns the auto-assigned row id.
    pub fn log_forward(&self, log: &ForwardLog) -> Result<i64> {
        let diagnostic_json = log
            .diagnostic
            .as_ref()
            .map(serde_json::to_string)
            .transpose()?;
        let attribution = ForwardLogNativeAttribution::inferred_from_forward_log(log);
        self.conn.execute(
            "INSERT INTO forward_logs
             (timestamp, model, account_id, account_name, client_key_id, client_key_name,
              route_account_id, provider_id, offering_id, credential_account_id,
              status, http_status, route,
              prompt_tokens, completion_tokens, cached_tokens, cache_creation_tokens, cost,
              raw_cost_usd, quota_debit, effective_paid_cost_usd,
              pricing_revision_id, quota_multiplier, local_adjustment_multiplier,
              service_tier, cost_state, error_message, request_id, attempt,
              error_source, error_stage, duration_ms, diagnostic_json,
              requested_model, resolved_alias, upstream_model,
              native_cost_value, native_cost_unit, native_cost_currency)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16,
                     ?17, ?18, ?19, ?20, ?21, ?22, ?23, ?24, ?25, ?26, ?27, ?28, ?29, ?30,
                     ?31, ?32, ?33, ?34, ?35, ?36, ?37, ?38, ?39)",
            params![
                log.timestamp.to_rfc3339(),
                log.model,
                log.account_id,
                log.account_name,
                log.client_key_id,
                log.client_key_name,
                log.route_account_id,
                log.provider_id,
                log.offering_id,
                log.credential_account_id,
                log.status,
                log.http_status,
                log.route,
                log.prompt_tokens,
                log.completion_tokens,
                log.cached_tokens,
                log.cache_creation_tokens,
                log.cost.unwrap_or(0.0),
                log.raw_cost_usd,
                log.quota_debit,
                log.effective_paid_cost_usd,
                log.pricing_revision_id,
                log.quota_multiplier,
                log.local_adjustment_multiplier,
                log.service_tier,
                log.cost_state,
                log.error_message,
                log.request_id,
                log.attempt,
                log.error_source,
                log.error_stage,
                log.duration_ms,
                diagnostic_json,
                attribution.requested_model,
                attribution.resolved_alias,
                attribution.upstream_model,
                attribution.native_cost_value,
                attribution.native_cost_unit,
                attribution.native_cost_currency,
            ],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    /// Finalize a forward_logs row once the upstream response ends. `http_status` and
    /// `error_message` may be `None` to leave them at their initial value. `id` is the
    /// primary key returned from the original `log_forward` insert.
    pub fn update_forward_log(
        &self,
        id: i64,
        status: &str,
        http_status: Option<i32>,
        mut metrics: ForwardMetrics,
        error_message: Option<&str>,
        diagnostic: Option<&ForwardLogDiagnosticUpdate<'_>>,
    ) -> Result<()> {
        let binding = self
            .conn
            .query_row(
                "SELECT provider_id, offering_id FROM forward_logs WHERE id = ?1",
                [id],
                |row| {
                    Ok((
                        row.get::<_, Option<String>>(0)?,
                        row.get::<_, Option<String>>(1)?,
                    ))
                },
            )
            .optional()?;
        if let Some((provider_id, offering_id)) = binding.as_ref() {
            metrics.scope_to_provider(
                provider_id.as_deref(),
                offering_id.as_deref(),
                status.starts_with("success"),
            );
        }
        let cost_state = match (metrics.cost_state, status) {
            ("not_applicable", "outcome_unknown") => "outcome_unknown",
            ("not_applicable", "success_no_usage") => "usage_missing",
            ("not_applicable", "success_unpriced") => "unpriced",
            (state, _) => state,
        };
        let stored_status = if status.starts_with("success") {
            match cost_state {
                "priced" | "free" => "success",
                "usage_missing" => "success_no_usage",
                _ => "success_unpriced",
            }
        } else {
            status
        };
        let stored_cost = if cost_state == "priced" {
            metrics.cost
        } else {
            0.0
        };
        // Stream inserts dual-write native USD from the preliminary row. Finalize
        // native_cost_* from the same cost/raw_cost_usd/cost_state tuple written
        // here so Go/Zen cannot keep a 0/NULL native snapshot after success.
        let (native_cost_value, native_cost_unit, native_cost_currency) =
            ForwardLogNativeAttribution::usd_fields_from_cost(
                metrics.raw_cost_usd,
                (cost_state == "priced").then_some(metrics.cost),
                cost_state,
            );
        self.conn.execute(
            "UPDATE forward_logs
             SET status = ?2,
                 http_status = COALESCE(?3, http_status),
                 prompt_tokens = ?4,
                 completion_tokens = ?5,
                 cached_tokens = ?6,
                 cache_creation_tokens = ?7,
                 cost = ?8,
                 raw_cost_usd = ?9,
                 quota_debit = ?10,
                 effective_paid_cost_usd = ?11,
                 pricing_revision_id = ?12,
                 quota_multiplier = ?13,
                 local_adjustment_multiplier = ?14,
                 service_tier = ?15,
                 cost_state = ?16,
                 error_message = COALESCE(?17, error_message),
                 error_source = COALESCE(?18, error_source),
                 error_stage = COALESCE(?19, error_stage),
                 duration_ms = COALESCE(?20, duration_ms),
                 diagnostic_json = COALESCE(?21, diagnostic_json),
                 native_cost_value = ?22,
                 native_cost_unit = ?23,
                 native_cost_currency = ?24
             WHERE id = ?1",
            params![
                id,
                stored_status,
                http_status,
                metrics.prompt_tokens,
                metrics.completion_tokens,
                metrics.cached_tokens,
                metrics.cache_creation_tokens,
                stored_cost,
                metrics.raw_cost_usd,
                metrics.quota_debit,
                metrics.effective_paid_cost_usd,
                metrics.pricing_revision_id,
                metrics.quota_multiplier,
                metrics.local_adjustment_multiplier,
                metrics.service_tier,
                cost_state,
                error_message,
                diagnostic.map(|diagnostic| diagnostic.error_source),
                diagnostic.map(|diagnostic| diagnostic.error_stage),
                diagnostic.map(|diagnostic| diagnostic.duration_ms),
                diagnostic.map(|diagnostic| diagnostic.diagnostic_json),
                native_cost_value,
                native_cost_unit,
                native_cost_currency,
            ],
        )?;
        Ok(())
    }

    pub fn list_gateway_logs(&self, limit: i64) -> Result<Vec<GatewayLog>> {
        self.query_gateway_logs(limit, None)
    }

    pub fn query_gateway_logs(
        &self,
        limit: i64,
        request_id: Option<&str>,
    ) -> Result<Vec<GatewayLog>> {
        let sql = if request_id.is_some() {
            "SELECT id, level, category, message, created_at, request_id, attempt,
                    error_source, error_stage, duration_ms, diagnostic_json
             FROM gateway_logs WHERE request_id = ?1 ORDER BY id DESC LIMIT ?2"
        } else {
            "SELECT id, level, category, message, created_at, request_id, attempt,
                    error_source, error_stage, duration_ms, diagnostic_json
             FROM gateway_logs ORDER BY id DESC LIMIT ?1"
        };
        let mut stmt = self.conn.prepare(sql)?;
        let map = |row: &rusqlite::Row<'_>| {
            Ok(GatewayLog {
                id: row.get(0)?,
                level: row.get(1)?,
                category: row.get(2)?,
                message: row.get(3)?,
                created_at: parse_datetime(row.get::<_, String>(4)?),
                request_id: row.get(5)?,
                attempt: row.get(6)?,
                error_source: row.get(7)?,
                error_stage: row.get(8)?,
                duration_ms: row.get(9)?,
                diagnostic: row
                    .get::<_, Option<String>>(10)?
                    .and_then(|json| serde_json::from_str(&json).ok()),
            })
        };
        let rows = if let Some(request_id) = request_id {
            stmt.query_map(params![request_id, limit], map)?
        } else {
            stmt.query_map(params![limit], map)?
        };
        rows.collect::<Result<Vec<_>, _>>().map_err(|e| e.into())
    }

    pub fn latest_gateway_error(&self) -> Result<Option<String>> {
        self.conn
            .query_row(
                "SELECT message FROM gateway_logs
                 WHERE lower(level) = 'error' AND category = 'gateway'
                 ORDER BY id DESC LIMIT 1",
                [],
                |row| row.get(0),
            )
            .optional()
            .map_err(|e| e.into())
    }

    pub fn latest_error_summary(&self) -> Result<Option<String>> {
        self.conn
            .query_row(
                "SELECT message FROM gateway_logs
                 WHERE lower(level) IN ('error', 'warn')
                 ORDER BY id DESC LIMIT 1",
                [],
                |row| row.get(0),
            )
            .optional()
            .map_err(|e| e.into())
    }

    pub fn list_forward_logs(&self, limit: i64) -> Result<Vec<ForwardLog>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, timestamp, model, account_id, account_name, status, http_status, route,
                    prompt_tokens, completion_tokens, cached_tokens, cache_creation_tokens, cost,
                    pricing_revision_id, quota_multiplier, local_adjustment_multiplier,
                    service_tier, cost_state, error_message, request_id, attempt,
                    error_source, error_stage, duration_ms, diagnostic_json,
                    client_key_id, client_key_name, route_account_id, provider_id,
                    offering_id, credential_account_id, raw_cost_usd, quota_debit,
                    effective_paid_cost_usd
             FROM forward_logs ORDER BY id DESC LIMIT ?1",
        )?;
        let rows = stmt.query_map([limit], forward_log_from_row)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(|e| e.into())
    }

    pub fn query_forward_logs(
        &self,
        options: ForwardLogQueryOptions<'_>,
    ) -> Result<ForwardLogPage> {
        let limit = options.limit.clamp(1, 200);
        let offset = options.offset.max(0);
        let (filter, filter_params) = forward_log_filter(&options);
        let order_clause = forward_log_order(options.sort_by, options.sort_order);
        let summary_sql = format!(
            "SELECT COUNT(*),
                    COALESCE(SUM(prompt_tokens), 0),
                    COALESCE(SUM(completion_tokens), 0),
                    COALESCE(SUM(cached_tokens), 0),
                    COALESCE(SUM(cost), 0.0)
             FROM forward_logs{filter}"
        );
        let summary = self.conn.query_row(
            &summary_sql,
            params_from_iter(filter_params.iter()),
            |row| {
                Ok(ForwardLogSummary {
                    total_requests: row.get(0)?,
                    prompt_tokens: row.get(1)?,
                    completion_tokens: row.get(2)?,
                    cached_tokens: row.get(3)?,
                    cost: row.get(4)?,
                })
            },
        )?;

        let items_sql = format!(
            "SELECT id, timestamp, model, account_id, account_name, status, http_status, route,
                    prompt_tokens, completion_tokens, cached_tokens, cache_creation_tokens, cost,
                    pricing_revision_id, quota_multiplier, local_adjustment_multiplier,
                    service_tier, cost_state, error_message, request_id, attempt,
                    error_source, error_stage, duration_ms, diagnostic_json,
                    client_key_id, client_key_name, route_account_id, provider_id,
                    offering_id, credential_account_id, raw_cost_usd, quota_debit,
                    effective_paid_cost_usd
             FROM forward_logs{filter}
             {order_clause}
             LIMIT ? OFFSET ?"
        );
        let mut item_params = filter_params;
        item_params.push(Value::Integer(limit));
        item_params.push(Value::Integer(offset));
        let mut stmt = self.conn.prepare(&items_sql)?;
        let items = stmt
            .query_map(params_from_iter(item_params.iter()), forward_log_from_row)?
            .collect::<Result<Vec<_>, _>>()?;

        Ok(ForwardLogPage { items, summary })
    }

    pub fn list_forward_log_models(&self) -> Result<Vec<String>> {
        let mut stmt = self
            .conn
            .prepare("SELECT DISTINCT model FROM forward_logs ORDER BY model ASC")?;
        let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
        rows.collect::<Result<Vec<_>, _>>().map_err(|e| e.into())
    }

    /// Distinct client keys that appear in forward logs, mirroring
    /// [`Database::list_forward_log_models`]. Includes disabled, soft-deleted,
    /// and dangling ids so historical logs stay filterable. Each id resolves
    /// its most recent non-null name snapshot, so renamed keys appear under
    /// their current name.
    pub fn list_forward_log_keys(&self) -> Result<Vec<ForwardLogClientKey>> {
        let mut stmt = self.conn.prepare(
            "SELECT client_key_id, COALESCE((
                    SELECT f2.client_key_name FROM forward_logs f2
                    WHERE f2.client_key_id = f.client_key_id
                      AND f2.client_key_name IS NOT NULL
                    ORDER BY f2.rowid DESC LIMIT 1
                ), '')
             FROM forward_logs f
             WHERE client_key_id IS NOT NULL
             GROUP BY client_key_id
             ORDER BY 2 ASC, client_key_id ASC",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok(ForwardLogClientKey {
                id: row.get(0)?,
                name: row.get::<_, String>(1)?,
            })
        })?;
        let mut keys = rows.collect::<Result<Vec<_>, _>>()?;
        // Empty snapshot names (logs written before the key existed in
        // config) still deserve a stable display label; within that
        // fallback the primary key's fixed display name takes precedence
        // over its raw id.
        for key in &mut keys {
            if key.name.is_empty() {
                key.name = if key.id == PRIMARY_KEY_ID {
                    PRIMARY_KEY_NAME.to_string()
                } else {
                    key.id.clone()
                };
            }
        }
        Ok(keys)
    }

    // ----- sub gateway keys (schema v20) -----

    /// All sub keys including soft-delete tombstones, in creation order.
    pub fn list_sub_gateway_keys(&self) -> Result<Vec<SubGatewayKey>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, name, key, enabled, deleted_at, created_at
             FROM sub_gateway_keys
             ORDER BY created_at ASC, rowid ASC",
        )?;
        let rows = stmt.query_map([], sub_gateway_key_from_row)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    /// Non-deleted sub keys (enabled and disabled alike); disabled rows keep
    /// their plaintext so re-enabling can revalidate it.
    pub fn list_active_sub_gateway_keys(&self) -> Result<Vec<SubGatewayKey>> {
        let mut keys = self.list_sub_gateway_keys()?;
        keys.retain(|key| key.is_active());
        Ok(keys)
    }

    /// Count of non-deleted sub keys; tombstones never count against the
    /// active ceiling.
    pub fn count_active_sub_gateway_keys(&self) -> Result<usize> {
        let count: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM sub_gateway_keys WHERE deleted_at IS NULL",
            [],
            |row| row.get(0),
        )?;
        Ok(count.max(0) as usize)
    }

    pub fn get_sub_gateway_key(&self, id: &str) -> Result<Option<SubGatewayKey>> {
        let key = self
            .conn
            .query_row(
                "SELECT id, name, key, enabled, deleted_at, created_at
                 FROM sub_gateway_keys WHERE id = ?1",
                params![id],
                sub_gateway_key_from_row,
            )
            .optional()?;
        Ok(key)
    }

    /// Inserts a new sub key. The partial unique index backstops value
    /// uniqueness among non-deleted sub keys; a collision surfaces as a
    /// constraint error the caller maps to a clear rejection.
    pub fn insert_sub_gateway_key(&self, key: &SubGatewayKey) -> Result<()> {
        self.conn.execute(
            "INSERT INTO sub_gateway_keys (id, name, key, enabled, deleted_at, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                key.id,
                key.name,
                key.key,
                key.enabled as i32,
                key.deleted_at.map(|t| t.to_rfc3339()),
                key.created_at.to_rfc3339(),
            ],
        )?;
        Ok(())
    }

    /// Renames a non-deleted sub key. Returns `false` when the id matches no
    /// active row.
    pub fn rename_sub_gateway_key(&self, id: &str, name: &str) -> Result<bool> {
        let updated = self.conn.execute(
            "UPDATE sub_gateway_keys SET name = ?2 WHERE id = ?1 AND deleted_at IS NULL",
            params![id, name],
        )?;
        Ok(updated == 1)
    }

    /// Flips the enabled flag of a non-deleted sub key. Returns `false` when
    /// the id matches no active row.
    pub fn set_sub_gateway_key_enabled(&self, id: &str, enabled: bool) -> Result<bool> {
        let updated = self.conn.execute(
            "UPDATE sub_gateway_keys SET enabled = ?2 WHERE id = ?1 AND deleted_at IS NULL",
            params![id, enabled as i32],
        )?;
        Ok(updated == 1)
    }

    /// Assigns a fresh value to a non-deleted sub key. Returns `false` when
    /// the id matches no active row.
    pub fn update_sub_gateway_key_value(&self, id: &str, new_value: &str) -> Result<bool> {
        let updated = self.conn.execute(
            "UPDATE sub_gateway_keys SET key = ?2 WHERE id = ?1 AND deleted_at IS NULL",
            params![id, new_value],
        )?;
        Ok(updated == 1)
    }

    /// Soft-deletes a sub key: clears the plaintext, disables it, and keeps
    /// id/name/deleted_at for log attribution. Returns `false` when the id
    /// matches no active row.
    pub fn soft_delete_sub_gateway_key(&self, id: &str, now: DateTime<Utc>) -> Result<bool> {
        let updated = self.conn.execute(
            "UPDATE sub_gateway_keys
             SET key = '', enabled = 0, deleted_at = ?2
             WHERE id = ?1 AND deleted_at IS NULL",
            params![id, now.to_rfc3339()],
        )?;
        Ok(updated == 1)
    }

    /// Plaintext values of all non-deleted sub keys (enabled and disabled);
    /// used to keep generated values unique across tiers.
    pub fn active_sub_gateway_key_values(&self) -> Result<Vec<String>> {
        let mut stmt = self.conn.prepare(
            "SELECT key FROM sub_gateway_keys
             WHERE deleted_at IS NULL AND key <> ''",
        )?;
        let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    /// Whether any non-deleted sub key (enabled or disabled) already holds
    /// this value; the cross-tier uniqueness gate for candidate primary key
    /// values.
    pub fn sub_gateway_key_value_exists(&self, value: &str) -> Result<bool> {
        let found: i64 = self.conn.query_row(
            "SELECT EXISTS(
                SELECT 1 FROM sub_gateway_keys
                WHERE deleted_at IS NULL AND key = ?1 LIMIT 1
            )",
            params![value],
            |row| row.get(0),
        )?;
        Ok(found == 1)
    }

    // ----- forward log client-key backfill -----

    /// Backfills `client_key_id`/`client_key_name` on historical rows in
    /// bounded rowid chunks. Each call performs at most one short transaction
    /// (range update + watermark persist) so callers can release the
    /// connection between chunks and keep the gateway responsive.
    /// Returns `true` while more chunks remain.
    pub fn backfill_forward_logs_client_key_step(
        &self,
        key_id: &str,
        key_name: &str,
        chunk_rows: i64,
    ) -> Result<bool> {
        let chunk_rows = chunk_rows.max(1);
        let Some(watermark) = self.backfill_watermark()? else {
            // Completion marker present. A downgrade window (an older binary
            // writing NULL rows after this marker was recorded) must not
            // leave those rows permanently unattributed: probe the index
            // once and restart the scan when any NULL row appears.
            if !self.forward_logs_have_unattributed_rows()? {
                return Ok(false);
            }
            self.set_setting(BACKFILL_SETTING_KEY, "0")?;
            return Ok(true);
        };
        let max_rowid: i64 = self.conn.query_row(
            "SELECT COALESCE(MAX(rowid), 0) FROM forward_logs",
            [],
            |row| row.get(0),
        )?;
        let start = watermark + 1;
        if start <= max_rowid {
            let end = (start + chunk_rows - 1).min(max_rowid);
            let tx = self.conn.unchecked_transaction()?;
            tx.execute(
                "UPDATE forward_logs
                 SET client_key_id = ?1, client_key_name = ?2
                 WHERE client_key_id IS NULL AND rowid BETWEEN ?3 AND ?4",
                params![key_id, key_name, start, end],
            )?;
            tx.execute(
                "INSERT OR REPLACE INTO settings (key, value) VALUES (?1, ?2)",
                params![BACKFILL_SETTING_KEY, end.to_string()],
            )?;
            tx.commit()?;
            if end < max_rowid {
                return Ok(true);
            }
        }
        // The whole table is covered. New writes always carry a key id, so
        // the NULL set can only shrink; record completion once nothing is
        // left, otherwise late NULL rows (an older binary still writing)
        // force a restart from the beginning.
        if !self.forward_logs_have_unattributed_rows()? {
            self.set_setting(BACKFILL_SETTING_KEY, BACKFILL_DONE)?;
            return Ok(false);
        }
        self.set_setting(BACKFILL_SETTING_KEY, "0")?;
        Ok(true)
    }

    /// Whether any forward log row still lacks a client key id; served by
    /// `idx_forward_logs_client_key` in one index probe.
    fn forward_logs_have_unattributed_rows(&self) -> Result<bool> {
        let found: i64 = self.conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM forward_logs WHERE client_key_id IS NULL LIMIT 1)",
            [],
            |row| row.get(0),
        )?;
        Ok(found == 1)
    }

    /// `None` when the backfill already completed; otherwise the max rowid
    /// whose range has been attributed.
    fn backfill_watermark(&self) -> Result<Option<i64>> {
        Ok(match self.get_setting(BACKFILL_SETTING_KEY)? {
            None => Some(0),
            Some(value) if value == BACKFILL_DONE => None,
            Some(value) => Some(value.parse::<i64>().unwrap_or(0)),
        })
    }

    /// Test/inspection helper: the raw persisted backfill marker.
    pub fn forward_log_backfill_marker(&self) -> Result<Option<String>> {
        self.get_setting(BACKFILL_SETTING_KEY)
    }

    // Cooldown
    /// Set or clear a per-account rate-limit cooldown.
    /// Pass `None` for both `until` and `err` to clear.
    pub fn set_account_cooldown(
        &self,
        id: &str,
        until: Option<DateTime<Utc>>,
        err: Option<&str>,
    ) -> Result<()> {
        let now = Utc::now().to_rfc3339();
        let tx = self.conn.unchecked_transaction()?;
        if until.is_none() && err.is_none() {
            tx.execute(
                "UPDATE accounts
                 SET cooldown_until = NULL,
                     cooldown_generic_until = NULL,
                     cooldown_5h_until = NULL,
                     cooldown_week_until = NULL,
                     cooldown_month_until = NULL,
                     cooldown_free_until = NULL,
                     last_error = NULL,
                     updated_at = ?2
                 WHERE id = ?1",
                params![id, now],
            )?;
        } else {
            tx.execute(
                "UPDATE accounts
                 SET cooldown_generic_until = ?2, last_error = ?3, updated_at = ?4
                 WHERE id = ?1",
                params![id, until.map(|t| t.to_rfc3339()), err, now],
            )?;
            let new_cooldown = Self::compute_cooldown_until(&tx, id, &now)?;
            tx.execute(
                "UPDATE accounts SET cooldown_until = ?2 WHERE id = ?1",
                params![id, new_cooldown],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    pub fn clear_account_cooldown(&self, id: &str) -> Result<()> {
        self.set_account_cooldown(id, None, None)
    }

    /// Persist or clear an account-specific upstream 401. This state is kept
    /// separate from cooldowns because authentication failures do not carry a
    /// reset deadline and must not be reported as rate limits.
    pub fn set_account_auth_error(&self, id: &str, error: Option<&str>) -> Result<()> {
        self.conn.execute(
            "UPDATE accounts SET auth_error = ?2, updated_at = ?3 WHERE id = ?1",
            params![id, error, Utc::now().to_rfc3339()],
        )?;
        Ok(())
    }

    /// Update auth state only when the stored credential is still the one that
    /// produced this upstream response. A late response from a replaced key
    /// must not break or recover the new credential.
    pub fn set_account_auth_error_if_key_matches(
        &self,
        id: &str,
        expected_key_cipher: &str,
        error: Option<&str>,
    ) -> Result<bool> {
        let updated = self.conn.execute(
            "UPDATE accounts
             SET auth_error = ?3, updated_at = ?4
             WHERE id = ?1 AND key_cipher = ?2",
            params![id, expected_key_cipher, error, Utc::now().to_rfc3339()],
        )?;
        Ok(updated > 0)
    }

    /// Record a real upstream 429 and reset only the identified manual usage window.
    pub fn set_account_rate_limit(
        &self,
        id: &str,
        until: DateTime<Utc>,
        err: &str,
        window: Option<UsageWindowKind>,
    ) -> Result<()> {
        self.set_account_rate_limit_inner(id, None, until, err, window)?;
        Ok(())
    }

    /// Record a 429 only when the credential that produced it is still current.
    /// This prevents a delayed response from an old key from cooling down a
    /// replacement credential.
    pub fn set_account_rate_limit_if_key_matches(
        &self,
        id: &str,
        expected_key_cipher: &str,
        until: DateTime<Utc>,
        err: &str,
        window: Option<UsageWindowKind>,
    ) -> Result<bool> {
        self.set_account_rate_limit_inner(id, Some(expected_key_cipher), until, err, window)
    }

    fn set_account_rate_limit_inner(
        &self,
        id: &str,
        expected_key_cipher: Option<&str>,
        until: DateTime<Utc>,
        err: &str,
        window: Option<UsageWindowKind>,
    ) -> Result<bool> {
        let now = Utc::now();
        let now_rfc = now.to_rfc3339();
        let tx = self.conn.unchecked_transaction()?;

        // Unknown upstream rate limits need their own slot so a later known window
        // cannot overwrite a still-active generic cooldown.
        let column = match window {
            Some(UsageWindowKind::FiveHours) => "cooldown_5h_until",
            Some(UsageWindowKind::Week) => "cooldown_week_until",
            Some(UsageWindowKind::Month) => "cooldown_month_until",
            Some(UsageWindowKind::Free) => "cooldown_free_until",
            None => "cooldown_generic_until",
        };
        let updated = tx.execute(
            &format!(
                "UPDATE accounts SET {column} = ?2, last_error = ?3, updated_at = ?4
                 WHERE id = ?1 AND (?5 IS NULL OR key_cipher = ?5)"
            ),
            params![id, until.to_rfc3339(), err, now_rfc, expected_key_cipher],
        )?;
        if updated == 0 && window != Some(UsageWindowKind::Free) {
            return Ok(false);
        }

        if updated > 0 {
            // Legacy callers use cooldown_until as the time when this account is usable.
            let new_cooldown = Self::compute_cooldown_until(&tx, id, &now_rfc)?;
            tx.execute(
                "UPDATE accounts SET cooldown_until = ?2 WHERE id = ?1",
                params![id, new_cooldown],
            )?;
        }

        if window == Some(UsageWindowKind::Free) {
            // A Free 429 proves the egress-IP quota is exhausted even if the
            // originating key was concurrently replaced or its account deleted.
            // Keep the furthest observed deadline and commit it atomically with
            // the account-local compatibility copy when that row still exists.
            Self::upsert_free_channel_cooldown(&tx, &until.to_rfc3339())?;
        }

        // ponytail: 不再在 429 时设置 baseline。固定窗口的"重置"由 forward_logs 自然驱动；
        // 冷却到期后账号恢复可用，用量窗口照常计算。429 仅用于阻断选择器重试。
        // 旧 baseline 列保留不读不写，避免迁移风险。
        tx.commit()?;
        Ok(updated > 0)
    }

    fn upsert_free_channel_cooldown(tx: &rusqlite::Transaction<'_>, until: &str) -> Result<()> {
        tx.execute(
            "INSERT INTO settings (key, value) VALUES (?1, ?2)
             ON CONFLICT(key) DO UPDATE SET value = CASE
                 WHEN excluded.value > settings.value THEN excluded.value
                 ELSE settings.value
             END",
            params![FREE_CHANNEL_COOLDOWN_SETTING, until],
        )?;
        Ok(())
    }

    fn compute_cooldown_until(
        tx: &rusqlite::Transaction,
        id: &str,
        now_rfc: &str,
    ) -> Result<Option<String>> {
        let max: Option<String> = tx.query_row(
            "SELECT MAX(until) FROM (
                SELECT cooldown_generic_until AS until FROM accounts WHERE id = ?1
                UNION ALL
                SELECT cooldown_5h_until FROM accounts WHERE id = ?1
                UNION ALL
                SELECT cooldown_week_until FROM accounts WHERE id = ?1
                UNION ALL
                SELECT cooldown_month_until FROM accounts WHERE id = ?1
                UNION ALL
                SELECT cooldown_free_until FROM accounts WHERE id = ?1
            ) WHERE until IS NOT NULL AND until > ?2",
            params![id, now_rfc],
            |row| row.get(0),
        )?;
        Ok(max)
    }

    /// Among all enabled accounts, return the first time any account becomes usable.
    /// `None` means no account is in cooldown.
    pub fn soonest_cooldown_reset(&self) -> Result<Option<DateTime<Utc>>> {
        let now = Utc::now().to_rfc3339();
        let res: Option<String> = self
            .conn
            .query_row(
                "SELECT MIN(cooldown_until)
                 FROM accounts
                 WHERE enabled = 1
                   AND setup_step = 'ready'
                   AND key_cipher <> ''
                   AND auth_error IS NULL
                   AND cooldown_until IS NOT NULL
                   AND cooldown_until > ?1",
                params![now],
                |row| row.get(0),
            )
            .optional()?
            .flatten();
        Ok(res.and_then(|s| {
            DateTime::parse_from_rfc3339(&s)
                .ok()
                .map(|dt| dt.with_timezone(&Utc))
        }))
    }

    // Usage
    /// 手动校准一个固定窗口的"当前已用百分比"与"距上游重置还剩多久"。
    /// `percent` = 当前已用百分比（0-100），`resets_in_minutes` = 距上游重置还剩多少分钟
    /// （None 表示从 now 起算满窗口时长；月窗口忽略此参数——窗口由 purchase_date/expires_on 决定）。
    /// `limit` = 当前窗口的限额（从 PricingSnapshot 读取，避免硬编码）。
    pub fn calibrate_account_usage(
        &self,
        account_id: &str,
        window: UsageWindowKind,
        percent: f64,
        resets_in_minutes: Option<i64>,
        limit: f64,
    ) -> Result<bool> {
        calibrate_account_usage_on(
            &self.conn,
            account_id,
            window,
            percent,
            resets_in_minutes,
            limit,
            Utc::now(),
        )
    }

    /// Atomically calibrate rolling, weekly, and monthly Go usage windows.
    /// Any input, SQL, or missing-account error rolls the whole transaction back.
    pub fn calibrate_account_usage_snapshot(
        &self,
        account_id: &str,
        snapshot: &AccountUsageCalibrationSnapshot,
        limits: &PricingLimits,
    ) -> Result<UsageWindow> {
        let tx = self.conn.unchecked_transaction()?;
        let now = Utc::now();
        if !calibrate_account_usage_on(
            &tx,
            account_id,
            UsageWindowKind::FiveHours,
            snapshot.rolling_percent,
            Some(snapshot.rolling_resets_in_minutes),
            limits.window_5h,
            now,
        )? {
            anyhow::bail!("account {account_id} not found");
        }
        if !calibrate_account_usage_on(
            &tx,
            account_id,
            UsageWindowKind::Week,
            snapshot.weekly_percent,
            Some(snapshot.weekly_resets_in_minutes),
            limits.window_week,
            now,
        )? {
            anyhow::bail!("account {account_id} not found");
        }
        if !calibrate_account_usage_on(
            &tx,
            account_id,
            UsageWindowKind::Month,
            snapshot.monthly_percent,
            None,
            limits.window_month,
            now,
        )? {
            anyhow::bail!("account {account_id} not found");
        }
        tx.commit()?;
        self.account_usage_with_limits(account_id, limits)
    }

    /// Atomically CAS the credential/setup state, calibrate all three official
    /// usage windows, persist sync-success metadata, and compute the returned
    /// usage. `None` means the account disappeared or changed while the
    /// network request was in flight. Any SQL/read failure rolls everything
    /// back, so a failed refresh never exposes a partially updated baseline.
    pub fn commit_official_usage_sync_success(
        &self,
        account_id: &str,
        expected_key_cipher: &str,
        snapshot: &AccountUsageCalibrationSnapshot,
        limits: &PricingLimits,
        metadata: AccountUsageSyncSuccessMetadata,
    ) -> Result<Option<UsageWindow>> {
        let tx = self.conn.unchecked_transaction()?;
        let matches: i64 = tx.query_row(
            "SELECT EXISTS(
                SELECT 1 FROM accounts
                WHERE id = ?1
                  AND key_cipher = ?2
                  AND key_cipher <> ''
                  AND setup_step = 'ready'
             )",
            params![account_id, expected_key_cipher],
            |row| row.get(0),
        )?;
        if matches == 0 {
            return Ok(None);
        }

        if !calibrate_account_usage_on(
            &tx,
            account_id,
            UsageWindowKind::FiveHours,
            snapshot.rolling_percent,
            Some(snapshot.rolling_resets_in_minutes),
            limits.window_5h,
            metadata.now,
        )? {
            anyhow::bail!("account {account_id} disappeared during official usage sync");
        }
        if !calibrate_account_usage_on(
            &tx,
            account_id,
            UsageWindowKind::Week,
            snapshot.weekly_percent,
            Some(snapshot.weekly_resets_in_minutes),
            limits.window_week,
            metadata.now,
        )? {
            anyhow::bail!("account {account_id} disappeared during official usage sync");
        }
        if !calibrate_account_usage_on(
            &tx,
            account_id,
            UsageWindowKind::Month,
            snapshot.monthly_percent,
            None,
            limits.window_month,
            metadata.now,
        )? {
            anyhow::bail!("account {account_id} disappeared during official usage sync");
        }

        record_account_usage_sync_success_on(&tx, account_id, metadata)?;
        let usage = account_usage_with_limits_on(&tx, account_id, limits, metadata.now)?;
        tx.commit()?;
        Ok(Some(usage))
    }

    pub fn account_usage(&self, account_id: &str) -> Result<UsageWindow> {
        let limits = self
            .latest_pricing_snapshot()?
            .map(|snapshot| snapshot.limits)
            .unwrap_or(SEED_LIMITS);
        self.account_usage_with_limits(account_id, &limits)
    }

    pub fn account_usage_with_limits(
        &self,
        account_id: &str,
        limits: &PricingLimits,
    ) -> Result<UsageWindow> {
        account_usage_with_limits_on(&self.conn, account_id, limits, Utc::now())
    }

    /// Project the canonical legacy Go accounting windows into the provider
    /// API shape. The v22 `quota_windows` rows are migration/interoperability
    /// storage, not a second Go accounting authority: local forward logs and
    /// calibration offsets continue to advance between official syncs.
    pub fn live_opencode_go_quota_windows(
        &self,
        account_id: &str,
        limits: &PricingLimits,
    ) -> Result<Vec<QuotaWindow>> {
        let now = Utc::now();
        let usage = account_usage_with_limits_on(&self.conn, account_id, limits, now)?;
        let metadata = self
            .conn
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
            )
            .optional()?
            .ok_or_else(|| anyhow::anyhow!("account {account_id} not found"))?;
        let observed_at = self
            .account_usage_sync_state(account_id)?
            .and_then(|sync| sync.last_success_at);
        let month_started_at = month_window_start_utc(&metadata.5).ok();

        Ok(vec![
            QuotaWindow {
                account_id: account_id.to_string(),
                window_kind: QUOTA_WINDOW_FIVE_HOURS.to_string(),
                used: usage.window_5h,
                limit_value: Some(limits.window_5h),
                started_at: metadata.0.map(parse_datetime),
                resets_at: usage.resets_in_5h,
                calibration_offset: metadata.1,
                unit: "usd".to_string(),
                source: "opencode-go-live".to_string(),
                observed_at,
                updated_at: now,
            },
            QuotaWindow {
                account_id: account_id.to_string(),
                window_kind: QUOTA_WINDOW_WEEK.to_string(),
                used: usage.window_week,
                limit_value: Some(limits.window_week),
                started_at: metadata.2.map(parse_datetime),
                resets_at: usage.resets_in_week,
                calibration_offset: metadata.3,
                unit: "usd".to_string(),
                source: "opencode-go-live".to_string(),
                observed_at,
                updated_at: now,
            },
            QuotaWindow {
                account_id: account_id.to_string(),
                window_kind: QUOTA_WINDOW_MONTH.to_string(),
                used: usage.window_month,
                limit_value: Some(limits.window_month),
                started_at: month_started_at,
                resets_at: usage.resets_in_month,
                calibration_offset: metadata.4,
                unit: "usd".to_string(),
                source: "opencode-go-live".to_string(),
                observed_at,
                updated_at: now,
            },
        ])
    }

    pub fn total_usage(&self) -> Result<(f64, f64, f64)> {
        let now = Utc::now();
        let today_start = now
            .date_naive()
            .and_hms_opt(0, 0, 0)
            .unwrap()
            .and_utc()
            .to_rfc3339();
        let week_ago = (now - Duration::days(7)).to_rfc3339();
        let month_ago = (now - Duration::days(30)).to_rfc3339();

        let today: f64 = self.conn.query_row(
            "SELECT COALESCE(SUM(cost), 0) FROM forward_logs WHERE cost_state IN ('priced', 'legacy_estimate') AND timestamp > ?1",
            [&today_start],
            |row| row.get(0),
        )?;
        let week: f64 = self.conn.query_row(
            "SELECT COALESCE(SUM(cost), 0) FROM forward_logs WHERE cost_state IN ('priced', 'legacy_estimate') AND timestamp > ?1",
            [&week_ago],
            |row| row.get(0),
        )?;
        let month: f64 = self.conn.query_row(
            "SELECT COALESCE(SUM(cost), 0) FROM forward_logs WHERE cost_state IN ('priced', 'legacy_estimate') AND timestamp > ?1",
            [&month_ago],
            |row| row.get(0),
        )?;

        Ok((today, week, month))
    }

    /// Aggregate `forward_logs` into per-day, per-model cost buckets covering
    /// the last `days` calendar days (UTC). Rows with zero activity on a given
    /// day are omitted — the frontend synthesizes empty days so the x-axis
    /// never collapses. Priced rows and preserved legacy estimates count,
    /// including an upstream success whose local response conversion failed.
    pub fn daily_cost_by_model(&self, days: i64) -> Result<Vec<DailyModelCost>> {
        // Bone-simple SQLite date math: store timestamps as RFC3339 strings,
        // so group by `substr(timestamp, 1, 10)` to collapse to YYYY-MM-DD.
        // UTC-only is fine — the gateway runs local and the dashboard is a
        // single-user tool; a TZ-correct grouping would need a calendar table
        // or a strftime('%Y-%m-%d', ...) with proper epoch arg, which is more
        // machinery than this needs right now.
        let since = (Utc::now() - Duration::days(days - 1)).to_rfc3339();
        let mut stmt = self.conn.prepare(
            "SELECT substr(timestamp, 1, 10) AS day, model, COALESCE(SUM(cost), 0)
             FROM forward_logs
             WHERE cost_state IN ('priced', 'legacy_estimate') AND timestamp > ?1
             GROUP BY day, model
             ORDER BY day ASC, model ASC",
        )?;
        let rows = stmt.query_map([&since], |row| {
            Ok(DailyModelCost {
                date: row.get::<_, String>(0)?,
                model: row.get::<_, String>(1)?,
                cost: row.get::<_, f64>(2)?,
            })
        })?;
        rows.collect::<Result<Vec<_>, _>>().map_err(|e| e.into())
    }
}

fn record_account_usage_sync_success_on(
    conn: &Connection,
    account_id: &str,
    metadata: AccountUsageSyncSuccessMetadata,
) -> Result<()> {
    let changed = if metadata.mark_expedited {
        conn.execute(
            "UPDATE provider_usage_sync_state
             SET last_success_at = ?1,
                 last_attempt_at = ?1,
                 next_eligible_at = ?2,
                 failure_streak = 0,
                 last_expedited_at = ?1
             WHERE account_id = ?3",
            params![
                metadata.now.to_rfc3339(),
                metadata.next_eligible_at.to_rfc3339(),
                account_id
            ],
        )?
    } else {
        conn.execute(
            "UPDATE provider_usage_sync_state
             SET last_success_at = ?1,
                 last_attempt_at = ?1,
                 next_eligible_at = ?2,
                 failure_streak = 0
             WHERE account_id = ?3",
            params![
                metadata.now.to_rfc3339(),
                metadata.next_eligible_at.to_rfc3339(),
                account_id
            ],
        )?
    };
    if changed != 1 {
        anyhow::bail!("account {account_id} disappeared while recording usage sync success");
    }
    Ok(())
}

fn account_usage_with_limits_on(
    conn: &Connection,
    account_id: &str,
    limits: &PricingLimits,
    now: DateTime<Utc>,
) -> Result<UsageWindow> {
    let row = conn.query_row(
        "SELECT usage_5h_window_started_at, usage_5h_window_cost_offset,
                usage_week_window_started_at, usage_week_window_cost_offset,
                usage_month_window_cost_offset,
                recharge_date
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
    );
    let (started_5h_str, offset_5h, started_week_str, offset_week, offset_month, purchase_date) =
        match row.optional()? {
            Some(value) => value,
            None => {
                return Ok(UsageWindow {
                    account_id: account_id.to_string(),
                    window_5h: 0.0,
                    window_week: 0.0,
                    window_month: 0.0,
                    resets_in_5h: None,
                    resets_in_week: None,
                    resets_in_month: None,
                });
            }
        };

    let (cost_5h, reset_5h) = compute_fixed_window(
        conn,
        account_id,
        started_5h_str.as_deref(),
        offset_5h,
        limits.window_5h,
        now,
        FixedWindowSpec {
            length: Duration::hours(5),
            started_col: "usage_5h_window_started_at",
            offset_col: "usage_5h_window_cost_offset",
        },
    )?;
    let (cost_week, reset_week) = compute_fixed_window(
        conn,
        account_id,
        started_week_str.as_deref(),
        offset_week,
        limits.window_week,
        now,
        FixedWindowSpec {
            length: Duration::days(7),
            started_col: "usage_week_window_started_at",
            offset_col: "usage_week_window_cost_offset",
        },
    )?;
    let (cost_month, reset_month) = compute_month_window(
        conn,
        account_id,
        &purchase_date,
        offset_month,
        limits.window_month,
    )?;

    Ok(UsageWindow {
        account_id: account_id.to_string(),
        window_5h: cost_5h,
        window_week: cost_week,
        window_month: cost_month,
        resets_in_5h: reset_5h,
        resets_in_week: reset_week,
        resets_in_month: reset_month,
    })
}

/// 计算固定窗口的当前用量与清零时刻。`started_at_str` 为 `None` 表示账号从未使用过该窗口；
/// 窗口已过期时从 `forward_logs` lazy 重建新起点。
struct FixedWindowSpec {
    length: Duration,
    started_col: &'static str,
    offset_col: &'static str,
}

fn compute_fixed_window(
    conn: &Connection,
    account_id: &str,
    started_at_str: Option<&str>,
    offset: f64,
    limit: f64,
    now: DateTime<Utc>,
    spec: FixedWindowSpec,
) -> Result<(f64, Option<DateTime<Utc>>)> {
    let mut started_at = match started_at_str {
        None => {
            // ponytail: lazy 初始化——查 forward_logs 第一条计费请求作为窗口起点。
            // 计费行 = cost_state IN ('priced', 'legacy_estimate')，
            // 与下方 SUM(cost) 的过滤保持一致，确保迁移后的 legacy error 也能触发窗口。
            let first: Option<String> = conn
                .query_row(
                    "SELECT MIN(timestamp) FROM forward_logs
                     WHERE account_id = ?1
                       AND cost_state IN ('priced', 'legacy_estimate')",
                    [account_id],
                    |row| row.get(0),
                )
                .optional()?
                .flatten();
            match first {
                None => return Ok((0.0, None)), // 真的没用过
                Some(s) => {
                    conn.execute(
                        &format!(
                            "UPDATE accounts SET {} = ?2, {} = 0
                             WHERE id = ?1",
                            spec.started_col, spec.offset_col
                        ),
                        params![account_id, &s],
                    )?;
                    parse_rfc3339(&s)?
                }
            }
        }
        Some(s) => parse_rfc3339(s)?,
    };
    // 第一次进入循环时使用调用方传入的 offset（来自手动校准）；任何一次前进后，
    // offset 都被清零（`offset_col = 0` 已写入 DB），用 effective_offset 跟踪。
    let mut effective_offset = offset;

    loop {
        let ends_at = started_at + spec.length;
        if now < ends_at {
            // 窗口仍有效：用量 = effective_offset + SUM(cost WHERE ts >= started_at)
            let cost: f64 = conn.query_row(
                "SELECT COALESCE(SUM(cost), 0) FROM forward_logs
                 WHERE account_id = ?1
                   AND cost_state IN ('priced', 'legacy_estimate')
                   AND timestamp >= ?2",
                params![account_id, started_at.to_rfc3339()],
                |row| row.get(0),
            )?;
            return Ok(((effective_offset + cost).min(limit), Some(ends_at)));
        }

        // 窗口已过期：找 forward_logs 中第一条 timestamp >= ends_at 的计费请求作为新起点。
        // 关键修复：旧实现只前进一次就 return，遇到多条稀疏日志（间隔 > 5h）时每次刷新
        // 只前进一个窗口，造成前端可见的"用量从 60 → 30 → 13 → 5.8 → 0"递减幻觉；
        // 当 next=None 清空后下次刷新又 lazy-init 回最旧日志，循环重启。
        // 用 loop 在一次调用内连过所有过期窗口，直到落在有效窗口或彻底无新请求。
        let next: Option<String> = conn
            .query_row(
                "SELECT MIN(timestamp) FROM forward_logs
                 WHERE account_id = ?1
                   AND cost_state IN ('priced', 'legacy_estimate')
                   AND timestamp >= ?2",
                params![account_id, ends_at.to_rfc3339()],
                |row| row.get(0),
            )
            .optional()?
            .flatten();
        match next {
            None => {
                // 过期后无新请求：清空窗口，等待下次请求触发新窗口。
                conn.execute(
                    &format!(
                        "UPDATE accounts SET {} = NULL, {} = 0
                         WHERE id = ?1",
                        spec.started_col, spec.offset_col
                    ),
                    [account_id],
                )?;
                return Ok((0.0, None));
            }
            Some(s) => {
                started_at = parse_rfc3339(&s)?;
                effective_offset = 0.0;
                conn.execute(
                    &format!(
                        "UPDATE accounts SET {} = ?2, {} = 0
                         WHERE id = ?1",
                        spec.started_col, spec.offset_col
                    ),
                    params![account_id, &s],
                )?;
                // 继续循环：新起点对应的窗口可能也已过期，需要再判一次。
            }
        }
    }
}

/// 月窗口：从 `purchase_date 00:00 本地时区` 累计到 `purchase_expires_on(purchase_date) 00:00 本地时区`，不重置。
/// `offset` = 手动校准时写入的 `usage_month_window_cost_offset`，与 `compute_fixed_window` 对齐：
/// 返回值 = `(offset + cost).min(limit)`，让月窗口支持手动校准。
fn compute_month_window(
    conn: &Connection,
    account_id: &str,
    purchase_date: &str,
    offset: f64,
    limit: f64,
) -> Result<(f64, Option<DateTime<Utc>>)> {
    if purchase_date.trim().is_empty() {
        return Ok((0.0, None));
    }
    let start = month_window_start_utc(purchase_date)?;
    let expires = purchase_expires_on(purchase_date)?;
    let end_naive = NaiveDate::parse_from_str(&expires, "%Y-%m-%d")?
        .and_hms_opt(0, 0, 0)
        .unwrap();
    let end: DateTime<Utc> = Local
        .from_local_datetime(&end_naive)
        .single()
        .ok_or_else(|| anyhow::anyhow!("ambiguous local datetime for expires_on"))?
        .with_timezone(&Utc);
    let cost: f64 = conn.query_row(
        "SELECT COALESCE(SUM(cost), 0) FROM forward_logs
         WHERE account_id = ?1
           AND cost_state IN ('priced', 'legacy_estimate')
           AND timestamp >= ?2",
        params![account_id, start.to_rfc3339()],
        |row| row.get(0),
    )?;
    // ponytail: 月窗口已过期也照常返回终点，前端按"已到期"显示。
    Ok(((offset + cost).min(limit), Some(end)))
}

fn calibrate_account_usage_on(
    conn: &Connection,
    account_id: &str,
    window: UsageWindowKind,
    percent: f64,
    resets_in_minutes: Option<i64>,
    limit: f64,
    now: DateTime<Utc>,
) -> Result<bool> {
    // (started_at, offset_col, started_col_or_empty)
    // started_col 为空字符串表示月窗口——不写 started_at 列（起点固定为 purchase_date）。
    let (started_at, started_col, offset_col): (Option<DateTime<Utc>>, &str, &str) = match window {
        UsageWindowKind::FiveHours => {
            let window_len = Duration::hours(5);
            let started_at = calibrated_window_start(now, window_len, resets_in_minutes, "5-hour")?;
            (
                Some(started_at),
                "usage_5h_window_started_at",
                "usage_5h_window_cost_offset",
            )
        }
        UsageWindowKind::Week => {
            let window_len = Duration::days(7);
            let started_at = calibrated_window_start(now, window_len, resets_in_minutes, "weekly")?;
            (
                Some(started_at),
                "usage_week_window_started_at",
                "usage_week_window_cost_offset",
            )
        }
        UsageWindowKind::Month => {
            // 月窗口的起点/终点由 purchase_date 决定，不写 started_at 列。
            // resets_in_minutes 被忽略——窗口已由账号购买日期固定。
            (None, "", "usage_month_window_cost_offset")
        }
        UsageWindowKind::Free => {
            anyhow::bail!("free promo quota cannot be calibrated as a Go usage window")
        }
    };

    // 计算 actual_cost：窗口内已有 forward_logs 的 cost 总和。
    // 5h/周窗口的起点是刚算出的 started_at；月窗口的起点是 purchase_date 00:00 本地时区。
    let actual_cost: f64 = match started_at {
        Some(started) => conn.query_row(
            "SELECT COALESCE(SUM(cost), 0) FROM forward_logs
             WHERE account_id = ?1
               AND cost_state IN ('priced', 'legacy_estimate')
               AND timestamp >= ?2",
            params![account_id, started.to_rfc3339()],
            |row| row.get(0),
        )?,
        None => {
            let purchase_date: String = conn
                .query_row(
                    "SELECT recharge_date FROM accounts WHERE id = ?1",
                    [account_id],
                    |row| row.get(0),
                )
                .optional()?
                .ok_or_else(|| anyhow::anyhow!("account not found"))?;
            let started = month_window_start_utc(&purchase_date)?;
            conn.query_row(
                "SELECT COALESCE(SUM(cost), 0) FROM forward_logs
                 WHERE account_id = ?1
                   AND cost_state IN ('priced', 'legacy_estimate')
                   AND timestamp >= ?2",
                params![account_id, started.to_rfc3339()],
                |row| row.get(0),
            )?
        }
    };

    let target_cost = limit * percent / 100.0;
    // Bug 1.5 修复：去掉 max(0, ...) 钳制，允许负 offset。
    // 之前 max(0, target - actual) 配合 schema CHECK (offset >= 0) 让向左拉
    // 滑块时被锁死在实际 cost 对应的百分比。现在 offset 可以为负，
    // compute_fixed_window 返回 offset + actual = target_cost，与用户输入一致。
    let offset = target_cost - actual_cost;

    let changed = if started_col.is_empty() {
        // 月窗口：只更新 cost_offset（started_at 由 purchase_date 派生，不存储）
        conn.execute(
            "UPDATE accounts
             SET usage_month_window_cost_offset = ?2,
                 updated_at = ?3
             WHERE id = ?1",
            params![account_id, offset, now.to_rfc3339()],
        )?
    } else {
        let started = started_at.unwrap();
        conn.execute(
            &format!(
                "UPDATE accounts
                 SET {started_col} = ?2,
                     {offset_col} = ?3,
                     updated_at = ?4
                 WHERE id = ?1"
            ),
            params![account_id, started.to_rfc3339(), offset, now.to_rfc3339()],
        )?
    };
    Ok(changed > 0)
}

fn calibrated_window_start(
    now: DateTime<Utc>,
    window_len: Duration,
    resets_in_minutes: Option<i64>,
    window_name: &str,
) -> Result<DateTime<Utc>> {
    let max_minutes = window_len.num_minutes();
    let remaining_minutes = resets_in_minutes.unwrap_or(max_minutes);
    if !(0..=max_minutes).contains(&remaining_minutes) {
        return Err(anyhow::anyhow!(
            "{window_name} resets_in_minutes must be between 0 and {max_minutes}"
        ));
    }
    let remaining = Duration::try_minutes(remaining_minutes)
        .ok_or_else(|| anyhow::anyhow!("resets_in_minutes is out of range"))?;
    let ends_at = now
        .checked_add_signed(remaining)
        .ok_or_else(|| anyhow::anyhow!("usage window end is out of range"))?;
    ends_at
        .checked_sub_signed(window_len)
        .ok_or_else(|| anyhow::anyhow!("usage window start is out of range"))
}

/// 把 `purchase_date`（YYYY-MM-DD）解释为本时区 00:00，转 UTC。
/// purchase_date 是 local_today() 写入的本地日期；转 UTC 时必须经过 Local 时区，
/// 否则本地早上的请求会被 UTC 午夜 cutoff 漏算。
fn month_window_start_utc(purchase_date: &str) -> Result<DateTime<Utc>> {
    let normalized = normalize_purchase_date(purchase_date)?;
    let start_naive = NaiveDate::parse_from_str(&normalized, "%Y-%m-%d")?
        .and_hms_opt(0, 0, 0)
        .unwrap();
    Ok(Local
        .from_local_datetime(&start_naive)
        .single()
        .ok_or_else(|| anyhow::anyhow!("ambiguous local datetime for purchase_date"))?
        .with_timezone(&Utc))
}

fn parse_rfc3339(s: &str) -> Result<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(s)
        .map(|dt| dt.with_timezone(&Utc))
        .map_err(|e| anyhow::anyhow!("invalid RFC3339 timestamp: {e}"))
}

fn effective_usage(
    local_window_cost: f64,
    baseline: Option<(f64, f64)>,
    total_success_cost: f64,
    limit: f64,
) -> f64 {
    baseline.map_or(local_window_cost, |(percent, anchor)| {
        (limit * percent / 100.0 + (total_success_cost - anchor).max(0.0)).min(limit)
    })
}

fn forward_log_filter(options: &ForwardLogQueryOptions<'_>) -> (String, Vec<Value>) {
    let mut filter = String::new();
    let mut params = Vec::new();
    // (clause, bound text values); clause order must match parameter order.
    // The key filter goes last because the unattributed sentinel expands to
    // a literal `IS NULL` clause with no parameter.
    let mut clauses: Vec<(String, Vec<&str>)> = [
        ("status = ?", options.status),
        ("account_id = ?", options.account_id),
        ("provider_id = ?", options.provider_id),
        ("offering_id = ?", options.offering_id),
        ("route_account_id = ?", options.route_account_id),
        ("credential_account_id = ?", options.credential_account_id),
    ]
    .into_iter()
    .filter_map(|(clause, value)| value.map(|value| (clause.to_string(), vec![value])))
    .collect();
    // Exact-match any stored identity so alias/upstream/legacy rows stay
    // filterable. Bind the same value once per column; OR stays inside this
    // predicate so other filters still AND and a row never duplicates.
    if let Some(model) = options.model.filter(|value| !value.is_empty()) {
        clauses.push((
            "(model = ? OR requested_model = ? OR resolved_alias = ? OR upstream_model = ?)"
                .to_string(),
            vec![model, model, model, model],
        ));
    }
    for (clause, value) in [
        ("request_id = ?", options.request_id),
        ("julianday(timestamp) >= julianday(?)", options.start_time),
        ("julianday(timestamp) <= julianday(?)", options.end_time),
    ] {
        if let Some(value) = value {
            clauses.push((clause.to_string(), vec![value]));
        }
    }
    match options.key_id {
        Some(UNATTRIBUTED_KEY_FILTER) => {
            clauses.push(("client_key_id IS NULL".to_string(), Vec::new()));
        }
        Some(id) => clauses.push(("client_key_id = ?".to_string(), vec![id])),
        None => {}
    }
    for (clause, values) in clauses {
        append_filter_clause(&mut filter, &clause);
        for value in values {
            params.push(Value::Text(value.to_owned()));
        }
    }
    (filter, params)
}

fn append_filter_clause(filter: &mut String, clause: &str) {
    filter.push_str(if filter.is_empty() {
        " WHERE "
    } else {
        " AND "
    });
    filter.push_str(clause);
}

fn forward_log_order(sort_by: Option<&str>, sort_order: Option<&str>) -> String {
    let column = match sort_by {
        Some("timestamp") => "timestamp",
        Some("attempt") => "attempt",
        Some("prompt_tokens") => "prompt_tokens",
        Some("completion_tokens") => "completion_tokens",
        Some("cached_tokens") => "cached_tokens",
        Some("cost") => "cost",
        Some("model") => "model",
        Some("status") => "status",
        _ => "id",
    };
    let direction = if sort_order == Some("asc") {
        "ASC"
    } else {
        "DESC"
    };
    format!("ORDER BY {column} {direction}, id DESC")
}

fn forward_log_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<ForwardLog> {
    // SELECT order: id,timestamp,model,account_id,account_name,status,http_status,
    // route,prompt,completion,cached,cache_creation,cost,pricing_revision,quota,
    // local_adjustment,service_tier,cost_state,error_message,request_id,attempt,
    // error_source,error_stage,duration_ms,diagnostic_json,client_key_id,client_key_name
    let raw_cost = row.get::<_, f64>(12)?;
    let cost_state = row.get::<_, String>(17)?;
    let cost = matches!(cost_state.as_str(), "priced" | "legacy_estimate").then_some(raw_cost);
    Ok(ForwardLog {
        id: row.get(0)?,
        timestamp: parse_datetime(row.get::<_, String>(1)?),
        model: row.get(2)?,
        account_id: row.get(3)?,
        account_name: row.get(4)?,
        client_key_id: row.get(25)?,
        client_key_name: row.get(26)?,
        route_account_id: row.get(27)?,
        provider_id: row.get(28)?,
        offering_id: row.get(29)?,
        credential_account_id: row.get(30)?,
        status: row.get(5)?,
        http_status: row.get(6)?,
        route: row.get(7)?,
        prompt_tokens: row.get(8)?,
        completion_tokens: row.get(9)?,
        cached_tokens: row.get(10)?,
        cache_creation_tokens: row.get(11)?,
        cost,
        raw_cost_usd: row.get(31)?,
        quota_debit: row.get(32)?,
        effective_paid_cost_usd: row.get(33)?,
        pricing_revision_id: row.get(13)?,
        quota_multiplier: row.get(14)?,
        local_adjustment_multiplier: row.get(15)?,
        service_tier: row.get(16)?,
        cost_state,
        error_message: row.get(18)?,
        request_id: row.get(19)?,
        attempt: row.get(20)?,
        error_source: row.get(21)?,
        error_stage: row.get(22)?,
        duration_ms: row.get(23)?,
        diagnostic: row
            .get::<_, Option<String>>(24)?
            .and_then(|json| serde_json::from_str(&json).ok()),
    })
}

fn sub_gateway_key_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<SubGatewayKey> {
    // SELECT order: id,name,key,enabled,deleted_at,created_at
    Ok(SubGatewayKey {
        id: row.get(0)?,
        name: row.get(1)?,
        key: row.get(2)?,
        enabled: row.get::<_, i64>(3)? != 0,
        deleted_at: row.get::<_, Option<String>>(4)?.map(parse_datetime),
        created_at: parse_datetime(row.get::<_, String>(5)?),
    })
}

fn account_verification_from_row(row: &Row<'_>) -> rusqlite::Result<AccountVerificationState> {
    let status_value = row.get::<_, String>(1)?;
    let status =
        ConnectionVerificationStatus::try_from(status_value.as_str()).map_err(|error| {
            rusqlite::Error::FromSqlConversionFailure(1, Type::Text, Box::new(error))
        })?;
    Ok(AccountVerificationState {
        account_id: row.get(0)?,
        status,
        connection_verified_at: row.get::<_, Option<String>>(2)?.map(parse_datetime),
        verification_error: row.get(3)?,
    })
}

fn custom_verification_contract_still_matches_on(
    conn: &Connection,
    contract: &crate::custom::CustomVerificationContract,
) -> Result<bool> {
    let Some((updated_at, key_cipher, status)): Option<(String, String, String)> = conn
        .query_row(
            "SELECT updated_at, key_cipher, verification_status
             FROM accounts WHERE id = ?1",
            [&contract.account_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()?
    else {
        return Ok(false);
    };
    if updated_at != contract.account_updated_at
        || key_cipher != contract.key_cipher
        || !matches!(status.as_str(), "pending" | "failed")
    {
        return Ok(false);
    }
    let Some((base_url, protocol, auth_scheme)): Option<(String, String, String)> = conn
        .query_row(
            "SELECT base_url, upstream_protocol, auth_scheme
             FROM account_custom_configs WHERE account_id = ?1",
            [&contract.account_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()?
    else {
        return Ok(false);
    };
    if base_url != contract.base_url
        || protocol != contract.upstream_protocol.as_str()
        || auth_scheme != contract.auth_scheme.as_str()
    {
        return Ok(false);
    }
    let mut stmt = conn.prepare(
        "SELECT model_id, protocol
         FROM account_model_capabilities
         WHERE account_id = ?1
         ORDER BY rowid ASC",
    )?;
    let rows = stmt.query_map([&contract.account_id], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
    })?;
    let mut current = Vec::new();
    for row in rows {
        let (model_id, protocol) = row?;
        let protocol = UpstreamProtocolKind::try_from(protocol.as_str())
            .map_err(|error| anyhow::anyhow!(error))?;
        current.push((model_id, protocol));
    }
    Ok(current == contract.capabilities)
}

fn account_custom_config_from_row(row: &Row<'_>) -> rusqlite::Result<AccountCustomConfig> {
    let protocol_value = row.get::<_, String>(2)?;
    let protocol = UpstreamProtocolKind::try_from(protocol_value.as_str()).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(2, Type::Text, Box::new(error))
    })?;
    let auth_value = row.get::<_, String>(3)?;
    let auth_scheme = UpstreamAuthScheme::try_from(auth_value.as_str()).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(3, Type::Text, Box::new(error))
    })?;
    Ok(AccountCustomConfig {
        account_id: row.get(0)?,
        base_url: row.get(1)?,
        upstream_protocol: protocol,
        auth_scheme,
        created_at: parse_datetime(row.get::<_, String>(4)?),
        updated_at: parse_datetime(row.get::<_, String>(5)?),
    })
}

fn account_model_capability_from_row(row: &Row<'_>) -> rusqlite::Result<AccountModelCapability> {
    let protocol_value = row.get::<_, String>(2)?;
    let protocol = UpstreamProtocolKind::try_from(protocol_value.as_str()).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(2, Type::Text, Box::new(error))
    })?;
    Ok(AccountModelCapability {
        account_id: row.get(0)?,
        model_id: row.get(1)?,
        protocol,
        verified_at: row.get::<_, Option<String>>(3)?.map(parse_datetime),
        source: row.get(4)?,
    })
}

fn account_acknowledgement_from_row(row: &Row<'_>) -> rusqlite::Result<AccountAcknowledgement> {
    Ok(AccountAcknowledgement {
        account_id: row.get(0)?,
        acknowledgement_id: row.get(1)?,
        version: row.get(2)?,
        content_hash: row.get(3)?,
        accepted_at: parse_datetime(row.get::<_, String>(4)?),
    })
}

fn forward_log_native_from_row(row: &Row<'_>) -> rusqlite::Result<ForwardLogNativeAttribution> {
    Ok(ForwardLogNativeAttribution {
        requested_model: row.get(0)?,
        resolved_alias: row.get(1)?,
        upstream_model: row.get(2)?,
        native_cost_value: row.get(3)?,
        native_cost_unit: row.get(4)?,
        native_cost_currency: row.get(5)?,
    })
}

fn account_from_row(row: &Row<'_>) -> rusqlite::Result<Account> {
    // SELECT order: id,name,username,password,key,enabled,referral,recharge,
    // cooldown_until,generic,5h,week,month,free,last_error,created,updated,auth,type,setup,notes,
    // provider,offering,credential,quota_scope
    let created_at = row.get::<_, String>(15)?;
    let purchase_date = match row.get::<_, Option<String>>(7)? {
        Some(value) if normalize_purchase_date(&value).is_ok() => value,
        _ => migration_fallback_purchase_date(&created_at).map_err(|error| {
            rusqlite::Error::FromSqlConversionFailure(
                15,
                Type::Text,
                Box::new(std::io::Error::other(error.to_string())),
            )
        })?,
    };
    let expires_on = purchase_expires_on(&purchase_date).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(7, Type::Text, Box::new(error))
    })?;
    let account_type_value = row.get::<_, String>(18)?;
    let account_type = AccountType::try_from(account_type_value.as_str()).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(
            18,
            Type::Text,
            Box::new(std::io::Error::other(error)),
        )
    })?;
    let setup_step_value = row.get::<_, String>(19)?;
    let setup_step = AccountSetupStep::try_from(setup_step_value.as_str()).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(
            19,
            Type::Text,
            Box::new(std::io::Error::other(error)),
        )
    })?;
    let credential_value = row.get::<_, String>(23)?;
    let credential_kind = CredentialKind::try_from(credential_value.as_str()).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(
            23,
            Type::Text,
            Box::new(std::io::Error::other(error)),
        )
    })?;
    let quota_scope_value = row.get::<_, String>(24)?;
    let quota_scope = QuotaScope::try_from(quota_scope_value.as_str()).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(
            24,
            Type::Text,
            Box::new(std::io::Error::other(error)),
        )
    })?;
    Ok(Account {
        id: row.get(0)?,
        provider_id: row.get(21)?,
        offering_id: row.get(22)?,
        credential_kind,
        quota_scope,
        name: row.get(1)?,
        username: row.get(2)?,
        password_cipher: row.get(3)?,
        key_cipher: row.get(4)?,
        enabled: row.get::<_, i32>(5)? != 0,
        account_type,
        setup_step,
        referral_code: row.get(6)?,
        purchase_date,
        expires_on,
        cooldown_until: row.get::<_, Option<String>>(8)?.map(parse_datetime),
        cooldown_generic_until: row.get::<_, Option<String>>(9)?.map(parse_datetime),
        cooldown_5h_until: row.get::<_, Option<String>>(10)?.map(parse_datetime),
        cooldown_week_until: row.get::<_, Option<String>>(11)?.map(parse_datetime),
        cooldown_month_until: row.get::<_, Option<String>>(12)?.map(parse_datetime),
        cooldown_free_until: row.get::<_, Option<String>>(13)?.map(parse_datetime),
        last_error: row.get(14)?,
        auth_error: row.get(17)?,
        notes: row.get(20)?,
        created_at: parse_datetime(created_at),
        updated_at: parse_datetime(row.get::<_, String>(16)?),
    })
}

fn migration_fallback_purchase_date(created_at: &str) -> Result<String> {
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

fn parse_datetime(s: String) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(&s)
        .map(|dt| dt.with_timezone(&Utc))
        .unwrap_or_else(|e| {
            eprintln!("error: failed to parse datetime '{}': {}, using now", s, e);
            Utc::now()
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn temp_data_dir(label: &str) -> PathBuf {
        let mut dir = std::env::temp_dir();
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock should be after epoch")
            .as_nanos();
        dir.push(format!("ocg-db-test-{label}-{nanos}"));
        fs::create_dir_all(&dir).expect("test data dir should be created");
        dir
    }

    fn create_v21_fixture(dir: &Path, include_reserved_account_conflict: bool) {
        let db = Database::open(dir.to_path_buf()).expect("fixture database should open");
        db.create_account(&account("rollback-account"))
            .expect("representative account should save");
        db.log_forward(&forward_log("rollback-account", "success", 4.25))
            .expect("representative forward log should save");
        if !include_reserved_account_conflict {
            db.conn
                .execute("DELETE FROM accounts WHERE id = ?1", [ZEN_FREE_ACCOUNT_ID])
                .expect("reserved v22 account should be removed from a normal v21 fixture");
        }
        drop(db);

        let conn = Connection::open(dir.join("data.sqlite")).expect("fixture db should reopen");
        conn.execute_batch(
            "PRAGMA foreign_keys=OFF;
             DROP INDEX IF EXISTS idx_forward_logs_route_account;
             DROP INDEX IF EXISTS idx_forward_logs_provider_offering;
             DROP INDEX IF EXISTS idx_account_model_capabilities_account;
             DROP INDEX IF EXISTS idx_account_acknowledgements_account;
             DROP TABLE IF EXISTS provider_usage_sync_state;
             DROP TABLE IF EXISTS provider_pricing_snapshots;
             DROP TABLE IF EXISTS credit_balances;
             DROP TABLE IF EXISTS quota_windows;
             DROP TABLE IF EXISTS account_custom_configs;
             DROP TABLE IF EXISTS account_model_capabilities;
             DROP TABLE IF EXISTS account_acknowledgements;
             ALTER TABLE accounts DROP COLUMN verification_error;
             ALTER TABLE accounts DROP COLUMN connection_verified_at;
             ALTER TABLE accounts DROP COLUMN verification_status;
             ALTER TABLE forward_logs DROP COLUMN native_cost_currency;
             ALTER TABLE forward_logs DROP COLUMN native_cost_unit;
             ALTER TABLE forward_logs DROP COLUMN native_cost_value;
             ALTER TABLE forward_logs DROP COLUMN upstream_model;
             ALTER TABLE forward_logs DROP COLUMN resolved_alias;
             ALTER TABLE forward_logs DROP COLUMN requested_model;
             ALTER TABLE accounts DROP COLUMN free_alias_enabled;
             ALTER TABLE accounts DROP COLUMN quota_scope;
             ALTER TABLE accounts DROP COLUMN credential_kind;
             ALTER TABLE accounts DROP COLUMN offering_id;
             ALTER TABLE accounts DROP COLUMN provider_id;
             ALTER TABLE forward_logs DROP COLUMN effective_paid_cost_usd;
             ALTER TABLE forward_logs DROP COLUMN quota_debit;
             ALTER TABLE forward_logs DROP COLUMN raw_cost_usd;
             ALTER TABLE forward_logs DROP COLUMN credential_account_id;
             ALTER TABLE forward_logs DROP COLUMN offering_id;
             ALTER TABLE forward_logs DROP COLUMN provider_id;
             ALTER TABLE forward_logs DROP COLUMN route_account_id;
             DELETE FROM schema_version;
             INSERT INTO schema_version (version) VALUES (21);
             PRAGMA foreign_keys=ON;",
        )
        .expect("v21 fixture should be created");
    }

    fn create_v20_fixture(dir: &Path, include_reserved_account_conflict: bool) {
        create_v21_fixture(dir, include_reserved_account_conflict);
        let conn = Connection::open(dir.join("data.sqlite")).expect("v21 fixture should reopen");
        conn.execute_batch(
            "ALTER TABLE accounts DROP COLUMN usage_sync_last_expedited_at;
             ALTER TABLE accounts DROP COLUMN usage_sync_failure_streak;
             ALTER TABLE accounts DROP COLUMN usage_sync_next_eligible_at;
             ALTER TABLE accounts DROP COLUMN usage_sync_last_attempt_at;
             ALTER TABLE accounts DROP COLUMN usage_sync_last_success_at;
             DELETE FROM schema_version;
             INSERT INTO schema_version (version) VALUES (20);",
        )
        .expect("v20 fixture should be created");
    }

    fn pre_v22_backup_paths(dir: &Path) -> Vec<PathBuf> {
        let mut paths = fs::read_dir(dir)
            .expect("fixture directory should be readable")
            .filter_map(|entry| entry.ok())
            .map(|entry| entry.path())
            .filter(|path| {
                path.file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| {
                        name.starts_with(PRE_V22_BACKUP_FILE_PREFIX) && name.ends_with(".bak")
                    })
            })
            .collect::<Vec<_>>();
        paths.sort();
        paths
    }

    fn backup_paths_with_prefix(dir: &Path, prefix: &str) -> Vec<PathBuf> {
        let mut paths = fs::read_dir(dir)
            .expect("fixture directory should be readable")
            .filter_map(|entry| entry.ok())
            .map(|entry| entry.path())
            .filter(|path| {
                path.file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| name.starts_with(prefix) && name.ends_with(".bak"))
            })
            .collect::<Vec<_>>();
        paths.sort();
        paths
    }

    fn pre_v23_backup_paths(dir: &Path) -> Vec<PathBuf> {
        backup_paths_with_prefix(dir, PRE_V23_BACKUP_FILE_PREFIX)
    }

    fn account(id: &str) -> Account {
        Account {
            id: id.into(),
            provider_id: default_provider_id(),
            offering_id: default_offering_id(),
            credential_kind: default_credential_kind(),
            quota_scope: default_quota_scope(),
            name: id.into(),
            username: None,
            password_cipher: None,
            key_cipher: "cipher".into(),
            enabled: true,
            account_type: AccountType::Key,
            setup_step: AccountSetupStep::Ready,
            referral_code: None,
            purchase_date: String::new(),
            expires_on: String::new(),
            cooldown_until: None,
            cooldown_generic_until: None,
            cooldown_5h_until: None,
            cooldown_week_until: None,
            cooldown_month_until: None,
            cooldown_free_until: None,
            last_error: None,
            auth_error: None,
            notes: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }
    }

    fn persist_unroutable_draft(db: &Database, plan: BuiltinPlan, id: &str, notes: &str) {
        let mut draft = account(id);
        draft.provider_id = plan.offering.provider_id.to_string();
        draft.offering_id = plan.offering.offering_id.to_string();
        draft.credential_kind = plan.offering.credential_kind;
        draft.quota_scope = plan.offering.quota_scope;
        draft.enabled = false;
        draft.notes = Some(notes.to_string());
        if plan_requires_custom_config(plan) {
            db.create_account_with_contract(
                &draft,
                Some(&AccountCustomConfigInput {
                    base_url: "https://api.example.com/v1".into(),
                    upstream_protocol: UpstreamProtocolKind::ChatCompletions,
                    auth_scheme: UpstreamAuthScheme::Bearer,
                }),
                &[AccountModelCapabilityInput {
                    model_id: "org/model".into(),
                    protocol: UpstreamProtocolKind::ChatCompletions,
                    source: None,
                }],
                plan.risk_notice,
            )
            .unwrap();
        } else {
            db.create_account_with_contract(&draft, None, &[], plan.risk_notice)
                .unwrap();
        }
    }

    fn leftover_enable(db: &Database, id: &str) {
        let changed = db
            .conn
            .execute("UPDATE accounts SET enabled = 1 WHERE id = ?1", [id])
            .unwrap();
        assert_eq!(changed, 1, "{id}");
    }

    fn clone_account_row_as_enabled(
        conn: &Connection,
        source_id: &str,
        new_id: &str,
        provider_id: &str,
        offering_id: &str,
    ) {
        let mut stmt = conn.prepare("PRAGMA table_info(accounts)").unwrap();
        let columns: Vec<String> = stmt
            .query_map([], |row| row.get::<_, String>(1))
            .unwrap()
            .map(|column| column.unwrap())
            .collect();
        let select_list = columns
            .iter()
            .map(|column| match column.as_str() {
                "id" | "name" => "?1".to_string(),
                "provider_id" => "?2".to_string(),
                "offering_id" => "?3".to_string(),
                "enabled" => "1".to_string(),
                other => other.to_string(),
            })
            .collect::<Vec<_>>()
            .join(", ");
        conn.execute(
            &format!(
                "INSERT INTO accounts ({cols}) SELECT {select_list} FROM accounts WHERE id = ?4",
                cols = columns.join(", ")
            ),
            params![new_id, provider_id, offering_id, source_id],
        )
        .unwrap();
    }

    #[derive(Debug, Clone, PartialEq, Eq)]
    struct SanitationSnapshot {
        enabled: bool,
        name: String,
        notes: Option<String>,
        updated_at: DateTime<Utc>,
        verification: ConnectionVerificationStatus,
        verification_error: Option<String>,
    }

    fn sanitation_snapshot(db: &Database, id: &str) -> SanitationSnapshot {
        let account = db.get_account(id).unwrap().expect(id);
        let verification = db.account_verification_state(id).unwrap().expect(id);
        SanitationSnapshot {
            enabled: account.enabled,
            name: account.name,
            notes: account.notes,
            updated_at: account.updated_at,
            verification: verification.status,
            verification_error: verification.verification_error,
        }
    }

    fn forward_log(account_id: &str, status: &str, cost: f64) -> ForwardLog {
        ForwardLog {
            id: 0,
            timestamp: Utc::now(),
            model: "test".into(),
            account_id: account_id.into(),
            account_name: account_id.into(),
            route_account_id: None,
            provider_id: None,
            offering_id: None,
            credential_account_id: None,
            client_key_id: None,
            client_key_name: None,
            status: status.into(),
            http_status: Some(200),
            route: String::new(),
            prompt_tokens: 0,
            completion_tokens: 0,
            cached_tokens: 0,
            cache_creation_tokens: 0,
            cost: Some(cost),
            raw_cost_usd: None,
            quota_debit: None,
            effective_paid_cost_usd: None,
            pricing_revision_id: None,
            quota_multiplier: None,
            local_adjustment_multiplier: None,
            service_tier: None,
            cost_state: "legacy_estimate".into(),
            error_message: None,
            request_id: None,
            attempt: None,
            error_source: None,
            error_stage: None,
            duration_ms: None,
            diagnostic: None,
        }
    }

    #[test]
    fn v24_adds_route_column_and_historical_rows_stay_unlabeled() {
        let dir = temp_data_dir("v24-route-column");
        let db = Database::open(dir.clone()).unwrap();
        assert_eq!(db.schema_version().unwrap(), CURRENT_SCHEMA_VERSION);

        // A row written before the column existed keeps the empty default
        // ("not recorded") — insert it without naming the route column.
        db.conn
            .execute(
                "INSERT INTO forward_logs
                 (timestamp, model, account_id, account_name, status, cost_state)
                 VALUES ('2026-01-01T00:00:00Z', 'glm-5.3', 'a1', 'a1', 'success',
                         'legacy_estimate')",
                [],
            )
            .unwrap();

        let mut modern = forward_log("a1", "success", 0.5);
        modern.route = "proxy".to_string();
        modern.model = "gpt-5.6-luna".to_string();
        db.log_forward(&modern).unwrap();

        let logs = db.list_forward_logs(10).unwrap();
        assert_eq!(logs.len(), 2);
        let historical = logs.iter().find(|log| log.model == "glm-5.3").unwrap();
        assert_eq!(historical.route, "");
        let labeled = logs.iter().find(|log| log.model == "gpt-5.6-luna").unwrap();
        assert_eq!(labeled.route, "proxy");

        // The paginated query surface exposes the same column.
        let page = db
            .query_forward_logs(ForwardLogQueryOptions {
                limit: 10,
                offset: 0,
                status: None,
                account_id: None,
                provider_id: None,
                offering_id: None,
                route_account_id: None,
                credential_account_id: None,
                model: None,
                key_id: None,
                request_id: None,
                start_time: None,
                end_time: None,
                sort_by: None,
                sort_order: None,
            })
            .unwrap();
        assert_eq!(page.items.len(), 2);
        assert!(page.items.iter().all(|log| {
            (log.model == "glm-5.3" && log.route.is_empty())
                || (log.model == "gpt-5.6-luna" && log.route == "proxy")
        }));

        drop(db);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn v16_migrates_existing_accounts_to_imported_ready_keys() {
        let dir = temp_data_dir("v16-account-lifecycle");
        let path = dir.join("data.sqlite");
        let now = Utc::now().to_rfc3339();
        let conn = Connection::open(&path).expect("fixture db should open");
        conn.execute_batch(
            "CREATE TABLE schema_version (version INTEGER PRIMARY KEY);
             INSERT INTO schema_version (version) VALUES (15);
             CREATE TABLE accounts (
                 id TEXT PRIMARY KEY, name TEXT NOT NULL, username TEXT,
                 password_cipher TEXT, key_cipher TEXT NOT NULL,
                 enabled INTEGER NOT NULL DEFAULT 1, referral_code TEXT,
                 recharge_date TEXT NOT NULL, sort_order INTEGER NOT NULL DEFAULT 0,
                 cooldown_until TEXT, cooldown_generic_until TEXT,
                 cooldown_5h_until TEXT, cooldown_week_until TEXT,
                 cooldown_month_until TEXT, last_error TEXT, auth_error TEXT,
                 created_at TEXT NOT NULL, updated_at TEXT NOT NULL
             );
             CREATE TABLE forward_logs (
                 id INTEGER PRIMARY KEY, timestamp TEXT NOT NULL,
                 cost_state TEXT NOT NULL DEFAULT 'not_applicable', diagnostic_json TEXT
             );
             CREATE TABLE gateway_logs (
                 id INTEGER PRIMARY KEY, created_at TEXT NOT NULL, diagnostic_json TEXT
             );",
        )
        .expect("v15 fixture should be created");
        conn.execute(
            "INSERT INTO accounts
             (id, name, key_cipher, enabled, recharge_date, created_at, updated_at)
             VALUES ('legacy', 'Legacy', 'cipher', 1, '2026-08-01', ?1, ?1)",
            [&now],
        )
        .expect("legacy account should be inserted");
        drop(conn);

        let db = Database::open(dir.clone()).expect("v16 migration should succeed");
        let legacy = db
            .get_account("legacy")
            .expect("legacy account should load")
            .expect("legacy account should remain");
        assert_eq!(legacy.account_type, AccountType::Key);
        assert_eq!(legacy.setup_step, AccountSetupStep::Ready);
        let version: i64 = db
            .conn
            .query_row("SELECT MAX(version) FROM schema_version", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(version as i32, CURRENT_SCHEMA_VERSION);
        drop(db);
        fs::remove_dir_all(dir).expect("test data dir should be removed");
    }

    #[test]
    fn managed_setup_requires_order_and_matching_verified_key() {
        let dir = temp_data_dir("managed-setup-state");
        let db = Database::open(dir.clone()).expect("db should open");
        let mut managed = account("managed");
        managed.account_type = AccountType::Managed;
        managed.setup_step = AccountSetupStep::GoogleAccount;
        managed.key_cipher.clear();
        managed.enabled = false;
        db.create_account(&managed).expect("draft should save");

        assert!(
            !db.advance_managed_setup(
                "managed",
                AccountSetupStep::OpencodeRegistration,
                AccountSetupStep::Payment,
            )
            .unwrap()
        );
        for (from, to) in [
            (
                AccountSetupStep::GoogleAccount,
                AccountSetupStep::OpencodeRegistration,
            ),
            (
                AccountSetupStep::OpencodeRegistration,
                AccountSetupStep::Payment,
            ),
        ] {
            assert!(db.advance_managed_setup("managed", from, to).unwrap());
        }
        db.conn
            .execute(
                "UPDATE accounts SET recharge_date = '2000-01-01', usage_month_window_cost_offset = 1 WHERE id = 'managed'",
                [],
            )
            .unwrap();
        assert!(
            db.advance_managed_setup(
                "managed",
                AccountSetupStep::Payment,
                AccountSetupStep::KeyVerification,
            )
            .unwrap()
        );
        let paid = db.get_account("managed").unwrap().unwrap();
        assert_eq!(paid.purchase_date, local_today());
        let month_offset: f64 = db
            .conn
            .query_row(
                "SELECT usage_month_window_cost_offset FROM accounts WHERE id = 'managed'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(month_offset, 0.0);
        assert!(
            db.save_managed_key_for_verification("managed", "candidate")
                .unwrap()
        );
        assert!(
            !db.complete_managed_setup_if_key_matches("managed", "stale")
                .unwrap()
        );
        assert!(
            db.complete_managed_setup_if_key_matches("managed", "candidate")
                .unwrap()
        );
        let ready = db.get_account("managed").unwrap().unwrap();
        assert_eq!(ready.setup_step, AccountSetupStep::Ready);
        assert!(ready.enabled);

        drop(db);
        fs::remove_dir_all(dir).expect("test data dir should be removed");
    }

    fn forward_log_at(
        account_id: &str,
        status: &str,
        cost: f64,
        timestamp: DateTime<Utc>,
    ) -> ForwardLog {
        let mut log = forward_log(account_id, status, cost);
        log.timestamp = timestamp;
        log
    }

    fn finalize_success(db: &Database, account_id: &str, cost: f64, timestamp: DateTime<Utc>) {
        let id = db
            .log_forward(&forward_log_at(account_id, "streaming", 0.0, timestamp))
            .expect("log should insert");
        db.update_forward_log(
            id,
            "success",
            None,
            ForwardMetrics {
                cost,
                cost_state: "priced",
                ..ForwardMetrics::default()
            },
            None,
            None,
        )
        .expect("stream should finalize");
    }

    fn assert_cost(actual: f64, expected: f64) {
        assert!(
            (actual - expected).abs() < 1e-9,
            "expected {expected}, got {actual}"
        );
    }

    fn create_v6_database(
        dir: &std::path::Path,
        extra_cooldown_columns: &str,
        extra_indexes: &str,
    ) -> Connection {
        let conn = Connection::open(dir.join("data.sqlite")).expect("v6 db should open");
        conn.execute_batch(&format!(
            "CREATE TABLE schema_version (version INTEGER PRIMARY KEY);
             INSERT INTO schema_version (version) VALUES (6);
             CREATE TABLE accounts (
                 id TEXT PRIMARY KEY,
                 name TEXT NOT NULL,
                 key_cipher TEXT NOT NULL,
                 enabled INTEGER NOT NULL DEFAULT 1,
                 referral_code TEXT,
                 recharge_date TEXT,
                 created_at TEXT NOT NULL,
                 updated_at TEXT NOT NULL,
                 cooldown_until TEXT,
                 last_error TEXT,
                 username TEXT,
                 password_cipher TEXT,
                 usage_5h_baseline_percent REAL,
                 usage_5h_anchor_success_cost REAL,
                 usage_week_baseline_percent REAL,
                 usage_week_anchor_success_cost REAL,
                 usage_month_baseline_percent REAL,
                 usage_month_anchor_success_cost REAL,
                 sort_order INTEGER NOT NULL DEFAULT 0
                 {extra_cooldown_columns}
             );
             CREATE TABLE forward_logs (
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
             {extra_indexes}"
        ))
        .expect("v6 schema should be created");
        conn
    }

    #[test]
    fn v7_migration_repairs_pr11_pr12_and_combined_v6_databases() {
        let future = (Utc::now() + Duration::days(2)).to_rfc3339();
        for (label, extra_columns, extra_indexes, source_column, error) in [
            (
                "pr11-v6",
                "",
                "CREATE INDEX idx_forward_logs_model ON forward_logs(model);\nCREATE INDEX idx_forward_logs_status ON forward_logs(status);",
                "",
                "5 hour usage limit reached",
            ),
            (
                "pr12-v6",
                ", cooldown_5h_until TEXT, cooldown_week_until TEXT, cooldown_month_until TEXT",
                "",
                "cooldown_week_until",
                "weekly usage limit reached",
            ),
            (
                "combined-v6",
                ", cooldown_5h_until TEXT, cooldown_week_until TEXT, cooldown_month_until TEXT",
                "CREATE INDEX idx_forward_logs_model ON forward_logs(model);\nCREATE INDEX idx_forward_logs_status ON forward_logs(status);",
                "cooldown_month_until",
                "monthly usage limit reached",
            ),
            (
                "generic-dev-v6",
                ", cooldown_generic_until TEXT, cooldown_5h_until TEXT, cooldown_week_until TEXT, cooldown_month_until TEXT",
                "CREATE INDEX idx_forward_logs_model ON forward_logs(model);\nCREATE INDEX idx_forward_logs_status ON forward_logs(status);",
                "cooldown_generic_until",
                "unknown rate limit",
            ),
        ] {
            let dir = temp_data_dir(label);
            let conn = create_v6_database(&dir, extra_columns, extra_indexes);
            conn.execute(
                "INSERT INTO accounts
                 (id, name, key_cipher, recharge_date, created_at, updated_at, cooldown_until, last_error)
                 VALUES ('old', 'old', 'cipher', '2026-07-01', ?1, ?1, ?2, ?3)",
                params![Utc::now().to_rfc3339(), future, error],
            )
            .expect("v6 account should be inserted");
            if !source_column.is_empty() {
                conn.execute(
                    &format!("UPDATE accounts SET {source_column} = ?1 WHERE id = 'old'"),
                    [&future],
                )
                .expect("existing cooldown source should be set");
            }
            drop(conn);

            let db = Database::open(dir.clone()).expect("v6 database should migrate");
            let version: i32 = db
                .conn
                .query_row("SELECT MAX(version) FROM schema_version", [], |row| {
                    row.get(0)
                })
                .expect("schema version should load");
            assert_eq!(version, CURRENT_SCHEMA_VERSION, "{label}");
            let account = db
                .get_account("old")
                .expect("account query should work")
                .expect("account should exist");
            assert!(account.cooldown_until.is_some(), "{label}");
            assert!(account.is_cooling_at(Utc::now()), "{label}");
            let indexes: i64 = db
                .conn
                .query_row(
                    "SELECT COUNT(*) FROM sqlite_master
                     WHERE type = 'index' AND name IN (
                         'idx_forward_logs_model',
                         'idx_forward_logs_status',
                         'idx_forward_logs_time_instant'
                     )",
                    [],
                    |row| row.get(0),
                )
                .expect("indexes should be queryable");
            assert_eq!(indexes, 3, "{label}");

            drop(db);
            fs::remove_dir_all(dir).expect("test data dir should be removed");
        }
    }

    #[test]
    fn v4_migration_preserves_uncalibrated_usage() {
        let dir = temp_data_dir("v4-migration");
        let conn = Connection::open(dir.join("data.sqlite")).expect("v3 db should open");
        conn.execute_batch(
            "CREATE TABLE schema_version (version INTEGER PRIMARY KEY);
             INSERT INTO schema_version (version) VALUES (3);
             CREATE TABLE accounts (
                 id TEXT PRIMARY KEY,
                 name TEXT NOT NULL,
                 key_cipher TEXT NOT NULL,
                 enabled INTEGER NOT NULL DEFAULT 1,
                 referral_code TEXT,
                 recharge_date TEXT,
                 created_at TEXT NOT NULL,
                 updated_at TEXT NOT NULL,
                 cooldown_until TEXT,
                 last_error TEXT,
                 username TEXT,
                 password_cipher TEXT
             );
             CREATE TABLE forward_logs (
                 timestamp TEXT NOT NULL,
                 model TEXT NOT NULL DEFAULT 'test',
                 account_id TEXT NOT NULL,
                 status TEXT NOT NULL,
                 cost REAL NOT NULL DEFAULT 0
             );",
        )
        .expect("v3 schema should be created");
        let now = Utc::now().to_rfc3339();
        conn.execute(
            "INSERT INTO accounts (id, name, key_cipher, created_at, updated_at) VALUES (?1, ?1, 'cipher', ?2, ?2)",
            params!["old", now],
        )
        .expect("v3 account should be inserted");
        conn.execute(
            "INSERT INTO forward_logs (timestamp, account_id, status, cost) VALUES (?1, 'old', 'success', 2.5)",
            [Utc::now().to_rfc3339()],
        )
        .expect("v3 usage should be inserted");
        drop(conn);

        let db = Database::open(dir.clone()).expect("v3 db should migrate");
        let version: i32 = db
            .conn
            .query_row("SELECT MAX(version) FROM schema_version", [], |row| {
                row.get(0)
            })
            .expect("schema version should be readable");
        let usage = db.account_usage("old").expect("usage should load");
        assert_eq!(version, CURRENT_SCHEMA_VERSION);
        assert_eq!(
            db.get_account("old")
                .expect("account should load")
                .expect("account should exist")
                .purchase_date,
            now[..10]
        );
        assert_cost(usage.window_5h, 2.5);
        assert_cost(usage.window_week, 2.5);
        assert_cost(usage.window_month, 2.5);

        drop(db);
        fs::remove_dir_all(dir).expect("test data dir should be removed");
    }

    #[test]
    fn v5_migration_backfills_dates_and_stable_dense_order() {
        let dir = temp_data_dir("v5-migration");
        let conn = Connection::open(dir.join("data.sqlite")).expect("v4 db should open");
        conn.execute_batch(
            "CREATE TABLE schema_version (version INTEGER PRIMARY KEY);
             INSERT INTO schema_version (version) VALUES (4);
             CREATE TABLE accounts (
                 id TEXT PRIMARY KEY,
                 name TEXT NOT NULL,
                 key_cipher TEXT NOT NULL,
                 enabled INTEGER NOT NULL DEFAULT 1,
                 referral_code TEXT,
                 recharge_date TEXT,
                 created_at TEXT NOT NULL,
                 updated_at TEXT NOT NULL,
                 cooldown_until TEXT,
                 last_error TEXT,
                 username TEXT,
                 password_cipher TEXT,
                 usage_5h_baseline_percent REAL,
                 usage_5h_anchor_success_cost REAL,
                 usage_week_baseline_percent REAL,
                 usage_week_anchor_success_cost REAL,
                 usage_month_baseline_percent REAL,
                 usage_month_anchor_success_cost REAL
             );
             CREATE TABLE forward_logs (
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
             );",
        )
        .expect("v4 schema should be created");
        let shared_created_at = "2026-01-02T01:30:00+02:00";
        for (id, recharge_date, created_at) in [
            ("a", Some("2025-12-31"), shared_created_at),
            ("b", None, shared_created_at),
            ("c", Some(""), shared_created_at),
            ("d", Some("2026-2-3"), "2026-02-04T04:00:00Z"),
        ] {
            conn.execute(
                "INSERT INTO accounts
                 (id, name, key_cipher, recharge_date, created_at, updated_at)
                 VALUES (?1, ?1, 'cipher', ?2, ?3, ?3)",
                params![id, recharge_date, created_at],
            )
            .expect("v4 account should be inserted");
        }
        drop(conn);

        let db = Database::open(dir.clone()).expect("v4 db should migrate");
        let accounts = db.list_accounts().expect("migrated accounts should load");
        assert_eq!(
            accounts
                .iter()
                .map(|account| account.id.as_str())
                .collect::<Vec<_>>(),
            ["a", "b", "c", "d", ZEN_FREE_ACCOUNT_ID]
        );
        assert_eq!(accounts[0].purchase_date, "2025-12-31");
        assert_eq!(accounts[1].purchase_date, "2026-01-01");
        assert_eq!(accounts[2].purchase_date, "2026-01-01");
        assert_eq!(accounts[3].purchase_date, "2026-02-04");
        let sort_orders = db
            .conn
            .prepare("SELECT sort_order FROM accounts ORDER BY sort_order")
            .expect("sort query should prepare")
            .query_map([], |row| row.get::<_, i64>(0))
            .expect("sort query should run")
            .collect::<rusqlite::Result<Vec<_>>>()
            .expect("sort orders should load");
        assert_eq!(sort_orders, [0, 1, 2, 3, 4]);
        drop(db);

        let reopened = Database::open(dir.clone()).expect("migrated db should reopen");
        assert_eq!(
            reopened
                .list_accounts()
                .expect("reopened accounts should load")
                .iter()
                .map(|account| account.id.as_str())
                .collect::<Vec<_>>(),
            ["a", "b", "c", "d", ZEN_FREE_ACCOUNT_ID]
        );

        drop(reopened);
        fs::remove_dir_all(dir).expect("test data dir should be removed");
    }

    #[test]
    fn v8_migration_repairs_purchase_dates_written_by_older_binaries() {
        let dir = temp_data_dir("v8-purchase-date-repair");
        let conn = create_v6_database(
            &dir,
            ", cooldown_generic_until TEXT, cooldown_5h_until TEXT, cooldown_week_until TEXT, cooldown_month_until TEXT",
            "",
        );
        conn.execute("INSERT INTO schema_version (version) VALUES (7)", [])
            .expect("v7 schema version should be recorded");

        let created_at = "2026-01-02T01:30:00+02:00";
        for (id, recharge_date) in [
            ("valid", Some("2025-12-31")),
            ("null", None),
            ("invalid", Some("2026-2-3")),
        ] {
            conn.execute(
                "INSERT INTO accounts
                 (id, name, key_cipher, recharge_date, created_at, updated_at)
                 VALUES (?1, ?1, 'cipher', ?2, ?3, ?3)",
                params![id, recharge_date, created_at],
            )
            .expect("legacy account should be inserted");
        }
        drop(conn);

        let db = Database::open(dir.clone()).expect("v7 database should migrate");
        let version: i32 = db
            .conn
            .query_row("SELECT MAX(version) FROM schema_version", [], |row| {
                row.get(0)
            })
            .expect("schema version should load");
        assert_eq!(version, CURRENT_SCHEMA_VERSION);
        assert_eq!(
            db.get_account("valid")
                .expect("valid account query should work")
                .expect("valid account should exist")
                .purchase_date,
            "2025-12-31"
        );
        for id in ["null", "invalid"] {
            assert_eq!(
                db.get_account(id)
                    .expect("repaired account query should work")
                    .expect("repaired account should exist")
                    .purchase_date,
                "2026-01-01",
                "{id}"
            );
        }

        drop(db);
        fs::remove_dir_all(dir).expect("test data dir should be removed");
    }

    #[test]
    fn v9_migration_preserves_charged_legacy_errors() {
        let dir = temp_data_dir("v9-charged-error-cost");
        let conn = create_v6_database(
            &dir,
            ", cooldown_generic_until TEXT, cooldown_5h_until TEXT, cooldown_week_until TEXT, cooldown_month_until TEXT",
            "",
        );
        conn.execute("INSERT INTO schema_version (version) VALUES (7)", [])
            .expect("v7 schema version should be recorded");
        let now = Utc::now().to_rfc3339();
        conn.execute(
            "INSERT INTO accounts
             (id, name, key_cipher, recharge_date, created_at, updated_at)
             VALUES ('legacy', 'legacy', 'cipher', '2026-07-01', ?1, ?1)",
            [&now],
        )
        .expect("legacy account should be inserted");
        for (status, cost) in [("error", 1.25), ("error", 0.0), ("success", 2.0)] {
            conn.execute(
                "INSERT INTO forward_logs
                 (timestamp, model, account_id, account_name, status, http_status, cost)
                 VALUES (?1, 'glm-5.2', 'legacy', 'legacy', ?2, 200, ?3)",
                params![now, status, cost],
            )
            .expect("legacy forward log should be inserted");
        }
        drop(conn);

        let db = Database::open(dir.clone()).expect("v7 database should migrate through v10");
        let states = db
            .conn
            .prepare("SELECT status, cost, cost_state FROM forward_logs ORDER BY id")
            .expect("migrated logs should prepare")
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, f64>(1)?,
                    row.get::<_, String>(2)?,
                ))
            })
            .expect("migrated logs should query")
            .collect::<rusqlite::Result<Vec<_>>>()
            .expect("migrated logs should load");
        assert_eq!(
            states,
            [
                ("error".to_string(), 1.25, "legacy_estimate".to_string()),
                ("error".to_string(), 0.0, "not_applicable".to_string()),
                ("success".to_string(), 2.0, "legacy_estimate".to_string()),
            ]
        );
        assert_cost(
            db.account_usage("legacy")
                .expect("legacy usage should load")
                .window_month,
            3.25,
        );

        drop(db);
        fs::remove_dir_all(dir).expect("test data dir should be removed");
    }

    #[test]
    fn v10_migration_repairs_charged_errors_from_original_v9() {
        let dir = temp_data_dir("v10-repair-v9-charged-error-cost");
        let conn = create_v6_database(
            &dir,
            ", cooldown_generic_until TEXT, cooldown_5h_until TEXT, cooldown_week_until TEXT, cooldown_month_until TEXT",
            "",
        );
        conn.execute_batch(
            "CREATE TABLE pricing_snapshots (
                 revision TEXT PRIMARY KEY,
                 activated_at TEXT NOT NULL,
                 document_updated_at TEXT NOT NULL,
                 source_url TEXT NOT NULL,
                 content_hash TEXT NOT NULL,
                 snapshot_json TEXT NOT NULL
             );
             CREATE INDEX idx_pricing_snapshots_activated
                 ON pricing_snapshots(activated_at DESC);
             ALTER TABLE forward_logs ADD COLUMN pricing_revision_id TEXT;
             ALTER TABLE forward_logs ADD COLUMN quota_multiplier REAL;
             ALTER TABLE forward_logs ADD COLUMN local_adjustment_multiplier REAL;
             ALTER TABLE forward_logs ADD COLUMN cache_creation_tokens INTEGER NOT NULL DEFAULT 0;
             ALTER TABLE forward_logs ADD COLUMN service_tier TEXT;
             ALTER TABLE forward_logs ADD COLUMN cost_state TEXT NOT NULL DEFAULT 'not_applicable';
             INSERT INTO schema_version (version) VALUES (9);",
        )
        .expect("original v9 schema should be created");
        let now = Utc::now().to_rfc3339();
        conn.execute(
            "INSERT INTO accounts
             (id, name, key_cipher, recharge_date, created_at, updated_at)
             VALUES ('legacy', 'legacy', 'cipher', '2026-07-01', ?1, ?1)",
            [&now],
        )
        .expect("legacy account should be inserted");
        for (cost, cost_state) in [
            (1.25, "not_applicable"),
            (0.0, "not_applicable"),
            (4.0, "unpriced"),
        ] {
            conn.execute(
                "INSERT INTO forward_logs
                 (timestamp, model, account_id, account_name, status, http_status, cost, cost_state)
                 VALUES (?1, 'glm-5.2', 'legacy', 'legacy', 'error', 200, ?2, ?3)",
                params![now, cost, cost_state],
            )
            .expect("original v9 forward log should be inserted");
        }
        drop(conn);

        let db = Database::open(dir.clone()).expect("v9 database should migrate through v11");
        let version: i32 = db
            .conn
            .query_row("SELECT MAX(version) FROM schema_version", [], |row| {
                row.get(0)
            })
            .expect("schema version should load");
        assert_eq!(version, CURRENT_SCHEMA_VERSION);
        let states = db
            .conn
            .prepare("SELECT cost, cost_state FROM forward_logs ORDER BY id")
            .expect("migrated logs should prepare")
            .query_map([], |row| {
                Ok((row.get::<_, f64>(0)?, row.get::<_, String>(1)?))
            })
            .expect("migrated logs should query")
            .collect::<rusqlite::Result<Vec<_>>>()
            .expect("migrated logs should load");
        assert_eq!(
            states,
            [
                (1.25, "legacy_estimate".to_string()),
                (0.0, "not_applicable".to_string()),
                (4.0, "unpriced".to_string()),
            ]
        );
        assert_cost(
            db.account_usage("legacy")
                .expect("legacy usage should load")
                .window_month,
            1.25,
        );

        drop(db);
        fs::remove_dir_all(dir).expect("test data dir should be removed");
    }

    #[test]
    fn account_reads_fallback_after_v8_data_is_corrupted() {
        let dir = temp_data_dir("post-v8-purchase-date-corruption");
        let conn = create_v6_database(
            &dir,
            ", cooldown_generic_until TEXT, cooldown_5h_until TEXT, cooldown_week_until TEXT, cooldown_month_until TEXT",
            "",
        );
        conn.execute("INSERT INTO schema_version (version) VALUES (7)", [])
            .expect("v7 schema version should be recorded");
        drop(conn);

        let db = Database::open(dir.clone()).expect("database should open");
        let version: i32 = db
            .conn
            .query_row("SELECT MAX(version) FROM schema_version", [], |row| {
                row.get(0)
            })
            .expect("schema version should load");
        assert_eq!(version, CURRENT_SCHEMA_VERSION);
        let created_at = DateTime::parse_from_rfc3339("2026-01-02T01:30:00+02:00")
            .expect("fixed timestamp should parse")
            .with_timezone(&Utc);
        for id in ["null", "invalid"] {
            let mut legacy = account(id);
            legacy.purchase_date = "2025-12-31".to_string();
            legacy.created_at = created_at;
            legacy.updated_at = created_at;
            db.create_account(&legacy)
                .expect("account should be created before corruption");
        }
        // v12 重建 accounts 表后 recharge_date 是 NOT NULL（恢复 v1 原始约束），
        // 无法再被 UPDATE 成 NULL；只测试 invalid-text 这一支。
        db.conn
            .execute(
                "UPDATE accounts SET recharge_date = 'not-a-date' WHERE id = 'invalid'",
                [],
            )
            .expect("purchase date should be corrupted to invalid text");

        let accounts = db
            .list_accounts()
            .expect("one corrupt row must not break the account list");
        assert_eq!(accounts.len(), 3);
        // 仅 invalid 被破坏；null 仍持有原始 2025-12-31。
        let invalid_account = accounts
            .iter()
            .find(|a| a.id == "invalid")
            .expect("invalid account should be present");
        assert_eq!(
            invalid_account.purchase_date, "2026-01-01",
            "list_accounts should fall back to default date for corrupted rows"
        );
        let invalid = db
            .get_account("invalid")
            .expect("corrupt account query should work")
            .expect("corrupt account should exist");
        assert_eq!(invalid.purchase_date, "2026-01-01");
        assert_eq!(invalid.expires_on, "2026-02-01");
        let remains_invalid: bool = db
            .conn
            .query_row(
                "SELECT recharge_date = 'not-a-date' FROM accounts WHERE id = 'invalid'",
                [],
                |row| row.get(0),
            )
            .expect("raw purchase date should remain queryable");
        assert!(
            remains_invalid,
            "read fallback must not hide a migration rerun"
        );

        drop(db);
        fs::remove_dir_all(dir).expect("test data dir should be removed");
    }

    #[test]
    fn account_creation_defaults_dates_and_appends_to_saved_order() {
        let dir = temp_data_dir("create-order");
        let db = Database::open(dir.clone()).expect("db should open");
        let purchase_date_is_not_null: bool = db
            .conn
            .query_row(
                "SELECT [notnull]
                 FROM pragma_table_info('accounts')
                 WHERE name = 'recharge_date'",
                [],
                |row| row.get(0),
            )
            .expect("fresh account schema should expose purchase date constraints");
        assert!(purchase_date_is_not_null);
        let mut first = account("first");
        first.created_at = Utc::now() + Duration::days(1);
        db.create_account(&first)
            .expect("first account should save");
        let mut second = account("second");
        second.created_at = Utc::now() - Duration::days(1);
        second.purchase_date = "2024-01-31".to_string();
        db.create_account(&second)
            .expect("second account should save");

        let accounts = db.list_accounts().expect("accounts should load");
        assert_eq!(
            accounts
                .iter()
                .map(|account| account.id.as_str())
                .collect::<Vec<_>>(),
            [ZEN_FREE_ACCOUNT_ID, "first", "second"]
        );
        assert_eq!(accounts[1].purchase_date, local_today());
        assert_eq!(
            accounts[1].expires_on,
            purchase_expires_on(&accounts[1].purchase_date)
                .expect("default date should have an expiry")
        );
        assert_eq!(accounts[2].expires_on, "2024-02-29");

        let mut invalid = account("invalid");
        invalid.purchase_date = "2026-2-03".to_string();
        assert!(db.create_account(&invalid).is_err());
        assert!(
            db.get_account("invalid")
                .expect("invalid account lookup should work")
                .is_none()
        );

        drop(db);
        fs::remove_dir_all(dir).expect("test data dir should be removed");
    }

    #[test]
    fn zen_enabled_has_a_dedicated_writer_and_generic_update_is_rejected() {
        let dir = temp_data_dir("zen-enabled-writer");
        let db = Database::open(dir.clone()).expect("db should open");
        db.set_setting("config", r#"{"marker":"before"}"#)
            .expect("initial config should save");
        let zen_before = db
            .get_account(ZEN_FREE_ACCOUNT_ID)
            .expect("Zen lookup should work")
            .expect("Zen singleton should exist");

        let generic = AccountUpdate {
            name: None,
            username: None,
            password: None,
            key: None,
            enabled: Some(!zen_before.enabled),
            referral_code: None,
            purchase_date: None,
            notes: None,
        };
        assert!(
            db.update_account(ZEN_FREE_ACCOUNT_ID, &generic, None, None)
                .is_err(),
            "generic account writers must not bypass the Zen facade"
        );

        db.conn
            .execute_batch(&format!(
                "CREATE TRIGGER reject_zen_provider_settings
                 BEFORE UPDATE OF enabled, free_alias_enabled ON accounts
                 WHEN OLD.id = '{ZEN_FREE_ACCOUNT_ID}'
                 BEGIN
                     SELECT RAISE(ABORT, 'forced Zen settings failure');
                 END;"
            ))
            .expect("failure trigger should install");
        db.set_config(r#"{"marker":"after"}"#)
            .expect("ordinary config should save independently");
        let error = db
            .set_zen_free_enabled(!zen_before.enabled)
            .expect_err("Zen row failure must abort the config write");
        assert!(error.to_string().contains("forced Zen settings failure"));
        assert_eq!(
            db.get_setting("config").unwrap().as_deref(),
            Some(r#"{"marker":"after"}"#)
        );
        let zen_after_failure = db.get_account(ZEN_FREE_ACCOUNT_ID).unwrap().unwrap();
        assert_eq!(zen_after_failure.enabled, zen_before.enabled);

        db.conn
            .execute("DROP TRIGGER reject_zen_provider_settings", [])
            .expect("failure trigger should drop");
        db.set_zen_free_enabled(true)
            .expect("Zen enabled setting should save");
        let zen_after = db.get_account(ZEN_FREE_ACCOUNT_ID).unwrap().unwrap();
        assert!(zen_after.enabled);

        drop(db);
        fs::remove_dir_all(dir).expect("test data dir should be removed");
    }

    #[test]
    fn reorder_accounts_validates_atomically_and_persists_dense_order() {
        let dir = temp_data_dir("reorder");
        let db = Database::open(dir.clone()).expect("db should open");
        for id in ["a", "b", "c"] {
            db.create_account(&account(id))
                .expect("account should be created");
        }

        db.reorder_accounts(&[
            "c".into(),
            "a".into(),
            "b".into(),
            ZEN_FREE_ACCOUNT_ID.into(),
        ])
        .expect("valid reorder should save");
        assert_eq!(account_ids(&db), ["c", "a", "b", ZEN_FREE_ACCOUNT_ID]);

        let duplicate = db
            .reorder_accounts(&[
                "c".into(),
                "c".into(),
                "b".into(),
                ZEN_FREE_ACCOUNT_ID.into(),
            ])
            .expect_err("duplicates should fail");
        assert!(matches!(
            duplicate,
            ReorderAccountsError::DuplicateAccountId
        ));
        assert_eq!(account_ids(&db), ["c", "a", "b", ZEN_FREE_ACCOUNT_ID]);

        for stale in [
            vec!["c".into(), "a".into()],
            vec!["c".into(), "a".into(), "missing".into()],
            Vec::<String>::new(),
        ] {
            let error = db
                .reorder_accounts(&stale)
                .expect_err("stale account set should fail");
            assert!(matches!(error, ReorderAccountsError::AccountSetMismatch));
            assert_eq!(account_ids(&db), ["c", "a", "b", ZEN_FREE_ACCOUNT_ID]);
        }

        let sort_orders = db
            .conn
            .prepare("SELECT sort_order FROM accounts ORDER BY sort_order")
            .expect("sort query should prepare")
            .query_map([], |row| row.get::<_, i64>(0))
            .expect("sort query should run")
            .collect::<rusqlite::Result<Vec<_>>>()
            .expect("sort orders should load");
        assert_eq!(sort_orders, [0, 1, 2, 3]);
        drop(db);

        let reopened = Database::open(dir.clone()).expect("db should reopen");
        assert_eq!(account_ids(&reopened), ["c", "a", "b", ZEN_FREE_ACCOUNT_ID]);
        drop(reopened);

        let empty_dir = temp_data_dir("reorder-empty");
        let empty = Database::open(empty_dir.clone()).expect("empty db should open");
        empty
            .reorder_accounts(&[ZEN_FREE_ACCOUNT_ID.into()])
            .expect("the built-in Zen row is the complete empty-user order");
        drop(empty);

        fs::remove_dir_all(dir).expect("test data dir should be removed");
        fs::remove_dir_all(empty_dir).expect("empty test data dir should be removed");
    }

    #[test]
    fn reorder_accounts_rolls_back_when_an_update_fails_mid_transaction() {
        let dir = temp_data_dir("reorder-write-failure");
        let db = Database::open(dir.clone()).expect("db should open");
        for id in ["a", "b", "c"] {
            db.create_account(&account(id))
                .expect("account should be created");
        }
        db.conn
            .execute_batch(
                "CREATE TRIGGER reject_b_sort_update
                 BEFORE UPDATE OF sort_order ON accounts
                 WHEN NEW.id = 'b'
                 BEGIN
                     SELECT RAISE(ABORT, 'forced reorder failure');
                 END;",
            )
            .expect("failure trigger should be installed");

        let error = db
            .reorder_accounts(&[
                "c".into(),
                "a".into(),
                "b".into(),
                ZEN_FREE_ACCOUNT_ID.into(),
            ])
            .expect_err("the trigger should interrupt the reorder");
        assert!(matches!(error, ReorderAccountsError::Database(_)));
        assert_eq!(account_ids(&db), [ZEN_FREE_ACCOUNT_ID, "a", "b", "c"]);

        drop(db);
        fs::remove_dir_all(dir).expect("test data dir should be removed");
    }

    fn account_ids(db: &Database) -> Vec<String> {
        db.list_accounts()
            .expect("accounts should load")
            .into_iter()
            .map(|account| account.id)
            .collect()
    }

    #[test]
    fn v22_migration_failure_rolls_back_to_usable_v21_source() {
        let dir = temp_data_dir("v22-atomic-migration");
        create_v21_fixture(&dir, true);

        assert!(Database::open(dir.clone()).is_err());
        let conn = Connection::open(dir.join("data.sqlite")).expect("db should reopen");
        let columns = conn
            .prepare("PRAGMA table_info(accounts)")
            .expect("table info should prepare")
            .query_map([], |row| row.get::<_, String>(1))
            .expect("table info should query")
            .collect::<rusqlite::Result<Vec<_>>>()
            .expect("columns should load");
        let version: i32 = conn
            .query_row("SELECT MAX(version) FROM schema_version", [], |row| {
                row.get(0)
            })
            .expect("schema version should load");
        let preserved_account: (String, String, i64) = conn
            .query_row(
                "SELECT name, key_cipher, enabled FROM accounts WHERE id = 'rollback-account'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .expect("source account should remain readable");
        let preserved_log: (String, String, String, f64) = conn
            .query_row(
                "SELECT account_id, model, status, cost FROM forward_logs LIMIT 1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .expect("source forward log should remain readable");
        assert!(!columns.iter().any(|name| name == "provider_id"));
        assert_eq!(version, 21);
        assert_eq!(
            preserved_account,
            ("rollback-account".into(), "cipher".into(), 1)
        );
        assert_eq!(
            preserved_log,
            (
                "rollback-account".into(),
                "test".into(),
                "success".into(),
                4.25
            )
        );

        drop(conn);
        let backups_before = pre_v22_backup_paths(&dir);
        assert_eq!(backups_before.len(), 1);
        let backup_bytes =
            fs::read(&backups_before[0]).expect("rollback backup should be readable");
        assert!(Database::open(dir.clone()).is_err());
        assert_eq!(pre_v22_backup_paths(&dir), backups_before);
        assert_eq!(
            fs::read(&backups_before[0]).expect("rollback backup should remain readable"),
            backup_bytes
        );
        fs::remove_dir_all(dir).expect("test data dir should be removed");
    }

    #[test]
    fn v20_to_v22_failure_rolls_back_v21_and_v22_writes() {
        let dir = temp_data_dir("v20-v22-atomic-migration");
        create_v20_fixture(&dir, true);

        assert!(Database::open(dir.clone()).is_err());
        let conn = Connection::open(dir.join("data.sqlite")).expect("db should reopen");
        let columns = conn
            .prepare("PRAGMA table_info(accounts)")
            .expect("table info should prepare")
            .query_map([], |row| row.get::<_, String>(1))
            .expect("table info should query")
            .collect::<rusqlite::Result<Vec<_>>>()
            .expect("columns should load");
        assert!(
            !columns
                .iter()
                .any(|name| name == "usage_sync_last_success_at")
        );
        assert!(!columns.iter().any(|name| name == "provider_id"));
        assert_eq!(schema_version_on(&conn).unwrap(), 20);
        drop(conn);

        let backups_before = pre_v22_backup_paths(&dir);
        assert_eq!(backups_before.len(), 1);
        let backup_bytes = fs::read(&backups_before[0]).expect("backup should be readable");
        let backup =
            Connection::open_with_flags(&backups_before[0], OpenFlags::SQLITE_OPEN_READ_ONLY)
                .expect("backup should open read-only");
        assert_eq!(schema_version_on(&backup).unwrap(), 20);
        drop(backup);

        assert!(Database::open(dir.clone()).is_err());
        assert_eq!(pre_v22_backup_paths(&dir), backups_before);
        assert_eq!(
            fs::read(&backups_before[0]).expect("backup should remain readable"),
            backup_bytes
        );
        fs::remove_dir_all(dir).expect("test data dir should be removed");
    }

    #[test]
    fn clear_account_cooldown_clears_free_window() {
        let dir = temp_data_dir("clear-free-cooldown");
        let db = Database::open(dir.clone()).expect("db should open");
        db.create_account(&account("free-cd"))
            .expect("account should be created");

        let until = Utc::now() + Duration::minutes(30);
        db.set_account_rate_limit(
            "free-cd",
            until,
            r#"{"type":"FreeUsageLimitError","message":"Free usage exceeded"}"#,
            Some(UsageWindowKind::Free),
        )
        .expect("free rate limit should save");

        let cooled = db
            .get_account("free-cd")
            .expect("account should load")
            .expect("account should exist");
        assert!(cooled.cooldown_free_until.is_some());
        assert!(cooled.cooldown_until.is_some());

        db.clear_account_cooldown("free-cd")
            .expect("clear should succeed");
        let cleared = db
            .get_account("free-cd")
            .expect("account should load")
            .expect("account should exist");
        assert!(cleared.cooldown_free_until.is_none());
        assert!(cleared.cooldown_until.is_none());
        assert!(cleared.cooldown_generic_until.is_none());
        assert!(cleared.last_error.is_none());

        drop(db);
        fs::remove_dir_all(dir).expect("test data dir should be removed");
    }

    #[test]
    fn free_channel_cooldown_survives_account_deletion_restart_and_expires() {
        let dir = temp_data_dir("global-free-cooldown");
        let until = Utc::now() + Duration::minutes(30);
        {
            let mut db = Database::open(dir.clone()).expect("db should open");
            db.create_account(&account("free-source"))
                .expect("source account should be created");
            db.set_account_rate_limit(
                "free-source",
                until,
                "free quota exhausted",
                Some(UsageWindowKind::Free),
            )
            .expect("free rate limit should save");
            db.delete_account("free-source")
                .expect("source account should be deleted");
            db.create_account(&account("replacement"))
                .expect("replacement account should be created");

            assert!(db.free_channel_cooldown_until().unwrap().is_some());
        }

        let db = Database::open(dir.clone()).expect("db should reopen");
        assert!(
            db.free_channel_cooldown_until()
                .expect("global cooldown should load")
                .is_some(),
            "deleting every source row and reopening must not clear the IP-wide cooldown"
        );
        assert!(
            db.free_channel_cooldown_until_at(until + Duration::seconds(1))
                .expect("expiry should be evaluated")
                .is_none(),
            "the global gate must reopen after its deadline"
        );

        drop(db);
        fs::remove_dir_all(dir).expect("test data dir should be removed");
    }

    #[test]
    fn account_stays_cooling_until_all_windows_expire() {
        let dir = temp_data_dir("multi-window-cooldown");
        let db = Database::open(dir.clone()).expect("db should open");
        db.create_account(&account("multi"))
            .expect("account should be created");

        let now = Utc::now();
        let past_5h = now - Duration::minutes(1);
        let future_week = now + Duration::days(2);
        db.set_account_rate_limit(
            "multi",
            past_5h,
            "5-hour usage limit reached. Resets in 13min.",
            Some(UsageWindowKind::FiveHours),
        )
        .expect("5h rate limit should save");
        db.set_account_rate_limit(
            "multi",
            future_week,
            "weekly usage limit reached. Resets in 4 days.",
            Some(UsageWindowKind::Week),
        )
        .expect("weekly rate limit should save");

        let account = db
            .get_account("multi")
            .expect("account should load")
            .expect("account should exist");
        assert!(account.cooldown_5h_until.is_some_and(|until| until <= now));
        assert!(account.cooldown_week_until.is_some_and(|until| until > now));
        assert!(
            account
                .cooldown_until
                .is_some_and(|until| (until - future_week).num_seconds().abs() < 2)
        );

        drop(db);
        fs::remove_dir_all(dir).expect("test data dir should be removed");
    }

    #[test]
    fn v13_migration_preserves_legacy_manual_usage_calibration() {
        let dir = temp_data_dir("v13-legacy-calibration");
        let db = Database::open(dir.clone()).expect("db should open");
        let mut acct = account("legacy-calibration");
        acct.purchase_date = local_today();
        db.create_account(&acct).expect("account should be created");
        finalize_success(&db, "legacy-calibration", 2.0, Utc::now());
        db.conn
            .execute(
                "UPDATE accounts SET
                    usage_5h_baseline_percent = 50,
                    usage_5h_anchor_success_cost = 2,
                    usage_week_baseline_percent = 40,
                    usage_week_anchor_success_cost = 2,
                    usage_month_baseline_percent = 25,
                    usage_month_anchor_success_cost = 2
                 WHERE id = 'legacy-calibration'",
                [],
            )
            .expect("legacy baselines should save");
        finalize_success(&db, "legacy-calibration", 1.0, Utc::now());
        db.conn
            .execute_batch(
                "DELETE FROM schema_version;
                 INSERT INTO schema_version (version) VALUES (10);",
            )
            .expect("legacy schema version should save");
        drop(db);

        let db = Database::open(dir.clone()).expect("legacy database should migrate");
        let usage = db
            .account_usage("legacy-calibration")
            .expect("migrated usage should load");
        // Old effective values: 50% * 12 + 1, 40% * 30 + 1,
        // and 25% * 60 + 1. The migration must preserve all three.
        assert_cost(usage.window_5h, 7.0);
        assert_cost(usage.window_week, 13.0);
        assert_cost(usage.window_month, 16.0);

        let (version, remaining_baselines): (i32, i64) = db
            .conn
            .query_row(
                "SELECT
                    (SELECT MAX(version) FROM schema_version),
                    COUNT(*)
                 FROM accounts
                 WHERE usage_5h_baseline_percent IS NOT NULL
                    OR usage_week_baseline_percent IS NOT NULL
                    OR usage_month_baseline_percent IS NOT NULL",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .expect("migration state should load");
        assert_eq!(version, CURRENT_SCHEMA_VERSION);
        assert_eq!(remaining_baselines, 0);

        finalize_success(&db, "legacy-calibration", 2.0, Utc::now());
        let usage = db
            .account_usage("legacy-calibration")
            .expect("new usage should accumulate after migration");
        assert_cost(usage.window_5h, 9.0);
        assert_cost(usage.window_week, 15.0);
        assert_cost(usage.window_month, 18.0);

        drop(db);
        fs::remove_dir_all(dir).expect("test data dir should be removed");
    }

    #[test]
    fn v14_migrates_v13_logs_and_adds_request_id_indexes() {
        let dir = temp_data_dir("v14-log-diagnostics");
        let db = Database::open(dir.clone()).expect("db should open");
        db.conn
            .execute_batch(
                "DROP INDEX idx_forward_logs_request_id;
                 DROP INDEX idx_gateway_logs_request_id;
                 ALTER TABLE forward_logs DROP COLUMN request_id;
                 ALTER TABLE forward_logs DROP COLUMN attempt;
                 ALTER TABLE forward_logs DROP COLUMN error_source;
                 ALTER TABLE forward_logs DROP COLUMN error_stage;
                 ALTER TABLE forward_logs DROP COLUMN duration_ms;
                 ALTER TABLE forward_logs DROP COLUMN diagnostic_json;
                 ALTER TABLE gateway_logs DROP COLUMN request_id;
                 ALTER TABLE gateway_logs DROP COLUMN attempt;
                 ALTER TABLE gateway_logs DROP COLUMN error_source;
                 ALTER TABLE gateway_logs DROP COLUMN error_stage;
                 ALTER TABLE gateway_logs DROP COLUMN duration_ms;
                 ALTER TABLE gateway_logs DROP COLUMN diagnostic_json;
                 INSERT INTO forward_logs
                    (timestamp, model, account_id, account_name, status, error_message)
                 VALUES ('2026-07-01T00:00:00Z', 'legacy-model', 'legacy', 'Legacy',
                         'client_error', 'legacy error');
                 INSERT INTO gateway_logs (level, category, message, created_at)
                 VALUES ('warn', 'legacy', 'legacy gateway error', '2026-07-01T00:00:00Z');
                 DELETE FROM schema_version;
                 INSERT INTO schema_version (version) VALUES (13);",
            )
            .expect("v13 schema should be prepared");
        drop(db);

        let db = Database::open(dir.clone()).expect("v13 database should migrate");
        let version: i32 = db
            .conn
            .query_row("SELECT MAX(version) FROM schema_version", [], |row| {
                row.get(0)
            })
            .expect("schema version should load");
        assert_eq!(version, CURRENT_SCHEMA_VERSION);
        for index in ["idx_forward_logs_request_id", "idx_gateway_logs_request_id"] {
            let exists: bool = db
                .conn
                .query_row(
                    "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='index' AND name=?1)",
                    [index],
                    |row| row.get(0),
                )
                .expect("index state should load");
            assert!(exists, "{index} should exist");
        }
        let forward = db
            .query_forward_logs(ForwardLogQueryOptions {
                limit: 10,
                offset: 0,
                status: None,
                account_id: None,
                provider_id: None,
                offering_id: None,
                route_account_id: None,
                credential_account_id: None,
                model: None,
                key_id: None,
                request_id: None,
                start_time: None,
                end_time: None,
                sort_by: None,
                sort_order: None,
            })
            .expect("legacy forward log should load")
            .items
            .pop()
            .expect("legacy forward log should remain");
        assert_eq!(forward.error_message.as_deref(), Some("legacy error"));
        assert!(forward.request_id.is_none());
        assert!(forward.diagnostic.is_none());
        let gateway = db
            .list_gateway_logs(10)
            .expect("legacy gateway log should load")
            .pop()
            .expect("legacy gateway log should remain");
        assert_eq!(gateway.message, "legacy gateway error");
        assert!(gateway.request_id.is_none());
        assert!(gateway.diagnostic.is_none());

        drop(db);
        fs::remove_dir_all(dir).expect("test data dir should be removed");
    }

    #[test]
    fn v15_migration_adds_nullable_auth_error() {
        let dir = temp_data_dir("v15-auth-error");
        let conn = Connection::open(dir.join("data.sqlite")).expect("legacy db should open");
        conn.execute_batch(
            "CREATE TABLE schema_version (version INTEGER PRIMARY KEY);
             INSERT INTO schema_version (version) VALUES (14);
             CREATE TABLE accounts (id TEXT PRIMARY KEY);
             INSERT INTO accounts (id) VALUES ('legacy');
             CREATE TABLE forward_logs (
                 timestamp TEXT,
                 cost_state TEXT NOT NULL DEFAULT 'not_applicable',
                 diagnostic_json TEXT
             );
             CREATE TABLE gateway_logs (created_at TEXT, diagnostic_json TEXT);",
        )
        .expect("v14 fixture should be created");
        drop(conn);

        let db = Database::open(dir.clone()).expect("v14 database should migrate");
        let (version, auth_error): (i32, Option<String>) = db
            .conn
            .query_row(
                "SELECT (SELECT MAX(version) FROM schema_version), auth_error
                 FROM accounts WHERE id = 'legacy'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .expect("v15 migration state should load");
        assert_eq!(version, CURRENT_SCHEMA_VERSION);
        assert!(auth_error.is_none());

        drop(db);
        fs::remove_dir_all(dir).expect("test data dir should be removed");
    }

    #[test]
    fn diagnostic_retention_removes_only_old_json() {
        let dir = temp_data_dir("diagnostic-retention");
        let db = Database::open(dir.clone()).expect("db should open");
        db.conn
            .execute_batch(
                "INSERT INTO forward_logs
                    (timestamp, model, account_id, account_name, status, error_message,
                     request_id, attempt, error_source, error_stage, duration_ms, diagnostic_json)
                 VALUES
                    (datetime('now', '-31 days'), 'old', 'a', 'A', 'client_error', 'keep me',
                     'ocg-old', 1, 'upstream', 'upstream_http', 12, '{\"old\":true}'),
                    (datetime('now', '-29 days'), 'new', 'a', 'A', 'client_error', 'keep new',
                     'ocg-new', 1, 'upstream', 'upstream_http', 13, '{\"new\":true}');
                 INSERT INTO gateway_logs
                    (level, category, message, created_at, request_id, error_source,
                     error_stage, duration_ms, diagnostic_json)
                 VALUES
                    ('warn', 'gateway', 'old gateway', datetime('now', '-31 days'),
                     'ocg-gateway-old', 'client', 'parse', 5, '{\"old\":true}'),
                    ('warn', 'gateway', 'new gateway', datetime('now', '-29 days'),
                     'ocg-gateway-new', 'client', 'parse', 6, '{\"new\":true}');",
            )
            .expect("diagnostic rows should insert");
        drop(db);

        let db = Database::open(dir.clone()).expect("db reopen should apply retention");
        let (old_detail, old_id, old_error, old_source): (Option<String>, String, String, String) =
            db.conn
                .query_row(
                    "SELECT diagnostic_json, request_id, error_message, error_source
                 FROM forward_logs WHERE request_id='ocg-old'",
                    [],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
                )
                .expect("old row should remain");
        assert!(old_detail.is_none());
        assert_eq!(old_id, "ocg-old");
        assert_eq!(old_error, "keep me");
        assert_eq!(old_source, "upstream");
        let new_detail: Option<String> = db
            .conn
            .query_row(
                "SELECT diagnostic_json FROM forward_logs WHERE request_id='ocg-new'",
                [],
                |row| row.get(0),
            )
            .expect("new detail should load");
        assert!(new_detail.is_some());
        let gateway_details: (Option<String>, Option<String>) = db
            .conn
            .query_row(
                "SELECT
                    (SELECT diagnostic_json FROM gateway_logs WHERE request_id='ocg-gateway-old'),
                    (SELECT diagnostic_json FROM gateway_logs WHERE request_id='ocg-gateway-new')",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .expect("gateway details should load");
        assert!(gateway_details.0.is_none());
        assert!(gateway_details.1.is_some());

        drop(db);
        fs::remove_dir_all(dir).expect("test data dir should be removed");
    }

    #[test]
    fn fixed_window_5h_starts_at_first_success_and_expires_after_5h() {
        let dir = temp_data_dir("fixed-5h");
        let db = Database::open(dir.clone()).expect("db should open");
        db.create_account(&account("fixed"))
            .expect("account should be created");

        // 第一条成功请求落在 4h 前：固定窗口起点 = 4h 前，倒计时 ≈ 1h
        let ts1 = Utc::now() - Duration::hours(4);
        finalize_success(&db, "fixed", 1.0, ts1);
        // 窗口内的第二条请求：累加
        let ts2 = ts1 + Duration::hours(1);
        finalize_success(&db, "fixed", 2.0, ts2);

        let usage = db.account_usage("fixed").expect("usage should load");
        assert_cost(usage.window_5h, 3.0);
        let reset = usage
            .resets_in_5h
            .expect("5h window reset should be set while window is active");
        let remaining_min = (reset - Utc::now()).num_minutes();
        assert!(
            (55..=65).contains(&remaining_min),
            "expected ~60min remaining, got {remaining_min}"
        );

        drop(db);
        fs::remove_dir_all(dir).expect("test data dir should be removed");
    }

    #[test]
    fn fixed_window_treats_exact_end_as_the_next_window_start() {
        let dir = temp_data_dir("fixed-boundary");
        let db = Database::open(dir.clone()).expect("db should open");
        db.create_account(&account("boundary"))
            .expect("account should be created");

        let first = Utc::now() - Duration::hours(5) - Duration::minutes(1);
        let exact_end = first + Duration::hours(5);
        finalize_success(&db, "boundary", 10.0, first);
        finalize_success(&db, "boundary", 2.0, exact_end);

        let usage = db.account_usage("boundary").expect("usage should load");
        assert_cost(usage.window_5h, 2.0);
        assert!(usage.resets_in_5h.is_some());

        drop(db);
        fs::remove_dir_all(dir).expect("test data dir should be removed");
    }

    #[test]
    fn fixed_window_5h_rebuilds_after_expiry_when_new_request_arrives() {
        let dir = temp_data_dir("fixed-5h-rebuild");
        let db = Database::open(dir.clone()).expect("db should open");
        db.create_account(&account("rebuild"))
            .expect("account should be created");

        // 6h 前的第一条请求：窗口已过期
        let ts1 = Utc::now() - Duration::hours(6);
        finalize_success(&db, "rebuild", 10.0, ts1);
        // 1h 前的第二条请求：触发新窗口
        let ts2 = Utc::now() - Duration::hours(1);
        finalize_success(&db, "rebuild", 5.0, ts2);

        let usage = db.account_usage("rebuild").expect("usage should load");
        // 新窗口只包含 ts2 之后：10 已被丢弃，只剩 5
        assert_cost(usage.window_5h, 5.0);
        let reset = usage
            .resets_in_5h
            .expect("5h window reset should be set after rebuild");
        let remaining_min = (reset - Utc::now()).num_minutes();
        assert!(
            (235..=245).contains(&remaining_min),
            "expected ~240min remaining, got {remaining_min}"
        );

        drop(db);
        fs::remove_dir_all(dir).expect("test data dir should be removed");
    }

    #[test]
    fn fixed_window_5h_advances_through_multiple_expired_windows_in_one_call() {
        // 复现用户报告的"刷新递减"循环 bug：
        //   4 条间隔 6h 的计费日志（全部已过期）。
        // 旧实现每次刷新只前进一个窗口，前端可见 60→30→13→5.8→0→60+ 循环；
        // 修复后一次调用内连过 4 个过期窗口，next=None 时清空并返回 0，
        // 第二次刷新仍为 0，不再回到最旧日志。
        let dir = temp_data_dir("fixed-5h-multi-expired");
        let db = Database::open(dir.clone()).expect("db should open");
        db.create_account(&account("cycle"))
            .expect("account should be created");

        // ts1 = -24h, ts2 = -18h, ts3 = -12h, ts4 = -6h：每条间隔 6h（> 5h 窗口长度）。
        let ts1 = Utc::now() - Duration::hours(24);
        let ts2 = ts1 + Duration::hours(6);
        let ts3 = ts2 + Duration::hours(6);
        let ts4 = ts3 + Duration::hours(6);
        finalize_success(&db, "cycle", 10.0, ts1);
        finalize_success(&db, "cycle", 5.0, ts2);
        finalize_success(&db, "cycle", 3.0, ts3);
        finalize_success(&db, "cycle", 2.0, ts4);

        // 第一次刷新：应直接走完所有过期窗口，返回 0（无新请求）。
        let usage = db.account_usage("cycle").expect("usage should load");
        assert_cost(usage.window_5h, 0.0);
        assert!(
            usage.resets_in_5h.is_none(),
            "no active window after all expired; resets_in_5h should be None"
        );

        // 第二次刷新：不应回到最旧日志循环重放，仍稳定为 0。
        let usage2 = db.account_usage("cycle").expect("usage should load again");
        assert_cost(usage2.window_5h, 0.0);
        assert!(usage2.resets_in_5h.is_none());

        drop(db);
        fs::remove_dir_all(dir).expect("test data dir should be removed");
    }

    #[test]
    fn fixed_window_5h_finds_active_window_after_multiple_expired() {
        // 多条已过期日志后跟一条近期日志：修复后第一次刷新就应落在有效窗口上，
        // 而不是停在某个过期窗口里返回错误的中间值。
        let dir = temp_data_dir("fixed-5h-active-after-expired");
        let db = Database::open(dir.clone()).expect("db should open");
        db.create_account(&account("active"))
            .expect("account should be created");

        // 三条过期日志间隔 6h，再加一条 1h 前的近期日志。
        let ts1 = Utc::now() - Duration::hours(19);
        let ts2 = ts1 + Duration::hours(6); // -13h
        let ts3 = ts2 + Duration::hours(6); // -7h，仍过期
        let ts4 = Utc::now() - Duration::hours(1); // 近期，落在有效窗口内
        finalize_success(&db, "active", 10.0, ts1);
        finalize_success(&db, "active", 5.0, ts2);
        finalize_success(&db, "active", 3.0, ts3);
        finalize_success(&db, "active", 2.0, ts4);

        // 第一次刷新：连过 3 个过期窗口，落在 ts4 上，只算 ts4 之后的 cost = 2.0。
        let usage = db.account_usage("active").expect("usage should load");
        assert_cost(usage.window_5h, 2.0);
        let reset = usage
            .resets_in_5h
            .expect("5h window reset should be anchored at ts4");
        let remaining_min = (reset - Utc::now()).num_minutes();
        assert!(
            (235..=245).contains(&remaining_min),
            "expected ~240min remaining (anchored at ts4 = now - 1h), got {remaining_min}"
        );

        drop(db);
        fs::remove_dir_all(dir).expect("test data dir should be removed");
    }

    #[test]
    fn fixed_window_5h_with_no_usage_returns_zero_and_full_window_remaining() {
        let dir = temp_data_dir("fixed-5h-empty");
        let db = Database::open(dir.clone()).expect("db should open");
        db.create_account(&account("empty"))
            .expect("account should be created");

        let usage = db.account_usage("empty").expect("usage should load");
        assert_cost(usage.window_5h, 0.0);
        // 没用过：倒计时为 None（前端显示"5h0min"由默认值决定）
        assert!(usage.resets_in_5h.is_none());

        drop(db);
        fs::remove_dir_all(dir).expect("test data dir should be removed");
    }

    #[test]
    fn month_window_accumulates_from_purchase_date_to_expires_on() {
        let dir = temp_data_dir("month-window");
        let db = Database::open(dir.clone()).expect("db should open");
        let mut acct = account("monthly");
        acct.purchase_date = "2026-07-01".into();
        db.create_account(&acct).expect("account should be created");

        // 模拟一条历史成功请求（任何时间都算，月窗口从 purchase_date 累计）
        finalize_success(&db, "monthly", 5.0, Utc::now());

        let usage = db.account_usage("monthly").expect("usage should load");
        assert_cost(usage.window_month, 5.0);
        let reset = usage
            .resets_in_month
            .expect("month window reset should be purchase_date + 1 month");
        // 2026-07-01 + 1 自然月 = 2026-08-01 00:00
        let expected = DateTime::parse_from_rfc3339("2026-08-01T00:00:00+00:00")
            .unwrap()
            .with_timezone(&Utc);
        assert!(
            (reset - expected).num_seconds().abs() < 86400,
            "expected ~2026-08-01, got {reset}"
        );

        drop(db);
        fs::remove_dir_all(dir).expect("test data dir should be removed");
    }

    #[test]
    fn manual_calibrate_5h_window_sets_started_at_and_cost_offset() {
        let dir = temp_data_dir("calibrate-5h");
        let db = Database::open(dir.clone()).expect("db should open");
        db.create_account(&account("calib"))
            .expect("account should be created");

        // 用户在别处已用 50%，距上游重置还剩 3 小时
        db.calibrate_account_usage("calib", UsageWindowKind::FiveHours, 50.0, Some(180), 12.0)
            .expect("calibrate should save");

        let usage = db.account_usage("calib").expect("usage should load");
        // 5h 限额 12.0，50% = 6.0
        assert_cost(usage.window_5h, 6.0);
        let reset = usage
            .resets_in_5h
            .expect("5h window reset should be set after manual calibrate");
        let remaining_min = (reset - Utc::now()).num_minutes();
        assert!(
            (175..=185).contains(&remaining_min),
            "expected ~180min remaining, got {remaining_min}"
        );

        // 后续网关内的请求累加到偏移之上
        finalize_success(&db, "calib", 1.0, Utc::now());
        let usage = db.account_usage("calib").expect("usage should reload");
        assert_cost(usage.window_5h, 7.0);

        drop(db);
        fs::remove_dir_all(dir).expect("test data dir should be removed");
    }

    #[test]
    fn calibrate_subtracts_existing_window_usage_from_offset() {
        // 回归测试：活跃账号（窗口内已有 forward_logs）校准时，
        // offset 必须 = target_cost - actual_cost，否则 compute_fixed_window
        // 返回 offset + actual_cost，显示百分比会高于用户输入。
        let dir = temp_data_dir("calibrate-with-usage");
        let db = Database::open(dir.clone()).expect("db should open");
        db.create_account(&account("active"))
            .expect("account should be created");

        // 1 小时前已用 $3（落在 5h 窗口内）
        let ts = Utc::now() - Duration::hours(1);
        finalize_success(&db, "active", 3.0, ts);

        // 用户说"我在别处用到了 50%"（5h 限额 12.0 → target_cost = 6.0）
        // 期望：offset = 6.0 - 3.0 = 3.0，compute_fixed_window 返回 3.0 + 3.0 = 6.0 = 50%
        // 修复前 bug：offset = 6.0，compute_fixed_window 返回 6.0 + 3.0 = 9.0 = 75%
        // 用 resets_in_minutes=180 让新窗口的 started_at = now + 3h - 5h = now - 2h，
        // 把 1 小时前的 log 稳稳包含进窗口（避开 finalize 与 calibrate 之间的微秒级时序差）。
        db.calibrate_account_usage("active", UsageWindowKind::FiveHours, 50.0, Some(180), 12.0)
            .expect("calibrate should save with existing usage");
        let usage = db.account_usage("active").expect("usage should load");
        assert_cost(usage.window_5h, 6.0);

        // 后续请求继续累加：offset=3.0 + actual=3.0 + new=2.0 = 8.0
        finalize_success(&db, "active", 2.0, Utc::now());
        let usage = db.account_usage("active").expect("usage should reload");
        assert_cost(usage.window_5h, 8.0);

        drop(db);
        fs::remove_dir_all(dir).expect("test data dir should be removed");
    }

    #[test]
    fn calibrate_below_actual_usage_allows_negative_offset() {
        // 回归测试（Bug 1.5）：用户校准的百分比低于窗口内实际 cost 时，offset 允许为负数，
        // 让 compute_fixed_window 返回 offset + actual = target_cost，与用户输入一致。
        // 之前 max(0, target - actual) 钳制 + schema CHECK (offset >= 0) 约束让向左拉
        // 滑块时被锁死在实际 cost 对应的百分比（9.0 / 12.0 * 100 = 75%，对应用户看到的 40.2%）。
        let dir = temp_data_dir("calibrate-below-usage");
        let db = Database::open(dir.clone()).expect("db should open");
        db.create_account(&account("clamp"))
            .expect("account should be created");

        // 已用 $9
        let ts = Utc::now() - Duration::hours(1);
        finalize_success(&db, "clamp", 9.0, ts);

        // 用户校准到 20%（target_cost = 2.4，但实际已用 9.0）
        // offset = 2.4 - 9.0 = -6.6；compute_fixed_window 返回 -6.6 + 9.0 = 2.4 = 20%。
        // 用 resets_in_minutes=180 让新窗口的 started_at = now - 2h，把 1 小时前的
        // $9 log 稳稳包含进窗口（避开 finalize 与 calibrate 之间的微秒级时序差）。
        db.calibrate_account_usage("clamp", UsageWindowKind::FiveHours, 20.0, Some(180), 12.0)
            .expect("calibrate below actual usage should allow negative offset");
        let usage = db.account_usage("clamp").expect("usage should load");
        // 显示的 cost = offset(-6.6) + actual(9.0) = 2.4（用户输入的 20%）
        assert_cost(usage.window_5h, 2.4);

        drop(db);
        fs::remove_dir_all(dir).expect("test data dir should be removed");
    }

    #[test]
    fn calibrate_month_window_writes_offset_without_started_at() {
        // 回归测试（Bug 2）：月窗口必须支持手动校准。
        // 月窗口不写 started_at 列（起点固定为 purchase_date），只更新 cost_offset。
        // resets_in_minutes 被忽略——窗口由 purchase_date/expires_on 决定。
        let dir = temp_data_dir("calibrate-month");
        let db = Database::open(dir.clone()).expect("db should open");
        let mut acct = account("monthly-calib");
        acct.purchase_date = "2026-07-01".into();
        db.create_account(&acct).expect("account should be created");

        // 已用 $5（落在月窗口内：purchase_date 00:00 起）
        finalize_success(&db, "monthly-calib", 5.0, Utc::now());

        // 用户校准到 50%（月限额 100.0 → target_cost = 50.0）
        // 期望：offset = 50.0 - 5.0 = 45.0；compute_month_window 返回 45.0 + 5.0 = 50.0 = 50%。
        db.calibrate_account_usage("monthly-calib", UsageWindowKind::Month, 50.0, None, 100.0)
            .expect("month window calibrate should save");
        let usage = db
            .account_usage("monthly-calib")
            .expect("usage should load");
        assert_cost(usage.window_month, 50.0);
        // resets_in_month 仍是 purchase_date + 1 自然月（不受 resets_in_minutes 影响）
        let reset = usage
            .resets_in_month
            .expect("month window reset should be purchase_date + 1 month");
        let expected = DateTime::parse_from_rfc3339("2026-08-01T00:00:00+00:00")
            .unwrap()
            .with_timezone(&Utc);
        assert!(
            (reset - expected).num_seconds().abs() < 86400,
            "expected ~2026-08-01, got {reset}"
        );

        drop(db);
        fs::remove_dir_all(dir).expect("test data dir should be removed");
    }

    #[test]
    fn changing_purchase_date_resets_month_calibration_offset() {
        let dir = temp_data_dir("month-renewal-reset");
        let db = Database::open(dir.clone()).expect("db should open");
        let new_purchase_date = local_today();
        let old_purchase_date = (Local::now().date_naive() - Duration::days(10))
            .format("%Y-%m-%d")
            .to_string();
        let mut acct = account("monthly-renewal");
        acct.purchase_date = old_purchase_date;
        db.create_account(&acct).expect("account should be created");

        finalize_success(&db, "monthly-renewal", 5.0, Utc::now() - Duration::days(2));
        db.calibrate_account_usage("monthly-renewal", UsageWindowKind::Month, 0.0, None, 100.0)
            .expect("month calibration should save a negative offset");
        assert_cost(
            db.account_usage("monthly-renewal")
                .expect("usage should load")
                .window_month,
            0.0,
        );

        db.update_account(
            "monthly-renewal",
            &AccountUpdate {
                name: None,
                username: None,
                password: None,
                key: None,
                enabled: None,
                referral_code: None,
                purchase_date: Some(new_purchase_date),
                notes: None,
            },
            None,
            None,
        )
        .expect("purchase date should update");
        let offset: f64 = db
            .conn
            .query_row(
                "SELECT usage_month_window_cost_offset FROM accounts WHERE id = ?1",
                ["monthly-renewal"],
                |row| row.get(0),
            )
            .expect("month offset should load");
        assert_cost(offset, 0.0);
        assert_cost(
            db.account_usage("monthly-renewal")
                .expect("renewed usage should load")
                .window_month,
            0.0,
        );

        finalize_success(&db, "monthly-renewal", 2.0, Utc::now());
        assert_cost(
            db.account_usage("monthly-renewal")
                .expect("new cycle usage should load")
                .window_month,
            2.0,
        );

        drop(db);
        fs::remove_dir_all(dir).expect("test data dir should be removed");
    }

    #[test]
    fn replacing_key_clears_auth_error_but_other_updates_preserve_it() {
        let dir = temp_data_dir("auth-error-key-replacement");
        let db = Database::open(dir.clone()).expect("db should open");
        db.create_account(&account("auth-failed"))
            .expect("account should be created");
        let old_key_cipher = db
            .get_account("auth-failed")
            .expect("account should load")
            .expect("account should exist")
            .key_cipher;
        db.set_account_auth_error("auth-failed", Some("upstream auth error 401"))
            .expect("auth error should save");

        let rename = AccountUpdate {
            name: Some("renamed".into()),
            username: None,
            password: None,
            key: None,
            enabled: None,
            referral_code: None,
            purchase_date: None,
            notes: None,
        };
        db.update_account("auth-failed", &rename, None, None)
            .expect("non-key update should save");
        assert!(
            db.get_account("auth-failed")
                .expect("account should load")
                .expect("account should exist")
                .auth_error
                .is_some()
        );

        let no_fields = AccountUpdate {
            name: None,
            username: None,
            password: None,
            key: None,
            enabled: None,
            referral_code: None,
            purchase_date: None,
            notes: None,
        };
        db.update_account("auth-failed", &no_fields, Some("replacement-cipher"), None)
            .expect("key replacement should save");
        assert!(
            db.get_account("auth-failed")
                .expect("account should load")
                .expect("account should exist")
                .auth_error
                .is_none()
        );

        assert!(
            !db.set_account_auth_error_if_key_matches(
                "auth-failed",
                &old_key_cipher,
                Some("late old-key 401"),
            )
            .expect("stale auth response should be ignored")
        );
        assert!(
            db.get_account("auth-failed")
                .expect("account should load")
                .expect("account should exist")
                .auth_error
                .is_none(),
            "a delayed 401 from the old key must not break its replacement"
        );

        assert!(
            db.set_account_auth_error_if_key_matches(
                "auth-failed",
                "replacement-cipher",
                Some("new-key auth error"),
            )
            .expect("current-key auth response should save")
        );
        assert!(
            !db.set_account_auth_error_if_key_matches("auth-failed", &old_key_cipher, None)
                .expect("stale success response should be ignored")
        );
        assert_eq!(
            db.get_account("auth-failed")
                .expect("account should load")
                .expect("account should exist")
                .auth_error
                .as_deref(),
            Some("new-key auth error"),
            "a delayed success from the old key must not recover its replacement"
        );
        assert!(
            db.set_account_auth_error_if_key_matches("auth-failed", "replacement-cipher", None)
                .expect("current-key success should clear auth state")
        );

        let stale_cooldown = Utc::now() + Duration::days(3);
        assert!(
            !db.set_account_rate_limit_if_key_matches(
                "auth-failed",
                &old_key_cipher,
                stale_cooldown,
                "late old-key 429",
                None,
            )
            .expect("stale rate limit should be ignored")
        );
        let stored = db
            .get_account("auth-failed")
            .expect("account should load")
            .expect("account should exist");
        assert!(stored.cooldown_until.is_none());
        assert!(stored.last_error.is_none());

        drop(db);
        fs::remove_dir_all(dir).expect("test data dir should be removed");
    }

    #[test]
    fn calibrate_rejects_reset_outside_fixed_window_without_panicking() {
        let dir = temp_data_dir("calibrate-reset-bounds");
        let db = Database::open(dir.clone()).expect("db should open");
        db.create_account(&account("reset-bounds"))
            .expect("account should be created");

        for (window, minutes) in [
            (UsageWindowKind::FiveHours, -1),
            (UsageWindowKind::FiveHours, 301),
            (UsageWindowKind::Week, 10_081),
            (UsageWindowKind::FiveHours, i64::MAX),
        ] {
            assert!(
                db.calibrate_account_usage("reset-bounds", window, 50.0, Some(minutes), 100.0,)
                    .is_err(),
                "{window:?} should reject {minutes} minutes"
            );
        }

        drop(db);
        fs::remove_dir_all(dir).expect("test data dir should be removed");
    }

    fn snapshot_limits() -> PricingLimits {
        PricingLimits {
            window_5h: 12.0,
            window_week: 30.0,
            window_month: 100.0,
        }
    }

    fn usage_calibration(
        rolling_percent: f64,
        weekly_percent: f64,
        monthly_percent: f64,
        rolling_resets_in_minutes: i64,
        weekly_resets_in_minutes: i64,
    ) -> AccountUsageCalibrationSnapshot {
        AccountUsageCalibrationSnapshot {
            rolling_percent,
            weekly_percent,
            monthly_percent,
            rolling_resets_in_minutes,
            weekly_resets_in_minutes,
        }
    }

    fn usage_offset_row(
        db: &Database,
        id: &str,
    ) -> (Option<String>, f64, Option<String>, f64, f64) {
        db.conn
            .query_row(
                "SELECT usage_5h_window_started_at, usage_5h_window_cost_offset,
                        usage_week_window_started_at, usage_week_window_cost_offset,
                        usage_month_window_cost_offset
                 FROM accounts WHERE id = ?1",
                [id],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                    ))
                },
            )
            .expect("usage offset row should load")
    }

    #[test]
    fn calibrate_account_usage_snapshot_updates_all_three_windows() {
        let dir = temp_data_dir("calibrate-snapshot-ok");
        let db = Database::open(dir.clone()).expect("db should open");
        let mut acct = account("snap-ok");
        acct.purchase_date = "2026-07-01".into();
        db.create_account(&acct).expect("account should be created");
        finalize_success(&db, "snap-ok", 3.0, Utc::now() - Duration::hours(1));

        let limits = snapshot_limits();
        let usage = db
            .calibrate_account_usage_snapshot(
                "snap-ok",
                &usage_calibration(50.0, 20.0, 10.0, 180, 1_440),
                &limits,
            )
            .expect("snapshot calibrate should save");
        assert_cost(usage.window_5h, 6.0);
        assert_cost(usage.window_week, 6.0);
        assert_cost(usage.window_month, 10.0);
        let remaining_5h =
            (usage.resets_in_5h.expect("5h reset should be set") - Utc::now()).num_minutes();
        assert!(
            (175..=185).contains(&remaining_5h),
            "expected ~180min remaining, got {remaining_5h}"
        );
        let remaining_week =
            (usage.resets_in_week.expect("week reset should be set") - Utc::now()).num_minutes();
        assert!(
            (1_435..=1_445).contains(&remaining_week),
            "expected ~1440min remaining, got {remaining_week}"
        );

        drop(db);
        fs::remove_dir_all(dir).expect("test data dir should be removed");
    }

    #[test]
    fn official_usage_sync_rolls_back_baseline_when_success_metadata_fails() {
        let dir = temp_data_dir("official-sync-atomic-failure");
        let db = Database::open(dir.clone()).expect("db should open");
        let mut acct = account("atomic-sync");
        acct.purchase_date = "2026-07-01".into();
        db.create_account(&acct).expect("account should be created");
        let limits = snapshot_limits();
        db.calibrate_account_usage_snapshot(
            "atomic-sync",
            &usage_calibration(10.0, 20.0, 30.0, 120, 1_200),
            &limits,
        )
        .expect("initial baseline should save");
        let previous_success = Utc::now() - Duration::hours(2);
        db.record_account_usage_sync_success(
            "atomic-sync",
            previous_success,
            previous_success + Duration::hours(24),
            false,
        )
        .expect("initial sync metadata should save");
        let before = db
            .account_usage_with_limits("atomic-sync", &limits)
            .expect("initial usage should load");
        let sync_before = db
            .account_usage_sync_state("atomic-sync")
            .expect("initial sync state should load")
            .expect("sync state should exist");

        db.conn
            .execute_batch(
                "CREATE TRIGGER fail_official_sync_metadata
                 BEFORE UPDATE OF last_success_at ON provider_usage_sync_state
                 WHEN NEW.account_id = 'atomic-sync'
                 BEGIN
                    SELECT RAISE(ABORT, 'forced usage sync metadata failure');
                 END;",
            )
            .expect("failure trigger should install");

        let now = Utc::now();
        let result = db.commit_official_usage_sync_success(
            "atomic-sync",
            "cipher",
            &usage_calibration(80.0, 70.0, 60.0, 180, 1_440),
            &limits,
            AccountUsageSyncSuccessMetadata {
                now,
                next_eligible_at: now + Duration::hours(1),
                mark_expedited: true,
            },
        );
        assert!(
            result.is_err(),
            "forced metadata failure must abort the sync"
        );

        let after = db
            .account_usage_with_limits("atomic-sync", &limits)
            .expect("usage should remain readable");
        assert_cost(after.window_5h, before.window_5h);
        assert_cost(after.window_week, before.window_week);
        assert_cost(after.window_month, before.window_month);
        let sync_after = db
            .account_usage_sync_state("atomic-sync")
            .expect("sync state should load")
            .expect("sync state should exist");
        assert_eq!(sync_after.last_success_at, sync_before.last_success_at);
        assert_eq!(sync_after.next_eligible_at, sync_before.next_eligible_at);
        assert_eq!(sync_after.failure_streak, sync_before.failure_streak);
        assert_eq!(sync_after.last_expedited_at, sync_before.last_expedited_at);

        drop(db);
        fs::remove_dir_all(dir).expect("test data dir should be removed");
    }

    #[test]
    fn calibrate_account_usage_snapshot_rolls_back_when_second_window_fails() {
        let dir = temp_data_dir("calibrate-snapshot-week-fail");
        let db = Database::open(dir.clone()).expect("db should open");
        let mut acct = account("snap-week");
        acct.purchase_date = "2026-07-01".into();
        db.create_account(&acct).expect("account should be created");
        let limits = snapshot_limits();
        db.calibrate_account_usage_snapshot(
            "snap-week",
            &usage_calibration(10.0, 20.0, 30.0, 100, 200),
            &limits,
        )
        .expect("initial snapshot should save");
        let before = usage_offset_row(&db, "snap-week");
        let before_usage = db
            .account_usage_with_limits("snap-week", &limits)
            .expect("usage should load");

        assert!(
            db.calibrate_account_usage_snapshot(
                "snap-week",
                &usage_calibration(80.0, 90.0, 40.0, 180, 10_081),
                &limits
            )
            .is_err(),
            "weekly minutes outside the 7-day window should fail"
        );

        assert_eq!(usage_offset_row(&db, "snap-week"), before);
        let after = db
            .account_usage_with_limits("snap-week", &limits)
            .expect("usage should reload");
        assert_cost(after.window_5h, before_usage.window_5h);
        assert_cost(after.window_week, before_usage.window_week);
        assert_cost(after.window_month, before_usage.window_month);

        drop(db);
        fs::remove_dir_all(dir).expect("test data dir should be removed");
    }

    #[test]
    fn calibrate_account_usage_snapshot_rolls_back_when_third_window_fails() {
        let dir = temp_data_dir("calibrate-snapshot-month-fail");
        let db = Database::open(dir.clone()).expect("db should open");
        let mut acct = account("snap-month");
        acct.purchase_date = "2026-07-01".into();
        db.create_account(&acct).expect("account should be created");
        let limits = snapshot_limits();
        db.calibrate_account_usage_snapshot(
            "snap-month",
            &usage_calibration(10.0, 20.0, 30.0, 100, 200),
            &limits,
        )
        .expect("initial snapshot should save");
        let before = usage_offset_row(&db, "snap-month");
        let before_usage = db
            .account_usage_with_limits("snap-month", &limits)
            .expect("usage should load");

        db.conn
            .execute_batch(
                "CREATE TRIGGER reject_month_calibrate
                 BEFORE UPDATE OF usage_month_window_cost_offset ON accounts
                 WHEN NEW.id = 'snap-month'
                 BEGIN
                     SELECT RAISE(ABORT, 'forced month calibrate failure');
                 END;",
            )
            .expect("failure trigger should be installed");

        assert!(
            db.calibrate_account_usage_snapshot(
                "snap-month",
                &usage_calibration(80.0, 90.0, 40.0, 180, 1_440),
                &limits
            )
            .is_err(),
            "month window trigger should fail the transaction"
        );

        assert_eq!(usage_offset_row(&db, "snap-month"), before);
        let after = db
            .account_usage_with_limits("snap-month", &limits)
            .expect("usage should reload");
        assert_cost(after.window_5h, before_usage.window_5h);
        assert_cost(after.window_week, before_usage.window_week);
        assert_cost(after.window_month, before_usage.window_month);

        drop(db);
        fs::remove_dir_all(dir).expect("test data dir should be removed");
    }

    #[test]
    fn soonest_reset_is_minimum_of_each_accounts_latest_active_cooldown() {
        let dir = temp_data_dir("soonest-account-reset");
        let db = Database::open(dir.clone()).expect("db should open");
        for id in ["first", "second"] {
            db.create_account(&account(id))
                .expect("account should be created");
        }

        let now = Utc::now();
        let first_early = now + Duration::hours(1);
        let first_latest = now + Duration::hours(4);
        let second_latest = now + Duration::hours(2);
        db.set_account_rate_limit(
            "first",
            first_early,
            "5-hour usage limit reached",
            Some(UsageWindowKind::FiveHours),
        )
        .expect("first short cooldown should save");
        db.set_account_rate_limit(
            "first",
            first_latest,
            "weekly usage limit reached",
            Some(UsageWindowKind::Week),
        )
        .expect("first long cooldown should save");
        db.set_account_rate_limit("second", second_latest, "unknown rate limit", None)
            .expect("second cooldown should save");

        let reset = db
            .soonest_cooldown_reset()
            .expect("reset query should work")
            .expect("a reset should exist");
        assert!((reset - second_latest).num_seconds().abs() < 2);

        db.set_account_auth_error("second", Some("upstream auth error 401"))
            .expect("auth breaker should save");
        let reset = db
            .soonest_cooldown_reset()
            .expect("reset query should work")
            .expect("an eligible reset should exist");
        assert!((reset - first_latest).num_seconds().abs() < 2);

        drop(db);
        fs::remove_dir_all(dir).expect("test data dir should be removed");
    }

    #[test]
    fn forward_log_time_filter_compares_rfc3339_offsets_by_instant() {
        let dir = temp_data_dir("forward-log-offset-filter");
        let db = Database::open(dir.clone()).expect("db should open");
        db.conn
            .execute(
                "INSERT INTO forward_logs
                 (timestamp, model, account_id, account_name, status, cost)
                 VALUES (?1, 'inside', 'a', 'a', 'success', 1)",
                ["2026-07-17T04:15:00Z"],
            )
            .expect("inside log should save");
        db.conn
            .execute(
                "INSERT INTO forward_logs
                 (timestamp, model, account_id, account_name, status, cost)
                 VALUES (?1, 'outside', 'a', 'a', 'success', 2)",
                ["2026-07-17T03:30:00Z"],
            )
            .expect("outside log should save");

        let page = db
            .query_forward_logs(ForwardLogQueryOptions {
                limit: 20,
                offset: 0,
                status: None,
                account_id: None,
                provider_id: None,
                offering_id: None,
                route_account_id: None,
                credential_account_id: None,
                model: None,
                key_id: None,
                request_id: None,
                start_time: Some("2026-07-17T12:00:00+08:00"),
                end_time: Some("2026-07-17T12:30:00+08:00"),
                sort_by: Some("cost"),
                sort_order: Some("asc"),
            })
            .expect("offset filter should query");
        assert_eq!(page.summary.total_requests, 1);
        assert_eq!(page.items.len(), 1);
        assert_eq!(page.items[0].model, "inside");

        drop(db);
        fs::remove_dir_all(dir).expect("test data dir should be removed");
    }

    #[test]
    fn forward_logs_can_sort_by_attempt() {
        let dir = temp_data_dir("forward-log-attempt-sort");
        let db = Database::open(dir.clone()).expect("db should open");
        for attempt in [2, 1] {
            db.conn
                .execute(
                    "INSERT INTO forward_logs
                     (timestamp, model, account_id, account_name, status, cost, attempt)
                     VALUES ('2026-07-23T00:00:00Z', ?1, 'a', 'a', 'client_error', 0, ?2)",
                    params![format!("attempt-{attempt}"), attempt],
                )
                .expect("forward log should save");
        }

        let page = db
            .query_forward_logs(ForwardLogQueryOptions {
                limit: 20,
                offset: 0,
                status: None,
                account_id: None,
                provider_id: None,
                offering_id: None,
                route_account_id: None,
                credential_account_id: None,
                model: None,
                key_id: None,
                request_id: None,
                start_time: None,
                end_time: None,
                sort_by: Some("attempt"),
                sort_order: Some("asc"),
            })
            .expect("attempt sort should query");
        assert_eq!(
            page.items
                .iter()
                .filter_map(|log| log.attempt)
                .collect::<Vec<_>>(),
            [1, 2]
        );

        drop(db);
        fs::remove_dir_all(dir).expect("test data dir should be removed");
    }

    fn attributed_log(account_id: &str, key_id: Option<&str>, cost: f64) -> ForwardLog {
        let mut log = forward_log(account_id, "success", cost);
        log.client_key_id = key_id.map(str::to_string);
        log.client_key_name = key_id.map(|id| format!("Key-{id}"));
        log
    }

    #[test]
    fn forward_logs_filter_by_key_and_unattributed_sentinel() {
        let dir = temp_data_dir("forward-key-filter");
        let db = Database::open(dir.clone()).unwrap();
        db.log_forward(&attributed_log("acct", Some("key-a"), 1.0))
            .unwrap();
        db.log_forward(&attributed_log("acct", Some("key-b"), 2.0))
            .unwrap();
        db.log_forward(&attributed_log("acct", None, 4.0)).unwrap();

        let query = |key_id: Option<&str>| {
            db.query_forward_logs(ForwardLogQueryOptions {
                limit: 50,
                offset: 0,
                status: None,
                account_id: None,
                provider_id: None,
                offering_id: None,
                route_account_id: None,
                credential_account_id: None,
                model: None,
                key_id,
                request_id: None,
                start_time: None,
                end_time: None,
                sort_by: Some("cost"),
                sort_order: Some("asc"),
            })
            .unwrap()
        };

        let all = query(None);
        assert_eq!(all.summary.total_requests, 3);
        assert_eq!(all.items.len(), 3);

        let key_a = query(Some("key-a"));
        assert_eq!(key_a.summary.total_requests, 1);
        assert_eq!(key_a.summary.cost, 1.0);
        assert_eq!(key_a.items[0].client_key_id.as_deref(), Some("key-a"));
        assert_eq!(key_a.items[0].client_key_name.as_deref(), Some("Key-key-a"));

        let unattributed = query(Some(UNATTRIBUTED_KEY_FILTER));
        assert_eq!(unattributed.summary.total_requests, 1);
        assert_eq!(unattributed.summary.cost, 4.0);
        assert!(unattributed.items[0].client_key_id.is_none());

        let keys = db.list_forward_log_keys().unwrap();
        assert_eq!(keys.len(), 2);
        assert!(
            keys.iter()
                .any(|key| key.id == "key-a" && key.name == "Key-key-a")
        );
        assert!(keys.iter().any(|key| key.id == "key-b"));

        drop(db);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn forward_logs_filter_by_provider_attribution_before_pagination() {
        let dir = temp_data_dir("forward-provider-filter");
        let db = Database::open(dir.clone()).unwrap();
        let insert = |model: &str,
                      provider_id: &str,
                      offering_id: &str,
                      route_account_id: &str,
                      credential_account_id: &str| {
            let mut log = forward_log(credential_account_id, "success", 1.0);
            log.model = model.into();
            log.provider_id = Some(provider_id.into());
            log.offering_id = Some(offering_id.into());
            log.route_account_id = Some(route_account_id.into());
            log.credential_account_id = Some(credential_account_id.into());
            db.log_forward(&log).unwrap();
        };

        insert("go-a", "opencode", "go", "go-a", "go-a");
        insert("go-b", "opencode", "go", "go-b", "go-b");
        // A Zen route may deliberately debit an OpenCode credential account.
        insert("zen", "opencode", "zen-free", "zen-free", "go-a");
        // These newer rows would hide OpenCode Go rows if filtering happened
        // after LIMIT/OFFSET.
        insert("goat-a", "goat", "goat", "goat-a", "goat-a");
        insert("goat-b", "goat", "goat", "goat-b", "goat-b");

        let query = |limit: i64,
                     offset: i64,
                     provider_id: Option<&str>,
                     offering_id: Option<&str>,
                     route_account_id: Option<&str>,
                     credential_account_id: Option<&str>| {
            db.query_forward_logs(ForwardLogQueryOptions {
                limit,
                offset,
                status: None,
                account_id: None,
                provider_id,
                offering_id,
                route_account_id,
                credential_account_id,
                model: None,
                key_id: None,
                request_id: None,
                start_time: None,
                end_time: None,
                sort_by: None,
                sort_order: None,
            })
            .unwrap()
        };

        let first_go = query(1, 0, Some("opencode"), Some("go"), None, None);
        assert_eq!(first_go.summary.total_requests, 2);
        assert_eq!(first_go.items[0].model, "go-b");
        let second_go = query(1, 1, Some("opencode"), Some("go"), None, None);
        assert_eq!(second_go.summary.total_requests, 2);
        assert_eq!(second_go.items[0].model, "go-a");

        let routed_zen = query(10, 0, Some("opencode"), None, Some("zen-free"), None);
        assert_eq!(routed_zen.summary.total_requests, 1);
        assert_eq!(routed_zen.items[0].model, "zen");
        assert_eq!(
            routed_zen.items[0].credential_account_id.as_deref(),
            Some("go-a")
        );

        let credential_go_a = query(10, 0, None, None, None, Some("go-a"));
        assert_eq!(credential_go_a.summary.total_requests, 2);
        assert_eq!(
            credential_go_a
                .items
                .iter()
                .map(|log| log.model.as_str())
                .collect::<Vec<_>>(),
            ["zen", "go-a"]
        );

        let goat = query(10, 0, Some("goat"), None, None, None);
        assert_eq!(goat.summary.total_requests, 2);
        assert_eq!(
            goat.items
                .iter()
                .map(|log| log.route_account_id.as_deref())
                .collect::<Vec<_>>(),
            [Some("goat-b"), Some("goat-a")]
        );

        drop(db);
        fs::remove_dir_all(dir).unwrap();
    }

    fn empty_forward_query<'a>() -> ForwardLogQueryOptions<'a> {
        ForwardLogQueryOptions {
            limit: 50,
            offset: 0,
            status: None,
            account_id: None,
            provider_id: None,
            offering_id: None,
            route_account_id: None,
            credential_account_id: None,
            model: None,
            request_id: None,
            start_time: None,
            end_time: None,
            sort_by: None,
            sort_order: None,
            key_id: None,
        }
    }

    fn insert_identity_log(
        db: &Database,
        log: ForwardLog,
        requested: Option<&str>,
        alias: Option<&str>,
        upstream: Option<&str>,
    ) -> i64 {
        let id = db.log_forward(&log).unwrap();
        db.set_forward_log_native_attribution(
            id,
            &ForwardLogNativeAttribution {
                requested_model: requested.map(str::to_string),
                resolved_alias: alias.map(str::to_string),
                upstream_model: upstream.map(str::to_string),
                native_cost_value: None,
                native_cost_unit: None,
                native_cost_currency: None,
            },
        )
        .unwrap();
        id
    }

    fn clear_v23_identity(db: &Database, id: i64) {
        db.conn
            .execute(
                "UPDATE forward_logs
                 SET requested_model = NULL, resolved_alias = NULL, upstream_model = NULL
                 WHERE id = ?1",
                [id],
            )
            .unwrap();
    }

    #[test]
    fn forward_log_model_filter_binds_each_identity_column() {
        let none = empty_forward_query();
        let (sql, params) = forward_log_filter(&none);
        assert!(!sql.to_ascii_lowercase().contains("model"));
        assert!(params.is_empty());

        let empty = ForwardLogQueryOptions {
            model: Some(""),
            ..empty_forward_query()
        };
        let (sql, params) = forward_log_filter(&empty);
        assert!(!sql.to_ascii_lowercase().contains("model"));
        assert!(params.is_empty());

        let filtered = ForwardLogQueryOptions {
            status: Some("success"),
            model: Some("glm-5.2"),
            ..empty_forward_query()
        };
        let (sql, params) = forward_log_filter(&filtered);
        assert!(sql.contains("status = ?"));
        assert!(sql.contains(
            "(model = ? OR requested_model = ? OR resolved_alias = ? OR upstream_model = ?)"
        ));
        assert!(sql.contains(" AND "));
        assert_eq!(params.len(), 5);
        assert_eq!(params[0], Value::Text("success".into()));
        assert!(
            params[1..]
                .iter()
                .all(|value| *value == Value::Text("glm-5.2".into()))
        );
    }

    #[test]
    fn forward_logs_model_filter_matches_each_identity_and_legacy_fallback() {
        let dir = temp_data_dir("forward-model-identity-filter");
        let db = Database::open(dir.clone()).unwrap();

        let mut legacy = forward_log("acct", "success", 1.0);
        legacy.model = "needle".into();
        legacy.prompt_tokens = 1;
        let legacy_id = db.log_forward(&legacy).unwrap();
        clear_v23_identity(&db, legacy_id);

        let mut requested_only = forward_log("acct", "success", 2.0);
        requested_only.model = "legacy-req".into();
        requested_only.prompt_tokens = 2;
        let requested_id = insert_identity_log(
            &db,
            requested_only,
            Some("needle"),
            Some("alias-req"),
            Some("up-req"),
        );

        let mut alias_only = forward_log("acct", "success", 3.0);
        alias_only.model = "legacy-alias".into();
        alias_only.prompt_tokens = 3;
        let alias_id = insert_identity_log(
            &db,
            alias_only,
            Some("req-alias"),
            Some("needle"),
            Some("up-alias"),
        );

        let mut upstream_only = forward_log("acct", "success", 4.0);
        upstream_only.model = "legacy-up".into();
        upstream_only.prompt_tokens = 4;
        let upstream_id = insert_identity_log(
            &db,
            upstream_only,
            Some("req-up"),
            Some("alias-up"),
            Some("needle"),
        );

        let mut empty_v23 = forward_log("acct", "success", 5.0);
        empty_v23.model = "kept-empty".into();
        empty_v23.prompt_tokens = 5;
        insert_identity_log(&db, empty_v23, Some(""), Some(""), Some(""));

        let mut other = forward_log("acct", "success", 100.0);
        other.model = "other-legacy".into();
        other.prompt_tokens = 100;
        insert_identity_log(
            &db,
            other,
            Some("other-req"),
            Some("other-alias"),
            Some("other-up"),
        );

        let mut overlap = forward_log("acct", "success", 6.0);
        overlap.model = "needle".into();
        overlap.prompt_tokens = 6;
        let overlap_id =
            insert_identity_log(&db, overlap, Some("needle"), Some("needle"), Some("needle"));

        let page = db
            .query_forward_logs(ForwardLogQueryOptions {
                model: Some("needle"),
                sort_by: Some("cost"),
                sort_order: Some("asc"),
                ..empty_forward_query()
            })
            .unwrap();
        let ids = page.items.iter().map(|log| log.id).collect::<Vec<_>>();
        assert_eq!(
            ids,
            [legacy_id, requested_id, alias_id, upstream_id, overlap_id]
        );
        let unique = ids.iter().copied().collect::<HashSet<_>>();
        assert_eq!(unique.len(), ids.len());
        assert_eq!(page.summary.total_requests, 5);
        assert_eq!(page.summary.prompt_tokens, 16);
        assert!((page.summary.cost - 16.0).abs() < f64::EPSILON);

        let requested = db
            .query_forward_logs(ForwardLogQueryOptions {
                model: Some("legacy-req"),
                ..empty_forward_query()
            })
            .unwrap();
        assert_eq!(
            requested.items.iter().map(|log| log.id).collect::<Vec<_>>(),
            [requested_id]
        );

        let alias = db
            .query_forward_logs(ForwardLogQueryOptions {
                model: Some("alias-req"),
                ..empty_forward_query()
            })
            .unwrap();
        assert_eq!(
            alias.items.iter().map(|log| log.id).collect::<Vec<_>>(),
            [requested_id]
        );

        let empty_identity = db
            .query_forward_logs(ForwardLogQueryOptions {
                model: Some("kept-empty"),
                ..empty_forward_query()
            })
            .unwrap();
        assert_eq!(empty_identity.summary.total_requests, 1);
        assert_eq!(empty_identity.items[0].model, "kept-empty");

        let missing = db
            .query_forward_logs(ForwardLogQueryOptions {
                model: Some("missing"),
                ..empty_forward_query()
            })
            .unwrap();
        assert!(missing.items.is_empty());
        assert_eq!(missing.summary.total_requests, 0);

        let substring = db
            .query_forward_logs(ForwardLogQueryOptions {
                model: Some("need"),
                ..empty_forward_query()
            })
            .unwrap();
        assert!(substring.items.is_empty());
        assert_eq!(substring.summary.total_requests, 0);

        drop(db);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn forward_logs_model_filter_ands_other_filters_before_pagination() {
        let dir = temp_data_dir("forward-model-combo-filter");
        let db = Database::open(dir.clone()).unwrap();
        let inside = DateTime::parse_from_rfc3339("2026-07-17T04:15:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let outside = DateTime::parse_from_rfc3339("2026-07-17T03:30:00Z")
            .unwrap()
            .with_timezone(&Utc);

        let matching = |suffix: &str, cost: f64| {
            let mut log = forward_log("acct", "success", cost);
            log.model = format!("legacy-{suffix}");
            log.provider_id = Some("opencode".into());
            log.offering_id = Some("go".into());
            log.client_key_id = Some("key-a".into());
            log.client_key_name = Some("Key-a".into());
            log.timestamp = inside;
            log.prompt_tokens = cost as i64;
            insert_identity_log(
                &db,
                log,
                Some("req-other"),
                Some("needle"),
                Some("up-other"),
            )
        };
        let first = matching("a", 1.0);
        let second = matching("b", 2.0);
        let third = matching("c", 3.0);

        let mut wrong_provider = forward_log("acct", "success", 9.0);
        wrong_provider.model = "legacy-provider".into();
        wrong_provider.provider_id = Some("goat".into());
        wrong_provider.offering_id = Some("goat".into());
        wrong_provider.client_key_id = Some("key-a".into());
        wrong_provider.timestamp = inside;
        insert_identity_log(
            &db,
            wrong_provider,
            Some("needle"),
            Some("alias-other"),
            Some("up-other"),
        );

        let mut wrong_key = forward_log("acct", "success", 8.0);
        wrong_key.model = "legacy-key".into();
        wrong_key.provider_id = Some("opencode".into());
        wrong_key.offering_id = Some("go".into());
        wrong_key.client_key_id = Some("key-b".into());
        wrong_key.timestamp = inside;
        insert_identity_log(&db, wrong_key, None, None, Some("needle"));

        let mut wrong_status = forward_log("acct", "error", 7.0);
        wrong_status.model = "needle".into();
        wrong_status.provider_id = Some("opencode".into());
        wrong_status.offering_id = Some("go".into());
        wrong_status.client_key_id = Some("key-a".into());
        wrong_status.timestamp = inside;
        let wrong_status_id = db.log_forward(&wrong_status).unwrap();
        clear_v23_identity(&db, wrong_status_id);

        let mut wrong_time = forward_log("acct", "success", 6.0);
        wrong_time.model = "legacy-time".into();
        wrong_time.provider_id = Some("opencode".into());
        wrong_time.offering_id = Some("go".into());
        wrong_time.client_key_id = Some("key-a".into());
        wrong_time.timestamp = outside;
        insert_identity_log(
            &db,
            wrong_time,
            Some("needle"),
            Some("needle"),
            Some("needle"),
        );

        for index in 0..5 {
            let mut decoy = forward_log("busy", "success", 100.0);
            decoy.model = format!("decoy-{index}");
            decoy.provider_id = Some("opencode".into());
            decoy.offering_id = Some("go".into());
            decoy.client_key_id = Some("key-a".into());
            decoy.timestamp = inside;
            db.log_forward(&decoy).unwrap();
        }

        let filtered = ForwardLogQueryOptions {
            limit: 1,
            offset: 0,
            status: Some("success"),
            provider_id: Some("opencode"),
            offering_id: Some("go"),
            model: Some("needle"),
            key_id: Some("key-a"),
            start_time: Some("2026-07-17T12:00:00+08:00"),
            end_time: Some("2026-07-17T12:30:00+08:00"),
            sort_by: Some("cost"),
            sort_order: Some("asc"),
            ..empty_forward_query()
        };
        let first_page = db.query_forward_logs(filtered).unwrap();
        assert_eq!(first_page.items.len(), 1);
        assert_eq!(first_page.items[0].id, first);
        assert_eq!(first_page.summary.total_requests, 3);
        assert_eq!(first_page.summary.prompt_tokens, 6);
        assert!((first_page.summary.cost - 6.0).abs() < f64::EPSILON);

        let second_page = db
            .query_forward_logs(ForwardLogQueryOptions {
                limit: 1,
                offset: 1,
                status: Some("success"),
                provider_id: Some("opencode"),
                offering_id: Some("go"),
                model: Some("needle"),
                key_id: Some("key-a"),
                start_time: Some("2026-07-17T12:00:00+08:00"),
                end_time: Some("2026-07-17T12:30:00+08:00"),
                sort_by: Some("cost"),
                sort_order: Some("asc"),
                ..empty_forward_query()
            })
            .unwrap();
        assert_eq!(second_page.items.len(), 1);
        assert_eq!(second_page.items[0].id, second);
        assert_eq!(second_page.summary.total_requests, 3);

        let rest = db
            .query_forward_logs(ForwardLogQueryOptions {
                limit: 50,
                offset: 2,
                status: Some("success"),
                provider_id: Some("opencode"),
                offering_id: Some("go"),
                model: Some("needle"),
                key_id: Some("key-a"),
                start_time: Some("2026-07-17T12:00:00+08:00"),
                end_time: Some("2026-07-17T12:30:00+08:00"),
                sort_by: Some("cost"),
                sort_order: Some("asc"),
                ..empty_forward_query()
            })
            .unwrap();
        assert_eq!(
            rest.items.iter().map(|log| log.id).collect::<Vec<_>>(),
            [third]
        );
        let unique = [first, second, third].into_iter().collect::<HashSet<_>>();
        assert_eq!(unique.len(), 3);

        drop(db);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn backfill_attributes_null_rows_in_chunks_with_resume_and_completion() {
        let dir = temp_data_dir("backfill-chunks");
        let db = Database::open(dir.clone()).unwrap();
        for index in 0..7 {
            let mut log = forward_log("acct", "success", index as f64);
            log.client_key_id = (index % 2 == 0).then(|| "already-set".to_string());
            db.log_forward(&log).unwrap();
        }

        // Chunk size 3 covers rowids 1..=7 in three steps; already-attributed
        // rows must never be overwritten by the range update.
        assert!(
            db.backfill_forward_logs_client_key_step("primary", "Primary", 3)
                .unwrap()
        );
        assert!(
            db.backfill_forward_logs_client_key_step("primary", "Primary", 3)
                .unwrap()
        );
        // The final chunk exactly reaches max rowid and records completion
        // in the same call; a further step is a no-op.
        assert!(
            !db.backfill_forward_logs_client_key_step("primary", "Primary", 3)
                .unwrap()
        );
        assert!(
            !db.backfill_forward_logs_client_key_step("primary", "Primary", 3)
                .unwrap()
        );
        assert_eq!(
            db.forward_log_backfill_marker().unwrap().as_deref(),
            Some(BACKFILL_DONE)
        );

        let rows = db.list_forward_logs(100).unwrap();
        assert_eq!(rows.len(), 7);
        for (index, row) in rows.iter().rev().enumerate() {
            if index % 2 == 0 {
                assert_eq!(row.client_key_id.as_deref(), Some("already-set"));
            } else {
                assert_eq!(row.client_key_id.as_deref(), Some("primary"));
                assert_eq!(row.client_key_name.as_deref(), Some("Primary"));
            }
        }

        // New NULL rows written by an older binary (a downgrade window)
        // restart the scan instead of staying "unattributed" forever.
        db.log_forward(&forward_log("acct", "success", 9.0))
            .unwrap();
        assert!(
            db.backfill_forward_logs_client_key_step("primary", "Primary", 3)
                .unwrap()
        );
        while db
            .backfill_forward_logs_client_key_step("primary", "Primary", 3)
            .unwrap()
        {}
        assert_eq!(
            db.forward_log_backfill_marker().unwrap().as_deref(),
            Some(BACKFILL_DONE)
        );
        let late_rows: Vec<_> = db
            .list_forward_logs(100)
            .unwrap()
            .into_iter()
            .filter(|row| row.client_key_name.as_deref() == Some("Primary"))
            .collect();
        assert!(late_rows.iter().any(|row| row.cost == Some(9.0)));

        drop(db);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn backfill_resumes_from_persisted_watermark_after_interruption() {
        let dir = temp_data_dir("backfill-resume");
        let db = Database::open(dir.clone()).unwrap();
        for index in 0..5 {
            db.log_forward(&forward_log("acct", "success", index as f64))
                .unwrap();
        }

        // Simulate a crash after the first chunk: the watermark persists but
        // the remaining rows are still NULL.
        assert!(
            db.backfill_forward_logs_client_key_step("primary", "Primary", 2)
                .unwrap()
        );
        assert_eq!(
            db.forward_log_backfill_marker().unwrap().as_deref(),
            Some("2")
        );
        let partial = db.list_forward_logs(100).unwrap();
        assert_eq!(
            partial
                .iter()
                .filter(|row| row.client_key_id.is_some())
                .count(),
            2
        );

        // A restarted run continues from the watermark instead of
        // rescanning; the last chunk completes the table and records done.
        assert!(
            db.backfill_forward_logs_client_key_step("primary", "Primary", 2)
                .unwrap()
        );
        assert!(
            !db.backfill_forward_logs_client_key_step("primary", "Primary", 2)
                .unwrap()
        );
        assert_eq!(
            db.forward_log_backfill_marker().unwrap().as_deref(),
            Some(BACKFILL_DONE)
        );
        let rows = db.list_forward_logs(100).unwrap();
        assert!(
            rows.iter()
                .all(|row| row.client_key_id.as_deref() == Some("primary"))
        );
        // No row was attributed twice: costs and row counts are unchanged.
        assert_eq!(
            rows.iter().map(|row| row.cost.unwrap_or(0.0)).sum::<f64>() as i64,
            10
        );

        drop(db);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn backfill_restarts_after_done_when_a_downgrade_writes_null_rows() {
        let dir = temp_data_dir("backfill-restart-after-done");
        let db = Database::open(dir.clone()).unwrap();
        db.log_forward(&forward_log("acct", "success", 1.0))
            .unwrap();
        assert!(
            !db.backfill_forward_logs_client_key_step("primary", "Primary", 50)
                .unwrap()
        );
        assert_eq!(
            db.forward_log_backfill_marker().unwrap().as_deref(),
            Some(BACKFILL_DONE)
        );

        // A downgrade window writes fresh rows the way the pre-v18 binary
        // did: without a client key id.
        db.log_forward(&forward_log("acct", "success", 2.0))
            .unwrap();
        db.log_forward(&forward_log("acct", "success", 4.0))
            .unwrap();

        // The completion marker no longer short-circuits: one index probe
        // sees the NULL rows, the scan restarts, and they are attributed.
        assert!(
            db.backfill_forward_logs_client_key_step("primary", "Primary", 1)
                .unwrap()
        );
        while db
            .backfill_forward_logs_client_key_step("primary", "Primary", 50)
            .unwrap()
        {}
        assert_eq!(
            db.forward_log_backfill_marker().unwrap().as_deref(),
            Some(BACKFILL_DONE)
        );
        let rows = db.list_forward_logs(100).unwrap();
        assert_eq!(rows.len(), 3);
        assert!(
            rows.iter()
                .all(|row| row.client_key_id.as_deref() == Some("primary"))
        );

        // With no fresh NULL rows the marker keeps short-circuiting.
        let mut attributed = forward_log("acct", "success", 8.0);
        attributed.client_key_id = Some("primary".into());
        attributed.client_key_name = Some("Primary".into());
        db.log_forward(&attributed).unwrap();
        assert!(
            !db.backfill_forward_logs_client_key_step("primary", "Primary", 50)
                .unwrap()
        );

        drop(db);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn backfill_completes_inline_for_empty_tables() {
        let dir = temp_data_dir("backfill-empty");
        let db = Database::open(dir.clone()).unwrap();
        assert!(
            !db.backfill_forward_logs_client_key_step("primary", "Primary", 50_000)
                .unwrap()
        );
        assert_eq!(
            db.forward_log_backfill_marker().unwrap().as_deref(),
            Some(BACKFILL_DONE)
        );

        drop(db);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn v19_client_key_migration_is_idempotent_and_crash_replay_safe() {
        let dir = temp_data_dir("v19-idempotent");
        let db = Database::open(dir.clone()).unwrap();
        let probe_columns = |conn: &Connection| {
            let mut stmt = conn.prepare("PRAGMA table_info(forward_logs)").unwrap();
            stmt.query_map([], |row| row.get::<_, String>(1))
                .unwrap()
                .collect::<Result<Vec<_>, _>>()
                .unwrap()
        };
        assert!(probe_columns(&db.conn).contains(&"client_key_id".to_string()));
        assert!(probe_columns(&db.conn).contains(&"client_key_name".to_string()));
        let version: i32 = db
            .conn
            .query_row("SELECT MAX(version) FROM schema_version", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(version, CURRENT_SCHEMA_VERSION);

        let index_exists: i64 = db
            .conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type = 'index' AND name = 'idx_forward_logs_client_key'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(index_exists, 1);

        // Replaying migrate (as after a crash between ALTER TABLE and the
        // version bump, or simply a second open) converges without error.
        drop(db);
        let db = Database::open(dir.clone()).unwrap();
        db.migrate().unwrap();
        let version: i32 = db
            .conn
            .query_row("SELECT MAX(version) FROM schema_version", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(version, CURRENT_SCHEMA_VERSION);

        drop(db);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn v20_creates_the_sub_gateway_keys_table_idempotently() {
        let dir = temp_data_dir("v20-idempotent");
        let db = Database::open(dir.clone()).unwrap();

        let probe = |conn: &Connection| {
            let table: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM sqlite_master
                     WHERE type = 'table' AND name = 'sub_gateway_keys'",
                    [],
                    |row| row.get(0),
                )
                .unwrap();
            let index: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM sqlite_master
                     WHERE type = 'index' AND name = 'idx_sub_gateway_keys_key'",
                    [],
                    |row| row.get(0),
                )
                .unwrap();
            (table, index)
        };
        assert_eq!(probe(&db.conn), (1, 1));
        let version: i32 = db
            .conn
            .query_row("SELECT MAX(version) FROM schema_version", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(version, CURRENT_SCHEMA_VERSION);

        // Replaying the migration converges to the same shape.
        db.migrate().unwrap();
        assert_eq!(probe(&db.conn), (1, 1));

        drop(db);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn v21_adds_usage_sync_columns_with_safe_defaults() {
        let dir = temp_data_dir("v21-usage-sync");
        let db = Database::open(dir.clone()).unwrap();
        let columns = {
            let mut stmt = db.conn.prepare("PRAGMA table_info(accounts)").unwrap();
            stmt.query_map([], |row| row.get::<_, String>(1))
                .unwrap()
                .collect::<Result<Vec<_>, _>>()
                .unwrap()
        };
        for name in [
            "usage_sync_last_success_at",
            "usage_sync_last_attempt_at",
            "usage_sync_next_eligible_at",
            "usage_sync_failure_streak",
            "usage_sync_last_expedited_at",
        ] {
            assert!(columns.contains(&name.to_string()), "missing {name}");
        }
        assert_eq!(db.schema_version().unwrap(), CURRENT_SCHEMA_VERSION);

        let account = account("sync-defaults");
        db.create_account(&account).unwrap();
        let sync = db
            .account_usage_sync_state("sync-defaults")
            .unwrap()
            .unwrap();
        assert!(sync.last_success_at.is_none());
        assert!(sync.last_attempt_at.is_none());
        assert!(sync.next_eligible_at.is_none());
        assert_eq!(sync.failure_streak, 0);
        assert!(sync.last_expedited_at.is_none());

        db.migrate().unwrap();
        assert_eq!(db.schema_version().unwrap(), CURRENT_SCHEMA_VERSION);

        drop(db);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn fresh_go_accounts_project_live_provider_quota_windows() {
        let dir = temp_data_dir("fresh-provider-quota");
        let db = Database::open(dir.clone()).unwrap();
        db.create_account(&account("fresh-go")).unwrap();
        assert!(db.list_quota_windows("fresh-go").unwrap().is_empty());

        let limits = SEED_LIMITS;
        db.calibrate_account_usage(
            "fresh-go",
            UsageWindowKind::FiveHours,
            50.0,
            Some(180),
            limits.window_5h,
        )
        .unwrap();
        let windows = db
            .live_opencode_go_quota_windows("fresh-go", &limits)
            .unwrap();
        assert_eq!(windows.len(), 3);
        let rolling = windows
            .iter()
            .find(|window| window.window_kind == QUOTA_WINDOW_FIVE_HOURS)
            .unwrap();
        assert!((rolling.used - limits.window_5h * 0.5).abs() < 1e-9);
        assert_eq!(rolling.limit_value, Some(limits.window_5h));
        assert_eq!(rolling.source, "opencode-go-live");

        drop(db);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn v21_to_v22_creates_one_usable_rollback_backup() {
        let dir = temp_data_dir("v21-v22-backup");
        create_v21_fixture(&dir, false);

        let db = Database::open(dir.clone()).expect("v21 database should migrate");
        assert_eq!(db.schema_version().unwrap(), CURRENT_SCHEMA_VERSION);
        assert!(
            db.get_account("rollback-account")
                .expect("migrated account should load")
                .is_some()
        );
        let migrated = db.list_quota_windows("rollback-account").unwrap();
        let migrated_rolling = migrated
            .iter()
            .find(|window| window.window_kind == QUOTA_WINDOW_FIVE_HOURS)
            .unwrap()
            .used;
        db.log_forward(&forward_log("rollback-account", "success", 1.5))
            .unwrap();
        let limits = SEED_LIMITS;
        let live = db
            .live_opencode_go_quota_windows("rollback-account", &limits)
            .unwrap();
        let live_rolling = live
            .iter()
            .find(|window| window.window_kind == QUOTA_WINDOW_FIVE_HOURS)
            .unwrap();
        assert!((live_rolling.used - (migrated_rolling + 1.5)).abs() < 1e-9);
        assert_eq!(
            db.list_quota_windows("rollback-account")
                .unwrap()
                .iter()
                .find(|window| window.window_kind == QUOTA_WINDOW_FIVE_HOURS)
                .unwrap()
                .used,
            migrated_rolling,
            "frozen migration rows must not be the provider API authority"
        );
        drop(db);

        let backups_before = pre_v22_backup_paths(&dir);
        assert_eq!(backups_before.len(), 1);
        let pre_v23 = pre_v23_backup_paths(&dir);
        assert_eq!(pre_v23.len(), 1);
        let pre_v23_backup =
            Connection::open_with_flags(&pre_v23[0], OpenFlags::SQLITE_OPEN_READ_ONLY)
                .expect("pre-v23 backup should open");
        assert_eq!(schema_version_on(&pre_v23_backup).unwrap(), 21);
        drop(pre_v23_backup);
        let backup_path = &backups_before[0];
        let backup_name = backup_path
            .file_name()
            .and_then(|name| name.to_str())
            .expect("backup should have a UTF-8 filename");
        let timestamp = backup_name
            .strip_prefix(PRE_V22_BACKUP_FILE_PREFIX)
            .and_then(|name| name.strip_suffix(".bak"))
            .expect("backup should use the v22 rollback name");
        assert_eq!(timestamp.len(), 25);
        assert!(timestamp.bytes().enumerate().all(|(index, byte)| {
            (index == 8 && byte == b'T')
                || (index == 24 && byte == b'Z')
                || !matches!(index, 8 | 24) && byte.is_ascii_digit()
        }));

        let backup = Connection::open_with_flags(backup_path, OpenFlags::SQLITE_OPEN_READ_ONLY)
            .expect("backup should open read-only");
        assert_eq!(schema_version_on(&backup).unwrap(), 21);
        let backed_up_account: (String, String, i64) = backup
            .query_row(
                "SELECT name, key_cipher, enabled FROM accounts WHERE id = 'rollback-account'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .expect("backup should retain the representative account");
        let backed_up_log: (String, String, String, f64) = backup
            .query_row(
                "SELECT account_id, model, status, cost FROM forward_logs LIMIT 1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .expect("backup should retain the representative forward log");
        assert_eq!(
            backed_up_account,
            ("rollback-account".into(), "cipher".into(), 1)
        );
        assert_eq!(
            backed_up_log,
            (
                "rollback-account".into(),
                "test".into(),
                "success".into(),
                4.25
            )
        );
        drop(backup);

        let backup_bytes = fs::read(backup_path).expect("backup should be readable");
        let reopened = Database::open(dir.clone()).expect("v22 database should reopen");
        assert_eq!(reopened.schema_version().unwrap(), CURRENT_SCHEMA_VERSION);
        drop(reopened);
        assert_eq!(pre_v22_backup_paths(&dir), backups_before);
        assert_eq!(
            fs::read(backup_path).expect("backup should remain readable"),
            backup_bytes
        );

        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn v20_to_v22_creates_verified_source_backup_before_direct_upgrade() {
        let dir = temp_data_dir("v20-v22-backup");
        create_v20_fixture(&dir, false);

        let db = Database::open(dir.clone()).expect("v20 database should migrate directly");
        assert_eq!(db.schema_version().unwrap(), CURRENT_SCHEMA_VERSION);
        let account_columns = db
            .conn
            .prepare("PRAGMA table_info(accounts)")
            .expect("table info should prepare")
            .query_map([], |row| row.get::<_, String>(1))
            .expect("table info should query")
            .collect::<rusqlite::Result<Vec<_>>>()
            .expect("columns should load");
        assert!(
            account_columns
                .iter()
                .any(|name| name == "usage_sync_last_success_at")
        );
        assert!(account_columns.iter().any(|name| name == "provider_id"));
        drop(db);

        let backups_before = pre_v22_backup_paths(&dir);
        assert_eq!(backups_before.len(), 1);
        let backup_bytes = fs::read(&backups_before[0]).expect("backup should be readable");
        let backup =
            Connection::open_with_flags(&backups_before[0], OpenFlags::SQLITE_OPEN_READ_ONLY)
                .expect("backup should open read-only");
        assert_eq!(schema_version_on(&backup).unwrap(), 20);
        assert!(!table_has_column(&backup, "accounts", "provider_id").unwrap());
        assert!(!table_has_column(&backup, "accounts", "usage_sync_last_success_at").unwrap());
        drop(backup);

        let reopened = Database::open(dir.clone()).expect("v22 database should reopen");
        assert_eq!(reopened.schema_version().unwrap(), CURRENT_SCHEMA_VERSION);
        drop(reopened);
        assert_eq!(pre_v22_backup_paths(&dir), backups_before);
        assert_eq!(
            fs::read(&backups_before[0]).expect("backup should remain readable"),
            backup_bytes
        );
        fs::remove_dir_all(dir).expect("test data dir should be removed");
    }

    #[test]
    fn draft_v19_libraries_without_notes_gain_the_column_on_reopen() {
        let dir = temp_data_dir("draft-v19-notes-repair");
        let db = Database::open(dir.clone()).unwrap();
        db.create_account(&account("legacy")).unwrap();
        // Unreleased #43 drafts already sat at version 19 (client-key
        // columns + sub-key table) and never received upstream v18 notes.
        db.conn
            .execute_batch(
                "ALTER TABLE accounts DROP COLUMN notes;
                 DELETE FROM schema_version;
                 INSERT INTO schema_version (version) VALUES (19);",
            )
            .expect("draft numbering should be reproducible");
        let notes_before: i64 = db
            .conn
            .query_row(
                "SELECT COUNT(*) FROM pragma_table_info('accounts') WHERE name = 'notes'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(notes_before, 0);
        drop(db);

        let db = Database::open(dir.clone()).expect("draft database should reopen");
        let (version, notes_after): (i32, i64) = db
            .conn
            .query_row(
                "SELECT
                    (SELECT MAX(version) FROM schema_version),
                    (SELECT COUNT(*) FROM pragma_table_info('accounts') WHERE name = 'notes')",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .expect("repaired schema should load");
        assert_eq!(version, CURRENT_SCHEMA_VERSION);
        assert_eq!(notes_after, 1);
        db.list_accounts()
            .expect("account reads must survive a missing notes column on the draft");

        drop(db);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn sub_gateway_key_crud_and_unique_index_backstop() {
        let dir = temp_data_dir("sub-keys-crud");
        let db = Database::open(dir.clone()).unwrap();
        let now = Utc::now();
        let key = SubGatewayKey {
            id: "sub-1".into(),
            name: "Laptop".into(),
            key: "ocg-laptop".into(),
            enabled: true,
            deleted_at: None,
            created_at: now,
        };
        db.insert_sub_gateway_key(&key).unwrap();
        assert_eq!(db.count_active_sub_gateway_keys().unwrap(), 1);
        assert_eq!(
            db.list_active_sub_gateway_keys().unwrap(),
            vec![key.clone()]
        );
        assert_eq!(db.list_sub_gateway_keys().unwrap().len(), 1);

        // Duplicate active values are rejected by the partial unique index.
        let duplicate = SubGatewayKey {
            id: "sub-2".into(),
            name: "Twin".into(),
            key: "ocg-laptop".into(),
            enabled: true,
            deleted_at: None,
            created_at: now,
        };
        assert!(db.insert_sub_gateway_key(&duplicate).is_err());

        // Disabled keys keep their plaintext, so they still block duplicates.
        assert!(db.set_sub_gateway_key_enabled("sub-1", false).unwrap());
        assert!(db.insert_sub_gateway_key(&duplicate).is_err());
        assert!(db.sub_gateway_key_value_exists("ocg-laptop").unwrap());
        assert_eq!(
            db.active_sub_gateway_key_values().unwrap(),
            vec!["ocg-laptop".to_string()]
        );

        // Renaming and regenerating address only non-deleted rows.
        assert!(db.rename_sub_gateway_key("sub-1", "Deck").unwrap());
        assert!(
            db.update_sub_gateway_key_value("sub-1", "ocg-deck")
                .unwrap()
        );
        assert!(db.sub_gateway_key_value_exists("ocg-deck").unwrap());

        // Soft delete clears the plaintext; tombstones free the value and do
        // not count as active.
        assert!(db.soft_delete_sub_gateway_key("sub-1", now).unwrap());
        let tombstone = db.get_sub_gateway_key("sub-1").unwrap().unwrap();
        assert!(tombstone.deleted_at.is_some());
        assert!(tombstone.key.is_empty());
        assert!(!tombstone.enabled);
        assert_eq!(db.count_active_sub_gateway_keys().unwrap(), 0);
        assert!(!db.sub_gateway_key_value_exists("ocg-deck").unwrap());
        assert!(!db.rename_sub_gateway_key("sub-1", "Gone").unwrap());
        assert!(!db.set_sub_gateway_key_enabled("sub-1", true).unwrap());
        assert!(!db.soft_delete_sub_gateway_key("sub-1", now).unwrap());

        // The freed value is insertable again, and missing ids report false.
        let recycled = SubGatewayKey {
            id: "sub-3".into(),
            name: "Recycled".into(),
            key: "ocg-deck".into(),
            enabled: true,
            deleted_at: None,
            created_at: now,
        };
        db.insert_sub_gateway_key(&recycled).unwrap();
        assert!(!db.rename_sub_gateway_key("missing", "X").unwrap());
        assert!(db.get_sub_gateway_key("missing").unwrap().is_none());

        drop(db);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn forward_log_keys_resolve_the_latest_name_per_id() {
        let dir = temp_data_dir("log-keys-latest-name");
        let db = Database::open(dir.clone()).unwrap();
        let base = ForwardLog {
            id: 0,
            timestamp: Utc::now(),
            model: "m".into(),
            account_id: "a".into(),
            account_name: "a".into(),
            route_account_id: None,
            provider_id: None,
            offering_id: None,
            credential_account_id: None,
            client_key_id: Some("sub-1".into()),
            client_key_name: Some("Laptop".into()),
            status: "success".into(),
            http_status: Some(200),
            route: String::new(),
            prompt_tokens: 1,
            completion_tokens: 1,
            cached_tokens: 0,
            cache_creation_tokens: 0,
            cost: None,
            raw_cost_usd: None,
            quota_debit: None,
            effective_paid_cost_usd: None,
            pricing_revision_id: None,
            quota_multiplier: None,
            local_adjustment_multiplier: None,
            service_tier: None,
            cost_state: "not_applicable".into(),
            error_message: None,
            request_id: None,
            attempt: None,
            error_source: None,
            error_stage: None,
            duration_ms: None,
            diagnostic: None,
        };
        // A lexicographically "larger" historical name must not win: it was
        // written first, the current name last.
        let mut zzz = base.clone();
        zzz.client_key_name = Some("zzz-old".into());
        db.log_forward(&zzz).unwrap();
        db.log_forward(&base).unwrap();
        let mut renamed = base.clone();
        renamed.client_key_name = Some("Deck".into());
        db.log_forward(&renamed).unwrap();

        let keys = db.list_forward_log_keys().unwrap();
        assert_eq!(keys.len(), 1, "one entry per distinct key id");
        assert_eq!(keys[0].id, "sub-1");
        assert_eq!(keys[0].name, "Deck", "the latest snapshot wins");

        // NULL-name rows fall back to the id label.
        let mut unnamed = base.clone();
        unnamed.client_key_id = Some("ghost".into());
        unnamed.client_key_name = None;
        db.log_forward(&unnamed).unwrap();
        let keys = db.list_forward_log_keys().unwrap();
        assert_eq!(keys.len(), 2);
        assert_eq!(
            keys.iter().find(|key| key.id == "ghost").unwrap().name,
            "ghost"
        );

        // The list stays purely log-driven: an id with no rows (e.g. the
        // primary key before its first attributed request) never appears.
        assert!(
            !keys
                .iter()
                .any(|key| key.id == "00000000-0000-0000-0000-000000000001")
        );

        // A primary-attributed row with a NULL name resolves to the fixed
        // display name, never the raw id constant.
        let mut unnamed_primary = base.clone();
        unnamed_primary.client_key_id = Some("00000000-0000-0000-0000-000000000001".into());
        unnamed_primary.client_key_name = None;
        db.log_forward(&unnamed_primary).unwrap();
        let keys = db.list_forward_log_keys().unwrap();
        let primary = keys
            .iter()
            .find(|key| key.id == "00000000-0000-0000-0000-000000000001")
            .unwrap();
        assert_eq!(primary.name, "Primary");

        drop(db);
        fs::remove_dir_all(dir).unwrap();
    }

    fn create_v22_fixture(dir: &Path) {
        let db = Database::open(dir.to_path_buf()).expect("fixture database should open");
        db.create_account(&account("v22-account"))
            .expect("representative account should save");
        let mut goat = account("v22-goat");
        goat.provider_id = COMMAND_CODE_PROVIDER_ID.to_string();
        goat.offering_id = GOAT_OFFERING_ID.to_string();
        goat.enabled = false;
        db.create_account(&goat)
            .expect("representative GOAT account should save");
        db.log_forward(&forward_log("v22-account", "success", 3.5))
            .expect("representative forward log should save");
        drop(db);

        let conn = Connection::open(dir.join("data.sqlite")).expect("v23 fixture should reopen");
        conn.execute_batch(
            "PRAGMA foreign_keys=OFF;
             DROP INDEX IF EXISTS idx_account_model_capabilities_account;
             DROP INDEX IF EXISTS idx_account_acknowledgements_account;
             DROP TABLE IF EXISTS account_custom_configs;
             DROP TABLE IF EXISTS account_model_capabilities;
             DROP TABLE IF EXISTS account_acknowledgements;
             ALTER TABLE accounts DROP COLUMN verification_error;
             ALTER TABLE accounts DROP COLUMN connection_verified_at;
             ALTER TABLE accounts DROP COLUMN verification_status;
             ALTER TABLE forward_logs DROP COLUMN native_cost_currency;
             ALTER TABLE forward_logs DROP COLUMN native_cost_unit;
             ALTER TABLE forward_logs DROP COLUMN native_cost_value;
             ALTER TABLE forward_logs DROP COLUMN upstream_model;
             ALTER TABLE forward_logs DROP COLUMN resolved_alias;
             ALTER TABLE forward_logs DROP COLUMN requested_model;
             DELETE FROM schema_version;
             INSERT INTO schema_version (version) VALUES (22);
             UPDATE accounts SET enabled = 1 WHERE id = 'v22-goat';
             PRAGMA foreign_keys=ON;",
        )
        .expect("v22 fixture should be created");
    }

    #[test]
    fn v22_to_v23_creates_one_usable_rollback_backup_and_contract_tables() {
        let dir = temp_data_dir("v22-v23-backup");
        create_v22_fixture(&dir);
        assert!(pre_v23_backup_paths(&dir).is_empty());

        let db = Database::open(dir.clone()).expect("v22 database should migrate");
        assert_eq!(db.schema_version().unwrap(), CURRENT_SCHEMA_VERSION);
        let go = db
            .account_verification_state("v22-account")
            .unwrap()
            .unwrap();
        assert_eq!(go.status, ConnectionVerificationStatus::NotRequired);
        let goat = db.get_account("v22-goat").unwrap().unwrap();
        assert!(!goat.enabled, "migrated GOAT rows must be fail-closed");
        let goat_state = db.account_verification_state("v22-goat").unwrap().unwrap();
        assert_eq!(goat_state.status, ConnectionVerificationStatus::Pending);
        let log_id: i64 = db
            .conn
            .query_row("SELECT id FROM forward_logs LIMIT 1", [], |row| row.get(0))
            .unwrap();
        let attribution = db.forward_log_native_attribution(log_id).unwrap().unwrap();
        assert_eq!(attribution.requested_model.as_deref(), Some("test"));
        assert_eq!(attribution.upstream_model.as_deref(), Some("test"));
        assert_eq!(attribution.native_cost_unit.as_deref(), Some("usd"));
        assert_eq!(attribution.native_cost_currency.as_deref(), Some("USD"));
        drop(db);

        let backups = pre_v23_backup_paths(&dir);
        assert_eq!(backups.len(), 1);
        let backup = Connection::open_with_flags(&backups[0], OpenFlags::SQLITE_OPEN_READ_ONLY)
            .expect("pre-v23 backup should open");
        assert_eq!(schema_version_on(&backup).unwrap(), 22);
        assert!(!table_has_column(&backup, "accounts", "verification_status").unwrap());
        drop(backup);

        let reopened = Database::open(dir.clone()).expect("v23 database should reopen");
        assert_eq!(reopened.schema_version().unwrap(), CURRENT_SCHEMA_VERSION);
        drop(reopened);
        assert_eq!(pre_v23_backup_paths(&dir).len(), 1);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn newer_unsupported_schema_is_rejected_without_writes() {
        let dir = temp_data_dir("schema-too-new");
        let db = Database::open(dir.clone()).unwrap();
        db.conn
            .execute_batch(
                "DELETE FROM schema_version;
                 INSERT INTO schema_version (version) VALUES (27);",
            )
            .unwrap();
        drop(db);

        let error = match Database::open(dir.clone()) {
            Ok(_) => panic!("unsupported schema must fail closed"),
            Err(error) => error,
        };
        assert!(
            error.to_string().contains("newer than this build supports"),
            "{error}"
        );
        let conn = Connection::open(dir.join("data.sqlite")).unwrap();
        assert_eq!(schema_version_on(&conn).unwrap(), 27);
        drop(conn);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn zen_free_model_catalog_survives_reopen() {
        let dir = temp_data_dir("zen-free-model-catalog");
        let refreshed_at = Utc::now();
        {
            let db = Database::open(dir.clone()).unwrap();
            db.set_zen_free_model_catalog(&crate::kernel::zen::ZenFreeModelCatalog {
                models: vec!["persisted-coder-free".into()],
                refreshed_at: Some(refreshed_at),
                source_url: crate::kernel::zen::ZEN_MODELS_SOURCE_URL.into(),
            })
            .unwrap();
        }
        {
            let db = Database::open(dir.clone()).unwrap();
            let catalog = db.zen_free_model_catalog().unwrap().unwrap();
            assert_eq!(catalog.models, ["persisted-coder-free"]);
            assert_eq!(catalog.refreshed_at, Some(refreshed_at));
        }
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn v26_fresh_database_has_contract_tables_and_reopens() {
        let dir = temp_data_dir("v26-fresh");
        let db = Database::open(dir.clone()).unwrap();
        assert_eq!(db.schema_version().unwrap(), CURRENT_SCHEMA_VERSION);
        let tables: i64 = db
            .conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master
                 WHERE type = 'table' AND name IN (
                    'provider_contract_scopes', 'provider_contract_model_protocols'
                 )",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(tables, 2);
        db.migrate().unwrap();
        assert_eq!(db.schema_version().unwrap(), CURRENT_SCHEMA_VERSION);
        drop(db);
        let reopened = Database::open(dir.clone()).unwrap();
        assert_eq!(reopened.schema_version().unwrap(), CURRENT_SCHEMA_VERSION);
        drop(reopened);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn v25_to_v26_backfills_zen_catalog_into_provider_scope() {
        let dir = temp_data_dir("v25-v26-zen-backfill");
        let refreshed_at = Utc::now();
        {
            let db = Database::open(dir.clone()).unwrap();
            db.set_zen_free_model_catalog(&crate::kernel::zen::ZenFreeModelCatalog {
                models: vec!["backfill-coder-free".into()],
                refreshed_at: Some(refreshed_at),
                source_url: crate::kernel::zen::ZEN_MODELS_SOURCE_URL.into(),
            })
            .unwrap();
            db.conn
                .execute_batch(
                    "DROP TABLE IF EXISTS provider_contract_model_protocols;
                     DROP TABLE IF EXISTS provider_contract_scopes;
                     DELETE FROM schema_version;
                     INSERT INTO schema_version (version) VALUES (25);",
                )
                .unwrap();
            assert_eq!(db.schema_version().unwrap(), 25);
        }
        let db = Database::open(dir.clone()).expect("v25 database should migrate to v26");
        assert_eq!(db.schema_version().unwrap(), CURRENT_SCHEMA_VERSION);
        let scope = db
            .load_persisted_scope(&ContractScope::provider(OPENCODE_ZEN_FREE_PROVIDER_ID))
            .unwrap()
            .expect("zen provider scope should be backfilled");
        assert_eq!(scope.catalog_models, ["backfill-coder-free"]);
        assert_eq!(scope.catalog_source, CATALOG_SOURCE_OFFICIAL_ZEN);
        assert!(scope.switches.chat_completions);
        drop(db);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn provider_and_custom_contract_scopes_are_isolated() {
        let dir = temp_data_dir("v26-scope-isolation");
        let db = Database::open(dir.clone()).unwrap();
        let now = Utc::now();
        let go = ContractScope::provider(OPENCODE_PROVIDER_ID);
        let custom = ContractScope::custom_endpoint("custom-a");
        db.upsert_model_protocol(&PersistedModelProtocol {
            scope: go.clone(),
            model_id: "glm-5.2".into(),
            protocol: UpstreamProtocolKind::ChatCompletions,
            source: ContractEvidenceSource::ProbeConfirmed,
            verified_at: Some(now),
            observed_at: Some(now),
            last_probe_result: Some(ProbeResultKind::Success),
            last_probe_at: Some(now),
            last_probe_error: None,
        })
        .unwrap();
        db.upsert_model_protocol(&PersistedModelProtocol {
            scope: custom.clone(),
            model_id: "local-model".into(),
            protocol: UpstreamProtocolKind::ChatCompletions,
            source: ContractEvidenceSource::Preset,
            verified_at: Some(now),
            observed_at: Some(now),
            last_probe_result: None,
            last_probe_at: None,
            last_probe_error: None,
        })
        .unwrap();
        db.set_protocol_switch(&go, UpstreamProtocolKind::Messages, false, now)
            .unwrap();
        let persisted = db.load_persisted_contracts().unwrap();
        assert!(
            persisted
                .evidence
                .get(&go)
                .unwrap()
                .iter()
                .any(|row| row.model_id == "glm-5.2")
        );
        assert!(
            persisted
                .evidence
                .get(&custom)
                .unwrap()
                .iter()
                .all(|row| row.model_id != "glm-5.2")
        );
        assert!(!persisted.scopes.get(&go).unwrap().switches.messages);
        assert!(
            persisted
                .scopes
                .get(&custom)
                .map(|row| row.switches.messages)
                .unwrap_or(true)
        );
        drop(db);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn probe_evidence_and_catalog_mutations_advance_scope_revision_atomically() {
        let dir = temp_data_dir("v26-revision-bump");
        let db = Database::open(dir.clone()).unwrap();
        let now = Utc::now();
        let scope = ContractScope::provider(OPENCODE_PROVIDER_ID);
        assert!(db.load_persisted_scope(&scope).unwrap().is_none());

        let success = db
            .upsert_model_protocol(&PersistedModelProtocol {
                scope: scope.clone(),
                model_id: "grok-4.5".into(),
                protocol: UpstreamProtocolKind::ChatCompletions,
                source: ContractEvidenceSource::ProbeConfirmed,
                verified_at: Some(now),
                observed_at: Some(now),
                last_probe_result: Some(ProbeResultKind::Success),
                last_probe_at: Some(now),
                last_probe_error: None,
            })
            .unwrap();
        assert_eq!(success.revision, 2);
        let after_success = db.load_persisted_scope(&scope).unwrap().unwrap();
        assert_eq!(after_success.revision, 2);

        let failure = db
            .upsert_model_protocol(&PersistedModelProtocol {
                scope: scope.clone(),
                model_id: "grok-4.5".into(),
                protocol: UpstreamProtocolKind::Messages,
                source: ContractEvidenceSource::ProbeObserved,
                verified_at: None,
                observed_at: Some(now),
                last_probe_result: Some(ProbeResultKind::Failure),
                last_probe_at: Some(now),
                last_probe_error: Some("upstream 500".into()),
            })
            .unwrap();
        assert_eq!(failure.revision, 3);

        let catalog = db
            .set_contract_catalog(
                &scope,
                &["grok-4.5".into()],
                Some(now),
                crate::provider_contracts::CATALOG_SOURCE_STATIC,
                "",
                now,
            )
            .unwrap();
        assert_eq!(catalog.revision, 4);

        db.conn
            .execute_batch(
                "CREATE TRIGGER fail_probe_write BEFORE INSERT ON provider_contract_model_protocols
                 BEGIN SELECT RAISE(ABORT, 'injected write failure'); END;",
            )
            .unwrap();
        let before_failed = db.load_persisted_scope(&scope).unwrap().unwrap().revision;
        let failed = db.upsert_model_protocol(&PersistedModelProtocol {
            scope: scope.clone(),
            model_id: "glm-5.3".into(),
            protocol: UpstreamProtocolKind::Responses,
            source: ContractEvidenceSource::ProbeObserved,
            verified_at: None,
            observed_at: Some(now),
            last_probe_result: Some(ProbeResultKind::Failure),
            last_probe_at: Some(now),
            last_probe_error: Some("should roll back".into()),
        });
        assert!(failed.is_err());
        let after_failed = db.load_persisted_scope(&scope).unwrap().unwrap();
        assert_eq!(after_failed.revision, before_failed);
        assert!(
            db.load_model_protocol(&scope, "glm-5.3", UpstreamProtocolKind::Responses)
                .unwrap()
                .is_none()
        );

        drop(db);
        fs::remove_dir_all(dir).unwrap();
    }

    fn probe_observation(
        scope: ContractScope,
        model_id: &str,
        protocol: UpstreamProtocolKind,
        now: DateTime<Utc>,
    ) -> PersistedModelProtocol {
        PersistedModelProtocol {
            scope,
            model_id: model_id.into(),
            protocol,
            source: ContractEvidenceSource::ProbeConfirmed,
            verified_at: Some(now),
            observed_at: Some(now),
            last_probe_result: Some(ProbeResultKind::Success),
            last_probe_at: Some(now),
            last_probe_error: None,
        }
    }

    #[test]
    fn probe_observation_batch_upserts_atomically_and_bumps_scope_once() {
        let dir = temp_data_dir("v26-probe-batch");
        let db = Database::open(dir.clone()).unwrap();
        let now = Utc::now();
        let go = ContractScope::provider(OPENCODE_PROVIDER_ID);
        let custom = ContractScope::custom_endpoint("custom-a");

        let empty = db.upsert_model_protocols(&[]);
        assert!(empty.is_err(), "{empty:?}");
        assert!(empty.unwrap_err().to_string().contains("nonempty"));
        assert!(db.load_persisted_scope(&go).unwrap().is_none());

        let mixed = db.upsert_model_protocols(&[
            probe_observation(
                go.clone(),
                "grok-4.5",
                UpstreamProtocolKind::ChatCompletions,
                now,
            ),
            probe_observation(
                custom.clone(),
                "local-model",
                UpstreamProtocolKind::ChatCompletions,
                now,
            ),
        ]);
        assert!(mixed.is_err(), "{mixed:?}");
        assert!(mixed.unwrap_err().to_string().contains("mix"));
        assert!(db.load_persisted_scope(&go).unwrap().is_none());
        assert!(db.load_persisted_scope(&custom).unwrap().is_none());
        assert!(
            db.load_model_protocol(&go, "grok-4.5", UpstreamProtocolKind::ChatCompletions)
                .unwrap()
                .is_none()
        );

        let persisted = db
            .upsert_model_protocols(&[
                probe_observation(
                    go.clone(),
                    "grok-4.5",
                    UpstreamProtocolKind::ChatCompletions,
                    now,
                ),
                probe_observation(go.clone(), "grok-4.5", UpstreamProtocolKind::Responses, now),
            ])
            .unwrap();
        assert_eq!(persisted.revision, 2);
        let after = db.load_persisted_scope(&go).unwrap().unwrap();
        assert_eq!(after.revision, 2);
        assert!(
            db.load_model_protocol(&go, "grok-4.5", UpstreamProtocolKind::ChatCompletions)
                .unwrap()
                .is_some()
        );
        assert!(
            db.load_model_protocol(&go, "grok-4.5", UpstreamProtocolKind::Responses)
                .unwrap()
                .is_some()
        );

        db.conn
            .execute_batch(
                "CREATE TRIGGER fail_second_probe_observation_write
                 BEFORE INSERT ON provider_contract_model_protocols
                 WHEN NEW.protocol = 'messages'
                 BEGIN SELECT RAISE(ABORT, 'injected second observation write failure'); END;",
            )
            .unwrap();
        let before_failed = db.load_persisted_scope(&go).unwrap().unwrap().revision;
        let failed = db.upsert_model_protocols(&[
            probe_observation(
                go.clone(),
                "glm-5.3",
                UpstreamProtocolKind::ChatCompletions,
                now,
            ),
            probe_observation(go.clone(), "glm-5.3", UpstreamProtocolKind::Messages, now),
        ]);
        assert!(failed.is_err(), "{failed:?}");
        assert_eq!(
            db.load_persisted_scope(&go).unwrap().unwrap().revision,
            before_failed
        );
        assert!(
            db.load_model_protocol(&go, "glm-5.3", UpstreamProtocolKind::ChatCompletions)
                .unwrap()
                .is_none()
        );
        assert!(
            db.load_model_protocol(&go, "glm-5.3", UpstreamProtocolKind::Messages)
                .unwrap()
                .is_none()
        );

        drop(db);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn scnet_offerings_share_one_persisted_provider_scope() {
        let dir = temp_data_dir("v26-shared-scnet");
        let db = Database::open(dir.clone()).unwrap();
        let now = Utc::now();
        let scope = ContractScope::from_offering(
            SCNET_PROVIDER_ID,
            SCNET_TOKEN_PLAN_BASIC_OFFERING_ID,
            None,
        )
        .unwrap();
        db.set_protocol_switch(&scope, UpstreamProtocolKind::ChatCompletions, false, now)
            .unwrap();
        let premium = ContractScope::from_offering(
            SCNET_PROVIDER_ID,
            SCNET_TOKEN_PLAN_PREMIUM_OFFERING_ID,
            None,
        )
        .unwrap();
        assert_eq!(scope, premium);
        let loaded = db.load_persisted_scope(&premium).unwrap().unwrap();
        assert!(!loaded.switches.chat_completions);
        drop(db);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn v23_persists_verification_custom_config_capabilities_and_acks() {
        let dir = temp_data_dir("v23-contracts");
        let db = Database::open(dir.clone()).unwrap();
        let mut goat = account("goat-draft");
        goat.provider_id = COMMAND_CODE_PROVIDER_ID.to_string();
        goat.offering_id = GOAT_OFFERING_ID.to_string();
        goat.enabled = false;
        db.create_account(&goat).unwrap();
        let goat_state = db
            .account_verification_state("goat-draft")
            .unwrap()
            .unwrap();
        assert_eq!(goat_state.status, ConnectionVerificationStatus::Pending);

        let mut custom = account("custom-1");
        custom.provider_id = CUSTOM_PROVIDER_ID.to_string();
        custom.offering_id = CUSTOM_API_OFFERING_ID.to_string();
        custom.enabled = false;
        db.create_account(&custom).unwrap();
        db.upsert_account_custom_config(
            "custom-1",
            &AccountCustomConfigInput {
                base_url: "https://api.example.com/v1".into(),
                upstream_protocol: UpstreamProtocolKind::ChatCompletions,
                auth_scheme: UpstreamAuthScheme::Bearer,
            },
            true,
        )
        .unwrap();
        let rejected = db.upsert_account_custom_config(
            "custom-1",
            &AccountCustomConfigInput {
                base_url: "https://api.example.com/v1".into(),
                upstream_protocol: UpstreamProtocolKind::Messages,
                auth_scheme: UpstreamAuthScheme::Bearer,
            },
            false,
        );
        assert!(
            rejected.is_err(),
            "protocol must stay immutable after create"
        );
        db.replace_account_model_capabilities(
            "custom-1",
            &[AccountModelCapabilityInput {
                model_id: "deepseek/deepseek-v4-flash".into(),
                protocol: UpstreamProtocolKind::ChatCompletions,
                source: Some("manual".into()),
            }],
        )
        .unwrap();
        let capabilities = db.list_account_model_capabilities("custom-1").unwrap();
        assert_eq!(capabilities[0].model_id, "deepseek/deepseek-v4-flash");

        let mut scnet = account("scnet-1");
        scnet.provider_id = SCNET_PROVIDER_ID.to_string();
        scnet.offering_id = SCNET_TOKEN_PLAN_BASIC_OFFERING_ID.to_string();
        scnet.enabled = false;
        db.create_account(&scnet).unwrap();
        let scnet_notice = builtin_plan(SCNET_PROVIDER_ID, SCNET_TOKEN_PLAN_BASIC_OFFERING_ID)
            .and_then(|plan| plan.risk_notice)
            .expect("SCNet plans require a versioned risk notice");
        db.record_account_acknowledgement("scnet-1", scnet_notice, Utc::now())
            .unwrap();
        assert!(
            db.account_has_acknowledgement("scnet-1", scnet_notice)
                .unwrap()
        );

        db.set_account_verification(
            "custom-1",
            ConnectionVerificationStatus::Verified,
            Some(Utc::now()),
            None,
        )
        .unwrap();
        db.update_account(
            "custom-1",
            &AccountUpdate {
                key: Some("rotated".into()),
                ..AccountUpdate::default()
            },
            Some("new-cipher"),
            None,
        )
        .unwrap();
        let after_key = db.account_verification_state("custom-1").unwrap().unwrap();
        assert_eq!(after_key.status, ConnectionVerificationStatus::Pending);
        let caps_after_key = db.list_account_model_capabilities("custom-1").unwrap();
        assert_eq!(caps_after_key.len(), 1);
        assert_eq!(caps_after_key[0].model_id, "deepseek/deepseek-v4-flash");

        let unknown = account("unknown");
        let mut unknown = unknown;
        unknown.provider_id = "no-such-provider".into();
        unknown.offering_id = "no-such-offering".into();
        assert!(db.create_account(&unknown).is_err());

        drop(db);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn forward_logs_dual_write_native_usd_attribution() {
        let dir = temp_data_dir("v23-native-logs");
        let db = Database::open(dir.clone()).unwrap();
        db.create_account(&account("priced")).unwrap();
        let mut log = forward_log("priced", "success", 1.25);
        log.raw_cost_usd = Some(1.25);
        log.cost_state = "priced".into();
        let id = db.log_forward(&log).unwrap();
        let attribution = db.forward_log_native_attribution(id).unwrap().unwrap();
        assert_eq!(attribution.native_cost_value, Some(1.25));
        assert_eq!(attribution.native_cost_unit.as_deref(), Some("usd"));
        assert_eq!(attribution.native_cost_currency.as_deref(), Some("USD"));
        assert_eq!(attribution.upstream_model.as_deref(), Some("test"));

        db.set_forward_log_native_attribution(
            id,
            &ForwardLogNativeAttribution {
                requested_model: Some("deepseek-v4-flash".into()),
                resolved_alias: Some("deepseek-v4-flash".into()),
                upstream_model: Some("deepseek/deepseek-v4-flash".into()),
                native_cost_value: Some(12.0),
                native_cost_unit: Some("credits".into()),
                native_cost_currency: None,
            },
        )
        .unwrap();
        let updated = db.forward_log_native_attribution(id).unwrap().unwrap();
        assert_eq!(
            updated.upstream_model.as_deref(),
            Some("deepseek/deepseek-v4-flash")
        );
        assert_eq!(updated.native_cost_unit.as_deref(), Some("credits"));

        drop(db);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn update_forward_log_finalizes_native_usd_with_cost_fields() {
        let dir = temp_data_dir("v23-native-finalize");
        let db = Database::open(dir.clone()).unwrap();
        db.create_account(&account("stream")).unwrap();

        let mut streaming = forward_log("stream", "streaming", 0.0);
        streaming.cost = None;
        streaming.raw_cost_usd = None;
        streaming.cost_state = "not_applicable".into();
        streaming.provider_id = Some(OPENCODE_PROVIDER_ID.to_string());
        streaming.offering_id = Some(GO_OFFERING_ID.to_string());
        let streaming_id = db.log_forward(&streaming).unwrap();
        let preliminary = db
            .forward_log_native_attribution(streaming_id)
            .unwrap()
            .unwrap();
        assert_eq!(preliminary.native_cost_value, None);
        assert_eq!(preliminary.native_cost_unit, None);

        db.update_forward_log(
            streaming_id,
            "success",
            Some(200),
            ForwardMetrics {
                cost: 1.25,
                raw_cost_usd: Some(1.25),
                cost_state: "priced",
                ..ForwardMetrics::default()
            },
            None,
            None,
        )
        .unwrap();
        let finalized = db
            .forward_log_native_attribution(streaming_id)
            .unwrap()
            .unwrap();
        assert_eq!(finalized.native_cost_value, Some(1.25));
        assert_eq!(finalized.native_cost_unit.as_deref(), Some("usd"));
        assert_eq!(finalized.native_cost_currency.as_deref(), Some("USD"));
        let stored_cost: f64 = db
            .conn
            .query_row(
                "SELECT cost FROM forward_logs WHERE id = ?1",
                [streaming_id],
                |row| row.get(0),
            )
            .unwrap();
        assert!((stored_cost - 1.25).abs() < 1e-9);

        let zero_id = db
            .log_forward(&forward_log("stream", "streaming", 0.0))
            .unwrap();
        assert_eq!(
            db.forward_log_native_attribution(zero_id)
                .unwrap()
                .unwrap()
                .native_cost_value,
            Some(0.0)
        );
        db.update_forward_log(
            zero_id,
            "success",
            None,
            ForwardMetrics {
                cost: 2.5,
                raw_cost_usd: Some(2.5),
                cost_state: "priced",
                ..ForwardMetrics::default()
            },
            None,
            None,
        )
        .unwrap();
        assert_eq!(
            db.forward_log_native_attribution(zero_id)
                .unwrap()
                .unwrap()
                .native_cost_value,
            Some(2.5)
        );

        let mut zen = forward_log("stream", "streaming", 0.0);
        zen.cost = None;
        zen.raw_cost_usd = None;
        zen.cost_state = "not_applicable".into();
        zen.provider_id = Some(OPENCODE_ZEN_FREE_PROVIDER_ID.to_string());
        zen.offering_id = Some(ANONYMOUS_FREE_OFFERING_ID.to_string());
        let zen_id = db.log_forward(&zen).unwrap();
        db.update_forward_log(
            zen_id,
            "success",
            Some(200),
            ForwardMetrics {
                cost: 1.0,
                raw_cost_usd: Some(1.0),
                cost_state: "priced",
                ..ForwardMetrics::default()
            },
            None,
            None,
        )
        .unwrap();
        let zen_native = db.forward_log_native_attribution(zen_id).unwrap().unwrap();
        assert_eq!(zen_native.native_cost_value, Some(0.0));
        assert_eq!(zen_native.native_cost_unit.as_deref(), Some("usd"));
        let zen_cost: (f64, String) = db
            .conn
            .query_row(
                "SELECT cost, cost_state FROM forward_logs WHERE id = ?1",
                [zen_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(zen_cost.0, 0.0);
        assert_eq!(zen_cost.1, "free");

        drop(db);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn create_account_with_contract_is_atomic_on_custom_config_failure() {
        let dir = temp_data_dir("v23-atomic-create");
        let db = Database::open(dir.clone()).unwrap();
        let mut custom = account("custom-atomic");
        custom.provider_id = CUSTOM_PROVIDER_ID.to_string();
        custom.offering_id = CUSTOM_API_OFFERING_ID.to_string();
        custom.enabled = false;
        db.conn
            .execute_batch(
                "CREATE TRIGGER fail_custom_config
                 BEFORE INSERT ON account_custom_configs
                 BEGIN
                     SELECT RAISE(ABORT, 'forced custom config failure');
                 END;",
            )
            .unwrap();

        let error = db
            .create_account_with_contract(
                &custom,
                Some(&AccountCustomConfigInput {
                    base_url: "https://api.example.com/v1".into(),
                    upstream_protocol: UpstreamProtocolKind::ChatCompletions,
                    auth_scheme: UpstreamAuthScheme::Bearer,
                }),
                &[AccountModelCapabilityInput {
                    model_id: "org/model".into(),
                    protocol: UpstreamProtocolKind::ChatCompletions,
                    source: None,
                }],
                None,
            )
            .expect_err("forced custom config failure should abort the create");
        assert!(
            error.to_string().contains("forced custom config failure"),
            "{error}"
        );
        assert!(db.get_account("custom-atomic").unwrap().is_none());
        assert!(db.account_custom_config("custom-atomic").unwrap().is_none());
        assert!(
            db.list_account_model_capabilities("custom-atomic")
                .unwrap()
                .is_empty()
        );

        db.conn
            .execute_batch("DROP TRIGGER fail_custom_config;")
            .unwrap();
        db.create_account_with_contract(
            &custom,
            Some(&AccountCustomConfigInput {
                base_url: "https://api.example.com/v1".into(),
                upstream_protocol: UpstreamProtocolKind::ChatCompletions,
                auth_scheme: UpstreamAuthScheme::Bearer,
            }),
            &[AccountModelCapabilityInput {
                model_id: "org/model".into(),
                protocol: UpstreamProtocolKind::ChatCompletions,
                source: None,
            }],
            None,
        )
        .unwrap();
        assert!(db.get_account("custom-atomic").unwrap().is_some());
        assert!(db.account_custom_config("custom-atomic").unwrap().is_some());

        let mut go = account("go-rejects-custom");
        let rejected = db.create_account_with_contract(
            &go,
            Some(&AccountCustomConfigInput {
                base_url: "https://api.example.com/v1".into(),
                upstream_protocol: UpstreamProtocolKind::ChatCompletions,
                auth_scheme: UpstreamAuthScheme::Bearer,
            }),
            &[],
            None,
        );
        assert!(rejected.is_err(), "non-Custom accounts must reject config");
        assert!(db.get_account("go-rejects-custom").unwrap().is_none());

        go.id = "go-rejects-caps".into();
        go.name = "go-rejects-caps".into();
        let rejected_caps = db.create_account_with_contract(
            &go,
            None,
            &[AccountModelCapabilityInput {
                model_id: "org/model".into(),
                protocol: UpstreamProtocolKind::ChatCompletions,
                source: None,
            }],
            None,
        );
        assert!(
            rejected_caps.is_err(),
            "non-Custom accounts must reject capabilities"
        );
        assert!(db.get_account("go-rejects-caps").unwrap().is_none());

        let mut custom_empty = account("custom-empty-caps");
        custom_empty.provider_id = CUSTOM_PROVIDER_ID.to_string();
        custom_empty.offering_id = CUSTOM_API_OFFERING_ID.to_string();
        custom_empty.enabled = false;
        let empty_caps = db.create_account_with_contract(
            &custom_empty,
            Some(&AccountCustomConfigInput {
                base_url: "https://api.example.com/v1".into(),
                upstream_protocol: UpstreamProtocolKind::ChatCompletions,
                auth_scheme: UpstreamAuthScheme::Bearer,
            }),
            &[],
            None,
        );
        assert!(
            empty_caps.is_err(),
            "Custom create must require at least one model capability"
        );
        assert!(db.get_account("custom-empty-caps").unwrap().is_none());

        drop(db);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn custom_capability_protocol_must_match_config_and_mismatch_is_atomic() {
        let dir = temp_data_dir("custom-protocol-mismatch");
        let db = Database::open(dir.clone()).unwrap();
        let mut custom = account("custom-protocol");
        custom.provider_id = CUSTOM_PROVIDER_ID.to_string();
        custom.offering_id = CUSTOM_API_OFFERING_ID.to_string();
        custom.enabled = false;
        let mismatch = db.create_account_with_contract(
            &custom,
            Some(&AccountCustomConfigInput {
                base_url: "https://api.example.com/v1".into(),
                upstream_protocol: UpstreamProtocolKind::Messages,
                auth_scheme: UpstreamAuthScheme::Bearer,
            }),
            &[AccountModelCapabilityInput {
                model_id: "org/model".into(),
                protocol: UpstreamProtocolKind::ChatCompletions,
                source: None,
            }],
            None,
        );
        assert!(
            mismatch
                .unwrap_err()
                .to_string()
                .contains("must match account custom_config.upstream_protocol")
        );
        assert!(db.get_account("custom-protocol").unwrap().is_none());

        db.create_account_with_contract(
            &custom,
            Some(&AccountCustomConfigInput {
                base_url: "https://api.example.com/v1".into(),
                upstream_protocol: UpstreamProtocolKind::Messages,
                auth_scheme: UpstreamAuthScheme::Bearer,
            }),
            &[AccountModelCapabilityInput {
                model_id: "org/model".into(),
                protocol: UpstreamProtocolKind::Messages,
                source: None,
            }],
            None,
        )
        .unwrap();
        let rejected = db.replace_account_model_capabilities(
            "custom-protocol",
            &[AccountModelCapabilityInput {
                model_id: "org/other".into(),
                protocol: UpstreamProtocolKind::ChatCompletions,
                source: None,
            }],
        );
        assert!(
            rejected
                .unwrap_err()
                .to_string()
                .contains("must match account custom_config.upstream_protocol")
        );
        let kept = db
            .list_account_model_capabilities("custom-protocol")
            .unwrap();
        assert_eq!(kept.len(), 1);
        assert_eq!(kept[0].model_id, "org/model");
        assert_eq!(kept[0].protocol, UpstreamProtocolKind::Messages);

        drop(db);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn custom_mutations_repend_and_disable_verified_accounts() {
        let dir = temp_data_dir("custom-lifecycle-stale");
        let db = Database::open(dir.clone()).unwrap();
        let mut custom = account("custom-stale");
        custom.provider_id = CUSTOM_PROVIDER_ID.to_string();
        custom.offering_id = CUSTOM_API_OFFERING_ID.to_string();
        custom.enabled = false;
        db.create_account_with_contract(
            &custom,
            Some(&AccountCustomConfigInput {
                base_url: "https://api.example.com/v1".into(),
                upstream_protocol: UpstreamProtocolKind::ChatCompletions,
                auth_scheme: UpstreamAuthScheme::Bearer,
            }),
            &[AccountModelCapabilityInput {
                model_id: "org/model".into(),
                protocol: UpstreamProtocolKind::ChatCompletions,
                source: None,
            }],
            None,
        )
        .unwrap();
        db.set_account_verification(
            "custom-stale",
            ConnectionVerificationStatus::Verified,
            Some(Utc::now()),
            Some("previous"),
        )
        .unwrap();
        db.conn
            .execute(
                "UPDATE accounts SET enabled = 1 WHERE id = 'custom-stale'",
                [],
            )
            .unwrap();

        db.upsert_account_custom_config(
            "custom-stale",
            &AccountCustomConfigInput {
                base_url: "https://api.example.net/v2".into(),
                upstream_protocol: UpstreamProtocolKind::ChatCompletions,
                auth_scheme: UpstreamAuthScheme::Bearer,
            },
            false,
        )
        .unwrap();
        let after_url = db.get_account("custom-stale").unwrap().unwrap();
        let after_url_state = db
            .account_verification_state("custom-stale")
            .unwrap()
            .unwrap();
        assert!(!after_url.enabled);
        assert_eq!(
            after_url_state.status,
            ConnectionVerificationStatus::Pending
        );
        assert!(after_url_state.connection_verified_at.is_none());
        assert!(after_url_state.verification_error.is_none());

        db.set_account_verification(
            "custom-stale",
            ConnectionVerificationStatus::Verified,
            Some(Utc::now()),
            None,
        )
        .unwrap();
        db.conn
            .execute(
                "UPDATE accounts SET enabled = 1 WHERE id = 'custom-stale'",
                [],
            )
            .unwrap();
        db.replace_account_model_capabilities(
            "custom-stale",
            &[AccountModelCapabilityInput {
                model_id: "org/other".into(),
                protocol: UpstreamProtocolKind::ChatCompletions,
                source: None,
            }],
        )
        .unwrap();
        let after_caps = db.get_account("custom-stale").unwrap().unwrap();
        let after_caps_state = db
            .account_verification_state("custom-stale")
            .unwrap()
            .unwrap();
        assert!(!after_caps.enabled);
        assert_eq!(
            after_caps_state.status,
            ConnectionVerificationStatus::Pending
        );
        assert!(after_caps_state.connection_verified_at.is_none());

        db.set_account_verification(
            "custom-stale",
            ConnectionVerificationStatus::Verified,
            Some(Utc::now()),
            Some("stale"),
        )
        .unwrap();
        db.conn
            .execute(
                "UPDATE accounts SET enabled = 1 WHERE id = 'custom-stale'",
                [],
            )
            .unwrap();
        db.update_account(
            "custom-stale",
            &AccountUpdate {
                key: Some("rotated".into()),
                enabled: Some(true),
                ..AccountUpdate::default()
            },
            Some("new-cipher"),
            None,
        )
        .unwrap();
        let after_key = db.get_account("custom-stale").unwrap().unwrap();
        let after_key_state = db
            .account_verification_state("custom-stale")
            .unwrap()
            .unwrap();
        assert!(!after_key.enabled);
        assert_eq!(
            after_key_state.status,
            ConnectionVerificationStatus::Pending
        );
        assert!(after_key_state.connection_verified_at.is_none());
        assert!(after_key_state.verification_error.is_none());
        let caps_after_key = db.list_account_model_capabilities("custom-stale").unwrap();
        assert_eq!(caps_after_key.len(), 1);
        assert_eq!(caps_after_key[0].model_id, "org/other");

        drop(db);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn custom_verification_cas_rejects_stale_key_config_caps_and_delete() {
        let dir = temp_data_dir("custom-verify-cas");
        let mut db = Database::open(dir.clone()).unwrap();
        let mut custom = account("custom-cas");
        custom.provider_id = CUSTOM_PROVIDER_ID.to_string();
        custom.offering_id = CUSTOM_API_OFFERING_ID.to_string();
        custom.enabled = false;
        custom.key_cipher = "cipher-a".into();
        db.create_account_with_contract(
            &custom,
            Some(&AccountCustomConfigInput {
                base_url: "https://api.example.com/v1".into(),
                upstream_protocol: UpstreamProtocolKind::ChatCompletions,
                auth_scheme: UpstreamAuthScheme::Bearer,
            }),
            &[AccountModelCapabilityInput {
                model_id: "one".into(),
                protocol: UpstreamProtocolKind::ChatCompletions,
                source: None,
            }],
            None,
        )
        .unwrap();

        let contract = db
            .capture_custom_verification_contract("custom-cas")
            .unwrap()
            .unwrap();
        assert_eq!(contract.key_cipher, "cipher-a");
        assert_eq!(contract.capabilities[0].0, "one");

        db.update_account(
            "custom-cas",
            &AccountUpdate {
                key: Some("rotated".into()),
                ..AccountUpdate::default()
            },
            Some("cipher-b"),
            None,
        )
        .unwrap();
        assert!(
            !db.commit_custom_verification_if_contract_matches(
                &contract,
                ConnectionVerificationStatus::Verified,
                Some(Utc::now()),
                None,
            )
            .unwrap()
        );
        assert_eq!(
            db.account_verification_state("custom-cas")
                .unwrap()
                .unwrap()
                .status,
            ConnectionVerificationStatus::Pending
        );

        let after_key = db
            .capture_custom_verification_contract("custom-cas")
            .unwrap()
            .unwrap();
        db.upsert_account_custom_config(
            "custom-cas",
            &AccountCustomConfigInput {
                base_url: "https://api.example.net/v2".into(),
                upstream_protocol: UpstreamProtocolKind::ChatCompletions,
                auth_scheme: UpstreamAuthScheme::Bearer,
            },
            false,
        )
        .unwrap();
        assert!(
            !db.commit_custom_verification_if_contract_matches(
                &after_key,
                ConnectionVerificationStatus::Verified,
                Some(Utc::now()),
                None,
            )
            .unwrap()
        );

        let after_config = db
            .capture_custom_verification_contract("custom-cas")
            .unwrap()
            .unwrap();
        db.replace_account_model_capabilities(
            "custom-cas",
            &[AccountModelCapabilityInput {
                model_id: "two".into(),
                protocol: UpstreamProtocolKind::ChatCompletions,
                source: None,
            }],
        )
        .unwrap();
        assert!(
            !db.commit_custom_verification_if_contract_matches(
                &after_config,
                ConnectionVerificationStatus::Verified,
                Some(Utc::now()),
                None,
            )
            .unwrap()
        );

        let matching = db
            .capture_custom_verification_contract("custom-cas")
            .unwrap()
            .unwrap();
        assert!(
            db.commit_custom_verification_if_contract_matches(
                &matching,
                ConnectionVerificationStatus::Verified,
                Some(Utc::now()),
                None,
            )
            .unwrap()
        );
        assert_eq!(
            db.account_verification_state("custom-cas")
                .unwrap()
                .unwrap()
                .status,
            ConnectionVerificationStatus::Verified
        );
        assert!(
            !db.commit_custom_verification_if_contract_matches(
                &matching,
                ConnectionVerificationStatus::Failed,
                None,
                Some("stale"),
            )
            .unwrap()
        );
        assert_eq!(
            db.account_verification_state("custom-cas")
                .unwrap()
                .unwrap()
                .status,
            ConnectionVerificationStatus::Verified
        );

        let mut leftover = account("custom-delete");
        leftover.provider_id = CUSTOM_PROVIDER_ID.to_string();
        leftover.offering_id = CUSTOM_API_OFFERING_ID.to_string();
        leftover.enabled = false;
        db.create_account_with_contract(
            &leftover,
            Some(&AccountCustomConfigInput {
                base_url: "https://api.example.com/v1".into(),
                upstream_protocol: UpstreamProtocolKind::ChatCompletions,
                auth_scheme: UpstreamAuthScheme::Bearer,
            }),
            &[AccountModelCapabilityInput {
                model_id: "one".into(),
                protocol: UpstreamProtocolKind::ChatCompletions,
                source: None,
            }],
            None,
        )
        .unwrap();
        let deleted_contract = db
            .capture_custom_verification_contract("custom-delete")
            .unwrap()
            .unwrap();
        db.delete_account("custom-delete").unwrap();
        assert!(
            !db.commit_custom_verification_if_contract_matches(
                &deleted_contract,
                ConnectionVerificationStatus::Verified,
                Some(Utc::now()),
                None,
            )
            .unwrap()
        );

        drop(db);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn unroutable_catalog_plans_cannot_persist_enabled_true() {
        let dir = temp_data_dir("enablement-gate");
        let db = Database::open(dir.clone()).unwrap();
        let mut go = account("go-enabled");
        go.enabled = true;
        db.create_account(&go).unwrap();
        assert!(db.get_account("go-enabled").unwrap().unwrap().enabled);

        for plan in BUILTIN_PLANS
            .iter()
            .copied()
            .filter(|plan| !plan.routable && plan.offering.singleton_account_id.is_none())
        {
            let id = format!("draft-{}", plan.offering.offering_id);
            let mut draft = account(&id);
            draft.provider_id = plan.offering.provider_id.to_string();
            draft.offering_id = plan.offering.offering_id.to_string();
            draft.credential_kind = plan.offering.credential_kind;
            draft.quota_scope = plan.offering.quota_scope;
            draft.enabled = true;
            let error = db
                .create_account(&draft)
                .expect_err("enabled unroutable create must fail closed");
            assert!(
                error.to_string().contains("not routable"),
                "{}/{}: {error}",
                plan.offering.provider_id,
                plan.offering.offering_id
            );
            assert!(db.get_account(&id).unwrap().is_none());

            draft.enabled = false;
            if plan_requires_custom_config(plan) {
                db.create_account_with_contract(
                    &draft,
                    Some(&AccountCustomConfigInput {
                        base_url: "https://api.example.com/v1".into(),
                        upstream_protocol: UpstreamProtocolKind::ChatCompletions,
                        auth_scheme: UpstreamAuthScheme::Bearer,
                    }),
                    &[AccountModelCapabilityInput {
                        model_id: "org/model".into(),
                        protocol: UpstreamProtocolKind::ChatCompletions,
                        source: None,
                    }],
                    plan.risk_notice,
                )
                .unwrap();
            } else {
                db.create_account_with_contract(&draft, None, &[], plan.risk_notice)
                    .unwrap();
            }
            let stored = db.get_account(&id).unwrap().unwrap();
            assert!(!stored.enabled, "{id} draft must stay disabled");
            let before = stored.updated_at;

            let enable_error = db
                .update_account(
                    &id,
                    &AccountUpdate {
                        enabled: Some(true),
                        ..AccountUpdate::default()
                    },
                    None,
                    None,
                )
                .expect_err("enable must fail closed");
            assert!(
                enable_error.to_string().contains("not routable"),
                "{id}: {enable_error}"
            );
            let after_reject = db.get_account(&id).unwrap().unwrap();
            assert!(!after_reject.enabled);
            assert_eq!(after_reject.updated_at, before);
            assert_eq!(after_reject.name, stored.name);

            db.update_account(
                &id,
                &AccountUpdate {
                    name: Some(format!("{id}-renamed")),
                    ..AccountUpdate::default()
                },
                None,
                None,
            )
            .unwrap();
            db.update_account(
                &id,
                &AccountUpdate {
                    enabled: Some(false),
                    ..AccountUpdate::default()
                },
                None,
                None,
            )
            .unwrap();
            let edited = db.get_account(&id).unwrap().unwrap();
            assert!(!edited.enabled);
            assert_eq!(edited.name, format!("{id}-renamed"));
        }

        db.update_account(
            "go-enabled",
            &AccountUpdate {
                enabled: Some(false),
                ..AccountUpdate::default()
            },
            None,
            None,
        )
        .unwrap();
        db.update_account(
            "go-enabled",
            &AccountUpdate {
                enabled: Some(true),
                ..AccountUpdate::default()
            },
            None,
            None,
        )
        .unwrap();
        assert!(db.get_account("go-enabled").unwrap().unwrap().enabled);

        drop(db);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn open_sanitizes_unroutable_catalog_leftovers_without_touching_go_zen_or_unknown() {
        let unroutable: Vec<_> = BUILTIN_PLANS
            .iter()
            .copied()
            .filter(|plan| !plan.routable)
            .collect();
        assert_eq!(
            unroutable
                .iter()
                .map(|plan| (plan.offering.provider_id, plan.offering.offering_id))
                .collect::<Vec<_>>(),
            vec![
                (COMMAND_CODE_PROVIDER_ID, GOAT_OFFERING_ID),
                (SCNET_PROVIDER_ID, SCNET_TOKEN_PLAN_BASIC_OFFERING_ID),
                (SCNET_PROVIDER_ID, SCNET_TOKEN_PLAN_STANDARD_OFFERING_ID),
                (SCNET_PROVIDER_ID, SCNET_TOKEN_PLAN_PREMIUM_OFFERING_ID),
            ]
        );
        assert!(
            builtin_plan(CUSTOM_PROVIDER_ID, CUSTOM_API_OFFERING_ID)
                .is_some_and(|plan| plan.routable)
        );

        let dir = temp_data_dir("unroutable-sanitation");
        let db = Database::open(dir.clone()).unwrap();

        let mut go = account("go-keep");
        go.notes = Some("go-notes".into());
        db.create_account(&go).unwrap();
        leftover_enable(&db, "go-keep");

        let mut unknown = account("unknown-keep");
        unknown.notes = Some("unknown-notes".into());
        db.create_account(&unknown).unwrap();
        leftover_enable(&db, "unknown-keep");
        db.conn
            .execute(
                "UPDATE accounts
                 SET provider_id = 'unknown-provider', offering_id = 'unknown-offering'
                 WHERE id = 'unknown-keep'",
                [],
            )
            .unwrap();

        persist_unroutable_draft(
            &db,
            builtin_plan(COMMAND_CODE_PROVIDER_ID, GOAT_OFFERING_ID).unwrap(),
            "goat-pending",
            "goat-pending-notes",
        );
        leftover_enable(&db, "goat-pending");

        persist_unroutable_draft(
            &db,
            builtin_plan(COMMAND_CODE_PROVIDER_ID, GOAT_OFFERING_ID).unwrap(),
            "goat-verified",
            "goat-verified-notes",
        );
        db.conn
            .execute(
                "UPDATE accounts
                 SET enabled = 1, verification_status = 'verified', verification_error = NULL
                 WHERE id = 'goat-verified'",
                [],
            )
            .unwrap();

        persist_unroutable_draft(
            &db,
            builtin_plan(COMMAND_CODE_PROVIDER_ID, GOAT_OFFERING_ID).unwrap(),
            "goat-failed",
            "goat-failed-notes",
        );
        db.conn
            .execute(
                "UPDATE accounts
                 SET enabled = 1, verification_status = 'failed', verification_error = 'boom'
                 WHERE id = 'goat-failed'",
                [],
            )
            .unwrap();

        for plan in unroutable.iter().filter(|plan| {
            !(plan.offering.provider_id == COMMAND_CODE_PROVIDER_ID
                && plan.offering.offering_id == GOAT_OFFERING_ID)
        }) {
            let id = format!("draft-{}", plan.offering.offering_id);
            persist_unroutable_draft(&db, *plan, &id, &format!("{id}-notes"));
            leftover_enable(&db, &id);
            if plan.offering.offering_id == SCNET_TOKEN_PLAN_STANDARD_OFFERING_ID {
                db.conn
                    .execute(
                        "UPDATE accounts SET verification_status = 'verified' WHERE id = ?1",
                        [&id],
                    )
                    .unwrap();
            }
        }

        persist_unroutable_draft(
            &db,
            builtin_plan(CUSTOM_PROVIDER_ID, CUSTOM_API_OFFERING_ID).unwrap(),
            "draft-api",
            "draft-api-notes",
        );
        leftover_enable(&db, "draft-api");

        let zen_before = sanitation_snapshot(&db, ZEN_FREE_ACCOUNT_ID);
        let go_before = sanitation_snapshot(&db, "go-keep");
        let unknown_before = sanitation_snapshot(&db, "unknown-keep");
        let goat_pending_before = sanitation_snapshot(&db, "goat-pending");
        let goat_verified_before = sanitation_snapshot(&db, "goat-verified");
        let goat_failed_before = sanitation_snapshot(&db, "goat-failed");
        let custom_before = sanitation_snapshot(&db, "draft-api");
        assert!(go_before.enabled);
        assert!(custom_before.enabled);
        assert!(unknown_before.enabled);
        assert!(goat_pending_before.enabled);
        assert!(goat_verified_before.enabled);
        assert!(goat_failed_before.enabled);
        assert_eq!(
            goat_pending_before.verification,
            ConnectionVerificationStatus::Pending
        );
        assert_eq!(
            goat_verified_before.verification,
            ConnectionVerificationStatus::Verified
        );
        assert_eq!(
            goat_failed_before.verification,
            ConnectionVerificationStatus::Failed
        );

        drop(db);
        let db = Database::open(dir.clone()).unwrap();

        let zen_after = sanitation_snapshot(&db, ZEN_FREE_ACCOUNT_ID);
        let go_after = sanitation_snapshot(&db, "go-keep");
        let unknown_after = sanitation_snapshot(&db, "unknown-keep");
        assert_eq!(zen_after, zen_before);
        assert_eq!(go_after, go_before);
        assert_eq!(unknown_after, unknown_before);

        let goat_pending_after = sanitation_snapshot(&db, "goat-pending");
        assert!(!goat_pending_after.enabled);
        assert_eq!(goat_pending_after.name, goat_pending_before.name);
        assert_eq!(goat_pending_after.notes, goat_pending_before.notes);
        assert_eq!(
            goat_pending_after.updated_at,
            goat_pending_before.updated_at
        );
        assert_eq!(
            goat_pending_after.verification,
            ConnectionVerificationStatus::Pending
        );

        let goat_verified_after = sanitation_snapshot(&db, "goat-verified");
        assert!(!goat_verified_after.enabled);
        assert_eq!(goat_verified_after.name, goat_verified_before.name);
        assert_eq!(goat_verified_after.notes, goat_verified_before.notes);
        assert_eq!(
            goat_verified_after.updated_at,
            goat_verified_before.updated_at
        );
        assert_eq!(
            goat_verified_after.verification,
            ConnectionVerificationStatus::Verified
        );

        let goat_failed_after = sanitation_snapshot(&db, "goat-failed");
        assert!(!goat_failed_after.enabled);
        assert_eq!(goat_failed_after.name, goat_failed_before.name);
        assert_eq!(goat_failed_after.notes, goat_failed_before.notes);
        assert_eq!(goat_failed_after.updated_at, goat_failed_before.updated_at);
        assert_eq!(
            goat_failed_after.verification,
            ConnectionVerificationStatus::Pending
        );
        assert_eq!(goat_failed_after.verification_error, None);

        for plan in unroutable.iter().filter(|plan| {
            !(plan.offering.provider_id == COMMAND_CODE_PROVIDER_ID
                && plan.offering.offering_id == GOAT_OFFERING_ID)
        }) {
            let id = format!("draft-{}", plan.offering.offering_id);
            let after = sanitation_snapshot(&db, &id);
            assert!(!after.enabled, "{id} leftover must be disabled");
            assert_eq!(after.name, id);
            assert_eq!(after.notes.as_deref(), Some(format!("{id}-notes").as_str()));
            if plan.offering.offering_id == SCNET_TOKEN_PLAN_STANDARD_OFFERING_ID {
                assert_eq!(after.verification, ConnectionVerificationStatus::Verified);
            } else {
                assert_eq!(after.verification, ConnectionVerificationStatus::Pending);
            }
        }

        let custom_after = sanitation_snapshot(&db, "draft-api");
        assert_eq!(custom_after, custom_before);
        assert!(
            custom_after.enabled,
            "now-routable Custom leftovers must not be disabled at open"
        );

        let first_pass: Vec<_> = [
            ZEN_FREE_ACCOUNT_ID,
            "go-keep",
            "unknown-keep",
            "goat-pending",
            "goat-verified",
            "goat-failed",
            "draft-token-plan-basic",
            "draft-token-plan-standard",
            "draft-token-plan-premium",
            "draft-api",
        ]
        .into_iter()
        .map(|id| (id.to_string(), sanitation_snapshot(&db, id)))
        .collect();

        drop(db);
        let db = Database::open(dir.clone()).unwrap();
        for (id, expected) in &first_pass {
            assert_eq!(
                sanitation_snapshot(&db, id),
                *expected,
                "second open must be idempotent for {id}"
            );
        }

        db.update_account(
            "draft-token-plan-basic",
            &AccountUpdate {
                name: Some("draft-token-plan-basic-renamed".into()),
                ..AccountUpdate::default()
            },
            None,
            None,
        )
        .unwrap();
        let recovered = db.get_account("draft-token-plan-basic").unwrap().unwrap();
        assert!(!recovered.enabled);
        assert_eq!(recovered.name, "draft-token-plan-basic-renamed");

        drop(db);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn v22_open_sanitizes_enabled_unroutable_catalog_rows() {
        let dir = temp_data_dir("v22-unroutable-sanitation");
        create_v22_fixture(&dir);
        let conn = Connection::open(dir.join("data.sqlite")).expect("v22 fixture should reopen");
        for plan in BUILTIN_PLANS.iter().filter(|plan| {
            !(plan.routable
                || plan.offering.provider_id == COMMAND_CODE_PROVIDER_ID
                    && plan.offering.offering_id == GOAT_OFFERING_ID)
        }) {
            clone_account_row_as_enabled(
                &conn,
                "v22-goat",
                &format!("v22-{}", plan.offering.offering_id),
                plan.offering.provider_id,
                plan.offering.offering_id,
            );
        }
        clone_account_row_as_enabled(
            &conn,
            "v22-account",
            "v22-unknown",
            "unknown-provider",
            "unknown-offering",
        );
        drop(conn);

        let db = Database::open(dir.clone()).expect("v22 database should migrate and sanitize");
        assert!(db.get_account("v22-account").unwrap().unwrap().enabled);
        assert!(db.get_account("v22-unknown").unwrap().unwrap().enabled);
        assert!(
            db.get_account(ZEN_FREE_ACCOUNT_ID)
                .unwrap()
                .unwrap()
                .enabled
        );
        assert!(!db.get_account("v22-goat").unwrap().unwrap().enabled);
        for plan in BUILTIN_PLANS.iter().filter(|plan| {
            !(plan.routable
                || plan.offering.provider_id == COMMAND_CODE_PROVIDER_ID
                    && plan.offering.offering_id == GOAT_OFFERING_ID)
        }) {
            let id = format!("v22-{}", plan.offering.offering_id);
            let stored = db.get_account(&id).unwrap().unwrap();
            assert!(!stored.enabled, "{id}");
            assert_eq!(stored.provider_id, plan.offering.provider_id);
            assert_eq!(stored.offering_id, plan.offering.offering_id);
        }
        db.update_account(
            "v22-goat",
            &AccountUpdate {
                name: Some("v22-goat-renamed".into()),
                ..AccountUpdate::default()
            },
            None,
            None,
        )
        .unwrap();
        let renamed = db.get_account("v22-goat").unwrap().unwrap();
        assert!(!renamed.enabled);
        assert_eq!(renamed.name, "v22-goat-renamed");

        drop(db);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn v23_migration_failure_rolls_back_to_usable_v22_source_and_backup() {
        let dir = temp_data_dir("v23-atomic-migration");
        create_v22_fixture(&dir);
        let conn = Connection::open(dir.join("data.sqlite")).expect("v22 fixture should reopen");
        conn.execute_batch(
            "CREATE TRIGGER fail_v23_migration
             BEFORE INSERT ON schema_version
             WHEN NEW.version = 23
             BEGIN
                 SELECT RAISE(ABORT, 'forced v23 migration failure');
             END;",
        )
        .expect("fault-injection trigger should install");
        drop(conn);

        assert!(Database::open(dir.clone()).is_err());
        let conn = Connection::open(dir.join("data.sqlite")).expect("db should reopen");
        let columns = conn
            .prepare("PRAGMA table_info(accounts)")
            .expect("table info should prepare")
            .query_map([], |row| row.get::<_, String>(1))
            .expect("table info should query")
            .collect::<rusqlite::Result<Vec<_>>>()
            .expect("columns should load");
        assert!(!columns.iter().any(|name| name == "verification_status"));
        assert!(!table_exists(&conn, "account_custom_configs").unwrap());
        assert_eq!(schema_version_on(&conn).unwrap(), 22);
        let preserved_account: (String, String, i64) = conn
            .query_row(
                "SELECT name, key_cipher, enabled FROM accounts WHERE id = 'v22-account'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .expect("source account should remain readable");
        let preserved_goat: (String, i64) = conn
            .query_row(
                "SELECT name, enabled FROM accounts WHERE id = 'v22-goat'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .expect("source GOAT account should remain readable");
        let preserved_log: (String, String, String, f64) = conn
            .query_row(
                "SELECT account_id, model, status, cost FROM forward_logs LIMIT 1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .expect("source forward log should remain readable");
        assert_eq!(
            preserved_account,
            ("v22-account".into(), "cipher".into(), 1)
        );
        assert_eq!(preserved_goat, ("v22-goat".into(), 1));
        assert_eq!(
            preserved_log,
            ("v22-account".into(), "test".into(), "success".into(), 3.5)
        );
        drop(conn);

        let backups_before = pre_v23_backup_paths(&dir);
        assert_eq!(backups_before.len(), 1);
        let backup_bytes =
            fs::read(&backups_before[0]).expect("rollback backup should be readable");
        let backup =
            Connection::open_with_flags(&backups_before[0], OpenFlags::SQLITE_OPEN_READ_ONLY)
                .expect("pre-v23 backup should open");
        assert_eq!(schema_version_on(&backup).unwrap(), 22);
        assert!(!table_has_column(&backup, "accounts", "verification_status").unwrap());
        drop(backup);

        assert!(Database::open(dir.clone()).is_err());
        assert_eq!(pre_v23_backup_paths(&dir), backups_before);
        assert_eq!(
            fs::read(&backups_before[0]).expect("rollback backup should remain readable"),
            backup_bytes
        );
        fs::remove_dir_all(dir).expect("test data dir should be removed");
    }
}
