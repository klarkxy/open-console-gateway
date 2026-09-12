use crate::crypto::{KeyCipher, is_legacy_local_ciphertext};
use crate::custom::validate_custom_endpoint_url;
use crate::dynamic::DynamicProviderRuntime;
use crate::kernel::ids::{PRIMARY_KEY_ID, PRIMARY_KEY_NAME};
use crate::kernel::pricing::{
    PricingLimits, PricingSnapshot, ProviderPricingSnapshot, SEED_LIMITS,
};
use crate::models::*;
use crate::provider::*;
use crate::provider_contracts::{
    CATALOG_SOURCE_COMMAND_CODE_MODELS, CATALOG_SOURCE_OFFICIAL_ZEN, ContractEvidenceSource,
    ContractScope, PersistedContracts, PersistedModelProtocol, PersistedModelProtocolOverride,
    PersistedScopeRow, ProbeResultKind, ProtocolOverrideState, SCOPE_KIND_CUSTOM_ENDPOINT,
};
use anyhow::{Context, Result};
use chrono::{DateTime, Duration, Local, NaiveDate, TimeZone, Utc};
use ocg_domain::dynamic::{DynamicAuthKind, DynamicModelMapping};
use ocg_infra::sqlite_logs::{
    ForwardLogIdentityPatch, ForwardLogInsertRow, ForwardLogUpdateRow, GatewayLogInsertRow,
};
use rusqlite::{
    Connection, OpenFlags, OptionalExtension, Row, Transaction, TransactionBehavior, params,
    params_from_iter,
    types::{Type, Value},
};
use serde::de::Error as SerdeError;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::HashSet,
    fmt,
    fs::OpenOptions,
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::Arc,
};

pub struct Database {
    conn: Connection,
}

pub(crate) mod identity;
mod platform;

/// Local configuration for the one code-owned CPA external integration.
/// Both credential values stay encrypted outside the short-lived V3 write path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CpaIntegrationRecord {
    pub account_id: String,
    pub base_url: String,
    pub management_key_cipher: String,
}

/// One row from the persisted CPA `/v1/models` snapshot.
/// `owned_by` is CPA's reported source when present; legacy ID-only snapshots
/// keep it empty until the next explicit refresh.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CpaCatalogModel {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owned_by: Option<String>,
}

impl From<String> for CpaCatalogModel {
    fn from(id: String) -> Self {
        Self { id, owned_by: None }
    }
}

impl From<&str> for CpaCatalogModel {
    fn from(id: &str) -> Self {
        Self {
            id: id.to_string(),
            owned_by: None,
        }
    }
}

impl CpaCatalogModel {
    pub fn ids(models: &[Self]) -> Vec<String> {
        models.iter().map(|model| model.id.clone()).collect()
    }
}

/// Persisted Dashboard V4 operation ledger row. `payload_digest` is HMAC-SHA256
/// hex; `result_json` is secret-free.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DashboardOperationRow {
    pub operation_id: String,
    pub kind: String,
    pub payload_digest: String,
    pub result_json: String,
    pub created_at: String,
}

/// Insert payload for a new dashboard operation row. `created_at` is assigned
/// at write time.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewDashboardOperation {
    pub operation_id: String,
    pub kind: String,
    pub payload_digest: String,
    pub result_json: String,
}

const DASHBOARD_OPERATION_PRUNE_DAYS: i64 = 30;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CpaCatalogRecord {
    pub models: Vec<CpaCatalogModel>,
    pub refreshed_at: Option<DateTime<Utc>>,
    pub source_url: String,
}

/// Accepts both the current `[{id, owned_by}]` snapshot and the legacy
/// `["id"]` array written before sources were persisted.
fn parse_cpa_catalog_models(models_json: &str) -> Result<Vec<CpaCatalogModel>, serde_json::Error> {
    let value: serde_json::Value = serde_json::from_str(models_json)?;
    let Some(rows) = value.as_array() else {
        return Err(SerdeError::custom("CPA catalog must be a JSON array"));
    };
    let mut models = Vec::new();
    let mut seen = HashSet::new();
    for row in rows {
        let model = match row {
            serde_json::Value::String(id) => {
                let id = id.trim();
                if id.is_empty() {
                    continue;
                }
                CpaCatalogModel {
                    id: id.to_string(),
                    owned_by: None,
                }
            }
            serde_json::Value::Object(object) => {
                let Some(id) = object
                    .get("id")
                    .and_then(serde_json::Value::as_str)
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                else {
                    continue;
                };
                let owned_by = object
                    .get("owned_by")
                    .or_else(|| object.get("ownedBy"))
                    .and_then(serde_json::Value::as_str)
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .map(str::to_string);
                CpaCatalogModel {
                    id: id.to_string(),
                    owned_by,
                }
            }
            _ => continue,
        };
        if seen.insert(model.id.clone()) {
            models.push(model);
        }
    }
    Ok(models)
}

/// One fully validated account definition ready for an atomic migration import.
/// Plaintext credentials never enter this type; callers must encrypt them with
/// the destination `CoreState` cipher before acquiring the database mutex.
#[derive(Debug, Clone)]
pub struct AccountImportRecord {
    pub account: Account,
    pub custom_config: Option<AccountCustomConfigInput>,
    pub capabilities: Vec<AccountModelCapabilityInput>,
    pub verification_status: ConnectionVerificationStatus,
    pub connection_verified_at: Option<DateTime<Utc>>,
    pub ollama_billing_tier: Option<OllamaBillingTier>,
}

/// One fully validated, portable node-state snapshot. Stable IDs merge into an
/// existing destination; destination-only state is retained according to the
/// node migration rules. All database-owned state is committed in one
/// transaction.
#[derive(Debug, Clone)]
pub struct NodeImportRecord {
    pub platform_links_authoritative: bool,
    pub platform_accounts: Vec<crate::platform::PortablePlatformAccount>,
    pub platform_links: Vec<crate::platform::PortablePlatformLink>,
    pub accounts: Vec<AccountImportRecord>,
    pub account_order: Vec<String>,
    pub config_json: String,
    pub sub_keys: Vec<SubGatewayKey>,
    pub zen_free_enabled: bool,
    pub zen_catalog: crate::kernel::zen::ZenFreeModelCatalog,
    pub provider_contracts: PersistedContracts,
    pub dynamic_providers: Vec<DynamicProviderRuntime>,
    /// V6 portable identity/credential/binding/quota-pool snapshot. `None`
    /// keeps the v45 1:1 satellite mapper used for V4/V5 packages.
    pub(crate) identity_snapshot: Option<identity::IdentityImportSnapshot>,
    /// Provider ids that stay persisted drafts. Routing snapshots exclude them.
    pub draft_provider_ids: HashSet<String>,
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
/// Fresh unique SQLite snapshot taken after a database has reached canonical
/// v26 and before any v27 write. Not created for a brand-new empty database.
pub const PRE_V3_BACKUP_FILE_PREFIX: &str = "data.sqlite.pre-v3.";
/// Unique never-overwritten SQLite snapshot taken before a non-empty v34
/// database is rewritten to provider-only identity in v35.
pub const PRE_V35_BACKUP_FILE_PREFIX: &str = "data.sqlite.pre-v35.";
/// Unique never-overwritten SQLite snapshot taken before a non-empty v41
/// database is rewritten to the unified providers/provider_models tables.
pub const PRE_V42_BACKUP_FILE_PREFIX: &str = "data.sqlite.pre-v42.";
/// Highest schema this binary can open or migrate. Newer databases fail closed.
pub const CURRENT_SCHEMA_VERSION: i32 = 47;
/// Canonical source schema for the v35 provider-identity rewrite.
pub const V34_SCHEMA_VERSION: i32 = 34;
/// Historical v34 offering IDs. Used only by v1–v34 SQL and the v35 preflight
/// pair map; they are not re-exported from the domain catalog.
const V34_OFFERING_GO: &str = "go";
const V34_OFFERING_ANONYMOUS_FREE: &str = "anonymous-free";
const V34_OFFERING_GOAT: &str = "goat";
const V34_OFFERING_CN: &str = "cn";
const V34_OFFERING_API: &str = "api";
const V34_OFFERING_LOCAL: &str = "local";
const V34_KNOWN_PROVIDER_OFFERING_PAIRS: &[(&str, &str)] = &[
    (OPENCODE_PROVIDER_ID, V34_OFFERING_GO),
    (OPENCODE_ZEN_FREE_PROVIDER_ID, V34_OFFERING_ANONYMOUS_FREE),
    (COMMAND_CODE_PROVIDER_ID, V34_OFFERING_GOAT),
    (MINIMAX_PROVIDER_ID, V34_OFFERING_CN),
    (KIMI_PROVIDER_ID, V34_OFFERING_CN),
    (CUSTOM_PROVIDER_ID, V34_OFFERING_API),
    (CPA_PROVIDER_ID, V34_OFFERING_LOCAL),
];
pub const V27_SCHEMA_VERSION: i32 = 27;
/// Schema the v27 rewrite expects as its committed source. Historical databases
/// always migrate through this version first.
pub const V26_SCHEMA_VERSION: i32 = 26;
/// Canonical source schema for the v42 unified providers rewrite.
pub const V41_SCHEMA_VERSION: i32 = 41;
/// Bounded retries of the whole v27 preflight/backup when a writer races the
/// captured `PRAGMA data_version`.
const V27_WRITER_RACE_RETRIES: u32 = 8;
/// Fixed read size for streaming SHA-256 evidence of a pre-v3 backup.
const BACKUP_HASH_BUFFER_LEN: usize = 64 * 1024;
/// Ceiling on active (non-deleted, non-primary) access keys. Matches the
/// key-lifecycle API; tombstones do not count.
const MAX_ACTIVE_NON_PRIMARY_ACCESS_KEYS: i64 = 64;

const USAGE_SYNC_ACCOUNT_COLUMNS: &[&str] = &[
    "usage_sync_last_success_at",
    "usage_sync_last_attempt_at",
    "usage_sync_next_eligible_at",
    "usage_sync_failure_streak",
    "usage_sync_last_expedited_at",
];

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

/// Row identity captured before an asynchronous managed-key verification.
///
/// `updated_at` is the row version for this schema. Keeping the original
/// ciphertext in the fingerprint additionally catches the legacy V2 verify
/// path, which can replace a candidate key without advancing the V3 control
/// revision while the upstream request is in flight.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManagedKeyVerificationCas {
    pub key_cipher: String,
    pub updated_at: DateTime<Utc>,
    pub provider_id: String,
    pub account_type: AccountType,
    pub setup_step: AccountSetupStep,
}

impl ManagedKeyVerificationCas {
    pub fn from_account(account: &Account) -> Self {
        Self {
            key_cipher: account.key_cipher.clone(),
            updated_at: account.updated_at,
            provider_id: account.provider_id.clone(),
            account_type: account.account_type,
            setup_step: account.setup_step,
        }
    }
}

/// Sanitized rate-limit state accepted as a successful managed-key probe.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManagedKeyVerificationRateLimit {
    pub until: DateTime<Utc>,
    pub error: String,
    pub window: Option<UsageWindowKind>,
}

/// Persistent result of one V3 managed-key verification.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ManagedKeyVerificationWrite {
    Verified {
        rate_limit: Option<ManagedKeyVerificationRateLimit>,
        account_name: String,
    },
    AuthFailed {
        auth_error: String,
    },
    Pending,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ManagedKeyVerificationCommit {
    Applied,
    Conflict,
}

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
        revision: row.get::<_, i64>(6)? as u64,
        updated_at: parse_rfc3339_column(row.get(7)?, 7)?,
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

fn persist_override_from_row(row: &Row<'_>) -> rusqlite::Result<PersistedModelProtocolOverride> {
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
    let state_value: String = row.get(4)?;
    let state = ProtocolOverrideState::try_from(state_value.as_str()).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(
            4,
            Type::Text,
            Box::new(std::io::Error::other(error)),
        )
    })?;
    Ok(PersistedModelProtocolOverride {
        scope: scope_from_row(&kind, &id)?,
        model_id: row.get(2)?,
        protocol,
        state,
        updated_at: parse_rfc3339_column(row.get(5)?, 5)?,
    })
}

fn load_scope_on(conn: &Connection, scope: &ContractScope) -> Result<Option<PersistedScopeRow>> {
    conn.query_row(
        "SELECT scope_kind, scope_id, catalog_models_json, catalog_refreshed_at,
                catalog_source, catalog_source_url, revision, updated_at
         FROM provider_contract_scopes
         WHERE scope_kind = ?1 AND scope_id = ?2",
        params![scope.kind_str(), scope.id()],
        persist_scope_from_row,
    )
    .optional()
    .map_err(Into::into)
}

fn preference_protocol_allowed(
    scope: &ContractScope,
    model_id: &str,
    protocol: UpstreamProtocolKind,
) -> bool {
    if scope.kind_str() != "provider"
        || !crate::provider_contracts::selectable_model_protocol(scope.id(), protocol)
    {
        return false;
    }
    let Some(descriptor) = crate::provider_contracts::provider_scope_descriptor(scope.id()) else {
        return true;
    };
    crate::provider_contracts::static_verified_protocols(descriptor.kind, model_id, &[])
        .contains(&protocol)
        || crate::provider_contracts::safety_ceiling_protocols(descriptor.protocol_probe, model_id)
            .contains(&protocol)
}

fn set_model_protocol_preferences_on(
    conn: &Connection,
    scope: &ContractScope,
    preferences: &[(String, UpstreamProtocolKind)],
) -> Result<()> {
    let mut seen = HashSet::new();
    for (model_id, protocol) in preferences {
        let model_key = model_id.trim().to_ascii_lowercase();
        anyhow::ensure!(
            preference_protocol_allowed(scope, model_id, *protocol)
                && !model_key.is_empty()
                && seen.insert(model_key.clone()),
            "invalid or duplicate model protocol preference"
        );
        conn.execute(
            "INSERT INTO provider_model_protocol_preferences(provider_id,model_id,protocol)
             VALUES(?1,?2,?3) ON CONFLICT(provider_id,model_id) DO UPDATE SET protocol=excluded.protocol",
            params![scope.id(), model_key, protocol.as_str()],
        )?;
    }
    Ok(())
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

fn set_model_protocol_override_on(
    conn: &Connection,
    scope: &ContractScope,
    model_id: &str,
    protocol: UpstreamProtocolKind,
    state: ProtocolOverrideState,
    now: DateTime<Utc>,
) -> Result<()> {
    match state {
        ProtocolOverrideState::Auto => {
            conn.execute(
                "DELETE FROM provider_contract_model_protocol_overrides
                 WHERE scope_kind = ?1 AND scope_id = ?2 AND model_id = ?3 AND protocol = ?4",
                params![scope.kind_str(), scope.id(), model_id, protocol.as_str()],
            )?;
        }
        ProtocolOverrideState::ForceOn | ProtocolOverrideState::ForceOff => {
            conn.execute(
                "INSERT OR REPLACE INTO provider_contract_model_protocol_overrides
                 (scope_kind, scope_id, model_id, protocol, state, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    scope.kind_str(),
                    scope.id(),
                    model_id,
                    protocol.as_str(),
                    state.as_str(),
                    now.to_rfc3339(),
                ],
            )?;
        }
    }
    Ok(())
}

fn insert_default_off_override_on(
    conn: &Connection,
    scope: &ContractScope,
    model_id: &str,
    protocol: UpstreamProtocolKind,
    now: DateTime<Utc>,
) -> Result<()> {
    conn.execute(
        "INSERT OR IGNORE INTO provider_contract_model_protocol_overrides
         (scope_kind, scope_id, model_id, protocol, state, updated_at)
         VALUES (?1, ?2, ?3, ?4, 'force_off', ?5)",
        params![
            scope.kind_str(),
            scope.id(),
            model_id,
            protocol.as_str(),
            now.to_rfc3339(),
        ],
    )?;
    Ok(())
}

fn mark_new_catalog_models_default_off_on(
    conn: &Connection,
    scope: &ContractScope,
    previous_models: &[String],
    refreshed_models: &[String],
    now: DateTime<Utc>,
) -> Result<()> {
    let previous: HashSet<String> = previous_models
        .iter()
        .map(|model_id| model_id.to_ascii_lowercase())
        .collect();
    for model_id in refreshed_models {
        if previous.contains(&model_id.to_ascii_lowercase()) {
            continue;
        }
        if scope.kind_str() == crate::provider_contracts::SCOPE_KIND_PROVIDER
            && scope.id() == COMMAND_CODE_PROVIDER_ID
            && command_code_goat_includes_model(model_id)
        {
            continue;
        }
        for protocol in [
            UpstreamProtocolKind::ChatCompletions,
            UpstreamProtocolKind::Responses,
            UpstreamProtocolKind::Messages,
        ] {
            insert_default_off_override_on(conn, scope, model_id, protocol, now)?;
        }
    }
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

/// Drop probe observations for one contract scope and bump its revision.
/// Explicit protocol overrides stay; they are operator policy, not probe evidence.
pub(crate) fn invalidate_probe_evidence_on(
    conn: &Connection,
    scope: &ContractScope,
    now: DateTime<Utc>,
) -> Result<()> {
    if !table_exists(conn, "provider_contract_model_protocols")? {
        return Ok(());
    }
    conn.execute(
        "DELETE FROM provider_contract_model_protocols
         WHERE scope_kind = ?1 AND scope_id = ?2",
        params![scope.kind_str(), scope.id()],
    )?;
    if table_exists(conn, "provider_contract_scopes")? {
        bump_scope_revision_on(conn, scope, now)?;
    }
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

fn is_fresh_empty_database(conn: &Connection, source_version: i32) -> Result<bool> {
    Ok(source_version == 0 && !has_unversioned_legacy_tables(conn)?)
}

fn sqlite_data_version(conn: &Connection) -> Result<i64> {
    Ok(conn.query_row("PRAGMA data_version", [], |row| row.get(0))?)
}

fn sqlite_quick_check(conn: &Connection) -> Result<()> {
    let mut stmt = conn.prepare("PRAGMA quick_check")?;
    let rows = stmt
        .query_map([], |row| row.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    anyhow::ensure!(
        rows.len() == 1 && rows[0].eq_ignore_ascii_case("ok"),
        "sqlite quick_check failed: {}",
        rows.join("; ")
    );
    Ok(())
}

fn sqlite_foreign_key_check(conn: &Connection) -> Result<()> {
    let mut stmt = conn.prepare("PRAGMA foreign_key_check")?;
    let rows = stmt
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, i64>(3)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    anyhow::ensure!(rows.is_empty(), "sqlite foreign_key_check failed: {rows:?}");
    Ok(())
}

fn probe_account_cipher(
    cipher: Option<&dyn KeyCipher>,
    id: &str,
    column: &str,
    value: &str,
) -> Result<()> {
    if value.is_empty() {
        return Ok(());
    }
    let Some(cipher) = cipher else {
        anyhow::bail!(
            "database open requires the host encryption cipher to migrate account {id}.{column}; use Database::open_with_cipher"
        );
    };
    cipher.decrypt(value).with_context(|| {
        format!("host cipher rejected account {id}.{column}; ciphertext bytes were not rewritten")
    })?;
    Ok(())
}

fn preflight_ciphertext_probes(conn: &Connection, cipher: Option<&dyn KeyCipher>) -> Result<()> {
    if !table_exists(conn, "accounts")? {
        return Ok(());
    }
    if table_has_column(conn, "accounts", "key_cipher")? {
        let mut stmt = conn.prepare("SELECT id, key_cipher FROM accounts")?;
        let rows = stmt
            .query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        for (id, value) in rows {
            probe_account_cipher(cipher, &id, "key_cipher", &value)?;
        }
    }
    if table_has_column(conn, "accounts", "password_cipher")? {
        let mut stmt = conn.prepare(
            "SELECT id, password_cipher FROM accounts WHERE password_cipher IS NOT NULL AND password_cipher <> ''",
        )?;
        let rows = stmt
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, Option<String>>(1)?.unwrap_or_default(),
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        for (id, value) in rows {
            probe_account_cipher(cipher, &id, "password_cipher", &value)?;
        }
    }
    Ok(())
}

fn repair_legacy_account_ciphertext(conn: &Connection, cipher: &dyn KeyCipher) -> Result<()> {
    if !table_exists(conn, "accounts")? {
        return Ok(());
    }
    let has_key = table_has_column(conn, "accounts", "key_cipher")?;
    let has_password = table_has_column(conn, "accounts", "password_cipher")?;
    if !has_key && !has_password {
        return Ok(());
    }

    let select_sql = match (has_key, has_password) {
        (true, true) => "SELECT id, key_cipher, password_cipher FROM accounts",
        (true, false) => "SELECT id, key_cipher, NULL FROM accounts",
        (false, true) => "SELECT id, '', password_cipher FROM accounts",
        (false, false) => return Ok(()),
    };

    let rows = {
        let mut stmt = conn.prepare(select_sql)?;
        stmt.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, Option<String>>(2)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?
    };
    if !rows.iter().any(|(_, key, password)| {
        is_legacy_local_ciphertext(key)
            || password.as_deref().is_some_and(is_legacy_local_ciphertext)
    }) {
        return Ok(());
    }

    let tx = Transaction::new_unchecked(conn, TransactionBehavior::Immediate)?;
    let rows = {
        let mut stmt = tx.prepare(select_sql)?;
        stmt.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, Option<String>>(2)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?
    };

    let mut updates: Vec<(String, Option<String>, Option<String>)> = Vec::new();
    for (id, key, password) in rows {
        let new_key = if is_legacy_local_ciphertext(&key) {
            let plaintext = cipher.decrypt(&key).with_context(|| {
                format!("host cipher rejected account {id}.key_cipher during v2 repair")
            })?;
            Some(
                cipher
                    .encrypt(&plaintext)
                    .with_context(|| format!("failed to rewrite account {id}.key_cipher to v2"))?,
            )
        } else {
            None
        };
        let new_password = if let Some(value) = password.as_deref() {
            if is_legacy_local_ciphertext(value) {
                let plaintext = cipher.decrypt(value).with_context(|| {
                    format!("host cipher rejected account {id}.password_cipher during v2 repair")
                })?;
                Some(cipher.encrypt(&plaintext).with_context(|| {
                    format!("failed to rewrite account {id}.password_cipher to v2")
                })?)
            } else {
                None
            }
        } else {
            None
        };
        if new_key.is_some() || new_password.is_some() {
            updates.push((id, new_key, new_password));
        }
    }

    for (id, new_key, new_password) in updates {
        match (new_key, new_password) {
            (Some(key), Some(password)) => {
                tx.execute(
                    "UPDATE accounts SET key_cipher = ?1, password_cipher = ?2 WHERE id = ?3",
                    params![key, password, id],
                )?;
            }
            (Some(key), None) => {
                tx.execute(
                    "UPDATE accounts SET key_cipher = ?1 WHERE id = ?2",
                    params![key, id],
                )?;
            }
            (None, Some(password)) => {
                tx.execute(
                    "UPDATE accounts SET password_cipher = ?1 WHERE id = ?2",
                    params![password, id],
                )?;
            }
            (None, None) => {}
        }
    }
    tx.commit()?;
    Ok(())
}

fn sha256_file(path: &Path) -> Result<String> {
    let mut file = std::fs::File::open(path)
        .with_context(|| format!("failed to read {} for SHA-256 evidence", path.display()))?;
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; BACKUP_HASH_BUFFER_LEN];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

fn sync_file(path: &Path) -> Result<()> {
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)
        .or_else(|_| std::fs::File::open(path))
        .with_context(|| format!("failed to open {} for durability sync", path.display()))?;
    file.sync_all()
        .with_context(|| format!("failed to sync {}", path.display()))?;
    Ok(())
}

fn sync_parent_dir(path: &Path) -> Result<()> {
    let Some(parent) = path.parent() else {
        return Ok(());
    };
    if parent.as_os_str().is_empty() {
        return Ok(());
    }
    #[cfg(unix)]
    {
        let dir = std::fs::File::open(parent).with_context(|| {
            format!(
                "failed to open directory {} for durability sync",
                parent.display()
            )
        })?;
        dir.sync_all()
            .with_context(|| format!("failed to sync directory {}", parent.display()))?;
    }
    #[cfg(not(unix))]
    {
        let _ = parent;
    }
    Ok(())
}

fn write_backup_sha256_evidence(backup_path: &Path) -> Result<String> {
    sync_file(backup_path)?;
    let digest = sha256_file(backup_path)?;
    let file_name = backup_path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| anyhow::anyhow!("backup path is not UTF-8: {}", backup_path.display()))?;
    let evidence_path = backup_path.with_file_name(format!("{file_name}.sha256"));
    let tmp_path = backup_path.with_file_name(format!(
        "{file_name}.sha256.{}.tmp",
        uuid::Uuid::new_v4().simple()
    ));
    {
        let mut tmp = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&tmp_path)
            .with_context(|| {
                format!(
                    "failed to create SHA-256 evidence temp {}",
                    tmp_path.display()
                )
            })?;
        tmp.write_all(format!("{digest}  {file_name}\n").as_bytes())
            .with_context(|| {
                format!(
                    "failed to write SHA-256 evidence temp {}",
                    tmp_path.display()
                )
            })?;
        tmp.flush()?;
        tmp.sync_all()?;
    }
    std::fs::rename(&tmp_path, &evidence_path).with_context(|| {
        format!(
            "failed to publish SHA-256 evidence {} -> {}",
            tmp_path.display(),
            evidence_path.display()
        )
    })?;
    sync_parent_dir(&evidence_path)?;
    Ok(digest)
}

fn verify_pre_v3_backup(path: &Path) -> Result<()> {
    sqlite_quick_check(&Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY,
    )?)?;
    verify_schema_backup(path, PRE_V3_BACKUP_FILE_PREFIX, V26_SCHEMA_VERSION)?;
    let backup = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .with_context(|| format!("failed to reopen pre-v3 backup {}", path.display()))?;
    sqlite_quick_check(&backup)?;
    Ok(())
}

fn create_pre_v3_backup(conn: &Connection, db_path: &Path) -> Result<PathBuf> {
    for _ in 0..8 {
        let timestamp = Utc::now().format("%Y%m%dT%H%M%S%9fZ");
        let backup_path =
            db_path.with_file_name(format!("{PRE_V3_BACKUP_FILE_PREFIX}{timestamp}.bak"));
        if backup_path.exists() {
            std::thread::sleep(std::time::Duration::from_millis(1));
            continue;
        }
        let backup_value = backup_path.to_string_lossy().into_owned();
        #[cfg(test)]
        let vacuum_race = v27_test_hooks::install_vacuum_race(db_path, &backup_path);
        let vacuum = conn.execute("VACUUM main INTO ?1", [&backup_value]);
        #[cfg(test)]
        if let Some(race) = vacuum_race {
            race.finish();
        }
        vacuum.with_context(|| {
            format!(
                "failed to create pre-v3 database backup {}",
                backup_path.display()
            )
        })?;
        verify_pre_v3_backup(&backup_path)?;
        write_backup_sha256_evidence(&backup_path)?;
        return Ok(backup_path);
    }
    anyhow::bail!("failed to allocate a unique pre-v3 backup filename")
}

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

fn sanitize_config_json_primary_key(json: &str) -> Result<(String, Option<String>)> {
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

fn upsert_primary_access_key_on(conn: &Connection, key: &str) -> Result<()> {
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

    sqlite_quick_check(tx)?;
    sqlite_foreign_key_check(tx)?;
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

fn migrate_to_v27(
    conn: &Connection,
    db_path: &Path,
    cipher: Option<&dyn KeyCipher>,
    is_fresh: bool,
) -> Result<()> {
    for _ in 0..V27_WRITER_RACE_RETRIES {
        let version = schema_version_on(conn)?;
        if version >= V27_SCHEMA_VERSION {
            return Ok(());
        }
        anyhow::ensure!(
            version == V26_SCHEMA_VERSION,
            "v27 requires a canonical schema v26 source, found {version}"
        );

        let data_version = sqlite_data_version(conn)?;
        v27_fault(V27MigrationFault::AfterDataVersionCapture)?;
        v27_fault(V27MigrationFault::BeforePreflight)?;
        sqlite_quick_check(conn)?;
        preflight_ciphertext_probes(conn, cipher)?;

        if !is_fresh {
            v27_fault(V27MigrationFault::BeforeBackup)?;
            create_pre_v3_backup(conn, db_path)?;
            v27_fault(V27MigrationFault::AfterBackup)?;
        }

        let migrated = with_foreign_keys_off(conn, || {
            let tx = Transaction::new_unchecked(conn, TransactionBehavior::Immediate)?;
            let version_locked = schema_version_on(&tx)?;
            if version_locked >= V27_SCHEMA_VERSION {
                tx.rollback()?;
                return Ok(true);
            }
            let data_version_locked = sqlite_data_version(&tx)?;
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
enum V27MigrationFault {
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

fn migrate_to_v28(conn: &Connection) -> Result<()> {
    let version = schema_version_on(conn)?;
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

fn migrate_to_v29(conn: &Connection) -> Result<()> {
    let version = schema_version_on(conn)?;
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
fn migrate_to_v30(conn: &Connection) -> Result<()> {
    let version = schema_version_on(conn)?;
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
fn migrate_to_v31(conn: &Connection) -> Result<()> {
    let version = schema_version_on(conn)?;
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
fn migrate_to_v32(conn: &Connection) -> Result<()> {
    let version = schema_version_on(conn)?;
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
fn migrate_to_v33(conn: &Connection) -> Result<()> {
    let version = schema_version_on(conn)?;
    if version >= 33 {
        return Ok(());
    }
    anyhow::ensure!(
        version == 32,
        "v33 requires a canonical schema v32 source, found {version}"
    );
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

/// v34: one local CPA configuration row. The inference credential, enablement,
/// and ordering remain on the reserved singleton account; the model snapshot
/// remains in `provider_model_catalogs`.
fn migrate_to_v34(conn: &Connection) -> Result<()> {
    let version = schema_version_on(conn)?;
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
    sqlite_quick_check(conn)?;
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

fn create_pre_version_backup(
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
        verify_schema_backup(&backup_path, prefix, source_version)?;
        write_backup_sha256_evidence(&backup_path)?;
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
    sqlite_quick_check(tx)?;
    sqlite_foreign_key_check(tx)?;
    Ok(())
}

fn dynamic_tx_fault(point: &'static str) -> Result<()> {
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

fn list_dynamic_providers_on(conn: &Connection) -> Result<Vec<DynamicProviderRuntime>> {
    list_dynamic_providers_filtered_on(conn, false)
}

fn list_control_plane_dynamic_providers_on(
    conn: &Connection,
) -> Result<Vec<DynamicProviderRuntime>> {
    list_dynamic_providers_filtered_on(conn, true)
}

fn list_dynamic_providers_filtered_on(
    conn: &Connection,
    include_drafts: bool,
) -> Result<Vec<DynamicProviderRuntime>> {
    let sql = if include_drafts {
        "SELECT id, name, endpoint_url, upstream_protocol, auth_kind, created_at, updated_at, preset_id, origin, offering
         FROM providers
         WHERE origin IN ('preset', 'custom')
         ORDER BY created_at ASC, id ASC"
    } else {
        "SELECT id, name, endpoint_url, upstream_protocol, auth_kind, created_at, updated_at, preset_id, origin, offering
         FROM providers
         WHERE origin IN ('preset', 'custom')
           AND COALESCE(onboarding_draft, 0) = 0
         ORDER BY created_at ASC, id ASC"
    };
    let mut stmt = conn.prepare(sql)?;
    let rows = stmt.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, String>(3)?,
            row.get::<_, String>(4)?,
            row.get::<_, String>(5)?,
            row.get::<_, String>(6)?,
            row.get::<_, Option<String>>(7)?,
            row.get::<_, String>(8)?,
            row.get::<_, String>(9)?,
        ))
    })?;
    let mut providers = Vec::new();
    for row in rows {
        let (
            id,
            name,
            endpoint_url,
            protocol,
            auth_kind,
            created_at,
            updated_at,
            preset_id,
            origin,
            offering,
        ) = row?;
        providers.push(load_dynamic_provider_runtime(
            conn,
            DynamicProviderRow {
                preset_id,
                id,
                name,
                endpoint_url,
                protocol,
                auth_kind,
                created_at,
                updated_at,
                origin,
                offering,
            },
        )?);
    }
    Ok(providers)
}

fn get_dynamic_provider_on(
    conn: &Connection,
    provider_id: &str,
) -> Result<Option<DynamicProviderRuntime>> {
    let row = conn
        .query_row(
            "SELECT id, name, endpoint_url, upstream_protocol, auth_kind, created_at, updated_at, preset_id, origin, offering
             FROM providers
             WHERE lower(id) = lower(?1) AND origin IN ('preset', 'custom')",
            [provider_id],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, String>(5)?,
                    row.get::<_, String>(6)?,
                    row.get::<_, Option<String>>(7)?,
                    row.get::<_, String>(8)?,
                    row.get::<_, String>(9)?,
                ))
            },
        )
        .optional()?;
    let Some((
        id,
        name,
        endpoint_url,
        protocol,
        auth_kind,
        created_at,
        updated_at,
        preset_id,
        origin,
        offering,
    )) = row
    else {
        return Ok(None);
    };
    Ok(Some(load_dynamic_provider_runtime(
        conn,
        DynamicProviderRow {
            preset_id,
            id,
            name,
            endpoint_url,
            protocol,
            auth_kind,
            created_at,
            updated_at,
            origin,
            offering,
        },
    )?))
}

fn onboarding_draft_provider_ids_on(conn: &Connection) -> Result<HashSet<String>> {
    let mut stmt = conn.prepare(
        "SELECT id FROM providers
         WHERE origin IN ('preset', 'custom')
           AND COALESCE(onboarding_draft, 0) != 0",
    )?;
    let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
    let mut ids = HashSet::new();
    for id in rows {
        ids.insert(id?);
    }
    Ok(ids)
}

fn provider_is_onboarding_draft_on(conn: &Connection, provider_id: &str) -> Result<Option<bool>> {
    conn.query_row(
        "SELECT COALESCE(onboarding_draft, 0) FROM providers
         WHERE lower(id) = lower(?1) AND origin IN ('preset', 'custom')",
        [provider_id],
        |row| row.get::<_, i32>(0),
    )
    .optional()
    .map(|value| value.map(|flag| flag != 0))
    .map_err(Into::into)
}

/// Read a single row from the unified `providers` table without filtering on
/// `origin`. Used by the public read path to surface builtin seeds alongside
/// dynamic rows. Returns `None` when the id is unknown.
fn get_provider_definition_on(
    conn: &Connection,
    provider_id: &str,
) -> Result<Option<DynamicProviderRuntime>> {
    let row = conn
        .query_row(
            "SELECT id, name, endpoint_url, upstream_protocol, auth_kind, created_at, updated_at, preset_id, origin, offering
             FROM providers
             WHERE lower(id) = lower(?1)",
            [provider_id],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, Option<String>>(2)?,
                    row.get::<_, Option<String>>(3)?,
                    row.get::<_, Option<String>>(4)?,
                    row.get::<_, String>(5)?,
                    row.get::<_, String>(6)?,
                    row.get::<_, Option<String>>(7)?,
                    row.get::<_, String>(8)?,
                    row.get::<_, String>(9)?,
                ))
            },
        )
        .optional()?;
    let Some((
        id,
        name,
        endpoint_url_opt,
        protocol_opt,
        auth_kind_opt,
        created_at,
        updated_at,
        preset_id,
        origin,
        offering,
    )) = row
    else {
        return Ok(None);
    };
    // Builtin rows leave endpoint_url/upstream_protocol/auth_kind as NULL;
    // synthesize safe placeholders the loader can parse so the runtime shape
    // stays uniform. The handler then maps to `Option<...>` on the wire.
    let endpoint_url = endpoint_url_opt.unwrap_or_default();
    let protocol = protocol_opt.unwrap_or_else(|| "chat_completions".to_string());
    let auth_kind = auth_kind_opt.unwrap_or_else(|| "bearer".to_string());
    Ok(Some(load_dynamic_provider_runtime(
        conn,
        DynamicProviderRow {
            preset_id,
            id,
            name,
            endpoint_url,
            protocol,
            auth_kind,
            created_at,
            updated_at,
            origin,
            offering,
        },
    )?))
}

struct DynamicProviderRow {
    preset_id: Option<String>,
    id: String,
    name: String,
    endpoint_url: String,
    protocol: String,
    auth_kind: String,
    created_at: String,
    updated_at: String,
    origin: String,
    offering: String,
}

fn load_dynamic_provider_runtime(
    conn: &Connection,
    provider: DynamicProviderRow,
) -> Result<DynamicProviderRuntime> {
    let upstream_protocol =
        ocg_domain::catalog::UpstreamProtocolKind::try_from(provider.protocol.as_str())
            .map_err(|error| anyhow::anyhow!(error.to_string()))?;
    let auth_kind = DynamicAuthKind::try_from(provider.auth_kind.as_str())
        .map_err(|error| anyhow::anyhow!(error.to_string()))?;
    let mut stmt = conn.prepare(
        "SELECT public_model, upstream_model, upstream_override
         FROM provider_models
         WHERE provider_id = ?1
         ORDER BY public_model_key ASC",
    )?;
    let rows = stmt.query_map([&provider.id], |row| {
        Ok(DynamicModelMapping {
            public_model: row.get(0)?,
            upstream_model: row.get(1)?,
            upstream_override: row
                .get::<_, Option<String>>(2)?
                .map(|value| {
                    serde_json::from_str(&value).map_err(|error| {
                        rusqlite::Error::FromSqlConversionFailure(
                            2,
                            rusqlite::types::Type::Text,
                            Box::new(error),
                        )
                    })
                })
                .transpose()?,
        })
    })?;
    let mut mappings = Vec::new();
    for row in rows {
        mappings.push(row?);
    }
    let created_at = DateTime::parse_from_rfc3339(&provider.created_at)
        .map(|value| value.with_timezone(&Utc))
        .map_err(|error| {
            anyhow::anyhow!(
                "dynamic provider {} has invalid created_at: {error}",
                provider.id
            )
        })?;
    let updated_at = DateTime::parse_from_rfc3339(&provider.updated_at)
        .map(|value| value.with_timezone(&Utc))
        .map_err(|error| {
            anyhow::anyhow!(
                "dynamic provider {} has invalid updated_at: {error}",
                provider.id
            )
        })?;
    let origin = ocg_domain::provider::ProviderOrigin::try_from(provider.origin.as_str())
        .map_err(|error| anyhow::anyhow!(error.to_string()))?;
    Ok(DynamicProviderRuntime {
        preset_id: provider.preset_id,
        id: provider.id,
        name: provider.name,
        endpoint_url: provider.endpoint_url,
        upstream_protocol,
        auth_kind,
        mappings,
        created_at,
        updated_at,
        origin,
        offering: provider.offering,
    })
}

fn find_dashboard_operation_on(
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

fn insert_dashboard_operation_on(
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

fn insert_dynamic_provider_on(
    conn: &Connection,
    runtime: &DynamicProviderRuntime,
    onboarding_draft: bool,
) -> Result<()> {
    let origin = runtime.origin.as_str();
    let offering = runtime.offering.as_str();
    conn.execute(
        "INSERT INTO providers
         (id, origin, adapter_kind, name, endpoint_url, upstream_protocol,
          auth_kind, preset_id, offering, display_family, endpoint_per_account,
          created_at, updated_at, onboarding_draft)
         VALUES (?1, ?2, 'configurable_http', ?3, ?4, ?5, ?6, ?7, ?8, NULL, 0, ?9, ?10, ?11)",
        params![
            runtime.id,
            origin,
            runtime.name,
            runtime.endpoint_url,
            runtime.upstream_protocol.as_str(),
            runtime.auth_kind.as_str(),
            runtime.preset_id,
            offering,
            runtime.created_at.to_rfc3339(),
            runtime.updated_at.to_rfc3339(),
            onboarding_draft as i32,
        ],
    )?;
    insert_dynamic_provider_models_on(conn, &runtime.id, &runtime.mappings)
}

fn count_accounts_for_provider_on(conn: &Connection, provider_id: &str) -> Result<i64> {
    conn.query_row(
        "SELECT COUNT(*) FROM accounts WHERE lower(provider_id) = lower(?1)",
        [provider_id],
        |row| row.get(0),
    )
    .map_err(Into::into)
}

fn upsert_imported_dynamic_provider_on(
    conn: &Connection,
    runtime: &DynamicProviderRuntime,
    imported_account_ids: &HashSet<String>,
    onboarding_draft: bool,
) -> Result<()> {
    anyhow::ensure!(
        builtin_provider(&runtime.id).is_none(),
        "dynamic provider id collides with a built-in provider"
    );
    if let Some(existing) = get_dynamic_provider_on(conn, &runtime.id)? {
        if existing.auth_kind.requires_key() != runtime.auth_kind.requires_key() {
            let mut statement =
                conn.prepare("SELECT id FROM accounts WHERE lower(provider_id) = lower(?1)")?;
            let existing_ids =
                statement.query_map([&existing.id], |row| row.get::<_, String>(0))?;
            for account_id in existing_ids {
                let account_id = account_id?;
                anyhow::ensure!(
                    imported_account_ids.contains(&account_id.to_ascii_lowercase()),
                    "cannot change dynamic provider auth while destination-only accounts still reference it"
                );
            }
        }
        conn.execute(
            "UPDATE providers
             SET name = ?1, endpoint_url = ?2, upstream_protocol = ?3, auth_kind = ?4,
                 updated_at = ?5, preset_id = ?7, offering = ?8, onboarding_draft = ?9
             WHERE id = ?6",
            params![
                runtime.name,
                runtime.endpoint_url,
                runtime.upstream_protocol.as_str(),
                runtime.auth_kind.as_str(),
                runtime.updated_at.to_rfc3339(),
                existing.id,
                runtime.preset_id,
                runtime.offering.as_str(),
                onboarding_draft as i32,
            ],
        )?;
        conn.execute(
            "DELETE FROM provider_models WHERE provider_id = ?1",
            [&existing.id],
        )?;
        insert_dynamic_provider_models_on(conn, &existing.id, &runtime.mappings)
    } else {
        insert_dynamic_provider_on(conn, runtime, onboarding_draft)
    }
}

fn ensure_dynamic_singleton_accounts_on(conn: &Connection) -> Result<()> {
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

fn insert_dynamic_provider_models_on(
    conn: &Connection,
    provider_id: &str,
    mappings: &[DynamicModelMapping],
) -> Result<()> {
    let mut stmt = conn.prepare(
        "INSERT INTO provider_models
         (provider_id, public_model, public_model_key, upstream_model, upstream_override)
         VALUES (?1, ?2, ?3, ?4, ?5)",
    )?;
    for mapping in mappings {
        stmt.execute(params![
            provider_id,
            mapping.public_model,
            mapping.public_model.to_ascii_lowercase(),
            mapping.upstream_model,
            mapping
                .upstream_override
                .as_ref()
                .map(serde_json::to_string)
                .transpose()?,
        ])?;
    }
    Ok(())
}

fn ensure_dynamic_provider_tables(conn: &Connection) -> Result<()> {
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
fn migrate_to_v40(conn: &Connection) -> Result<()> {
    if schema_version_on(conn)? >= 40 {
        return Ok(());
    }
    let tx = conn.unchecked_transaction()?;
    let version = schema_version_on(&tx)?;
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
fn migrate_to_v41(conn: &Connection) -> Result<()> {
    if schema_version_on(conn)? >= 41 {
        return Ok(());
    }
    let tx = Transaction::new_unchecked(conn, TransactionBehavior::Immediate)?;
    let version = schema_version_on(&tx)?;
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
fn migrate_to_v42(conn: &Connection, db_path: &Path, is_fresh: bool) -> Result<()> {
    // Read once before the writer lock: a concurrent migration can commit
    // between two reads, so checking `>= 42` and `== 41` separately is racy.
    let source_version = schema_version_on(conn)?;
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
    sqlite_quick_check(conn)?;
    with_foreign_keys_off(conn, || {
        let tx = Transaction::new_unchecked(conn, TransactionBehavior::Immediate)?;
        let version_locked = schema_version_on(&tx)?;
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
        sqlite_quick_check(&tx)?;
        sqlite_foreign_key_check(&tx)?;
        tx.commit()?;
        Ok(())
    })
}

/// v43: allow Responses as a stored preferred protocol, and restore Auto on
/// available CN sibling protocols that the exclusive radio force_off'd.
fn migrate_to_v43(conn: &Connection) -> Result<()> {
    if schema_version_on(conn)? >= 43 {
        return Ok(());
    }
    let tx = Transaction::new_unchecked(conn, TransactionBehavior::Immediate)?;
    let version = schema_version_on(&tx)?;
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
fn migrate_to_v44(conn: &Connection) -> Result<()> {
    if schema_version_on(conn)? >= 44 {
        return Ok(());
    }
    let tx = Transaction::new_unchecked(conn, TransactionBehavior::Immediate)?;
    let version = schema_version_on(&tx)?;
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
fn migrate_to_v47(conn: &Connection) -> Result<()> {
    if schema_version_on(conn)? >= 47 {
        return Ok(());
    }
    let tx = Transaction::new_unchecked(conn, TransactionBehavior::Immediate)?;
    let version = schema_version_on(&tx)?;
    if version >= 47 {
        return Ok(());
    }
    anyhow::ensure!(version == 46, "v47 requires schema v46");
    anyhow::ensure!(
        table_exists(&tx, "providers")?,
        "v47 requires the unified providers table"
    );
    ensure_column(
        &tx,
        "providers",
        "onboarding_draft",
        "INTEGER NOT NULL DEFAULT 0",
    )?;
    tx.execute_batch("INSERT OR REPLACE INTO schema_version(version) VALUES (47);")?;
    tx.commit()?;
    Ok(())
}

fn migrate_v42_body(tx: &Transaction<'_>) -> Result<()> {
    let now = Utc::now().to_rfc3339();
    let v41_dynamic_providers_exists = table_exists(tx, "dynamic_providers")?;
    let v41_dynamic_models_exists = table_exists(tx, "dynamic_provider_models")?;
    let v41_preferences_exists = table_exists(tx, "provider_model_protocol_preferences")?;

    // v22-v40 sources never had the v42-renamed tables; the reverse helpers
    // in the test suite leave them behind so the v22-v40 fixtures can be
    // re-migrated. Drop those leftovers before the rename so it does not
    // collide. The preferences table is rebuilt by the body below, so it
    // must not be dropped here.
    if table_exists(tx, "providers")? {
        tx.execute_batch("DROP TABLE providers;")?;
    }
    if table_exists(tx, "provider_models")? {
        tx.execute_batch("DROP TABLE provider_models;")?;
    }
    if table_exists(tx, "provider_model_protocol_preferences_v42")? {
        tx.execute_batch("DROP TABLE provider_model_protocol_preferences_v42;")?;
    }

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
    let seeds: [(
        &str,
        ProviderAdapterKind,
        &str,
        &str,
        &str,
        &str,
        &str,
        &str,
        i32,
    ); 7] = [
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
fn migrate_to_v39(conn: &Connection) -> Result<()> {
    if schema_version_on(conn)? >= 39 {
        return Ok(());
    }
    let tx = conn.unchecked_transaction()?;
    let version = schema_version_on(&tx)?;
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
fn migrate_to_v35(conn: &Connection, db_path: &Path, is_fresh: bool) -> Result<()> {
    let version = schema_version_on(conn)?;
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
        let version_locked = schema_version_on(&tx)?;
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
fn migrate_to_v36(conn: &Connection) -> Result<()> {
    let version = schema_version_on(conn)?;
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
fn migrate_to_v37(conn: &Connection) -> Result<()> {
    let version = schema_version_on(conn)?;
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

#[cfg(test)]
pub(crate) mod dynamic_provider_fault {
    use std::cell::Cell;

    thread_local! {
        static FAULT: Cell<Option<&'static str>> = const { Cell::new(None) };
    }

    pub(crate) fn install(point: &'static str) {
        FAULT.set(Some(point));
    }

    pub(crate) fn clear() {
        FAULT.set(None);
    }

    pub(crate) fn inject(point: &'static str) -> anyhow::Result<()> {
        if FAULT.get() == Some(point) {
            FAULT.set(None);
            anyhow::bail!("injected dynamic provider fault at {point}");
        }
        Ok(())
    }
}

#[cfg(test)]
mod v27_test_hooks {
    use super::V27MigrationFault;
    use rusqlite::Connection;
    use std::cell::Cell;
    use std::path::Path;
    use std::thread::{self, JoinHandle};
    use std::time::Duration;

    thread_local! {
        static FAULT: Cell<Option<V27MigrationFault>> = const { Cell::new(None) };
        static RACE_DURING_VACUUM: Cell<bool> = const { Cell::new(false) };
    }

    pub(crate) fn inject(point: V27MigrationFault) -> anyhow::Result<()> {
        if FAULT.get() == Some(point) {
            anyhow::bail!("injected v27 fault at {point:?}");
        }
        Ok(())
    }

    pub(crate) struct VacuumRace {
        join: Option<JoinHandle<()>>,
    }

    impl VacuumRace {
        pub(crate) fn finish(mut self) {
            if let Some(join) = self.join.take() {
                let _ = join.join();
            }
        }
    }

    pub(crate) fn install_vacuum_race(db_path: &Path, backup_path: &Path) -> Option<VacuumRace> {
        if !RACE_DURING_VACUUM.get() {
            return None;
        }
        RACE_DURING_VACUUM.set(false);
        let path = db_path.to_path_buf();
        let backup = backup_path.to_path_buf();
        let join = thread::spawn(move || {
            let wait_deadline = std::time::Instant::now() + Duration::from_secs(5);
            while std::time::Instant::now() < wait_deadline && !backup.exists() {
                thread::yield_now();
            }
            let write_deadline = std::time::Instant::now() + Duration::from_secs(5);
            while std::time::Instant::now() < write_deadline {
                if let Ok(writer) = Connection::open(&path) {
                    let _ = writer.busy_timeout(Duration::from_millis(250));
                    if writer
                        .execute(
                            "INSERT INTO settings (key, value) VALUES ('v27-vacuum-race', 'committed')
                             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
                            [],
                        )
                        .is_ok()
                    {
                        return;
                    }
                }
                thread::yield_now();
            }
        });
        Some(VacuumRace { join: Some(join) })
    }

    pub(crate) fn set_fault(point: Option<V27MigrationFault>) {
        FAULT.set(point);
    }

    pub(crate) fn set_race_during_vacuum(enabled: bool) {
        RACE_DURING_VACUUM.set(enabled);
    }

    pub(crate) fn reset() {
        FAULT.set(None);
        RACE_DURING_VACUUM.set(false);
    }
}

fn insert_account_row(
    conn: &Connection,
    account: &Account,
    purchase_date: &str,
    verification_status: ConnectionVerificationStatus,
) -> Result<()> {
    insert_account_columns(conn, account, purchase_date, verification_status)?;
    let sort_order: i64 = conn.query_row(
        "SELECT sort_order FROM accounts WHERE id = ?1",
        [&account.id],
        |row| row.get(0),
    )?;
    identity::persist_account_identity_model(
        conn,
        account,
        purchase_date,
        verification_status,
        sort_order,
        None,
        Utc::now(),
    )?;
    Ok(())
}

pub(crate) fn insert_account_columns(
    conn: &Connection,
    account: &Account,
    purchase_date: &str,
    verification_status: ConnectionVerificationStatus,
) -> Result<()> {
    conn.execute(
        "INSERT INTO accounts (id, name, username, password_cipher, key_cipher, enabled, referral_code, recharge_date, sort_order, cooldown_until, cooldown_generic_until, cooldown_5h_until, cooldown_week_until, cooldown_month_until, cooldown_free_until, last_error, auth_error, account_type, setup_step, notes, created_at, updated_at, provider_id, credential_kind, quota_scope, verification_status, connection_verified_at, verification_error)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, (SELECT COALESCE(MAX(sort_order), -1) + 1 FROM accounts), ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21, ?22, ?23, ?24, ?25, NULL, NULL)",
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

fn insert_import_account_on(conn: &Connection, record: &AccountImportRecord) -> Result<()> {
    validate_import_account_on(conn, record)?;
    let account = &record.account;
    let purchase_date = if account.purchase_date.trim().is_empty() {
        local_today()
    } else {
        normalize_purchase_date(&account.purchase_date)?
    };
    insert_account_row(conn, account, &purchase_date, record.verification_status)?;
    if let Some(config) = &record.custom_config {
        persist_account_custom_config_on(conn, &account.id, config)?;
    }
    if !record.capabilities.is_empty() {
        persist_account_model_capabilities_on(conn, &account.id, &record.capabilities)?;
    }
    restore_import_verification_on(conn, record)?;
    persist_ollama_billing_on(conn, record)?;
    Ok(())
}

fn validate_import_account_on(conn: &Connection, record: &AccountImportRecord) -> Result<()> {
    let account = &record.account;
    anyhow::ensure!(
        account.id != ZEN_FREE_ACCOUNT_ID,
        "Zen Free is database-owned and cannot be imported"
    );
    if let Some(plan) = builtin_provider(&account.provider_id) {
        account.validate_provider_binding()?;
        ensure_enabled_provider_is_routable(&account.provider_id, account.enabled)?;
        let verification_gates_enablement = plan.verification_policy
            == VerificationPolicy::Required
            && ProviderRegistry::get(&account.provider_id)
                .is_some_and(|descriptor| descriptor.card_actions.enable_requires_verification);
        anyhow::ensure!(
            !account.enabled
                || !verification_gates_enablement
                || record.verification_status.allows_enablement(),
            "an enabled imported account must retain an enabling verification state"
        );
        if plan_requires_custom_config(plan) {
            anyhow::ensure!(
                record.custom_config.is_some(),
                "Custom API accounts require a complete endpoint"
            );
            anyhow::ensure!(
                !record.capabilities.is_empty(),
                "Custom API accounts require at least one model capability"
            );
        } else {
            anyhow::ensure!(
                record.custom_config.is_none(),
                "custom config is only available for Custom API accounts"
            );
            anyhow::ensure!(
                record.capabilities.is_empty(),
                "model capabilities are only available for Custom API accounts"
            );
        }
        return Ok(());
    }
    let runtime = get_dynamic_provider_on(conn, &account.provider_id)?
        .ok_or_else(|| anyhow::anyhow!("unknown provider `{}`", account.provider_id))?;
    anyhow::ensure!(
        account.credential_kind == runtime.auth_kind.credential_kind()
            && account.quota_scope == runtime.auth_kind.quota_scope(),
        "provider binding does not match `{}`",
        account.provider_id
    );
    anyhow::ensure!(
        record.custom_config.is_none(),
        "custom config is only available for Custom API accounts"
    );
    anyhow::ensure!(
        record.capabilities.is_empty(),
        "model capabilities are only available for Custom API accounts"
    );
    Ok(())
}

fn restore_import_verification_on(conn: &Connection, record: &AccountImportRecord) -> Result<()> {
    conn.execute(
        "UPDATE accounts SET verification_status = ?2,
             connection_verified_at = ?3, verification_error = NULL
         WHERE id = ?1",
        params![
            record.account.id,
            record.verification_status.as_str(),
            record
                .connection_verified_at
                .map(|value| value.to_rfc3339()),
        ],
    )?;
    Ok(())
}

fn merge_import_account_on(conn: &Connection, record: &AccountImportRecord) -> Result<()> {
    if conn
        .query_row(
            "SELECT 1 FROM accounts WHERE id = ?1",
            [&record.account.id],
            |_| Ok(()),
        )
        .optional()?
        .is_none()
    {
        return insert_import_account_on(conn, record);
    }
    validate_import_account_on(conn, record)?;
    let account = &record.account;
    let purchase_date = if account.purchase_date.trim().is_empty() {
        local_today()
    } else {
        normalize_purchase_date(&account.purchase_date)?
    };
    conn.execute(
        "UPDATE accounts SET
             name = ?2, username = ?3, key_cipher = ?4, enabled = ?5,
             recharge_date = ?6, account_type = ?7, setup_step = ?8, notes = ?9,
             provider_id = ?10, credential_kind = ?11,
             quota_scope = ?12, verification_status = ?13,
             connection_verified_at = ?14, verification_error = NULL,
             auth_error = NULL, last_error = NULL, updated_at = ?15
         WHERE id = ?1",
        params![
            account.id,
            account.name,
            account.username,
            account.key_cipher,
            account.enabled as i32,
            purchase_date,
            account.account_type.as_str(),
            account.setup_step.as_str(),
            account.notes,
            account.provider_id,
            account.credential_kind.as_str(),
            account.quota_scope.as_str(),
            record.verification_status.as_str(),
            record
                .connection_verified_at
                .map(|value| value.to_rfc3339()),
            Utc::now().to_rfc3339(),
        ],
    )?;
    conn.execute(
        "DELETE FROM account_model_capabilities WHERE account_id = ?1",
        [&account.id],
    )?;
    conn.execute(
        "DELETE FROM account_custom_configs WHERE account_id = ?1",
        [&account.id],
    )?;
    conn.execute(
        "DELETE FROM provider_contract_model_protocol_overrides
         WHERE scope_kind = ?1 AND scope_id = ?2",
        params![SCOPE_KIND_CUSTOM_ENDPOINT, account.id],
    )?;
    conn.execute(
        "DELETE FROM provider_contract_model_protocols
         WHERE scope_kind = ?1 AND scope_id = ?2",
        params![SCOPE_KIND_CUSTOM_ENDPOINT, account.id],
    )?;
    conn.execute(
        "DELETE FROM provider_contract_scopes WHERE scope_kind = ?1 AND scope_id = ?2",
        params![SCOPE_KIND_CUSTOM_ENDPOINT, account.id],
    )?;
    if let Some(config) = &record.custom_config {
        persist_account_custom_config_on(conn, &account.id, config)?;
    }
    if !record.capabilities.is_empty() {
        persist_account_model_capabilities_on(conn, &account.id, &record.capabilities)?;
    }
    // Child-table writers correctly invalidate verification during ordinary
    // edits. A validated node snapshot is different: it carries the source
    // verification state as part of the portable account definition.
    restore_import_verification_on(conn, record)?;
    persist_ollama_billing_on(conn, record)?;
    Ok(())
}

fn persist_ollama_billing_on(conn: &Connection, record: &AccountImportRecord) -> Result<()> {
    let is_ollama = record.account.provider_id == OLLAMA_PROVIDER_ID;
    if let Some(tier) = record.ollama_billing_tier {
        anyhow::ensure!(
            is_ollama,
            "Ollama billing tier is only valid for Ollama Cloud accounts"
        );
        if tier.requires_purchase_date() && record.account.purchase_date.trim().is_empty() {
            anyhow::bail!("a configured Ollama paid tier requires purchase_date");
        }
    } else if !is_ollama {
        return Ok(());
    }
    set_ollama_cloud_billing_tier_on(conn, &record.account.id, record.ollama_billing_tier)
}

fn persist_account_custom_config_on(
    conn: &Connection,
    account_id: &str,
    input: &AccountCustomConfigInput,
) -> Result<()> {
    let endpoint_url = validate_custom_endpoint_url(&input.endpoint_url)?;
    let now = Utc::now().to_rfc3339();
    let existing = conn
        .query_row(
            "SELECT endpoint_url, upstream_protocol FROM account_custom_configs WHERE account_id = ?1",
            [account_id],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
        )
        .optional()?;
    let endpoint_changed = existing.as_ref().is_some_and(|(url, protocol)| {
        url != &endpoint_url || protocol != input.upstream_protocol.as_str()
    });
    if existing.is_some() {
        conn.execute(
            "UPDATE account_custom_configs
             SET endpoint_url = ?2, upstream_protocol = ?3, updated_at = ?4
             WHERE account_id = ?1",
            params![
                account_id,
                endpoint_url,
                input.upstream_protocol.as_str(),
                now,
            ],
        )?;
    } else {
        conn.execute(
            "INSERT INTO account_custom_configs (
                account_id, endpoint_url, upstream_protocol, created_at, updated_at
             ) VALUES (?1, ?2, ?3, ?4, ?4)",
            params![
                account_id,
                endpoint_url,
                input.upstream_protocol.as_str(),
                now,
            ],
        )?;
    }
    identity::fill_uninitialized_binding_grants_on(conn, Some(account_id))?;
    mark_required_verification_stale_on(conn, account_id)?;
    if endpoint_changed {
        invalidate_probe_evidence_on(
            conn,
            &ContractScope::custom_endpoint(account_id),
            Utc::now(),
        )?;
    }
    Ok(())
}

/// Re-open a verification-required account as a pending draft. Enablement is
/// left untouched: for Custom, verification is an optional tool rather than an
/// enablement gate. Go (`not_required`) rows are left untouched.
fn mark_required_verification_stale_on(conn: &Connection, account_id: &str) -> Result<()> {
    conn.execute(
        "UPDATE accounts
         SET verification_status = 'pending',
             connection_verified_at = NULL,
             verification_error = NULL,
             updated_at = ?2
         WHERE id = ?1 AND verification_status <> 'not_required'",
        params![account_id, Utc::now().to_rfc3339()],
    )?;
    Ok(())
}

fn refresh_goat_provider_catalog_on(conn: &Connection) -> Result<()> {
    let mut stmt = conn.prepare(
        "SELECT c.model_id
         FROM account_model_capabilities c
         INNER JOIN accounts a ON a.id = c.account_id
         WHERE a.provider_id = ?1
           AND c.source = ?2
           AND a.verification_status = 'verified'
         ORDER BY a.sort_order ASC, a.created_at ASC, a.id ASC, c.rowid ASC",
    )?;
    let rows = stmt.query_map(
        params![COMMAND_CODE_PROVIDER_ID, COMMAND_CODE_GOAT_MODELS_SOURCE],
        |row| row.get::<_, String>(0),
    )?;
    let mut models = Vec::new();
    let mut seen = HashSet::new();
    for row in rows {
        let model = row?;
        let key = model.to_ascii_lowercase();
        if seen.insert(key) {
            models.push(model);
        }
    }
    let now = Utc::now();
    conn.execute(
        "INSERT INTO provider_model_catalogs
         (provider_id, models_json, refreshed_at, source_url)
         VALUES (?1, ?2, ?3, ?4)
         ON CONFLICT(provider_id) DO UPDATE SET
             models_json = excluded.models_json,
             refreshed_at = excluded.refreshed_at,
             source_url = excluded.source_url",
        params![
            COMMAND_CODE_PROVIDER_ID,
            serde_json::to_string(&models)?,
            now.to_rfc3339(),
            COMMAND_CODE_GOAT_BASE_URL,
        ],
    )?;
    upsert_contract_catalog_on(
        conn,
        &ContractScope::provider(COMMAND_CODE_PROVIDER_ID),
        &models,
        Some(now),
        CATALOG_SOURCE_COMMAND_CODE_MODELS,
        COMMAND_CODE_GOAT_BASE_URL,
        now,
    )?;
    Ok(())
}

#[cfg(test)]
fn persist_goat_catalog_on(
    conn: &Connection,
    account_id: &str,
    models: &[String],
    verified_at: Option<DateTime<Utc>>,
) -> Result<()> {
    conn.execute(
        "DELETE FROM account_model_capabilities
         WHERE account_id = ?1 AND source = ?2",
        params![account_id, COMMAND_CODE_GOAT_MODELS_SOURCE],
    )?;
    let verified = verified_at.map(|value| value.to_rfc3339());
    let mut seen = HashSet::new();
    for model in models {
        let model_id = validate_custom_model_id(model)?;
        let key = model_id.to_ascii_lowercase();
        if !seen.insert(key) {
            continue;
        }
        let protocol = match ocg_domain::protocol::command_code_preferred_format(&model_id) {
            Some(ocg_domain::protocol::ApiFormat::Messages) => UpstreamProtocolKind::Messages,
            _ => UpstreamProtocolKind::ChatCompletions,
        };
        conn.execute(
            "INSERT INTO account_model_capabilities
             (account_id, model_id, upstream_model, protocol, verified_at, source)
             VALUES (?1, ?2, ?2, ?3, ?4, ?5)",
            params![
                account_id,
                model_id,
                protocol.as_str(),
                verified,
                COMMAND_CODE_GOAT_MODELS_SOURCE,
            ],
        )?;
    }
    Ok(())
}

fn persist_account_model_capabilities_on(
    conn: &Connection,
    account_id: &str,
    capabilities: &[AccountModelCapabilityInput],
) -> Result<()> {
    if !capabilities.is_empty() {
        let expected_json: String = conn
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
        let expected_protocol = UpstreamProtocolKind::try_from(expected_json.as_str())
            .map_err(|error| anyhow::anyhow!(error))?;
        crate::custom::validate_custom_capability_expansion(expected_protocol, capabilities)
            .map_err(|message| anyhow::anyhow!(message))?;
    }
    conn.execute(
        "DELETE FROM account_model_capabilities WHERE account_id = ?1",
        [account_id],
    )?;
    let mut seen = HashSet::new();
    for capability in capabilities {
        let public_model = validate_custom_model_id(&capability.public_model)?;
        let upstream_model = validate_custom_model_id(&capability.upstream_model)?;
        let source = capability
            .source
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .unwrap_or("manual");
        let key = (
            public_model.to_ascii_lowercase(),
            capability.protocol.as_str().to_string(),
        );
        anyhow::ensure!(
            seen.insert(key),
            "duplicate model capability `{public_model}` / {}",
            capability.protocol.as_str()
        );
        conn.execute(
            "INSERT INTO account_model_capabilities (
                account_id, model_id, upstream_model, protocol, verified_at, source
             ) VALUES (?1, ?2, ?3, ?4, NULL, ?5)",
            params![
                account_id,
                public_model,
                upstream_model,
                capability.protocol.as_str(),
                source
            ],
        )?;
    }
    mark_required_verification_stale_on(conn, account_id)?;
    Ok(())
}

fn clear_custom_protocol_state_except_on(
    conn: &Connection,
    account_id: &str,
    protocol: UpstreamProtocolKind,
) -> Result<()> {
    conn.execute(
        "DELETE FROM provider_contract_model_protocols
         WHERE scope_kind = 'custom_endpoint' AND scope_id = ?1 AND protocol <> ?2",
        params![account_id, protocol.as_str()],
    )?;
    conn.execute(
        "DELETE FROM provider_contract_model_protocol_overrides
         WHERE scope_kind = 'custom_endpoint' AND scope_id = ?1 AND protocol <> ?2",
        params![account_id, protocol.as_str()],
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
/// catalog plan with `routable=false` are forced off. Providers come from
/// [`BUILTIN_PROVIDERS`]; Go, Zen, and unknown provider rows are skipped.
fn disable_unroutable_catalog_accounts(tx: &Transaction<'_>) -> Result<()> {
    if !(table_has_column(tx, "accounts", "provider_id")?
        && table_has_column(tx, "accounts", "enabled")?)
    {
        return Ok(());
    }
    for plan in BUILTIN_PROVIDERS.iter().filter(|plan| !plan.routable) {
        tx.execute(
            "UPDATE accounts SET enabled = 0
             WHERE provider_id = ?1 AND enabled <> 0",
            params![plan.provider_id],
        )?;
    }
    Ok(())
}

fn ensure_account_provider_binding(db: &Database, account: &Account) -> Result<()> {
    if builtin_provider(&account.provider_id).is_some() {
        account.validate_provider_binding()?;
        ensure_enabled_provider_is_routable(&account.provider_id, account.enabled)?;
        return Ok(());
    }
    let Some(runtime) = db.get_dynamic_provider(&account.provider_id)? else {
        anyhow::bail!("unknown provider `{}`", account.provider_id);
    };
    anyhow::ensure!(
        account.credential_kind == runtime.auth_kind.credential_kind()
            && account.quota_scope == runtime.auth_kind.quota_scope(),
        "provider binding does not match `{}`",
        account.provider_id
    );
    if runtime.auth_kind.is_singleton() {
        let count = db.count_accounts_for_provider(&runtime.id)?;
        anyhow::ensure!(
            count == 0,
            "no-auth provider already has a singleton account"
        );
    }
    Ok(())
}

impl Database {
    /// Test/open convenience. Production hosts must call
    /// [`Self::open_with_cipher`] so account ciphertext probes use the
    /// Host-resolved cipher. This path still runs the v27 rewrite and fails
    /// closed on any non-empty `accounts.key_cipher` /
    /// `accounts.password_cipher`. Plaintext access keys are not probed.
    pub fn open(data_dir: PathBuf) -> Result<Self> {
        Self::open_internal(data_dir, None)
    }

    /// Production open path: migrate with the already-resolved Host cipher.
    /// Persisted account key/password ciphertext is probed in place before
    /// migration. Decrypt failure fails closed. Authenticated `v2:` ciphertext
    /// rejects a wrong host cipher; legacy XOR remains readable so backups
    /// restore, then remaining legacy rows are rewritten to v2 in one
    /// transaction.
    pub fn open_with_cipher(
        data_dir: PathBuf,
        cipher: Arc<dyn KeyCipher + Send + Sync>,
    ) -> Result<Self> {
        Self::open_internal(data_dir, Some(cipher.as_ref()))
    }

    fn open_internal(data_dir: PathBuf, cipher: Option<&dyn KeyCipher>) -> Result<Self> {
        std::fs::create_dir_all(&data_dir)?;
        let db_path = data_dir.join("data.sqlite");
        let conn = Connection::open(&db_path)?;
        conn.pragma_update(None, "busy_timeout", 5000)?;
        let existing_version = schema_version_on(&conn)?;
        anyhow::ensure!(
            existing_version <= CURRENT_SCHEMA_VERSION,
            "database schema version {existing_version} is newer than this build supports ({CURRENT_SCHEMA_VERSION}); restore a matching data directory and encryption key"
        );
        let is_fresh = is_fresh_empty_database(&conn, existing_version)?;
        // Host-cipher opens probe persisted account key/password ciphertext
        // before migrate() can mutate the file. Decrypt failure fails closed.
        // v2 AEAD rejects a wrong host cipher; legacy XOR is still readable
        // so backups restore. Database::open (cipher None) skips this; v27
        // still probes when that rewrite runs. Empty or no-auth rows have
        // nothing to decrypt.
        if cipher.is_some() {
            preflight_ciphertext_probes(&conn, cipher)?;
        }
        ensure_pre_v22_backup(&conn, &db_path)?;
        ensure_pre_v23_backup(&conn, &db_path)?;
        // WAL keeps request-path log writes off the rollback-journal FULL fsync;
        // must be set outside any transaction, hence before migrate().
        let _journal_mode: String =
            conn.query_row("PRAGMA journal_mode = WAL", [], |row| row.get(0))?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        let db = Self { conn };
        db.migrate()?;
        migrate_to_v27(&db.conn, &db_path, cipher, is_fresh)?;
        migrate_to_v28(&db.conn)?;
        migrate_to_v29(&db.conn)?;
        migrate_to_v30(&db.conn)?;
        migrate_to_v31(&db.conn)?;
        migrate_to_v32(&db.conn)?;
        migrate_to_v33(&db.conn)?;
        migrate_to_v34(&db.conn)?;
        migrate_to_v35(&db.conn, &db_path, is_fresh)?;
        migrate_to_v36(&db.conn)?;
        migrate_to_v37(&db.conn)?;
        platform::migrate_to_v38(&db.conn)?;
        ensure_dynamic_provider_tables(&db.conn)?;
        migrate_to_v39(&db.conn)?;
        migrate_to_v40(&db.conn)?;
        migrate_to_v41(&db.conn)?;
        migrate_to_v42(&db.conn, &db_path, is_fresh)?;
        migrate_to_v43(&db.conn)?;
        migrate_to_v44(&db.conn)?;
        identity::migrate_to_v45(&db.conn)?;
        identity::migrate_to_v46(&db.conn)?;
        migrate_to_v47(&db.conn)?;
        identity::ensure_identity_model_consistent(&db.conn)?;
        if let Some(cipher) = cipher {
            repair_legacy_account_ciphertext(&db.conn, cipher)?;
        }
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
        // on released v1.6.3 libraries and on fresh installs.
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
            );",
        )?;
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
             (provider_id, revision, activated_at, document_updated_at,
              source_url, content_hash, snapshot_json)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                OPENCODE_PROVIDER_ID,
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
                 WHERE provider_id = ?1",
                params![OPENCODE_ZEN_FREE_PROVIDER_ID],
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
        self.set_zen_free_model_catalog_with_default_off(catalog, &catalog.models)
    }

    pub fn set_zen_free_model_catalog_with_default_off(
        &self,
        catalog: &crate::kernel::zen::ZenFreeModelCatalog,
        previous_models: &[String],
    ) -> Result<()> {
        let now = Utc::now();
        let models_json = serde_json::to_string(&catalog.models)?;
        let refreshed_at = catalog.refreshed_at.map(|value| value.to_rfc3339());
        let tx = self.conn.unchecked_transaction()?;
        tx.execute(
            "INSERT INTO provider_model_catalogs
             (provider_id, models_json, refreshed_at, source_url)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(provider_id) DO UPDATE SET
                 models_json = excluded.models_json,
                 refreshed_at = excluded.refreshed_at,
                 source_url = excluded.source_url",
            params![
                OPENCODE_ZEN_FREE_PROVIDER_ID,
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
            now,
        )?;
        mark_new_catalog_models_default_off_on(
            &tx,
            &ContractScope::provider(OPENCODE_ZEN_FREE_PROVIDER_ID),
            previous_models,
            &catalog.models,
            now,
        )?;
        tx.commit()?;
        Ok(())
    }

    pub fn load_persisted_contracts(&self) -> Result<PersistedContracts> {
        let mut persisted = PersistedContracts::default();
        {
            let mut stmt = self.conn.prepare(
                "SELECT provider_id, model_id, protocol FROM provider_model_protocol_preferences ORDER BY provider_id, model_id",
            )?;
            let rows = stmt.query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                ))
            })?;
            for row in rows {
                let (provider_id, model_id, protocol) = row?;
                let protocol = UpstreamProtocolKind::try_from(protocol.as_str())?;
                persisted
                    .preferences
                    .entry(ContractScope::provider(&provider_id))
                    .or_default()
                    .push((model_id, protocol));
            }
        }
        {
            let mut stmt = self.conn.prepare(
                "SELECT scope_kind, scope_id, catalog_models_json, catalog_refreshed_at,
                        catalog_source, catalog_source_url, revision, updated_at
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
        {
            let mut stmt = self.conn.prepare(
                "SELECT scope_kind, scope_id, model_id, protocol, state, updated_at
                 FROM provider_contract_model_protocol_overrides",
            )?;
            let rows = stmt.query_map([], persist_override_from_row)?;
            for row in rows {
                let row = row?;
                persisted
                    .overrides
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
                        catalog_source, catalog_source_url, revision, updated_at
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

    pub fn refresh_contract_catalog_with_default_off(
        &self,
        scope: &ContractScope,
        previous_models: &[String],
        models: &[String],
        refreshed_at: DateTime<Utc>,
        source: &str,
        source_url: &str,
    ) -> Result<PersistedScopeRow> {
        let tx = self.conn.unchecked_transaction()?;
        upsert_contract_catalog_on(
            &tx,
            scope,
            models,
            Some(refreshed_at),
            source,
            source_url,
            refreshed_at,
        )?;
        mark_new_catalog_models_default_off_on(&tx, scope, previous_models, models, refreshed_at)?;
        let row = load_scope_on(&tx, scope)?
            .ok_or_else(|| anyhow::anyhow!("contract scope was not persisted"))?;
        tx.commit()?;
        Ok(row)
    }

    pub fn set_model_protocol_overrides(
        &self,
        scope: &ContractScope,
        rows: &[(String, UpstreamProtocolKind, ProtocolOverrideState)],
        now: DateTime<Utc>,
    ) -> Result<PersistedScopeRow> {
        self.set_model_protocol_settings(scope, rows, &[], now)
    }

    /// `preferences` is merged into the saved preferred protocol choices; an empty
    /// slice leaves existing preferences untouched. Only
    /// `reset_provider_static_model_protocols` clears them.
    pub fn set_model_protocol_settings(
        &self,
        scope: &ContractScope,
        rows: &[(String, UpstreamProtocolKind, ProtocolOverrideState)],
        preferences: &[(String, UpstreamProtocolKind)],
        now: DateTime<Utc>,
    ) -> Result<PersistedScopeRow> {
        anyhow::ensure!(
            !rows.is_empty(),
            "model protocol override batch must be nonempty"
        );
        let tx = self.conn.unchecked_transaction()?;
        ensure_contract_scope_row(&tx, scope, now)?;
        for (model_id, protocol, state) in rows {
            set_model_protocol_override_on(&tx, scope, model_id, *protocol, *state, now)?;
        }
        set_model_protocol_preferences_on(&tx, scope, preferences)?;
        bump_scope_revision_on(&tx, scope, now)?;
        let scope = load_scope_on(&tx, scope)?
            .ok_or_else(|| anyhow::anyhow!("contract scope was not persisted"))?;
        tx.commit()?;
        Ok(scope)
    }

    /// Clear mutable protocol judgments for a built-in snapshot provider while
    /// preserving its current catalog. Static-supported pairs stay Auto against the
    /// official protocol baseline. Protocols inside the adapter ceiling but
    /// absent from that baseline receive ForceOff; pairs outside the ceiling
    /// are omitted rather than resurrected.
    pub fn reset_provider_static_model_protocols(
        &self,
        scope: &ContractScope,
        current_models: &[String],
        now: DateTime<Utc>,
    ) -> Result<PersistedScopeRow> {
        anyhow::ensure!(
            scope.kind_str() == crate::provider_contracts::SCOPE_KIND_PROVIDER
                && crate::provider_contracts::static_protocol_snapshot_date(scope.id()).is_some(),
            "static protocol reset is only valid for a built-in snapshot provider"
        );
        let tx = self.conn.unchecked_transaction()?;
        ensure_contract_scope_row(&tx, scope, now)?;
        tx.execute(
            "DELETE FROM provider_model_protocol_preferences WHERE provider_id=?1",
            [scope.id()],
        )?;
        tx.execute(
            "DELETE FROM provider_contract_model_protocols
             WHERE scope_kind = ?1 AND scope_id = ?2",
            params![scope.kind_str(), scope.id()],
        )?;
        tx.execute(
            "DELETE FROM provider_contract_model_protocol_overrides
             WHERE scope_kind = ?1 AND scope_id = ?2",
            params![scope.kind_str(), scope.id()],
        )?;
        let descriptor = crate::provider_contracts::provider_scope_descriptor(scope.id())
            .expect("validated built-in snapshot provider");
        for model_id in current_models {
            let static_protocols = crate::provider_contracts::static_verified_protocols(
                descriptor.kind,
                model_id,
                &[],
            );
            let ceiling = crate::provider_contracts::safety_ceiling_protocols(
                descriptor.protocol_probe,
                model_id,
            );
            for protocol in [
                UpstreamProtocolKind::ChatCompletions,
                UpstreamProtocolKind::Responses,
                UpstreamProtocolKind::Messages,
            ] {
                if ceiling.contains(&protocol) && !static_protocols.contains(&protocol) {
                    set_model_protocol_override_on(
                        &tx,
                        scope,
                        model_id,
                        protocol,
                        ProtocolOverrideState::ForceOff,
                        now,
                    )?;
                }
            }
        }
        bump_scope_revision_on(&tx, scope, now)?;
        let row = load_scope_on(&tx, scope)?
            .ok_or_else(|| anyhow::anyhow!("contract scope was not persisted"))?;
        tx.commit()?;
        Ok(row)
    }

    /// Commit one probe batch — evidence observations plus the binary
    /// overrides each probed protocol implies — in a single transaction with
    /// a single scope-revision bump.
    pub fn commit_model_protocol_probe_results(
        &self,
        scope: &ContractScope,
        observations: &[PersistedModelProtocol],
        overrides: &[(String, UpstreamProtocolKind, ProtocolOverrideState)],
        now: DateTime<Utc>,
    ) -> Result<PersistedScopeRow> {
        anyhow::ensure!(
            !observations.is_empty() || !overrides.is_empty(),
            "probe result batch must be nonempty"
        );
        anyhow::ensure!(
            observations.iter().all(|row| row.scope == *scope),
            "probe observations must belong to the committed contract scope"
        );
        let tx = self.conn.unchecked_transaction()?;
        for row in observations {
            upsert_model_protocol_row_on(&tx, row)?;
        }
        for (model_id, protocol, state) in overrides {
            set_model_protocol_override_on(&tx, scope, model_id, *protocol, *state, now)?;
        }
        bump_scope_revision_on(&tx, scope, now)?;
        let scope = load_scope_on(&tx, scope)?
            .ok_or_else(|| anyhow::anyhow!("contract scope was not persisted"))?;
        tx.commit()?;
        Ok(scope)
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
            builtin_provider(&snapshot.provider_id).is_some(),
            "unknown provider `{}`",
            snapshot.provider_id
        );
        self.conn.execute(
            "INSERT OR IGNORE INTO provider_pricing_snapshots
             (provider_id, revision, activated_at, document_updated_at,
              source_url, content_hash, snapshot_json)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                snapshot.provider_id,
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
    ) -> Result<Option<ProviderPricingSnapshot>> {
        self.conn
            .query_row(
                "SELECT provider_id, revision, activated_at,
                        document_updated_at, source_url, content_hash, snapshot_json
                 FROM provider_pricing_snapshots
                 WHERE provider_id = ?1
                 ORDER BY activated_at DESC, rowid DESC LIMIT 1",
                params![provider_id],
                |row| {
                    Ok(ProviderPricingSnapshot {
                        provider_id: row.get(0)?,
                        revision: row.get(1)?,
                        activated_at: row.get(2)?,
                        document_updated_at: row.get(3)?,
                        source_url: row.get(4)?,
                        content_hash: row.get(5)?,
                        snapshot_json: row.get(6)?,
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
        ensure_account_provider_binding(self, account)?;
        let purchase_date = if account.purchase_date.trim().is_empty() {
            local_today()
        } else {
            normalize_purchase_date(&account.purchase_date)?
        };
        let verification_status = builtin_provider(&account.provider_id)
            .map(default_verification_status)
            .unwrap_or(ConnectionVerificationStatus::NotRequired);
        let tx = self.conn.unchecked_transaction()?;
        insert_account_row(&tx, account, &purchase_date, verification_status)?;
        tx.commit()?;
        Ok(())
    }

    /// Persist the account row together with Custom config/capabilities in one
    /// SQLite transaction. A crash or constraint failure leaves no orphan account
    /// and does not rely on compensating deletes.
    pub fn create_account_with_contract(
        &self,
        account: &Account,
        custom_config: Option<&AccountCustomConfigInput>,
        capabilities: &[AccountModelCapabilityInput],
    ) -> Result<()> {
        self.create_account_with_contract_and_billing(account, custom_config, capabilities, None)
    }

    /// Same as [`Self::create_account_with_contract`], writing Ollama billing in
    /// the same SQLite transaction when the account is an Ollama Cloud row.
    pub fn create_account_with_contract_and_billing(
        &self,
        account: &Account,
        custom_config: Option<&AccountCustomConfigInput>,
        capabilities: &[AccountModelCapabilityInput],
        ollama_billing: Option<OllamaBillingTier>,
    ) -> Result<()> {
        anyhow::ensure!(
            account.id != ZEN_FREE_ACCOUNT_ID,
            "Zen Free is database-owned and cannot be created through the generic account API"
        );
        ensure_account_provider_binding(self, account)?;
        let plan = builtin_provider(&account.provider_id)
            .ok_or_else(|| anyhow::anyhow!("unknown provider offering"))?;
        if plan_requires_custom_config(plan) {
            anyhow::ensure!(
                custom_config.is_some(),
                "Custom API accounts require a base URL, at least one upstream protocol, and an auth scheme"
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
        if let Some(tier) = ollama_billing {
            anyhow::ensure!(
                account.provider_id == OLLAMA_PROVIDER_ID,
                "Ollama billing tier is only valid for Ollama Cloud accounts"
            );
            if tier.requires_purchase_date() && account.purchase_date.trim().is_empty() {
                anyhow::bail!("a configured Ollama paid tier requires purchase_date");
            }
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
            persist_account_custom_config_on(&tx, &account.id, config)?;
        }
        if !capabilities.is_empty() {
            persist_account_model_capabilities_on(&tx, &account.id, capabilities)?;
        }
        if account.provider_id == OLLAMA_PROVIDER_ID || ollama_billing.is_some() {
            set_ollama_cloud_billing_tier_on(&tx, &account.id, ollama_billing)?;
        }
        tx.commit()?;
        Ok(())
    }

    pub fn list_dynamic_providers(&self) -> Result<Vec<DynamicProviderRuntime>> {
        list_dynamic_providers_on(&self.conn)
    }

    /// Control-plane listing: configured rows plus persisted onboarding drafts.
    pub fn list_control_plane_dynamic_providers(&self) -> Result<Vec<DynamicProviderRuntime>> {
        list_control_plane_dynamic_providers_on(&self.conn)
    }

    pub fn onboarding_draft_provider_ids(&self) -> Result<HashSet<String>> {
        onboarding_draft_provider_ids_on(&self.conn)
    }

    pub fn provider_is_onboarding_draft(&self, provider_id: &str) -> Result<Option<bool>> {
        provider_is_onboarding_draft_on(&self.conn, provider_id)
    }

    #[cfg(test)]
    pub(crate) fn replace_credential_id(
        &self,
        account_id: &str,
        credential_id: &str,
    ) -> Result<()> {
        let updated = self.conn.execute(
            "UPDATE credential_state SET credential_id = ?2 WHERE account_id = ?1",
            params![account_id, credential_id],
        )?;
        anyhow::ensure!(
            updated == 1,
            "account {account_id} is missing credential_state"
        );
        self.conn.execute(
            "UPDATE legacy_identity_map SET new_id = ?2
             WHERE legacy_id = ?1 AND new_kind = 'credential'",
            params![account_id, credential_id],
        )?;
        Ok(())
    }

    pub fn get_dynamic_provider(
        &self,
        provider_id: &str,
    ) -> Result<Option<DynamicProviderRuntime>> {
        get_dynamic_provider_on(&self.conn, provider_id)
    }

    /// Read any `providers` row (builtin preset/custom) by id. Used by the
    /// public GET to surface the unified view; the handler still rejects
    /// PATCH/DELETE on builtin rows.
    pub fn get_provider_definition(
        &self,
        provider_id: &str,
    ) -> Result<Option<DynamicProviderRuntime>> {
        get_provider_definition_on(&self.conn, provider_id)
    }

    pub fn count_accounts_for_provider(&self, provider_id: &str) -> Result<i64> {
        count_accounts_for_provider_on(&self.conn, provider_id)
    }

    pub fn create_dynamic_provider(
        &self,
        runtime: &DynamicProviderRuntime,
        first_account: &Account,
    ) -> Result<Vec<DynamicProviderRuntime>> {
        let tx = self.conn.unchecked_transaction()?;
        insert_dynamic_provider_on(&tx, runtime, false)?;
        dynamic_tx_fault("after_provider_insert")?;
        let purchase_date = if first_account.purchase_date.trim().is_empty() {
            local_today()
        } else {
            normalize_purchase_date(&first_account.purchase_date)?
        };
        insert_account_row(
            &tx,
            first_account,
            &purchase_date,
            ConnectionVerificationStatus::NotRequired,
        )?;
        dynamic_tx_fault("after_account_insert")?;
        let snapshot = list_dynamic_providers_on(&tx)?;
        tx.commit()?;
        Ok(snapshot)
    }

    /// Persist a user-defined Provider with no first account.
    /// Keyed definitions use this when the Key will be added later on Accounts.
    pub fn create_dynamic_provider_definition(
        &self,
        runtime: &DynamicProviderRuntime,
    ) -> Result<Vec<DynamicProviderRuntime>> {
        let tx = self.conn.unchecked_transaction()?;
        insert_dynamic_provider_on(&tx, runtime, false)?;
        let snapshot = list_dynamic_providers_on(&tx)?;
        tx.commit()?;
        Ok(snapshot)
    }

    pub fn find_dashboard_operation(
        &self,
        operation_id: &str,
    ) -> Result<Option<DashboardOperationRow>> {
        find_dashboard_operation_on(&self.conn, operation_id)
    }

    /// Create a user-defined Provider (and optional first account) together with
    /// the dashboard operation ledger row in one SQLite transaction.
    pub fn commit_onboarding_new(
        &self,
        runtime: &DynamicProviderRuntime,
        first_account: Option<&Account>,
        onboarding_draft: bool,
        operation: &NewDashboardOperation,
    ) -> Result<Vec<DynamicProviderRuntime>> {
        let tx = self.conn.unchecked_transaction()?;
        insert_dynamic_provider_on(&tx, runtime, onboarding_draft)?;
        dynamic_tx_fault("after_provider_insert")?;
        if let Some(account) = first_account {
            let purchase_date = if account.purchase_date.trim().is_empty() {
                local_today()
            } else {
                normalize_purchase_date(&account.purchase_date)?
            };
            insert_account_row(
                &tx,
                account,
                &purchase_date,
                ConnectionVerificationStatus::NotRequired,
            )?;
            dynamic_tx_fault("after_account_insert")?;
        }
        insert_dashboard_operation_on(&tx, operation)?;
        let snapshot = list_dynamic_providers_on(&tx)?;
        tx.commit()?;
        Ok(snapshot)
    }

    /// Add a Key account to an existing user-defined Provider together with the
    /// dashboard operation ledger row in one SQLite transaction.
    pub fn commit_onboarding_existing_account(
        &self,
        account: &Account,
        operation: &NewDashboardOperation,
    ) -> Result<()> {
        anyhow::ensure!(
            account.id != ZEN_FREE_ACCOUNT_ID,
            "Zen Free is database-owned and cannot be created through the generic account API"
        );
        ensure_account_provider_binding(self, account)?;
        let purchase_date = if account.purchase_date.trim().is_empty() {
            local_today()
        } else {
            normalize_purchase_date(&account.purchase_date)?
        };
        let verification_status = builtin_provider(&account.provider_id)
            .map(default_verification_status)
            .unwrap_or(ConnectionVerificationStatus::NotRequired);
        let tx = self.conn.unchecked_transaction()?;
        insert_account_row(&tx, account, &purchase_date, verification_status)?;
        dynamic_tx_fault("after_account_insert")?;
        insert_dashboard_operation_on(&tx, operation)?;
        tx.commit()?;
        Ok(())
    }

    /// Resume or complete a stored draft in one transaction with the ledger row.
    /// Routing snapshot excludes remaining drafts.
    pub fn commit_onboarding_resume(
        &self,
        runtime: &DynamicProviderRuntime,
        onboarding_draft: bool,
        create_account: Option<&Account>,
        rotate: Option<(&str, &str)>,
        account_meta: Option<(&str, Option<&str>, Option<Option<&str>>)>,
        grant_union: Option<(&str, &[String], &[String])>,
        sync_auth: Option<(&str, &str, &str)>,
        operation: &NewDashboardOperation,
    ) -> Result<Vec<DynamicProviderRuntime>> {
        let tx = self.conn.unchecked_transaction()?;
        let existing = get_dynamic_provider_on(&tx, &runtime.id)?
            .ok_or_else(|| anyhow::anyhow!("unknown provider `{}`", runtime.id))?;
        tx.execute(
            "UPDATE providers
             SET name = ?1, endpoint_url = ?2, upstream_protocol = ?3, auth_kind = ?4,
                 updated_at = ?5, preset_id = ?7, offering = ?8, onboarding_draft = ?9
             WHERE id = ?6",
            params![
                runtime.name,
                runtime.endpoint_url,
                runtime.upstream_protocol.as_str(),
                runtime.auth_kind.as_str(),
                runtime.updated_at.to_rfc3339(),
                existing.id,
                runtime.preset_id,
                runtime.offering.as_str(),
                onboarding_draft as i32,
            ],
        )?;
        tx.execute(
            "DELETE FROM provider_models WHERE provider_id = ?1",
            [&existing.id],
        )?;
        insert_dynamic_provider_models_on(&tx, &existing.id, &runtime.mappings)?;
        dynamic_tx_fault("after_mapping_replace")?;
        if let Some(account) = create_account {
            let purchase_date = if account.purchase_date.trim().is_empty() {
                local_today()
            } else {
                normalize_purchase_date(&account.purchase_date)?
            };
            insert_account_row(
                &tx,
                account,
                &purchase_date,
                ConnectionVerificationStatus::NotRequired,
            )?;
            dynamic_tx_fault("after_account_insert")?;
        }
        if let Some((account_id, key_cipher)) = rotate {
            identity::rotate_account_credential_in(&tx, account_id, key_cipher)?;
        }
        if let Some((account_id, name, notes)) = account_meta {
            let now = Utc::now().to_rfc3339();
            match (name, notes) {
                (Some(name), Some(notes)) => {
                    tx.execute(
                        "UPDATE accounts SET name = ?2, notes = ?3, updated_at = ?4 WHERE id = ?1",
                        params![account_id, name, notes, now],
                    )?;
                }
                (Some(name), None) => {
                    tx.execute(
                        "UPDATE accounts SET name = ?2, updated_at = ?3 WHERE id = ?1",
                        params![account_id, name, now],
                    )?;
                }
                (None, Some(notes)) => {
                    tx.execute(
                        "UPDATE accounts SET notes = ?2, updated_at = ?3 WHERE id = ?1",
                        params![account_id, notes, now],
                    )?;
                }
                (None, None) => {}
            }
        }
        if let Some((account_id, ids, origins)) = grant_union {
            identity::replace_binding_grants_for_account_on(&tx, account_id, ids, origins)?;
        }
        if let Some((account_id, credential_kind, quota_scope)) = sync_auth {
            tx.execute(
                "UPDATE accounts SET credential_kind = ?2, quota_scope = ?3,
                     setup_step = 'ready', updated_at = ?4
                 WHERE id = ?1",
                params![
                    account_id,
                    credential_kind,
                    quota_scope,
                    Utc::now().to_rfc3339(),
                ],
            )?;
        }
        insert_dashboard_operation_on(&tx, operation)?;
        let snapshot = list_dynamic_providers_on(&tx)?;
        tx.commit()?;
        Ok(snapshot)
    }

    pub fn replace_dynamic_provider(
        &self,
        runtime: &DynamicProviderRuntime,
        clear_runtime_state: bool,
        clear_keys: bool,
        replacement_key_cipher: Option<&str>,
    ) -> Result<Vec<DynamicProviderRuntime>> {
        let tx = self.conn.unchecked_transaction()?;
        let existing = get_dynamic_provider_on(&tx, &runtime.id)?
            .ok_or_else(|| anyhow::anyhow!("unknown provider `{}`", runtime.id))?;
        tx.execute(
            "UPDATE providers
             SET name = ?1, endpoint_url = ?2, upstream_protocol = ?3, auth_kind = ?4,
                 updated_at = ?5, preset_id = ?7, offering = ?8
             WHERE id = ?6",
            params![
                runtime.name,
                runtime.endpoint_url,
                runtime.upstream_protocol.as_str(),
                runtime.auth_kind.as_str(),
                runtime.updated_at.to_rfc3339(),
                existing.id,
                runtime.preset_id,
                runtime.offering.as_str(),
            ],
        )?;
        tx.execute(
            "DELETE FROM provider_models WHERE provider_id = ?1",
            [&existing.id],
        )?;
        insert_dynamic_provider_models_on(&tx, &existing.id, &runtime.mappings)?;
        dynamic_tx_fault("after_mapping_replace")?;
        if clear_runtime_state {
            tx.execute(
                "UPDATE accounts SET
                    auth_error = NULL,
                    last_error = NULL,
                    cooldown_until = NULL,
                    cooldown_generic_until = NULL,
                    cooldown_5h_until = NULL,
                    cooldown_week_until = NULL,
                    cooldown_month_until = NULL,
                    cooldown_free_until = NULL,
                    updated_at = ?1
                 WHERE lower(provider_id) = lower(?2)",
                params![runtime.updated_at.to_rfc3339(), existing.id],
            )?;
            let mut account_ids = Vec::new();
            {
                let mut stmt =
                    tx.prepare("SELECT id FROM accounts WHERE lower(provider_id) = lower(?1)")?;
                let rows = stmt.query_map([&existing.id], |row| row.get::<_, String>(0))?;
                for id in rows {
                    account_ids.push(id?);
                }
            }
            for account_id in account_ids {
                invalidate_probe_evidence_on(
                    &tx,
                    &ContractScope::custom_endpoint(&account_id),
                    runtime.updated_at,
                )?;
            }
        }
        if clear_keys {
            tx.execute(
                "UPDATE accounts SET key_cipher = '', credential_kind = ?1, quota_scope = ?2, updated_at = ?3
                 WHERE lower(provider_id) = lower(?4)",
                params![
                    runtime.auth_kind.credential_kind().as_str(),
                    runtime.auth_kind.quota_scope().as_str(),
                    runtime.updated_at.to_rfc3339(),
                    existing.id,
                ],
            )?;
        } else if let Some(key_cipher) = replacement_key_cipher {
            let count = count_accounts_for_provider_on(&tx, &existing.id)?;
            anyhow::ensure!(
                count == 1,
                "replacement Key can only be written to the singleton account"
            );
            tx.execute(
                "UPDATE accounts SET key_cipher = ?1, credential_kind = ?2, quota_scope = ?3, updated_at = ?4
                 WHERE lower(provider_id) = lower(?5)",
                params![
                    key_cipher,
                    runtime.auth_kind.credential_kind().as_str(),
                    runtime.auth_kind.quota_scope().as_str(),
                    runtime.updated_at.to_rfc3339(),
                    existing.id,
                ],
            )?;
        } else {
            tx.execute(
                "UPDATE accounts SET credential_kind = ?1, quota_scope = ?2, updated_at = ?3
                 WHERE lower(provider_id) = lower(?4)",
                params![
                    runtime.auth_kind.credential_kind().as_str(),
                    runtime.auth_kind.quota_scope().as_str(),
                    runtime.updated_at.to_rfc3339(),
                    existing.id,
                ],
            )?;
        }
        let snapshot = list_dynamic_providers_on(&tx)?;
        tx.commit()?;
        Ok(snapshot)
    }

    pub fn delete_dynamic_provider(
        &self,
        provider_id: &str,
    ) -> Result<Vec<DynamicProviderRuntime>> {
        let tx = self.conn.unchecked_transaction()?;
        let existing = get_dynamic_provider_on(&tx, provider_id)?
            .ok_or_else(|| anyhow::anyhow!("unknown provider `{provider_id}`"))?;
        let count: i64 = tx.query_row(
            "SELECT COUNT(*) FROM accounts WHERE lower(provider_id) = lower(?1)",
            [&existing.id],
            |row| row.get(0),
        )?;
        anyhow::ensure!(
            count == 0,
            "dynamic provider still has {count} referencing account(s)"
        );
        tx.execute(
            "DELETE FROM provider_models WHERE provider_id = ?1",
            [&existing.id],
        )?;
        tx.execute("DELETE FROM providers WHERE id = ?1", [&existing.id])?;
        let snapshot = list_dynamic_providers_on(&tx)?;
        tx.commit()?;
        Ok(snapshot)
    }

    /// Insert every migrated account and its Custom contract in one SQLite
    /// transaction. Rows append in the supplied order. Any validation,
    /// constraint, or child-table failure rolls the entire batch back.
    pub fn import_accounts_with_contracts(&self, records: &[AccountImportRecord]) -> Result<()> {
        let tx = self.conn.unchecked_transaction()?;
        for record in records {
            insert_import_account_on(&tx, record)?;
        }
        tx.commit()?;
        Ok(())
    }

    /// Merge one V2 node migration package by stable account/Key id. Existing
    /// destination rows keep their current order; source-only rows append in
    /// package order. All database-owned state shares one SQLite transaction.
    pub fn import_node_state<T>(
        &self,
        record: &NodeImportRecord,
        prepare_runtime: impl FnOnce(&Database) -> Result<T>,
    ) -> Result<T> {
        let tx = self.conn.unchecked_transaction()?;
        let mut ordered_ids = Vec::new();
        {
            let mut stmt = tx.prepare(
                "SELECT id FROM accounts ORDER BY sort_order ASC, created_at ASC, id ASC",
            )?;
            let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
            for id in rows {
                ordered_ids.push(id?);
            }
        }
        let imported_account_ids = record
            .accounts
            .iter()
            .map(|record| record.account.id.to_ascii_lowercase())
            .collect::<HashSet<_>>();
        for runtime in &record.dynamic_providers {
            let onboarding_draft = record.draft_provider_ids.contains(&runtime.id)
                || record
                    .draft_provider_ids
                    .iter()
                    .any(|id| id.eq_ignore_ascii_case(&runtime.id));
            upsert_imported_dynamic_provider_on(
                &tx,
                runtime,
                &imported_account_ids,
                onboarding_draft,
            )?;
        }
        if record.platform_links_authoritative {
            for account in &record.accounts {
                tx.execute(
                    "DELETE FROM platform_links WHERE account_id=?1",
                    [&account.account.id],
                )?;
            }
        }
        for account in &record.accounts {
            merge_import_account_on(&tx, account)?;
            if record.identity_snapshot.is_some() {
                // V6 carries cooldowns. Merging a package must not shorten a
                // deadline already observed on the destination host.
                let current = self
                    .get_account(&account.account.id)?
                    .ok_or_else(|| anyhow::anyhow!("imported account is missing"))?;
                let source = &account.account;
                let generic = current
                    .cooldown_generic_until
                    .max(source.cooldown_generic_until);
                let five_hours = current.cooldown_5h_until.max(source.cooldown_5h_until);
                let week = current.cooldown_week_until.max(source.cooldown_week_until);
                let month = current
                    .cooldown_month_until
                    .max(source.cooldown_month_until);
                let free = current.cooldown_free_until.max(source.cooldown_free_until);
                let until = current
                    .cooldown_until
                    .max(source.cooldown_until)
                    .max(generic)
                    .max(five_hours)
                    .max(week)
                    .max(month)
                    .max(free);
                tx.execute(
                    "UPDATE accounts SET cooldown_until=?2, cooldown_generic_until=?3,
                         cooldown_5h_until=?4, cooldown_week_until=?5,
                         cooldown_month_until=?6, cooldown_free_until=?7 WHERE id=?1",
                    params![
                        source.id,
                        until.map(|v| v.to_rfc3339()),
                        generic.map(|v| v.to_rfc3339()),
                        five_hours.map(|v| v.to_rfc3339()),
                        week.map(|v| v.to_rfc3339()),
                        month.map(|v| v.to_rfc3339()),
                        free.map(|v| v.to_rfc3339())
                    ],
                )?;
            }
        }
        platform::merge_platforms_on(
            &tx,
            &record.platform_accounts,
            &record.platform_links,
            &imported_account_ids,
        )?;

        let (sanitized, primary) = sanitize_config_json_primary_key(&record.config_json)?;
        let primary =
            primary.ok_or_else(|| anyhow::anyhow!("node migration primary Key is missing"))?;
        tx.execute(
            "INSERT INTO settings (key, value) VALUES ('config', ?1)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            [sanitized],
        )?;
        for key in &record.sub_keys {
            anyhow::ensure!(key.deleted_at.is_none(), "migrated sub Key must be active");
            anyhow::ensure!(
                key.id != PRIMARY_KEY_ID,
                "sub Key cannot use the primary id"
            );
            tx.execute(
                "DELETE FROM access_keys WHERE id = ?1 AND is_primary = 0",
                [&key.id],
            )?;
        }
        upsert_primary_access_key_on(&tx, &primary)?;
        for key in &record.sub_keys {
            tx.execute(
                "INSERT INTO access_keys (id, name, key, is_primary, enabled, deleted_at, created_at)
                 VALUES (?1, ?2, ?3, 0, ?4, NULL, ?5)",
                params![
                    key.id,
                    key.name,
                    key.key,
                    key.enabled as i32,
                    key.created_at.to_rfc3339(),
                ],
            )?;
        }
        let merged_sub_keys: i64 = tx.query_row(
            "SELECT COUNT(*) FROM access_keys WHERE is_primary = 0 AND deleted_at IS NULL",
            [],
            |row| row.get(0),
        )?;
        anyhow::ensure!(
            merged_sub_keys <= 64,
            "merged node would exceed the 64 active sub Key limit"
        );

        let zen_changed = tx.execute(
            "UPDATE accounts SET enabled = ?2, free_alias_enabled = 0, updated_at = ?3
             WHERE id = ?1 AND provider_id = ?4 ",
            params![
                ZEN_FREE_ACCOUNT_ID,
                record.zen_free_enabled as i32,
                Utc::now().to_rfc3339(),
                OPENCODE_ZEN_FREE_PROVIDER_ID,
            ],
        )?;
        anyhow::ensure!(zen_changed == 1, "Zen Free singleton is missing");

        let mut ordered_set = ordered_ids.iter().cloned().collect::<HashSet<_>>();
        for id in &record.account_order {
            if ordered_set.insert(id.clone()) {
                ordered_ids.push(id.clone());
            }
        }
        let current_ids = {
            let mut stmt = tx.prepare("SELECT id FROM accounts")?;
            stmt.query_map([], |row| row.get::<_, String>(0))?
                .collect::<rusqlite::Result<HashSet<_>>>()?
        };
        anyhow::ensure!(
            ordered_ids.len() == current_ids.len()
                && ordered_ids.iter().collect::<HashSet<_>>().len() == ordered_ids.len()
                && ordered_ids.iter().all(|id| current_ids.contains(id)),
            "migrated account order does not cover the merged account set"
        );
        for (sort_order, id) in ordered_ids.iter().enumerate() {
            tx.execute(
                "UPDATE accounts SET sort_order = ?1 WHERE id = ?2",
                params![sort_order as i64, id],
            )?;
        }
        let zen_models_json = serde_json::to_string(&record.zen_catalog.models)?;
        tx.execute(
            "INSERT INTO provider_model_catalogs
             (provider_id, models_json, refreshed_at, source_url)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(provider_id) DO UPDATE SET
                 models_json = excluded.models_json,
                 refreshed_at = excluded.refreshed_at,
                 source_url = excluded.source_url",
            params![
                OPENCODE_ZEN_FREE_PROVIDER_ID,
                zen_models_json,
                record
                    .zen_catalog
                    .refreshed_at
                    .map(|value| value.to_rfc3339()),
                record.zen_catalog.source_url,
            ],
        )?;

        for (scope, row) in &record.provider_contracts.scopes {
            anyhow::ensure!(
                scope.kind() == crate::provider_contracts::ContractScopeKind::Provider,
                "node migration only accepts Provider contract scopes"
            );
            upsert_contract_catalog_on(
                &tx,
                scope,
                &row.catalog_models,
                row.catalog_refreshed_at,
                &row.catalog_source,
                &row.catalog_source_url,
                row.updated_at,
            )?;
            tx.execute(
                "DELETE FROM provider_contract_model_protocols
                 WHERE scope_kind = ?1 AND scope_id = ?2",
                params![scope.kind_str(), scope.id()],
            )?;
            tx.execute(
                "DELETE FROM provider_contract_model_protocol_overrides
                 WHERE scope_kind = ?1 AND scope_id = ?2",
                params![scope.kind_str(), scope.id()],
            )?;
            for evidence in record
                .provider_contracts
                .evidence
                .get(scope)
                .into_iter()
                .flatten()
            {
                upsert_model_protocol_row_on(&tx, evidence)?;
            }
            for override_row in record
                .provider_contracts
                .overrides
                .get(scope)
                .into_iter()
                .flatten()
            {
                set_model_protocol_override_on(
                    &tx,
                    scope,
                    &override_row.model_id,
                    override_row.protocol,
                    override_row.state,
                    override_row.updated_at,
                )?;
            }
            tx.execute(
                "DELETE FROM provider_model_protocol_preferences WHERE provider_id=?1",
                [scope.id()],
            )?;
            set_model_protocol_preferences_on(
                &tx,
                scope,
                record
                    .provider_contracts
                    .preferences
                    .get(scope)
                    .map(Vec::as_slice)
                    .unwrap_or(&[]),
            )?;
        }

        if let Some(snapshot) = &record.identity_snapshot {
            identity::restore_imported_identity_snapshot_on(&tx, snapshot, &imported_account_ids)?;
        }
        ensure_dynamic_singleton_accounts_on(&tx)?;
        sqlite_foreign_key_check(&tx)?;
        // The callback reads through this same SQLite connection, so it sees
        // the uncommitted merged rows. Every fallible runtime construction
        // step must finish before commit; the caller installs the returned
        // snapshots only after this transaction succeeds.
        let runtime = prepare_runtime(self)?;
        tx.commit()?;
        Ok(runtime)
    }

    pub fn update_account(
        &self,
        id: &str,
        update: &AccountUpdate,
        key_cipher: Option<&str>,
        password_cipher: Option<&str>,
    ) -> Result<()> {
        self.update_account_with_billing(id, update, key_cipher, password_cipher, None)
    }

    /// Persist account field updates and an optional Ollama billing write in
    /// one SQLite transaction. `None` leaves billing unchanged; `Some(None)`
    /// clears the billing row. A billing-write failure leaves the account row
    /// and Key ciphertext untouched.
    pub fn update_account_with_billing(
        &self,
        id: &str,
        update: &AccountUpdate,
        key_cipher: Option<&str>,
        password_cipher: Option<&str>,
        ollama_billing: Option<Option<OllamaBillingTier>>,
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
        let requires_verification = builtin_provider(&existing.provider_id)
            .is_some_and(|plan| plan.verification_policy == VerificationPolicy::Required);
        // Key replacement invalidates verification for every Required plan.
        // Only a descriptor that explicitly gates enablement on verification
        // would also force the account off; current built-ins do not do so.
        let verification_gates_enablement = requires_verification
            && ProviderRegistry::get(&existing.provider_id)
                .is_some_and(|descriptor| descriptor.card_actions.enable_requires_verification);
        // Gate the value that will actually persist. Verification-gated key
        // replacement still forces enabled=0 in SQL; that write is not an
        // enablement of an unroutable Plan.
        let enabled = if key_replaced && verification_gates_enablement {
            false
        } else {
            requested_enabled
        };
        if builtin_provider(&existing.provider_id).is_some() {
            ensure_enabled_provider_is_routable(&existing.provider_id, enabled)?;
        } else {
            anyhow::ensure!(
                self.get_dynamic_provider(&existing.provider_id)?.is_some(),
                "unknown provider `{}`",
                existing.provider_id
            );
        }

        let tx = self.conn.unchecked_transaction()?;
        tx.execute(
            "UPDATE accounts SET name = ?1, username = ?2, password_cipher = ?3, key_cipher = ?4,
             enabled = CASE WHEN ?10 AND ?14 THEN 0 ELSE ?5 END, referral_code = ?6, recharge_date = ?7, notes = ?8,
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
                verification_gates_enablement,
            ],
        )?;
        if key_replaced && requires_verification && is_command_code_goat(&existing.provider_id) {
            tx.execute(
                "DELETE FROM account_model_capabilities
                 WHERE account_id = ?1 AND source = ?2",
                params![id, COMMAND_CODE_GOAT_MODELS_SOURCE],
            )?;
            refresh_goat_provider_catalog_on(&tx)?;
        }
        if let Some(tier) = ollama_billing {
            set_ollama_cloud_billing_tier_on(&tx, id, tier)?;
        }
        if key_replaced {
            tx.execute(
                "UPDATE platform_links SET snapshot=NULL,version=version+1 WHERE account_id=?1",
                [id],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    pub fn set_config(&self, config_json: &str) -> Result<()> {
        let (sanitized, primary) = sanitize_config_json_primary_key(config_json)?;
        let tx = self.conn.unchecked_transaction()?;
        tx.execute(
            "INSERT INTO settings (key, value) VALUES ('config', ?1)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            [sanitized],
        )?;
        if table_exists(&tx, "access_keys")?
            && let Some(primary) = primary
        {
            upsert_primary_access_key_on(&tx, &primary)?;
        }
        tx.commit()?;
        Ok(())
    }

    pub fn cpa_integration(&self) -> Result<Option<CpaIntegrationRecord>> {
        self.conn
            .query_row(
                "SELECT account_id, base_url, management_key_cipher
                   FROM cpa_integration WHERE id = 'cpa'",
                [],
                |row| {
                    Ok(CpaIntegrationRecord {
                        account_id: row.get(0)?,
                        base_url: row.get(1)?,
                        management_key_cipher: row.get(2)?,
                    })
                },
            )
            .optional()
            .map_err(Into::into)
    }

    /// Atomically creates or updates the one CPA route account and its local
    /// Management connection. Callers pass ciphertext only.
    pub fn upsert_cpa_integration(
        &self,
        account: &Account,
        base_url: &str,
        management_key_cipher: &str,
    ) -> Result<()> {
        anyhow::ensure!(
            account.id == CPA_ACCOUNT_ID,
            "invalid CPA singleton account id"
        );
        anyhow::ensure!(
            account.provider_id == CPA_PROVIDER_ID,
            "invalid CPA singleton binding"
        );
        let plan = builtin_provider(CPA_PROVIDER_ID)
            .ok_or_else(|| anyhow::anyhow!("CPA provider is not registered"))?;
        let mut account = account.clone();
        account.enabled = account.enabled && plan.routable;
        validate_account_binding(
            &account.id,
            &account.provider_id,
            account.credential_kind,
            account.quota_scope,
        )?;
        let tx = self.conn.unchecked_transaction()?;
        let exists = tx
            .query_row(
                "SELECT 1 FROM accounts WHERE id = ?1",
                [CPA_ACCOUNT_ID],
                |_| Ok(()),
            )
            .optional()?
            .is_some();
        if exists {
            tx.execute(
                "UPDATE accounts
                    SET name = ?2,
                        auth_error = CASE WHEN key_cipher <> ?3 THEN NULL ELSE auth_error END,
                        key_cipher = ?3, enabled = ?4,
                        account_type = ?5, setup_step = ?6, updated_at = ?7,
                        provider_id = ?8,
                        credential_kind = ?9, quota_scope = ?10,
                        verification_status = 'not_required',
                        connection_verified_at = NULL, verification_error = NULL
                  WHERE id = ?1",
                params![
                    account.id,
                    account.name,
                    account.key_cipher,
                    account.enabled as i32,
                    account.account_type.as_str(),
                    account.setup_step.as_str(),
                    account.updated_at.to_rfc3339(),
                    account.provider_id,
                    account.credential_kind.as_str(),
                    account.quota_scope.as_str(),
                ],
            )?;
        } else {
            let purchase_date = local_today();
            insert_account_row(
                &tx,
                &account,
                &purchase_date,
                default_verification_status(plan),
            )?;
        }
        tx.execute(
            "INSERT INTO cpa_integration
                 (id, account_id, base_url, management_key_cipher, updated_at)
             VALUES ('cpa', ?1, ?2, ?3, ?4)
             ON CONFLICT(id) DO UPDATE SET
                 account_id = excluded.account_id,
                 base_url = excluded.base_url,
                 management_key_cipher = excluded.management_key_cipher,
                 updated_at = excluded.updated_at",
            params![
                CPA_ACCOUNT_ID,
                base_url,
                management_key_cipher,
                Utc::now().to_rfc3339(),
            ],
        )?;
        tx.commit()?;
        Ok(())
    }

    /// Removes only OCG-owned CPA state. CPA auth files remain owned by CPA.
    pub fn delete_cpa_integration(&self) -> Result<()> {
        let tx = self.conn.unchecked_transaction()?;
        tx.execute("DELETE FROM cpa_integration WHERE id = 'cpa'", [])?;
        tx.execute(
            "DELETE FROM provider_model_catalogs WHERE provider_id = ?1",
            params![CPA_PROVIDER_ID],
        )?;
        tx.execute(
            "DELETE FROM provider_contract_model_protocol_overrides
              WHERE scope_kind = 'provider' AND scope_id = ?1",
            [CPA_PROVIDER_ID],
        )?;
        tx.execute(
            "DELETE FROM provider_contract_model_protocols
              WHERE scope_kind = 'provider' AND scope_id = ?1",
            [CPA_PROVIDER_ID],
        )?;
        tx.execute(
            "DELETE FROM provider_contract_scopes
              WHERE scope_kind = 'provider' AND scope_id = ?1",
            [CPA_PROVIDER_ID],
        )?;
        let identity_id = identity::account_identity_id(&tx, CPA_ACCOUNT_ID)?;
        identity::delete_account_identity_satellites(&tx, CPA_ACCOUNT_ID)?;
        tx.execute("DELETE FROM accounts WHERE id = ?1", [CPA_ACCOUNT_ID])?;
        identity::delete_orphan_identity_for_account(&tx, CPA_ACCOUNT_ID, identity_id.as_deref())?;
        tx.commit()?;
        Ok(())
    }

    pub fn cpa_model_catalog(&self) -> Result<Option<CpaCatalogRecord>> {
        self.conn
            .query_row(
                "SELECT models_json, refreshed_at, source_url
                   FROM provider_model_catalogs
                  WHERE provider_id = ?1",
                params![CPA_PROVIDER_ID],
                |row| {
                    let models_json: String = row.get(0)?;
                    let refreshed_at: Option<String> = row.get(1)?;
                    let models = parse_cpa_catalog_models(&models_json).map_err(|error| {
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
                    Ok(CpaCatalogRecord {
                        models,
                        refreshed_at,
                        source_url: row.get(2)?,
                    })
                },
            )
            .optional()
            .map_err(Into::into)
    }

    pub fn replace_cpa_model_catalog(
        &self,
        models: &[CpaCatalogModel],
        source_url: &str,
        refreshed_at: DateTime<Utc>,
    ) -> Result<()> {
        let models_json = serde_json::to_string(models)?;
        self.conn.execute(
            "INSERT INTO provider_model_catalogs
                 (provider_id, models_json, refreshed_at, source_url)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(provider_id) DO UPDATE SET
                 models_json = excluded.models_json,
                 refreshed_at = excluded.refreshed_at,
                 source_url = excluded.source_url",
            params![
                CPA_PROVIDER_ID,
                models_json,
                refreshed_at.to_rfc3339(),
                source_url,
            ],
        )?;
        Ok(())
    }

    /// Live primary access-key value. After schema v27 this row is the
    /// database authority; sanitized config JSON is not.
    pub fn primary_access_key_value(&self) -> Result<Option<String>> {
        if !table_exists(&self.conn, "access_keys")? {
            return Ok(None);
        }
        let value = self
            .conn
            .query_row(
                "SELECT key FROM access_keys
                 WHERE is_primary = 1 AND deleted_at IS NULL
                 LIMIT 1",
                [],
                |row| row.get::<_, String>(0),
            )
            .optional()?;
        Ok(value.filter(|key| !key.trim().is_empty()))
    }

    /// Test-only seam: drop the live unique index so out-of-model collision
    /// drills can still exercise snapshot/API gates. Compiled only for unit
    /// tests and debug builds; release production source does not expose it.
    #[cfg(any(test, debug_assertions))]
    #[doc(hidden)]
    pub fn test_drop_access_key_unique_index(&self) -> Result<()> {
        self.conn
            .execute_batch("DROP INDEX IF EXISTS idx_access_keys_active_key;")?;
        Ok(())
    }

    /// The Zen Free singleton has one canonical user setting: enabled.
    /// The retired `free_alias_enabled` column is forced to zero for rollback
    /// compatibility but no longer participates in runtime behavior.
    pub fn set_zen_free_enabled(&self, enabled: bool) -> Result<()> {
        ensure_enabled_provider_is_routable(OPENCODE_ZEN_FREE_PROVIDER_ID, enabled)?;
        let changed = self.conn.execute(
            "UPDATE accounts SET enabled = ?2, free_alias_enabled = 0, updated_at = ?3
             WHERE id = ?1 AND provider_id = ?4",
            params![
                ZEN_FREE_ACCOUNT_ID,
                enabled as i32,
                Utc::now().to_rfc3339(),
                OPENCODE_ZEN_FREE_PROVIDER_ID,
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
        tx.execute("DELETE FROM platform_links WHERE account_id = ?1", [id])?;
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
            "DELETE FROM provider_contract_model_protocols
             WHERE scope_kind = ?1 AND scope_id = ?2",
            params![SCOPE_KIND_CUSTOM_ENDPOINT, id],
        )?;
        tx.execute(
            "DELETE FROM provider_contract_model_protocol_overrides
             WHERE scope_kind = ?1 AND scope_id = ?2",
            params![SCOPE_KIND_CUSTOM_ENDPOINT, id],
        )?;
        tx.execute(
            "DELETE FROM provider_contract_scopes
             WHERE scope_kind = ?1 AND scope_id = ?2",
            params![SCOPE_KIND_CUSTOM_ENDPOINT, id],
        )?;
        let identity_id = identity::account_identity_id(&tx, id)?;
        identity::delete_account_identity_satellites(&tx, id)?;
        tx.execute("DELETE FROM accounts WHERE id = ?1", [id])?;
        identity::delete_orphan_identity_for_account(&tx, id, identity_id.as_deref())?;
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
                "Custom API accounts require a persisted endpoint URL and upstream protocol"
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
                "SELECT account_id, endpoint_url, upstream_protocol, created_at, updated_at
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
    ) -> Result<AccountCustomConfig> {
        self.commit_account_custom_config(account_id, input)?;
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
    ) -> Result<()> {
        anyhow::ensure!(self.get_account(account_id)?.is_some(), "account not found");
        let tx = self.conn.unchecked_transaction()?;
        persist_account_custom_config_on(&tx, account_id, input)?;
        tx.commit()?;
        Ok(())
    }

    /// Atomically replace the Custom endpoint binding and its complete model
    /// capability list so request-entry snapshots never observe mismatched
    /// protocols.
    pub(crate) fn commit_account_custom_config_and_capabilities(
        &self,
        account_id: &str,
        input: &AccountCustomConfigInput,
        capabilities: &[AccountModelCapabilityInput],
    ) -> Result<()> {
        anyhow::ensure!(self.get_account(account_id)?.is_some(), "account not found");
        let tx = self.conn.unchecked_transaction()?;
        persist_account_custom_config_on(&tx, account_id, input)?;
        persist_account_model_capabilities_on(&tx, account_id, capabilities)?;
        clear_custom_protocol_state_except_on(&tx, account_id, input.upstream_protocol)?;
        tx.commit()?;
        Ok(())
    }

    pub fn list_account_model_capabilities(
        &self,
        account_id: &str,
    ) -> Result<Vec<AccountModelCapability>> {
        let mut stmt = self.conn.prepare(
            "SELECT account_id, model_id, upstream_model, protocol, verified_at, source
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
            "SELECT account_id, model_id, upstream_model, protocol, verified_at, source
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
                    c.account_id, c.endpoint_url, c.upstream_protocol,
                    c.created_at, c.updated_at
             FROM accounts a
             INNER JOIN account_custom_configs c ON c.account_id = a.id
             WHERE a.provider_id = ?1
             ORDER BY a.sort_order ASC, a.created_at ASC, a.id ASC",
        )?;
        let rows = stmt.query_map(params![CUSTOM_PROVIDER_ID], |row| {
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
                endpoint_url: row.get(6)?,
                upstream_protocol: {
                    let value = row.get::<_, String>(7)?;
                    UpstreamProtocolKind::try_from(value.as_str()).map_err(|error| {
                        rusqlite::Error::FromSqlConversionFailure(7, Type::Text, Box::new(error))
                    })?
                },
                created_at: parse_datetime(row.get::<_, String>(8)?),
                updated_at: parse_datetime(row.get::<_, String>(9)?),
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
            let (account_id, enabled, verification_status, setup_ready, has_key, mut config) = row?;
            if let Some(endpoint) = self.platform_endpoint(&account_id, config.upstream_protocol)? {
                config.endpoint_url = endpoint;
            }
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

    pub fn list_goat_account_runtimes(&self) -> Result<Vec<crate::goat::GoatAccountRuntime>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, enabled, verification_status, setup_step, key_cipher
             FROM accounts
             WHERE provider_id = ?1
             ORDER BY sort_order ASC, created_at ASC, id ASC",
        )?;
        let rows = stmt.query_map(params![COMMAND_CODE_PROVIDER_ID], |row| {
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
            Ok((
                account_id,
                enabled,
                verification_status,
                setup_step.is_ready(),
                !key_cipher.is_empty(),
            ))
        })?;
        let mut runtimes = Vec::new();
        for row in rows {
            let (account_id, enabled, verification_status, setup_ready, has_key) = row?;
            runtimes.push(crate::goat::GoatAccountRuntime {
                account_id,
                enabled,
                verification_status,
                setup_ready,
                has_key,
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
        let changed = ocg_infra::sqlite_logs::patch_forward_log_identity(
            &self.conn,
            &ForwardLogIdentityPatch {
                id,
                requested_model: attribution.requested_model.as_deref(),
                resolved_alias: attribution.resolved_alias.as_deref(),
                upstream_model: attribution.upstream_model.as_deref(),
                native_cost_value: attribution.native_cost_value,
                native_cost_unit: attribution.native_cost_unit.as_deref(),
                native_cost_currency: attribution.native_cost_currency.as_deref(),
            },
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

    /// Configured Ollama Cloud billing tier, if the account has a side-table row.
    pub fn ollama_cloud_billing_tier(&self, account_id: &str) -> Result<Option<OllamaBillingTier>> {
        ollama_cloud_billing_tier_on(&self.conn, account_id)
    }

    pub fn set_ollama_cloud_billing_tier(
        &self,
        account_id: &str,
        tier: Option<OllamaBillingTier>,
    ) -> Result<()> {
        anyhow::ensure!(self.get_account(account_id)?.is_some(), "account not found");
        set_ollama_cloud_billing_tier_on(&self.conn, account_id, tier)
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

    /// Atomically replace the authoritative snapshot for one sealed Provider.
    pub fn replace_quota_windows_by_source(
        &self,
        account_id: &str,
        source: &str,
        windows: &[QuotaWindow],
    ) -> Result<()> {
        anyhow::ensure!(self.get_account(account_id)?.is_some(), "account not found");
        anyhow::ensure!(
            windows
                .iter()
                .all(|window| window.account_id == account_id && window.source == source),
            "provider quota snapshot contains a mismatched account or source"
        );
        let tx = self.conn.unchecked_transaction()?;
        tx.execute(
            "DELETE FROM quota_windows WHERE account_id = ?1 AND source = ?2",
            params![account_id, source],
        )?;
        for window in windows {
            tx.execute(
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
        }
        tx.commit()?;
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
            "SELECT id, name, username, password_cipher, key_cipher, enabled, referral_code, recharge_date, cooldown_until, cooldown_generic_until, cooldown_5h_until, cooldown_week_until, cooldown_month_until, cooldown_free_until, last_error, created_at, updated_at, auth_error, account_type, setup_step, notes, provider_id, credential_kind, quota_scope FROM accounts WHERE id = ?1"
        )?;
        let account = stmt.query_row([id], account_from_row).optional()?;
        Ok(account)
    }

    pub fn list_accounts(&self) -> Result<Vec<Account>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, name, username, password_cipher, key_cipher, enabled, referral_code, recharge_date, cooldown_until, cooldown_generic_until, cooldown_5h_until, cooldown_week_until, cooldown_month_until, cooldown_free_until, last_error, created_at, updated_at, auth_error, account_type, setup_step, notes, provider_id, credential_kind, quota_scope FROM accounts ORDER BY sort_order ASC, created_at ASC, id ASC"
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

    /// Commit a V3 managed-key verification as one all-or-nothing SQLite
    /// transaction. The initial row fingerprint is checked by the first write,
    /// before the candidate ciphertext can replace a concurrent V2 update.
    pub fn commit_managed_key_verification(
        &self,
        id: &str,
        expected: &ManagedKeyVerificationCas,
        candidate_key_cipher: &str,
        write: &ManagedKeyVerificationWrite,
    ) -> Result<ManagedKeyVerificationCommit> {
        if matches!(write, ManagedKeyVerificationWrite::Verified { .. }) {
            ensure_enabled_provider_is_routable(&expected.provider_id, true)?;
        }

        let now = Utc::now();
        let now_rfc = now.to_rfc3339();
        let tx = self.conn.unchecked_transaction()?;
        let changed = tx.execute(
            "UPDATE accounts
             SET key_cipher = ?1, enabled = 0, auth_error = NULL, last_error = NULL,
                 updated_at = ?2
             WHERE id = ?3 AND key_cipher = ?4 AND updated_at = ?5
               AND provider_id = ?6
               AND account_type = ?7 AND setup_step = ?8",
            params![
                candidate_key_cipher,
                now_rfc,
                id,
                expected.key_cipher,
                expected.updated_at.to_rfc3339(),
                expected.provider_id,
                expected.account_type.as_str(),
                expected.setup_step.as_str(),
            ],
        )?;
        if changed != 1 {
            return Ok(ManagedKeyVerificationCommit::Conflict);
        }

        match write {
            ManagedKeyVerificationWrite::Verified {
                rate_limit,
                account_name,
            } => {
                if let Some(rate_limit) = rate_limit {
                    let column = match rate_limit.window {
                        Some(UsageWindowKind::FiveHours) => "cooldown_5h_until",
                        Some(UsageWindowKind::Week) => "cooldown_week_until",
                        Some(UsageWindowKind::Month) => "cooldown_month_until",
                        Some(UsageWindowKind::Free) => "cooldown_free_until",
                        None => "cooldown_generic_until",
                    };
                    let completed = tx.execute(
                        &format!(
                            "UPDATE accounts
                             SET {column} = ?2, last_error = ?3,
                                 setup_step = 'ready', enabled = 1, auth_error = NULL,
                                 verification_status = CASE
                                     WHEN verification_status = 'not_required'
                                     THEN 'not_required' ELSE 'verified' END,
                                 connection_verified_at = CASE
                                     WHEN verification_status = 'not_required'
                                     THEN NULL ELSE ?4 END,
                                 verification_error = NULL, updated_at = ?4
                             WHERE id = ?1 AND key_cipher = ?5"
                        ),
                        params![
                            id,
                            rate_limit.until.to_rfc3339(),
                            rate_limit.error,
                            now_rfc,
                            candidate_key_cipher,
                        ],
                    )?;
                    anyhow::ensure!(completed == 1, "managed verification row disappeared");
                    let cooldown_until = Self::compute_cooldown_until(&tx, id, &now_rfc)?;
                    tx.execute(
                        "UPDATE accounts SET cooldown_until = ?2 WHERE id = ?1",
                        params![id, cooldown_until],
                    )?;
                    if rate_limit.window == Some(UsageWindowKind::Free) {
                        Self::upsert_free_channel_cooldown(&tx, &rate_limit.until.to_rfc3339())?;
                    }
                } else {
                    let completed = tx.execute(
                        "UPDATE accounts
                         SET setup_step = 'ready', enabled = 1, auth_error = NULL,
                             cooldown_until = NULL, cooldown_generic_until = NULL,
                             cooldown_5h_until = NULL, cooldown_week_until = NULL,
                             cooldown_month_until = NULL, cooldown_free_until = NULL,
                             last_error = NULL,
                             verification_status = CASE
                                 WHEN verification_status = 'not_required'
                                 THEN 'not_required' ELSE 'verified' END,
                             connection_verified_at = CASE
                                 WHEN verification_status = 'not_required'
                                 THEN NULL ELSE ?2 END,
                             verification_error = NULL, updated_at = ?2
                         WHERE id = ?1 AND key_cipher = ?3",
                        params![id, now_rfc, candidate_key_cipher],
                    )?;
                    anyhow::ensure!(completed == 1, "managed verification row disappeared");
                }
                let message = format!("verified managed account {account_name}");
                ocg_infra::sqlite_logs::insert_gateway_log(
                    &tx,
                    &GatewayLogInsertRow {
                        level: "info",
                        category: "account",
                        message: &message,
                        created_at: &now_rfc,
                        request_id: None,
                        attempt: None,
                        error_source: None,
                        error_stage: None,
                        duration_ms: None,
                        diagnostic_json: None,
                    },
                )?;
            }
            ManagedKeyVerificationWrite::AuthFailed { auth_error } => {
                let updated = tx.execute(
                    "UPDATE accounts SET auth_error = ?2, updated_at = ?3
                     WHERE id = ?1 AND key_cipher = ?4",
                    params![id, auth_error, now_rfc, candidate_key_cipher],
                )?;
                anyhow::ensure!(updated == 1, "managed verification row disappeared");
            }
            ManagedKeyVerificationWrite::Pending => {}
        }

        tx.commit()?;
        Ok(ManagedKeyVerificationCommit::Applied)
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
        ensure_enabled_provider_is_routable(&account.provider_id, true)?;
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

    /// Evaluate the durable Free cooldown against an explicit wall time.
    pub(crate) fn free_channel_cooldown_until_at(
        &self,
        now: DateTime<Utc>,
    ) -> Result<Option<DateTime<Utc>>> {
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
        let created_at = Utc::now().to_rfc3339();
        ocg_infra::sqlite_logs::insert_gateway_log(
            &self.conn,
            &GatewayLogInsertRow {
                level,
                category,
                message,
                created_at: &created_at,
                request_id,
                attempt,
                error_source,
                error_stage,
                duration_ms,
                diagnostic_json,
            },
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
                  route_account_id, provider_id, credential_account_id,
                  status, http_status, prompt_tokens, completion_tokens, cached_tokens,
                  cache_creation_tokens, cost, cost_state, raw_cost_usd, quota_debit,
                  effective_paid_cost_usd)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11,
                         0, 0, 0, 0, 0, 'legacy_estimate', ?12, ?13, ?14)",
                params![
                    log.timestamp.to_rfc3339(),
                    log.model,
                    log.account_id,
                    log.account_name,
                    log.client_key_id,
                    log.client_key_name,
                    log.route_account_id,
                    log.provider_id,
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
        let timestamp = log.timestamp.to_rfc3339();
        Ok(ocg_infra::sqlite_logs::insert_forward_log(
            &self.conn,
            &ForwardLogInsertRow {
                timestamp: &timestamp,
                model: &log.model,
                account_id: &log.account_id,
                account_name: &log.account_name,
                client_key_id: log.client_key_id.as_deref(),
                client_key_name: log.client_key_name.as_deref(),
                route_account_id: log.route_account_id.as_deref(),
                provider_id: log.provider_id.as_deref(),

                credential_account_id: log.credential_account_id.as_deref(),
                status: &log.status,
                http_status: log.http_status,
                route: &log.route,
                prompt_tokens: log.prompt_tokens,
                completion_tokens: log.completion_tokens,
                cached_tokens: log.cached_tokens,
                cache_creation_tokens: log.cache_creation_tokens,
                cost: log.cost.unwrap_or(0.0),
                raw_cost_usd: log.raw_cost_usd,
                quota_debit: log.quota_debit,
                effective_paid_cost_usd: log.effective_paid_cost_usd,
                pricing_revision_id: log.pricing_revision_id.as_deref(),
                quota_multiplier: log.quota_multiplier,
                local_adjustment_multiplier: log.local_adjustment_multiplier,
                service_tier: log.service_tier.as_deref(),
                cost_state: &log.cost_state,
                error_message: log.error_message.as_deref(),
                request_id: log.request_id.as_deref(),
                attempt: log.attempt,
                error_source: log.error_source.as_deref(),
                error_stage: log.error_stage.as_deref(),
                duration_ms: log.duration_ms,
                diagnostic_json: diagnostic_json.as_deref(),
                requested_model: attribution.requested_model.as_deref(),
                resolved_alias: attribution.resolved_alias.as_deref(),
                upstream_model: attribution.upstream_model.as_deref(),
                native_cost_value: attribution.native_cost_value,
                native_cost_unit: attribution.native_cost_unit.as_deref(),
                native_cost_currency: attribution.native_cost_currency.as_deref(),
            },
        )?)
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
                "SELECT provider_id FROM forward_logs WHERE id = ?1",
                [id],
                |row| row.get::<_, Option<String>>(0),
            )
            .optional()?;
        if let Some(provider_id) = binding.as_ref() {
            metrics.scope_to_provider(provider_id.as_deref(), status.starts_with("success"));
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
        ocg_infra::sqlite_logs::update_forward_log(
            &self.conn,
            &ForwardLogUpdateRow {
                id,
                status: stored_status,
                http_status,
                prompt_tokens: metrics.prompt_tokens,
                completion_tokens: metrics.completion_tokens,
                cached_tokens: metrics.cached_tokens,
                cache_creation_tokens: metrics.cache_creation_tokens,
                cost: stored_cost,
                raw_cost_usd: metrics.raw_cost_usd,
                quota_debit: metrics.quota_debit,
                effective_paid_cost_usd: metrics.effective_paid_cost_usd,
                pricing_revision_id: metrics.pricing_revision_id.as_deref(),
                quota_multiplier: metrics.quota_multiplier,
                local_adjustment_multiplier: metrics.local_adjustment_multiplier,
                service_tier: metrics.service_tier.as_deref(),
                cost_state,
                error_message,
                error_source: diagnostic.map(|diagnostic| diagnostic.error_source),
                error_stage: diagnostic.map(|diagnostic| diagnostic.error_stage),
                duration_ms: diagnostic.map(|diagnostic| diagnostic.duration_ms),
                diagnostic_json: diagnostic.map(|diagnostic| diagnostic.diagnostic_json),
                native_cost_value,
                native_cost_unit: native_cost_unit.as_deref(),
                native_cost_currency: native_cost_currency.as_deref(),
            },
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
                    credential_account_id, raw_cost_usd, quota_debit,
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
                    credential_account_id, raw_cost_usd, quota_debit,
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

    // ----- access keys (schema v27; sub-key view excludes the primary row) -----

    /// All non-primary keys including soft-delete tombstones, in creation order.
    pub fn list_sub_gateway_keys(&self) -> Result<Vec<SubGatewayKey>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, name, key, enabled, deleted_at, created_at
             FROM access_keys
             WHERE is_primary = 0
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
    /// active ceiling. The live primary row is not counted.
    pub fn count_active_sub_gateway_keys(&self) -> Result<usize> {
        let count: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM access_keys
             WHERE is_primary = 0 AND deleted_at IS NULL",
            [],
            |row| row.get(0),
        )?;
        Ok(count.max(0) as usize)
    }

    pub fn get_sub_gateway_key(&self, id: &str) -> Result<Option<SubGatewayKey>> {
        if id == PRIMARY_KEY_ID {
            return Ok(None);
        }
        let key = self
            .conn
            .query_row(
                "SELECT id, name, key, enabled, deleted_at, created_at
                 FROM access_keys WHERE id = ?1 AND is_primary = 0",
                params![id],
                sub_gateway_key_from_row,
            )
            .optional()?;
        Ok(key)
    }

    /// Inserts a new sub key. The partial unique index backstops value
    /// uniqueness among all non-deleted access keys, including the primary;
    /// a collision surfaces as a constraint error the caller maps to a clear
    /// rejection.
    pub fn insert_sub_gateway_key(&self, key: &SubGatewayKey) -> Result<()> {
        anyhow::ensure!(
            key.id != PRIMARY_KEY_ID,
            "sub access keys cannot use the fixed primary id"
        );
        self.conn.execute(
            "INSERT INTO access_keys (id, name, key, is_primary, enabled, deleted_at, created_at)
             VALUES (?1, ?2, ?3, 0, ?4, ?5, ?6)",
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
        if id == PRIMARY_KEY_ID {
            return Ok(false);
        }
        let updated = self.conn.execute(
            "UPDATE access_keys SET name = ?2
             WHERE id = ?1 AND is_primary = 0 AND deleted_at IS NULL",
            params![id, name],
        )?;
        Ok(updated == 1)
    }

    /// Flips the enabled flag of a non-deleted sub key. Returns `false` when
    /// the id matches no active row. The primary row cannot be disabled.
    pub fn set_sub_gateway_key_enabled(&self, id: &str, enabled: bool) -> Result<bool> {
        if id == PRIMARY_KEY_ID {
            return Ok(false);
        }
        let updated = self.conn.execute(
            "UPDATE access_keys SET enabled = ?2
             WHERE id = ?1 AND is_primary = 0 AND deleted_at IS NULL",
            params![id, enabled as i32],
        )?;
        Ok(updated == 1)
    }

    /// Assigns a fresh value to a non-deleted sub key. Returns `false` when
    /// the id matches no active row.
    pub fn update_sub_gateway_key_value(&self, id: &str, new_value: &str) -> Result<bool> {
        if id == PRIMARY_KEY_ID {
            return Ok(false);
        }
        let updated = self.conn.execute(
            "UPDATE access_keys SET key = ?2
             WHERE id = ?1 AND is_primary = 0 AND deleted_at IS NULL",
            params![id, new_value],
        )?;
        Ok(updated == 1)
    }

    /// Soft-deletes a sub key: clears the plaintext, disables it, and keeps
    /// id/name/deleted_at for log attribution. Returns `false` when the id
    /// matches no active row. The primary row cannot be deleted.
    pub fn soft_delete_sub_gateway_key(&self, id: &str, now: DateTime<Utc>) -> Result<bool> {
        if id == PRIMARY_KEY_ID {
            return Ok(false);
        }
        let updated = self.conn.execute(
            "UPDATE access_keys
             SET key = '', enabled = 0, deleted_at = ?2
             WHERE id = ?1 AND is_primary = 0 AND deleted_at IS NULL",
            params![id, now.to_rfc3339()],
        )?;
        Ok(updated == 1)
    }

    /// Plaintext values of all non-deleted sub keys (enabled and disabled);
    /// used to keep generated values unique across tiers.
    pub fn active_sub_gateway_key_values(&self) -> Result<Vec<String>> {
        let mut stmt = self.conn.prepare(
            "SELECT key FROM access_keys
             WHERE is_primary = 0 AND deleted_at IS NULL AND key <> ''",
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
                SELECT 1 FROM access_keys
                WHERE is_primary = 0 AND deleted_at IS NULL AND key = ?1 LIMIT 1
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
        identity::fanout_shared_pool_cooldown(&tx, id, !(until.is_none() && err.is_none()))?;
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

        if updated > 0 {
            identity::fanout_shared_pool_cooldown(&tx, id, true)?;
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
        let observed_at = self
            .account_usage_sync_state(account_id)?
            .and_then(|sync| sync.last_success_at);
        self.live_fixed_quota_windows(account_id, limits, "opencode-go-live", observed_at)
    }

    /// Project locally priced request logs plus manual calibration into the
    /// provider-neutral quota window shape. This is the single read authority
    /// for plans such as GOAT that have no machine-readable upstream usage API.
    pub fn live_local_quota_windows(
        &self,
        account_id: &str,
        limits: &PricingLimits,
        source: &str,
    ) -> Result<Vec<QuotaWindow>> {
        self.live_fixed_quota_windows(account_id, limits, source, None)
    }

    /// One monthly USD-credit window from locally priced request logs plus
    /// calibration. Used credit is not clamped to the soft limit. 5h/week
    /// windows are not published.
    pub fn live_ollama_month_quota_window(
        &self,
        account_id: &str,
        month_limit: f64,
    ) -> Result<Vec<QuotaWindow>> {
        let now = Utc::now();
        let (offset, purchase_date): (f64, String) = self.conn.query_row(
            "SELECT usage_month_window_cost_offset, recharge_date FROM accounts WHERE id = ?1",
            [account_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        let (used, resets_at) =
            compute_ollama_month_window(&self.conn, account_id, &purchase_date, offset)?;
        Ok(vec![QuotaWindow {
            account_id: account_id.to_string(),
            window_kind: QUOTA_WINDOW_MONTH.to_string(),
            used,
            limit_value: Some(month_limit),
            started_at: month_window_start_utc(&purchase_date).ok(),
            resets_at,
            calibration_offset: offset,
            unit: "usd_credits".to_string(),
            source: "ollama-cloud-local".to_string(),
            observed_at: None,
            updated_at: now,
        }])
    }

    /// Unclamped Ollama month used credit and reset instant.
    pub fn ollama_month_usage(&self, account_id: &str) -> Result<(f64, Option<DateTime<Utc>>)> {
        let Some((offset, purchase_date)) = self
            .conn
            .query_row(
                "SELECT usage_month_window_cost_offset, recharge_date FROM accounts WHERE id = ?1",
                [account_id],
                |row| Ok((row.get::<_, f64>(0)?, row.get::<_, String>(1)?)),
            )
            .optional()?
        else {
            return Ok((0.0, None));
        };
        compute_ollama_month_window(&self.conn, account_id, &purchase_date, offset)
    }

    pub fn calibrate_ollama_month_usage(
        &self,
        account_id: &str,
        percent: f64,
        limit: f64,
        now: DateTime<Utc>,
    ) -> Result<bool> {
        let purchase_date: String = match self
            .conn
            .query_row(
                "SELECT recharge_date FROM accounts WHERE id = ?1",
                [account_id],
                |row| row.get(0),
            )
            .optional()?
        {
            Some(value) => value,
            None => return Ok(false),
        };
        let actual_cost = sum_ollama_month_cost_on(&self.conn, account_id, &purchase_date)?;
        let offset = limit * percent / 100.0 - actual_cost;
        let changed = self.conn.execute(
            "UPDATE accounts
             SET usage_month_window_cost_offset = ?2,
                 updated_at = ?3
             WHERE id = ?1",
            params![account_id, offset, now.to_rfc3339()],
        )?;
        Ok(changed > 0)
    }

    fn live_fixed_quota_windows(
        &self,
        account_id: &str,
        limits: &PricingLimits,
        source: &str,
        observed_at: Option<DateTime<Utc>>,
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
                source: source.to_string(),
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
                source: source.to_string(),
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
                source: source.to_string(),
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

    /// Aggregate `forward_logs` into per-day, per-model token buckets covering
    /// the last `days` calendar days (UTC). Rows with zero tokens on a given
    /// day are omitted — the frontend synthesizes empty days so the x-axis
    /// never collapses. Token totals are independent of pricing state: free,
    /// priced, legacy estimate, and not_applicable rows all contribute as long
    /// as they carry non-zero prompt or completion tokens.
    pub fn daily_tokens_by_model(&self, days: i64) -> Result<Vec<DailyModelTokens>> {
        // Bone-simple SQLite date math: store timestamps as RFC3339 strings,
        // so group by `substr(timestamp, 1, 10)` to collapse to YYYY-MM-DD.
        // UTC-only is fine — the gateway runs local and the dashboard is a
        // single-user tool; a TZ-correct grouping would need a calendar table
        // or a strftime('%Y-%m-%d', ...) with proper epoch arg, which is more
        // machinery than this needs right now.
        let since = (Utc::now() - Duration::days(days - 1)).to_rfc3339();
        let mut stmt = self.conn.prepare(
            "SELECT substr(timestamp, 1, 10) AS day, model, COALESCE(SUM(prompt_tokens + completion_tokens), 0)
             FROM forward_logs
             WHERE timestamp > ?1 AND prompt_tokens + completion_tokens > 0
             GROUP BY day, model
             ORDER BY day ASC, model ASC",
        )?;
        let rows = stmt.query_map([&since], |row| {
            Ok(DailyModelTokens {
                date: row.get::<_, String>(0)?,
                model: row.get::<_, String>(1)?,
                tokens: row.get::<_, i64>(2)?,
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

/// Ollama month window: `[purchase_date 00:00 local, next-month-same-day 00:00 local)`.
/// Used credit is `offset + cost` and is not clamped to the soft limit.
fn compute_ollama_month_window(
    conn: &Connection,
    account_id: &str,
    purchase_date: &str,
    offset: f64,
) -> Result<(f64, Option<DateTime<Utc>>)> {
    if purchase_date.trim().is_empty() {
        return Ok((0.0, None));
    }
    let (start, end) = ollama_month_bounds(purchase_date)?;
    let cost = sum_priced_cost_between(conn, account_id, start, end)?;
    Ok((offset + cost, Some(end)))
}

fn ollama_month_bounds(purchase_date: &str) -> Result<(DateTime<Utc>, DateTime<Utc>)> {
    let start = month_window_start_utc(purchase_date)?;
    let expires = purchase_expires_on(purchase_date)?;
    let end_naive = NaiveDate::parse_from_str(&expires, "%Y-%m-%d")?
        .and_hms_opt(0, 0, 0)
        .unwrap();
    let end = Local
        .from_local_datetime(&end_naive)
        .single()
        .ok_or_else(|| anyhow::anyhow!("ambiguous local datetime for expires_on"))?
        .with_timezone(&Utc);
    Ok((start, end))
}

fn sum_ollama_month_cost_on(
    conn: &Connection,
    account_id: &str,
    purchase_date: &str,
) -> Result<f64> {
    if purchase_date.trim().is_empty() {
        return Ok(0.0);
    }
    let (start, end) = ollama_month_bounds(purchase_date)?;
    sum_priced_cost_between(conn, account_id, start, end)
}

fn sum_priced_cost_between(
    conn: &Connection,
    account_id: &str,
    start: DateTime<Utc>,
    end: DateTime<Utc>,
) -> Result<f64> {
    Ok(conn.query_row(
        "SELECT COALESCE(SUM(cost), 0) FROM forward_logs
         WHERE account_id = ?1
           AND cost_state IN ('priced', 'legacy_estimate')
           AND timestamp >= ?2
           AND timestamp < ?3",
        params![account_id, start.to_rfc3339(), end.to_rfc3339()],
        |row| row.get(0),
    )?)
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
        credential_account_id: row.get(29)?,
        status: row.get(5)?,
        http_status: row.get(6)?,
        route: row.get(7)?,
        prompt_tokens: row.get(8)?,
        completion_tokens: row.get(9)?,
        cached_tokens: row.get(10)?,
        cache_creation_tokens: row.get(11)?,
        cost,
        raw_cost_usd: row.get(30)?,
        quota_debit: row.get(31)?,
        effective_paid_cost_usd: row.get(32)?,
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
    let Some((endpoint_url, protocol)): Option<(String, String)> = conn
        .query_row(
            "SELECT endpoint_url, upstream_protocol
             FROM account_custom_configs WHERE account_id = ?1",
            [&contract.account_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?
    else {
        return Ok(false);
    };
    if endpoint_url != contract.endpoint_url || protocol != contract.upstream_protocol.as_str() {
        return Ok(false);
    }
    let mut stmt = conn.prepare(
        "SELECT model_id, upstream_model, protocol
         FROM account_model_capabilities
         WHERE account_id = ?1
         ORDER BY rowid ASC",
    )?;
    let rows = stmt.query_map([&contract.account_id], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
        ))
    })?;
    let mut current = Vec::new();
    for row in rows {
        let (public_model, upstream_model, protocol) = row?;
        let protocol = UpstreamProtocolKind::try_from(protocol.as_str())
            .map_err(|error| anyhow::anyhow!(error))?;
        current.push((public_model, upstream_model, protocol));
    }
    Ok(current == contract.capabilities)
}

fn ollama_cloud_billing_tier_on(
    conn: &Connection,
    account_id: &str,
) -> Result<Option<OllamaBillingTier>> {
    let value: Option<String> = conn
        .query_row(
            "SELECT billing_tier FROM ollama_cloud_billing WHERE account_id = ?1",
            [account_id],
            |row| row.get(0),
        )
        .optional()?;
    match value {
        Some(tier) => OllamaBillingTier::parse(&tier)
            .map(Some)
            .map_err(|error| anyhow::anyhow!(error)),
        None => Ok(None),
    }
}

fn set_ollama_cloud_billing_tier_on(
    conn: &Connection,
    account_id: &str,
    tier: Option<OllamaBillingTier>,
) -> Result<()> {
    let current = ollama_cloud_billing_tier_on(conn, account_id)?;
    match tier {
        None => {
            conn.execute(
                "DELETE FROM ollama_cloud_billing WHERE account_id = ?1",
                [account_id],
            )?;
        }
        Some(tier) => {
            conn.execute(
                "INSERT INTO ollama_cloud_billing (account_id, billing_tier)
                 VALUES (?1, ?2)
                 ON CONFLICT(account_id) DO UPDATE SET billing_tier = excluded.billing_tier",
                params![account_id, tier.as_str()],
            )?;
        }
    }
    if current != tier {
        conn.execute(
            "UPDATE accounts SET usage_month_window_cost_offset = 0 WHERE id = ?1",
            [account_id],
        )?;
    }
    Ok(())
}

fn account_custom_config_from_row(row: &Row<'_>) -> rusqlite::Result<AccountCustomConfig> {
    let protocol_value = row.get::<_, String>(2)?;
    let upstream_protocol =
        UpstreamProtocolKind::try_from(protocol_value.as_str()).map_err(|error| {
            rusqlite::Error::FromSqlConversionFailure(2, Type::Text, Box::new(error))
        })?;
    Ok(AccountCustomConfig {
        account_id: row.get(0)?,
        endpoint_url: row.get(1)?,
        upstream_protocol,
        created_at: parse_datetime(row.get::<_, String>(3)?),
        updated_at: parse_datetime(row.get::<_, String>(4)?),
    })
}

fn account_model_capability_from_row(row: &Row<'_>) -> rusqlite::Result<AccountModelCapability> {
    let protocol_value = row.get::<_, String>(3)?;
    let protocol = UpstreamProtocolKind::try_from(protocol_value.as_str()).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(3, Type::Text, Box::new(error))
    })?;
    Ok(AccountModelCapability {
        account_id: row.get(0)?,
        public_model: row.get(1)?,
        upstream_model: row.get(2)?,
        protocol,
        verified_at: row.get::<_, Option<String>>(4)?.map(parse_datetime),
        source: row.get(5)?,
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
    let credential_value = row.get::<_, String>(22)?;
    let credential_kind = CredentialKind::try_from(credential_value.as_str()).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(
            22,
            Type::Text,
            Box::new(std::io::Error::other(error)),
        )
    })?;
    let quota_scope_value = row.get::<_, String>(23)?;
    let quota_scope = QuotaScope::try_from(quota_scope_value.as_str()).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(
            23,
            Type::Text,
            Box::new(std::io::Error::other(error)),
        )
    })?;
    Ok(Account {
        id: row.get(0)?,
        provider_id: row.get(21)?,

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
mod tests;
