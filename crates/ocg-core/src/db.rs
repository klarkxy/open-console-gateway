use crate::crypto::{KeyCipher, is_legacy_local_ciphertext};
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
    SCOPE_KIND_PROVIDER,
};
use anyhow::{Context, Result};
use chrono::{DateTime, Duration, Local, NaiveDate, TimeZone, Utc};
use ocg_domain::connection::{
    EndpointOperation, LegacyConnectionKind, connection_id_for_legacy, endpoint_id_for,
};
use ocg_domain::credential::{
    RouteSpec, assigned_endpoints_for_routes, normalize_origin, safe_default_grants,
};
use ocg_domain::destination::AuthScheme;
use ocg_domain::dynamic::DynamicProviderDefinition;
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
    collections::{HashMap, HashSet},
    fmt,
    fs::OpenOptions,
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::Arc,
};

pub struct Database {
    // Field order closes SQLite before releasing the directory's lifetime lock.
    pub(crate) conn: Connection,
    open_guard: open_guard::DatabaseOpenGuard,
    pub(crate) log_level: crate::runtime_log::Level,
}

pub(crate) mod account_store;
pub(crate) mod billing;
mod catalog_edit;
pub(crate) mod cpa;
pub(crate) mod credit_lifecycle;
pub(crate) mod custom_store;
pub(crate) mod destination_commands;
pub(crate) mod destination_store;
pub(crate) mod dynamic_store;
pub(crate) mod http_routes;
pub(crate) mod identity;
pub(crate) mod identity_v57;
mod official_api;
mod open_guard;
pub(crate) mod platform;
pub(crate) mod quota_recovery;
pub(crate) mod routing_cards;
pub(crate) mod routing_credentials;

/// Local configuration for the one code-owned CPA external integration.
/// Both credential values stay encrypted outside the short-lived V3 write path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CpaIntegrationRecord {
    pub account_id: String,
    pub base_url: String,
    pub management_key_cipher: String,
}

fn cpa_catalog_enabled_default() -> bool {
    true
}

/// One row from the persisted CPA `/v1/models` snapshot.
/// `owned_by` is CPA's reported source when present; legacy ID-only snapshots
/// keep it empty until the next explicit refresh.
/// Missing `enabled` stays on so existing catalogs keep routing until the next
/// refresh; newly discovered IDs after that default off.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CpaCatalogModel {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owned_by: Option<String>,
    #[serde(default = "cpa_catalog_enabled_default")]
    pub enabled: bool,
}

impl From<String> for CpaCatalogModel {
    fn from(id: String) -> Self {
        Self {
            id,
            owned_by: None,
            enabled: true,
        }
    }
}

impl From<&str> for CpaCatalogModel {
    fn from(id: &str) -> Self {
        Self {
            id: id.to_string(),
            owned_by: None,
            enabled: true,
        }
    }
}

impl CpaCatalogModel {
    pub fn ids(models: &[Self]) -> Vec<String> {
        models.iter().map(|model| model.id.clone()).collect()
    }

    pub fn enabled_ids(models: &[Self]) -> Vec<String> {
        models
            .iter()
            .filter(|model| model.enabled)
            .map(|model| model.id.clone())
            .collect()
    }

    /// Keep prior selection for IDs that still exist; new IDs stay off.
    pub fn merge_refresh(incoming: Vec<Self>, previous: &[Self]) -> Vec<Self> {
        let previous_enabled: HashMap<&str, bool> = previous
            .iter()
            .map(|model| (model.id.as_str(), model.enabled))
            .collect();
        incoming
            .into_iter()
            .map(|mut model| {
                model.enabled = previous_enabled
                    .get(model.id.as_str())
                    .copied()
                    .unwrap_or(false);
                model
            })
            .collect()
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
                    enabled: true,
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
                let enabled = object
                    .get("enabled")
                    .and_then(serde_json::Value::as_bool)
                    .unwrap_or(true);
                CpaCatalogModel {
                    id: id.to_string(),
                    owned_by,
                    enabled,
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

/// One validated V8 Custom HTTP connection definition. Credentials attach by
/// `NodeImportRecord::custom_credential_destinations`; keeping the connection
/// separate preserves shared and zero-Key destinations during import.
#[derive(Debug, Clone)]
pub struct ImportedCustomDestination {
    pub id: String,
    pub legacy_id: String,
    pub name: String,
    pub endpoint_url: String,
    pub protocol: UpstreamProtocolKind,
    pub auth_scheme: AuthScheme,
    pub models: Vec<ocg_domain::dynamic::DynamicModelMapping>,
    pub enabled: bool,
}

/// One fully validated, portable node-state snapshot. Stable IDs merge into an
/// existing destination; destination-only state is retained according to the
/// node migration rules. All database-owned state is committed in one
/// transaction.
#[derive(Debug, Clone)]
pub struct NodeImportRecord {
    /// Explicit portable controls, applied after compatibility conversion.
    pub destination_controls: Vec<ocg_domain::destination::Destination>,
    pub platform_links_authoritative: bool,
    pub platform_accounts: Vec<crate::platform::PortablePlatformAccount>,
    pub platform_links: Vec<crate::platform::PortablePlatformLink>,
    /// V7/V8 platform destination catalogs keyed by portable platform parent id.
    /// Empty catalogs are authoritative and clear an imported parent's models.
    pub platform_catalogs: HashMap<String, Vec<ocg_domain::destination::CatalogModel>>,
    pub accounts: Vec<AccountImportRecord>,
    pub account_order: Vec<String>,
    pub config_json: String,
    pub sub_keys: Vec<SubGatewayKey>,
    pub zen_free_enabled: bool,
    pub zen_catalog: crate::kernel::zen::ZenFreeModelCatalog,
    pub provider_contracts: PersistedContracts,
    pub dynamic_providers: Vec<DynamicProviderRuntime>,
    /// V8 Custom connection definitions, including connections with no Keys.
    pub custom_destinations: Vec<ImportedCustomDestination>,
    /// Imported inference account id -> V8 Custom destination id.
    pub custom_credential_destinations: HashMap<String, String>,
    /// V6 portable identity/credential/binding/quota-pool snapshot. `None`
    /// keeps the v45 1:1 satellite mapper used for V4/V5 packages.
    pub(crate) identity_snapshot: Option<identity::IdentityImportSnapshot>,
    /// Provider ids that stay persisted drafts. Routing snapshots exclude them.
    pub draft_provider_ids: HashSet<String>,
    /// Platform observer ciphertext keyed by platform parent id.
    pub platform_observer_ciphers: HashMap<String, String>,
    /// Destination-id keyed platform snapshot JSON from a V7/V8 package.
    pub platform_snapshots: HashMap<String, String>,
    /// Destination-id keyed platform versions from a V7/V8 package.
    pub platform_versions: HashMap<String, i64>,
    pub cpa_base_url: Option<String>,
    pub cpa_management_key_cipher: Option<String>,
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
/// Unique never-overwritten SQLite snapshot taken before a non-empty v47
/// database drops inert columns and empty leftover dynamic provider tables.
pub const PRE_V48_BACKUP_FILE_PREFIX: &str = "data.sqlite.pre-v48.";
/// Unique never-overwritten SQLite snapshot taken before a non-empty v57
/// database enables connection-owned Custom HTTP configuration and multi-Key
/// destinations.
pub const PRE_V58_BACKUP_FILE_PREFIX: &str = "data.sqlite.pre-v58.";
pub const PRE_V59_BACKUP_FILE_PREFIX: &str = "data.sqlite.pre-v59.";
/// Highest schema this binary can open or migrate. Newer databases fail closed.
pub const CURRENT_SCHEMA_VERSION: i32 = 64;
pub const V57_SCHEMA_VERSION: i32 = 57;
/// Canonical source schema for the v48 inert-column / empty-table cleanup.
pub const V47_SCHEMA_VERSION: i32 = 47;
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
    Layout(anyhow::Error),
}

impl fmt::Display for ReorderAccountsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::DuplicateAccountId => f.write_str("account_ids must not contain duplicates"),
            Self::AccountSetMismatch => {
                f.write_str("account set changed; reload the account list and retry")
            }
            Self::Database(error) => write!(f, "failed to reorder accounts: {error}"),
            Self::Layout(error) => write!(f, "failed to preserve routing cards: {error}"),
        }
    }
}

impl std::error::Error for ReorderAccountsError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Database(error) => Some(error),
            Self::Layout(error) => Some(error.as_ref()),
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

pub(crate) fn table_exists(conn: &Connection, table: &str) -> Result<bool> {
    Ok(conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1)",
        [table],
        |row| row.get::<_, i64>(0),
    )? != 0)
}

pub(crate) fn table_has_column(conn: &Connection, table: &str, column: &str) -> Result<bool> {
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

pub(crate) fn credentials_store_secrets(conn: &Connection) -> bool {
    table_has_column(conn, "credentials", "key_cipher").unwrap_or(false)
}

mod contract_store;
pub(crate) use contract_store::invalidate_probe_evidence_on;
use contract_store::*;

pub(super) mod lifecycle;
pub use lifecycle::{peek_app_config, peek_schema_version};

mod migrations;
use migrations::{
    count_accounts_for_provider_on, create_pre_v58_backup, create_pre_version_backup,
    dynamic_tx_fault, ensure_dynamic_provider_tables, ensure_dynamic_singleton_accounts_on,
    ensure_v58_destination_column, find_dashboard_operation_on, get_dynamic_provider_on,
    get_provider_definition_on, insert_dashboard_operation_on, insert_dynamic_provider_on,
    list_control_plane_dynamic_providers_on, list_dynamic_providers_on, migrate_to_v27,
    migrate_to_v28, migrate_to_v29, migrate_to_v30, migrate_to_v31, migrate_to_v32, migrate_to_v33,
    migrate_to_v34, migrate_to_v35, migrate_to_v36, migrate_to_v37, migrate_to_v39, migrate_to_v40,
    migrate_to_v41, migrate_to_v42, migrate_to_v43, migrate_to_v44, migrate_to_v47, migrate_to_v48,
    migrate_to_v49, migrate_to_v50, migrate_to_v51, migrate_to_v52, migrate_to_v53, migrate_to_v54,
    migrate_to_v55, migrate_to_v56, migrate_to_v57, migrate_to_v58, migrate_to_v59, migrate_to_v60,
    migrate_to_v61, migrate_to_v62, migrate_to_v63, migrate_to_v64,
    onboarding_draft_provider_ids_on, provider_is_onboarding_draft_on,
    sanitize_config_json_primary_key, upsert_imported_dynamic_provider_on,
    upsert_primary_access_key_on,
};

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
    use super::migrations::V27MigrationFault;
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
    let sort_order: i64 = account_store::select_account_sort_order(conn, &account.id)?;
    identity::persist_account_identity_model(
        conn,
        account,
        purchase_date,
        verification_status,
        sort_order,
        None,
        Utc::now(),
    )?;
    account_store::sync_inference_credential_projection_on(conn, &account.id)?;
    Ok(())
}

fn insert_account_row_for_destination(
    conn: &Connection,
    account: &Account,
    destination_id: &str,
    purchase_date: &str,
    verification_status: ConnectionVerificationStatus,
) -> Result<()> {
    account_store::insert_account_columns_for_destination(
        conn,
        account,
        purchase_date,
        verification_status,
        destination_id,
    )?;
    let sort_order: i64 = account_store::select_account_sort_order(conn, &account.id)?;
    identity::persist_account_identity_model(
        conn,
        account,
        purchase_date,
        verification_status,
        sort_order,
        None,
        Utc::now(),
    )?;
    account_store::sync_inference_credential_projection_on(conn, &account.id)?;
    Ok(())
}

pub(crate) fn insert_account_columns(
    conn: &Connection,
    account: &Account,
    purchase_date: &str,
    verification_status: ConnectionVerificationStatus,
) -> Result<()> {
    account_store::insert_account_columns(conn, account, purchase_date, verification_status)
}

mod access_key_store;
mod import_store;
mod log_store;
mod usage_store;
// Re-exported only so the sibling test module can reach it through `super::*`;
// nothing in the non-test build resolves this binding.
#[cfg(test)]
pub(crate) use usage_store::utc_token_day_bounds;
mod settings_store;
use import_store::insert_import_account_on;

fn persist_account_custom_config_on(
    conn: &Connection,
    account_id: &str,
    input: &AccountCustomConfigInput,
) -> Result<()> {
    let endpoint_changed = custom_store::persist_custom_config_on(conn, account_id, input)?;
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
        "UPDATE credentials
         SET verification_status = 'pending',
             connection_verified_at = NULL,
             verification_error = NULL,
             updated_at = ?2
         WHERE legacy_account_id = ?1 AND verification_status <> 'not_required'",
        params![account_id, Utc::now().to_rfc3339()],
    )?;
    Ok(())
}

fn refresh_goat_provider_catalog_on(conn: &Connection) -> Result<()> {
    if !table_exists(conn, "account_model_capabilities")? {
        return Ok(());
    }
    let mut stmt = conn.prepare(
        "SELECT c.model_id
         FROM account_model_capabilities c
         INNER JOIN credentials a ON a.legacy_account_id = c.account_id
         WHERE a.provider_id = ?1
           AND c.source = ?2
           AND a.verification_status = 'verified'
         ORDER BY a.routing_rank ASC, a.created_at ASC, a.legacy_account_id ASC, c.rowid ASC",
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
    if table_exists(conn, "account_model_capabilities")? {
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
    }
    let now = verified_at.unwrap_or_else(Utc::now);
    let catalog: Vec<String> = models
        .iter()
        .filter_map(|model| validate_custom_model_id(model).ok())
        .collect();
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
            serde_json::to_string(&catalog)?,
            now.to_rfc3339(),
            COMMAND_CODE_GOAT_BASE_URL,
        ],
    )?;
    upsert_contract_catalog_on(
        conn,
        &ContractScope::provider(COMMAND_CODE_PROVIDER_ID),
        &catalog,
        Some(now),
        CATALOG_SOURCE_COMMAND_CODE_MODELS,
        COMMAND_CODE_GOAT_BASE_URL,
        now,
    )?;
    Ok(())
}

fn persist_account_model_capabilities_on(
    conn: &Connection,
    account_id: &str,
    capabilities: &[AccountModelCapabilityInput],
) -> Result<()> {
    custom_store::persist_custom_capabilities_on(conn, account_id, capabilities)?;
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

/// Account id, optional name, and notes (`None` = leave, `Some(None)` = clear).
type OnboardingAccountMeta<'a> = (&'a str, Option<&'a str>, Option<Option<&'a str>>);

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
        let open_guard = open_guard::DatabaseOpenGuard::acquire(&data_dir)?;
        let db_path = data_dir.join("data.sqlite");
        let conn = Connection::open(&db_path)?;
        conn.pragma_update(None, "busy_timeout", 5000)?;
        let existing_version = lifecycle::schema_version_on(&conn)?;
        anyhow::ensure!(
            existing_version <= CURRENT_SCHEMA_VERSION,
            "database schema version {existing_version} is newer than this build supports ({CURRENT_SCHEMA_VERSION}); restore a matching data directory and encryption key"
        );
        let is_fresh = lifecycle::is_fresh_empty_database(&conn, existing_version)?;
        // Host-cipher opens probe persisted account key/password ciphertext
        // before migrate() can mutate the file. Decrypt failure fails closed.
        // v2 AEAD rejects a wrong host cipher; legacy XOR is still readable
        // so backups restore. Database::open (cipher None) skips this; v27
        // still probes when that rewrite runs. Empty or no-auth rows have
        // nothing to decrypt.
        if cipher.is_some() {
            lifecycle::preflight_ciphertext_probes(&conn, cipher)?;
        }
        lifecycle::ensure_pre_v22_backup(&conn, &db_path)?;
        lifecycle::ensure_pre_v23_backup(&conn, &db_path)?;
        // WAL keeps request-path log writes off the rollback-journal FULL fsync;
        // must be set outside any transaction, hence before migrate().
        let _journal_mode: String =
            conn.query_row("PRAGMA journal_mode = WAL", [], |row| row.get(0))?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        let mut db = Self {
            conn,
            open_guard,
            log_level: crate::runtime_log::Level::from_env(),
        };
        let pre_v59_backup_created = existing_version == 58 && !is_fresh;
        if pre_v59_backup_created {
            create_pre_version_backup(&db.conn, &db_path, PRE_V59_BACKUP_FILE_PREFIX, 58)?;
        }
        db.migrate()?;
        // A canonical v57 source receives its verified snapshot before even
        // idempotent older migration helpers can touch leftover tables.
        let pre_v58_backup_created = existing_version == V57_SCHEMA_VERSION && !is_fresh;
        if pre_v58_backup_created {
            create_pre_v58_backup(&db.conn, &db_path)?;
        }
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
        if lifecycle::schema_version_on(&db.conn)? < 42 {
            ensure_dynamic_provider_tables(&db.conn)?;
        }
        migrate_to_v39(&db.conn)?;
        migrate_to_v40(&db.conn)?;
        migrate_to_v41(&db.conn)?;
        migrate_to_v42(&db.conn, &db_path, is_fresh)?;
        migrate_to_v43(&db.conn)?;
        migrate_to_v44(&db.conn)?;
        identity::migrate_to_v45(&db.conn)?;
        identity::migrate_to_v46(&db.conn)?;
        migrate_to_v47(&db.conn)?;
        migrate_to_v48(&db.conn, &db_path, is_fresh)?;
        migrate_to_v49(&db.conn)?;
        migrate_to_v50(&db.conn)?;
        migrate_to_v51(&db.conn)?;
        identity::ensure_identity_model_consistent(&db.conn)?;
        ensure_v58_destination_column(&db.conn)?;
        migrate_to_v52(&db)?;
        migrate_to_v53(&db.conn)?;
        migrate_to_v54(&db.conn)?;
        migrate_to_v55(&db.conn)?;
        migrate_to_v56(&db.conn)?;
        identity::ensure_identity_model_consistent(&db.conn)?;
        migrate_to_v57(&db.conn)?;
        identity::ensure_identity_model_consistent(&db.conn)?;
        migrate_to_v58(&db.conn, &db_path, is_fresh, pre_v58_backup_created)?;
        if !crate::destination_projection::should_skip_persist_on_open(&db)? {
            let _ = crate::destination_projection::replace_persisted(&db)?;
        }
        crate::db::destination_store::ensure_zen_destination(&db.conn)?;
        migrate_to_v59(&db, &db_path, is_fresh, pre_v59_backup_created)?;
        migrate_to_v60(&db.conn)?;
        migrate_to_v61(&db.conn)?;
        migrate_to_v62(&db.conn)?;
        migrate_to_v63(&db.conn)?;
        migrate_to_v64(&db.conn)?;
        if db.open_guard.can_recover_pending() {
            let tx = db.conn.unchecked_transaction()?;
            billing::recover_pending_on(&tx, Utc::now())?;
            tx.commit()?;
        }
        if let Some(cipher) = cipher {
            lifecycle::repair_legacy_account_ciphertext(&db.conn, cipher)?;
        }
        db.open_guard.finish_open()?;
        Ok(db)
    }

    fn migrate(&self) -> Result<()> {
        migrations::migrate_baseline(self)
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

    /// Align builtin destination catalogs with persisted contracts.
    /// Does not rebuild destination or credential rows from `project()`.
    pub(crate) fn refresh_destination_shadow(&self) -> Result<()> {
        crate::destination_projection::refresh_destination_shadow(self)?;
        identity::backfill_authorization_connections_on(&self.conn)
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
        self.refresh_destination_shadow()?;
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
        self.refresh_destination_shadow()?;
        routing_cards::reconcile_on(&tx)?;
        tx.commit()?;
        Ok(())
    }

    /// Persist a Custom Key and attach it to a platform parent in one SQLite
    /// transaction. A link failure rolls back the account so import cannot
    /// leave an unlinked orphan.
    pub fn create_account_with_contract_linked_to_platform(
        &self,
        account: &Account,
        custom_config: &AccountCustomConfigInput,
        capabilities: &[AccountModelCapabilityInput],
        parent_id: &str,
        group: &crate::platform::PlatformGroup,
    ) -> Result<()> {
        anyhow::ensure!(
            account.id != ZEN_FREE_ACCOUNT_ID,
            "Zen Free is database-owned and cannot be created through the generic account API"
        );
        ensure_account_provider_binding(self, account)?;
        let plan = builtin_provider(&account.provider_id)
            .ok_or_else(|| anyhow::anyhow!("unknown provider offering"))?;
        anyhow::ensure!(
            plan_requires_custom_config(plan),
            "only Custom API accounts can be linked to a platform parent"
        );
        anyhow::ensure!(
            !capabilities.is_empty(),
            "Custom API accounts require at least one model capability"
        );
        let purchase_date = if account.purchase_date.trim().is_empty() {
            local_today()
        } else {
            normalize_purchase_date(&account.purchase_date)?
        };
        let verification_status = default_verification_status(plan);
        let tx = self.conn.unchecked_transaction()?;
        insert_account_row(&tx, account, &purchase_date, verification_status)?;
        persist_account_custom_config_on(&tx, &account.id, custom_config)?;
        persist_account_model_capabilities_on(&tx, &account.id, capabilities)?;
        platform::apply_platform_link_on(&tx, &account.id, parent_id, group)?;
        self.refresh_destination_shadow()?;
        routing_cards::reconcile_on(&tx)?;
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
        if table_exists(&self.conn, "credential_state")? {
            let updated = self.conn.execute(
                "UPDATE credential_state SET credential_id = ?2 WHERE account_id = ?1",
                params![account_id, credential_id],
            )?;
            anyhow::ensure!(
                updated == 1,
                "account {account_id} is missing credential_state"
            );
            if table_exists(&self.conn, "legacy_identity_map")? {
                self.conn.execute(
                    "UPDATE legacy_identity_map SET new_id = ?2
                     WHERE legacy_id = ?1 AND new_kind = 'credential'",
                    params![account_id, credential_id],
                )?;
            }
            return Ok(());
        }
        let current_id: String = self.conn.query_row(
            "SELECT id FROM credentials WHERE legacy_account_id = ?1",
            [account_id],
            |row| row.get(0),
        )?;
        self.conn.execute(
            "UPDATE credential_grants SET credential_id = ?2 WHERE credential_id = ?1",
            params![current_id, credential_id],
        )?;
        let updated = self.conn.execute(
            "UPDATE credentials SET id = ?2 WHERE legacy_account_id = ?1",
            params![account_id, credential_id],
        )?;
        anyhow::ensure!(updated == 1, "account {account_id} is missing a credential");
        Ok(())
    }

    pub fn get_dynamic_provider(
        &self,
        provider_id: &str,
    ) -> Result<Option<DynamicProviderRuntime>> {
        get_dynamic_provider_on(&self.conn, provider_id)
    }

    /// Read a sealed builtin catalog row or a destination-backed dynamic
    /// definition. The handler still rejects PATCH/DELETE on builtin rows.
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
        routing_cards::reconcile_on(&tx)?;
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
        routing_cards::reconcile_on(&tx)?;
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
        self.commit_onboarding_new_with_routes(
            runtime,
            first_account,
            onboarding_draft,
            operation,
            None,
        )
    }

    pub fn commit_onboarding_new_with_routes(
        &self,
        runtime: &DynamicProviderRuntime,
        first_account: Option<&Account>,
        onboarding_draft: bool,
        operation: &NewDashboardOperation,
        protocol_routes: Option<&[ocg_domain::destination::HttpProtocolRoute]>,
    ) -> Result<Vec<DynamicProviderRuntime>> {
        let tx = self.conn.unchecked_transaction()?;
        insert_dynamic_provider_on(&tx, runtime, onboarding_draft)?;
        if let Some(routes) = protocol_routes {
            destination_commands::configure_new_http_routes_on(&tx, runtime, routes)?;
        }
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
        routing_cards::reconcile_on(&tx)?;
        tx.commit()?;
        Ok(snapshot)
    }

    /// Add a Key account to an existing user-defined Provider together with the
    /// dashboard operation ledger row in one SQLite transaction.
    pub fn commit_onboarding_existing_account(
        &self,
        account: &Account,
        destination_id: Option<&str>,
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
        match destination_id {
            Some(destination_id) => insert_account_row_for_destination(
                &tx,
                account,
                destination_id,
                &purchase_date,
                verification_status,
            )?,
            None => insert_account_row(&tx, account, &purchase_date, verification_status)?,
        }
        dynamic_tx_fault("after_account_insert")?;
        insert_dashboard_operation_on(&tx, operation)?;
        routing_cards::reconcile_on(&tx)?;
        tx.commit()?;
        Ok(())
    }

    /// Resume or complete a stored draft in one transaction with the ledger row.
    /// Routing snapshot excludes remaining drafts.
    /// One transaction: provider snapshot, account create/rotate, grants, auth sync, ledger.
    #[allow(clippy::too_many_arguments)]
    pub fn commit_onboarding_resume(
        &self,
        runtime: &DynamicProviderRuntime,
        onboarding_draft: bool,
        create_account: Option<&Account>,
        rotate: Option<(&str, &str)>,
        account_meta: Option<OnboardingAccountMeta<'_>>,
        grant_union: Option<(&str, &[String], &[String])>,
        sync_auth: Option<(&str, &str, &str)>,
        operation: &NewDashboardOperation,
    ) -> Result<Vec<DynamicProviderRuntime>> {
        self.commit_onboarding_resume_with_routes(
            runtime,
            onboarding_draft,
            create_account,
            rotate,
            account_meta,
            grant_union,
            sync_auth,
            operation,
            None,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn commit_onboarding_resume_with_routes(
        &self,
        runtime: &DynamicProviderRuntime,
        onboarding_draft: bool,
        create_account: Option<&Account>,
        rotate: Option<(&str, &str)>,
        account_meta: Option<OnboardingAccountMeta<'_>>,
        grant_union: Option<(&str, &[String], &[String])>,
        sync_auth: Option<(&str, &str, &str)>,
        operation: &NewDashboardOperation,
        protocol_routes: Option<&[ocg_domain::destination::HttpProtocolRoute]>,
    ) -> Result<Vec<DynamicProviderRuntime>> {
        let tx = self.conn.unchecked_transaction()?;
        let existing = get_dynamic_provider_on(&tx, &runtime.id)?
            .ok_or_else(|| anyhow::anyhow!("unknown provider `{}`", runtime.id))?;
        let mut stored = runtime.clone();
        stored.id = existing.id.clone();
        let destination_id = ocg_domain::destination::destination_id_for_dynamic(&stored.id);
        let before = destination_commands::load_http_destination(self, &destination_id)?;
        if let Some(routes) = protocol_routes {
            let normalized = destination_commands::normalize_http_protocol_routes(routes)?;
            let first = &normalized[0];
            anyhow::ensure!(
                first.endpoint_url == stored.endpoint_url
                    && first.protocol == stored.upstream_protocol
                    && first.auth_scheme == AuthScheme::from(stored.auth_kind),
                "default endpoint, protocol and authentication must match the first protocol route"
            );
            // Stage the legacy constructor inside this transaction, then restore
            // the complete validated declaration before any Key is initialized.
            tx.execute(
                "UPDATE destinations SET protocol_routes_json = NULL WHERE id = ?1",
                [&destination_id],
            )?;
        }
        dynamic_store::replace_dynamic_provider_definition_on(
            &tx,
            &stored,
            Some(onboarding_draft),
        )?;
        if let Some(routes) = protocol_routes {
            tx.execute(
                "UPDATE destinations SET protocol_routes_json = ?2 WHERE id = ?1",
                params![
                    destination_id,
                    serde_json::to_string(&before.protocol_routes)?
                ],
            )?;
            let catalog = destination_commands::catalog_from_definition(
                &before.catalog,
                &stored.definition(),
                true,
            );
            destination_store::replace_destination_catalog(&tx, &destination_id, &catalog)?;
            destination_commands::configure_new_http_routes_on(&tx, &stored, routes)?;
        }
        let after = destination_commands::load_http_destination(self, &destination_id)?;
        destination_commands::remap_http_grants_on(&tx, &before, &after)?;
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
                        "UPDATE credentials SET name = ?2, notes = ?3, updated_at = ?4 WHERE legacy_account_id = ?1",
                        params![account_id, name, notes, now],
                    )?;
                }
                (Some(name), None) => {
                    tx.execute(
                        "UPDATE credentials SET name = ?2, updated_at = ?3 WHERE legacy_account_id = ?1",
                        params![account_id, name, now],
                    )?;
                }
                (None, Some(notes)) => {
                    tx.execute(
                        "UPDATE credentials SET notes = ?2, updated_at = ?3 WHERE legacy_account_id = ?1",
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
                "UPDATE credentials SET credential_kind = ?2, quota_scope = ?3,
                     setup_step = 'ready', updated_at = ?4
                 WHERE legacy_account_id = ?1",
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
        routing_cards::reconcile_on(&tx)?;
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
        self.replace_dynamic_provider_authorized(
            runtime,
            clear_runtime_state,
            clear_keys,
            replacement_key_cipher,
            &[],
        )
    }

    pub fn replace_dynamic_provider_authorized(
        &self,
        runtime: &DynamicProviderRuntime,
        _clear_runtime_state: bool,
        _clear_keys: bool,
        replacement_key_cipher: Option<&str>,
        authorize_credential_ids: &[String],
    ) -> Result<Vec<DynamicProviderRuntime>> {
        let tx = self.conn.unchecked_transaction()?;
        let destination_id = ocg_domain::destination::destination_id_for_dynamic(&runtime.id);
        destination_commands::replace_http_destination_on(
            self,
            &destination_id,
            &runtime.definition(),
            authorize_credential_ids,
        )?;
        dynamic_tx_fault("after_mapping_replace")?;
        if let Some(cipher) = replacement_key_cipher {
            self.replace_destination_singleton_key_on(&destination_id, cipher)?;
        }
        let snapshot = list_dynamic_providers_on(&tx)?;
        tx.commit()?;
        Ok(snapshot)
    }

    pub(crate) fn replace_destination_singleton_key_on(
        &self,
        destination_id: &str,
        cipher: &str,
    ) -> Result<()> {
        let ids = {
            let mut stmt = self.conn.prepare("SELECT legacy_account_id FROM credentials WHERE destination_id = ?1 AND COALESCE(credential_purpose, 'inference') = 'inference'")?;
            stmt.query_map([destination_id], |row| row.get::<_, String>(0))?
                .collect::<rusqlite::Result<Vec<_>>>()?
        };
        anyhow::ensure!(
            ids.len() == 1,
            "replacement Key requires exactly one credential"
        );
        self.conn.execute("UPDATE credentials SET key_cipher = ?2, credential_version = COALESCE(credential_version, 1) + 1, auth_state_version = COALESCE(auth_state_version, 1) + 1, auth_error = NULL, verification_status = 'pending', connection_verified_at = NULL, verification_error = NULL, quota_recovery_json = NULL WHERE destination_id = ?1",
            params![destination_id, cipher])?;
        account_store::sync_inference_credential_projection_on(&self.conn, &ids[0])
    }

    pub fn delete_dynamic_provider(
        &self,
        provider_id: &str,
    ) -> Result<Vec<DynamicProviderRuntime>> {
        let tx = self.conn.unchecked_transaction()?;
        let existing = get_dynamic_provider_on(&tx, provider_id)?
            .ok_or_else(|| anyhow::anyhow!("unknown provider `{provider_id}`"))?;
        let count: i64 = tx.query_row(
            "SELECT COUNT(*) FROM credentials WHERE lower(provider_id) = lower(?1)",
            [&existing.id],
            |row| row.get(0),
        )?;
        anyhow::ensure!(
            count == 0,
            "dynamic provider still has {count} referencing account(s)"
        );
        dynamic_store::delete_dynamic_provider_on(&tx, &existing.id)?;
        tx.execute(
            "DELETE FROM provider_pricing_snapshots WHERE provider_id = ?1",
            [&existing.id],
        )?;
        let snapshot = list_dynamic_providers_on(&tx)?;
        routing_cards::reconcile_on(&tx)?;
        tx.commit()?;
        Ok(snapshot)
    }

    /// Insert every migrated account and its Custom contract in one SQLite
    /// transaction. Rows append in the supplied order. Any validation,
    /// constraint, or child-table failure rolls the entire batch back.
    pub fn import_accounts_with_contracts(&self, records: &[AccountImportRecord]) -> Result<()> {
        let tx = self.conn.unchecked_transaction()?;
        for record in records {
            insert_import_account_on(&tx, record, false)?;
        }
        self.refresh_destination_shadow()?;
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
        self.import_node_state_with_cipher(record, None, prepare_runtime)
    }

    /// Same merge as [`Self::import_node_state`], comparing Host-decrypted Keys
    /// so AES-GCM re-encryption of the same plaintext keeps local recovery.
    pub fn import_node_state_with_cipher<T>(
        &self,
        record: &NodeImportRecord,
        cipher: Option<&dyn KeyCipher>,
        prepare_runtime: impl FnOnce(&Database) -> Result<T>,
    ) -> Result<T> {
        import_store::import_node_state_with_cipher(self, record, cipher, prepare_runtime)
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
            "UPDATE credentials SET name = ?1, username = ?2, password_cipher = ?3, key_cipher = ?4,
             enabled = CASE WHEN ?10 AND ?14 THEN 0 ELSE ?5 END, referral_code = ?6, purchase_date = ?7, notes = ?8,
             usage_month_window_cost_offset = CASE WHEN ?9 THEN 0 ELSE usage_month_window_cost_offset END,
             auth_error = CASE WHEN ?10 THEN NULL ELSE auth_error END,
             verification_status = CASE WHEN ?10 AND ?13 THEN 'pending' ELSE verification_status END,
             connection_verified_at = CASE WHEN ?10 AND ?13 THEN NULL ELSE connection_verified_at END,
             verification_error = CASE WHEN ?10 AND ?13 THEN NULL ELSE verification_error END,
             updated_at = ?11 WHERE legacy_account_id = ?12",
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
            if table_exists(&tx, "account_model_capabilities")? {
                tx.execute(
                    "DELETE FROM account_model_capabilities
                     WHERE account_id = ?1 AND source = ?2",
                    params![id, COMMAND_CODE_GOAT_MODELS_SOURCE],
                )?;
            }
            refresh_goat_provider_catalog_on(&tx)?;
        }
        if let Some(tier) = ollama_billing {
            set_ollama_cloud_billing_tier_on(&tx, id, tier)?;
        }
        if key_replaced {
            platform::clear_link_snapshot_for_account(&tx, id)?;
            quota_recovery::clear_for_account_on(&tx, id)?;
        }
        account_store::sync_inference_credential_projection_on(&tx, id)?;
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
        cpa::integration_on(&self.conn)
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
                "SELECT 1 FROM credentials WHERE legacy_account_id = ?1",
                [CPA_ACCOUNT_ID],
                |_| Ok(()),
            )
            .optional()?
            .is_some();
        if exists {
            tx.execute(
                "UPDATE credentials
                    SET name = ?2,
                        auth_error = CASE WHEN key_cipher <> ?3 THEN NULL ELSE auth_error END,
                        key_cipher = ?3, enabled = ?4,
                        account_type = ?5, setup_step = ?6, updated_at = ?7,
                        provider_id = ?8,
                        credential_kind = ?9, quota_scope = ?10,
                        verification_status = 'not_required',
                        connection_verified_at = NULL, verification_error = NULL
                  WHERE legacy_account_id = ?1",
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
        cpa::upsert_destination_and_observer_on(&tx, base_url, management_key_cipher)?;
        account_store::sync_inference_credential_projection_on(&tx, CPA_ACCOUNT_ID)?;
        routing_cards::reconcile_on(&tx)?;
        tx.commit()?;
        Ok(())
    }

    /// Removes only OCG-owned CPA state. CPA auth files remain owned by CPA.
    pub fn delete_cpa_integration(&self) -> Result<()> {
        let tx = self.conn.unchecked_transaction()?;
        cpa::delete_destination_and_observer_on(&tx)?;
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
        account_store::delete_credential_grants_for_legacy_account_on(&tx, CPA_ACCOUNT_ID)?;
        tx.execute(
            "DELETE FROM credentials WHERE legacy_account_id = ?1",
            [CPA_ACCOUNT_ID],
        )?;
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
        let tx = self.conn.unchecked_transaction()?;
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
        let destination_id = cpa::destination_id();
        if destination_store::destination_exists(&tx, &destination_id)? {
            destination_store::replace_destination_catalog(
                &tx,
                &destination_id,
                &destination_store::cpa_catalog(models),
            )?;
        }
        tx.commit()?;
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

    /// Test-only seam: recreate leftover identity satellites from credentials
    /// and mark schema v56 so the next open exercises v57 copy+drop.
    #[cfg(any(test, debug_assertions))]
    #[doc(hidden)]
    pub fn test_rewind_identity_satellites_to_v56(&self) -> Result<()> {
        identity_v57::rewind_identity_satellites_to_v56(&self.conn)
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
    pub fn set_zen_free_enabled(&self, enabled: bool) -> Result<()> {
        ensure_enabled_provider_is_routable(OPENCODE_ZEN_FREE_PROVIDER_ID, enabled)?;
        let tx = self.conn.unchecked_transaction()?;
        let changed = tx.execute(
            "UPDATE credentials SET enabled = ?2, updated_at = ?3
             WHERE legacy_account_id = ?1 AND provider_id = ?4",
            params![
                ZEN_FREE_ACCOUNT_ID,
                enabled as i32,
                Utc::now().to_rfc3339(),
                OPENCODE_ZEN_FREE_PROVIDER_ID,
            ],
        )?;
        anyhow::ensure!(changed == 1, "Zen Free singleton is missing");
        tx.commit()?;
        Ok(())
    }

    pub fn delete_account(&mut self, id: &str) -> Result<()> {
        anyhow::ensure!(
            id != ZEN_FREE_ACCOUNT_ID,
            "Zen Free is a built-in singleton and cannot be deleted"
        );
        let tx = self.conn.unchecked_transaction()?;
        tx.execute("DELETE FROM quota_windows WHERE account_id = ?1", [id])?;
        platform::unlink_imported_accounts(&tx, &[id.to_string()])?;
        tx.execute("DELETE FROM credit_balances WHERE account_id = ?1", [id])?;
        tx.execute(
            "DELETE FROM provider_usage_sync_state WHERE account_id = ?1",
            [id],
        )?;
        custom_store::delete_custom_destination_facts(&tx, id)?;
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
        tx.execute(
            "DELETE FROM ollama_cloud_billing WHERE account_id = ?1",
            [id],
        )?;
        account_store::delete_credential_grants_for_legacy_account_on(&tx, id)?;
        tx.execute("DELETE FROM credentials WHERE legacy_account_id = ?1", [id])?;
        identity::delete_orphan_identity_for_account(&tx, id, identity_id.as_deref())?;
        routing_cards::reconcile_on(&tx)?;
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
                "SELECT legacy_account_id, verification_status, connection_verified_at, verification_error
                 FROM credentials WHERE legacy_account_id = ?1",
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
        let tx = self.conn.unchecked_transaction()?;
        let changed = tx.execute(
            "UPDATE credentials
             SET verification_status = ?2,
                 connection_verified_at = ?3,
                 verification_error = ?4,
                 updated_at = ?5
             WHERE legacy_account_id = ?1",
            params![
                account_id,
                status.as_str(),
                verified_at.map(|value| value.to_rfc3339()),
                error,
                Utc::now().to_rfc3339(),
            ],
        )?;
        if changed == 1 {
            account_store::sync_inference_credential_projection_on(&tx, account_id)?;
        }
        tx.commit()?;
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
            "UPDATE credentials
             SET verification_status = ?2,
                 connection_verified_at = ?3,
                 verification_error = ?4,
                 updated_at = ?5
             WHERE legacy_account_id = ?1
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
        account_store::sync_inference_credential_projection_on(&tx, &contract.account_id)?;
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
                 FROM credentials WHERE legacy_account_id = ?1",
                [account_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()
            .map_err(Into::into)
    }

    pub fn account_custom_config(&self, account_id: &str) -> Result<Option<AccountCustomConfig>> {
        custom_store::account_custom_config_on(&self.conn, account_id)
    }

    pub(crate) fn custom_destination_for_account(
        &self,
        account_id: &str,
    ) -> Result<Option<custom_store::CustomDestinationRecord>> {
        custom_store::custom_destination_for_account_on(&self.conn, account_id)
    }

    pub(crate) fn custom_auth_kind(
        &self,
        account_id: &str,
    ) -> Result<Option<ocg_domain::dynamic::DynamicAuthKind>> {
        custom_store::custom_auth_kind_on(&self.conn, account_id)
    }

    pub fn custom_connection_credential_count(&self, account_id: &str) -> Result<i64> {
        custom_store::custom_connection_credential_count_on(&self.conn, account_id)
    }

    /// Replace connection-owned configuration for a legacy Custom HTTP
    /// destination. Existing Keys keep their scopes and grants. Only the
    /// explicitly selected credential ids receive grants for the new routes.
    pub fn replace_custom_destination(
        &self,
        destination_id: &str,
        definition: &DynamicProviderDefinition,
        authorize_credential_ids: &[String],
    ) -> Result<()> {
        let tx = Transaction::new_unchecked(&self.conn, TransactionBehavior::Immediate)?;
        destination_commands::replace_http_destination_on(
            self,
            destination_id,
            definition,
            authorize_credential_ids,
        )?;
        tx.commit()?;
        Ok(())
    }

    pub fn delete_empty_custom_destination(&self, destination_id: &str) -> Result<()> {
        let tx = Transaction::new_unchecked(&self.conn, TransactionBehavior::Immediate)?;
        custom_store::delete_empty_custom_destination_on(&tx, destination_id)?;
        tx.commit()?;
        Ok(())
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
        custom_store::list_capabilities_on(&self.conn, account_id, false, false)
    }

    pub fn list_account_model_capabilities_declared(
        &self,
        account_id: &str,
    ) -> Result<Vec<AccountModelCapability>> {
        custom_store::list_capabilities_on(&self.conn, account_id, true, false)
    }

    /// Projection catalog rows. Unknown leftover protocols are omitted rather
    /// than failing the mapping; dashboard post-reads still use the strict list.
    pub(crate) fn list_account_model_capabilities_for_projection(
        &self,
        account_id: &str,
    ) -> Result<Vec<AccountModelCapability>> {
        custom_store::list_capabilities_on(&self.conn, account_id, true, true)
    }

    /// Custom accounts in saved account order, with config and declared capabilities.
    pub fn list_custom_account_runtimes(&self) -> Result<Vec<crate::custom::CustomAccountRuntime>> {
        let mut stmt = self.conn.prepare(
            "SELECT legacy_account_id, enabled, verification_status, setup_step, key_cipher
             FROM credentials
             WHERE provider_id = ?1
             ORDER BY routing_rank ASC, created_at ASC, legacy_account_id ASC",
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
            let Some(config) = self.account_custom_config(&account_id)? else {
                continue;
            };
            let auth_kind = custom_store::custom_auth_kind_on(&self.conn, &account_id)?
                .ok_or_else(|| anyhow::anyhow!("Custom account destination is missing"))?;
            let route_overrides = custom_store::custom_route_overrides_on(&self.conn, &account_id)?;
            let protocol_passthrough =
                custom_store::platform_parent_id(&self.conn, &account_id)?.is_some();
            let capabilities = self.list_account_model_capabilities_declared(&account_id)?;
            runtimes.push(crate::custom::CustomAccountRuntime {
                account_id,
                enabled,
                verification_status,
                setup_ready,
                has_key,
                auth_kind,
                config,
                capabilities,
                route_overrides,
                protocol_passthrough,
            });
        }
        Ok(runtimes)
    }

    pub fn list_goat_account_runtimes(&self) -> Result<Vec<crate::goat::GoatAccountRuntime>> {
        let mut stmt = self.conn.prepare(
            "SELECT legacy_account_id, enabled, verification_status, setup_step, key_cipher
             FROM credentials
             WHERE provider_id = ?1
             ORDER BY routing_rank ASC, created_at ASC, legacy_account_id ASC",
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
        // After v53 destination_models is the store. Re-projecting through
        // skip-unknown catalog join would DELETE + rewrite those rows and
        // hide a committed unreadable protocols_json from the post-commit
        // DTO read (revision already advanced).
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
             FROM credentials WHERE legacy_account_id = ?1",
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
    let Some((endpoint_url, protocol)) =
        custom_store::custom_endpoint_protocol_on(conn, &contract.account_id)?
    else {
        return Ok(false);
    };
    if endpoint_url != contract.endpoint_url || protocol != contract.upstream_protocol {
        return Ok(false);
    }
    let current = custom_store::capability_triples_on(conn, &contract.account_id)?;
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
            "UPDATE credentials SET usage_month_window_cost_offset = 0 WHERE legacy_account_id = ?1",
            [account_id],
        )?;
    }
    Ok(())
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
    let created_at = row
        .get::<_, Option<String>>(15)?
        .unwrap_or_else(|| Utc::now().to_rfc3339());
    let purchase_date = match row.get::<_, Option<String>>(7)? {
        Some(value) if normalize_purchase_date(&value).is_ok() => value,
        _ => migrations::migration_fallback_purchase_date(&created_at).map_err(|error| {
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
    let account_type_value = row
        .get::<_, Option<String>>(18)?
        .unwrap_or_else(|| "key".to_string());
    let account_type = AccountType::try_from(account_type_value.as_str()).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(
            18,
            Type::Text,
            Box::new(std::io::Error::other(error)),
        )
    })?;
    let setup_step_value = row
        .get::<_, Option<String>>(19)?
        .unwrap_or_else(|| "ready".to_string());
    let setup_step = AccountSetupStep::try_from(setup_step_value.as_str()).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(
            19,
            Type::Text,
            Box::new(std::io::Error::other(error)),
        )
    })?;
    let credential_value = row
        .get::<_, Option<String>>(22)?
        .unwrap_or_else(|| "api_key".to_string());
    let credential_kind = CredentialKind::try_from(credential_value.as_str()).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(
            22,
            Type::Text,
            Box::new(std::io::Error::other(error)),
        )
    })?;
    let quota_scope_value = row
        .get::<_, Option<String>>(23)?
        .unwrap_or_else(|| "key".to_string());
    let quota_scope = QuotaScope::try_from(quota_scope_value.as_str()).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(
            23,
            Type::Text,
            Box::new(std::io::Error::other(error)),
        )
    })?;
    Ok(Account {
        id: row.get(0)?,
        provider_id: row.get::<_, Option<String>>(21)?.unwrap_or_default(),

        credential_kind,
        quota_scope,
        name: row.get::<_, Option<String>>(1)?.unwrap_or_default(),
        username: row.get(2)?,
        password_cipher: row.get(3)?,
        key_cipher: row.get::<_, Option<String>>(4)?.unwrap_or_default(),
        enabled: row.get::<_, Option<i32>>(5)?.unwrap_or(0) != 0,
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
        updated_at: parse_datetime(
            row.get::<_, Option<String>>(16)?
                .unwrap_or_else(|| Utc::now().to_rfc3339()),
        ),
    })
}

fn parse_datetime(s: String) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(&s)
        .map(|dt| dt.with_timezone(&Utc))
        .unwrap_or_else(|e| {
            eprintln!("error: failed to parse datetime '{s}': {e}, using now");
            Utc::now()
        })
}

#[cfg(test)]
mod billing_tests;
#[cfg(test)]
mod contract_tests;
#[cfg(test)]
mod cpa_tests;
#[cfg(test)]
mod dynamic_tests;
#[cfg(test)]
mod import_tests;
#[cfg(test)]
mod log_tests;
#[cfg(test)]
mod migration_tests;
#[cfg(test)]
mod platform_tests;
#[cfg(test)]
mod tests;
#[cfg(test)]
mod usage_tests;
