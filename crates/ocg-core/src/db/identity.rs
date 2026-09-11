//! Additive identity / credential / binding satellites (schema v45).
use super::*;
use ocg_domain::connection::{ConnectionId, LegacyConnectionKind, connection_id_for_legacy};
use ocg_domain::credential::{
    CooldownFacts, DeclaredPlatformRelation, IdentityConfidence, LegacyAccountFacts, ModelScope,
    OnboardingTaskKind, OnboardingTaskState, QuotaPolicyMode, QuotaSubject, RelationConfidence,
    SubscriptionSource, credential_id_for_legacy_account, identity_id_for_legacy_account,
    identity_id_for_platform_account, legacy_account_objects,
    onboarding_task_id_for_legacy_account, quota_pool_id_for_identity,
};

pub(crate) const IDENTITY_MODEL_VERSION: i32 = 45;

const LEGACY_KIND_ACCOUNT: &str = "account";
const LEGACY_KIND_PLATFORM: &str = "platform_account";
const NEW_KIND_IDENTITY: &str = "identity";
const NEW_KIND_CREDENTIAL: &str = "credential";
const NEW_KIND_BINDING: &str = "binding";

#[derive(Debug, Clone)]
pub struct StoredIdentity {
    pub id: String,
    pub label: String,
    pub identity_confidence: String,
    pub authority_site: Option<String>,
    pub authority_subject: Option<String>,
    pub enabled: bool,
    pub notes: Option<String>,
}

#[derive(Debug, Clone)]
pub struct StoredOnboarding {
    pub id: String,
    pub kind: String,
    pub step: String,
    pub state: String,
}

#[derive(Debug, Clone)]
pub struct StoredSubscription {
    pub source: String,
    pub purchase_date: String,
    pub expires_on: String,
}

#[derive(Debug, Clone)]
pub struct IdentityAccountRecord {
    pub account: Account,
    pub sort_order: i64,
    pub identity_id: String,
    pub verification_status: ConnectionVerificationStatus,
    pub has_key_material: bool,
    pub credential_id: String,
    pub credential_version: u64,
    pub auth_state_version: u64,
    pub binding_id: String,
    pub binding_enabled: bool,
    pub binding_model_scope: ModelScope,
    pub onboarding: Option<StoredOnboarding>,
    pub subscription: Option<StoredSubscription>,
    pub declared_relation: Option<DeclaredPlatformRelation>,
}

#[derive(Debug, Clone)]
pub struct PlatformIdentityRecord {
    pub platform_id: String,
    pub name: String,
    pub base_url: String,
    pub has_credential: bool,
    pub identity: StoredIdentity,
}

#[derive(Debug, Clone)]
pub struct IdentityModelSnapshot {
    pub accounts: Vec<IdentityAccountRecord>,
    pub identities: Vec<StoredIdentity>,
    pub platform_parents: Vec<PlatformIdentityRecord>,
}

#[derive(Debug, Clone)]
pub struct StoredInferenceBinding {
    pub account_id: String,
    pub binding_id: String,
    pub enabled: bool,
    pub model_scope: ModelScope,
}

#[derive(Debug, Clone)]
pub struct CreatedIdentityCredential {
    pub account_id: String,
    pub identity_id: String,
    pub credential_id: String,
    pub binding_id: String,
    pub version: u64,
    pub auth_state_version: u64,
}

pub(crate) fn connection_legacy_for_account(
    provider_id: &str,
    account_id: &str,
) -> (LegacyConnectionKind, String) {
    if provider_id == CUSTOM_PROVIDER_ID {
        (LegacyConnectionKind::CustomAccount, account_id.to_string())
    } else if builtin_provider(provider_id).is_some() {
        (
            LegacyConnectionKind::BuiltinProvider,
            provider_id.to_string(),
        )
    } else {
        (
            LegacyConnectionKind::DynamicProvider,
            provider_id.to_string(),
        )
    }
}

pub(crate) fn connection_id_for_account(provider_id: &str, account_id: &str) -> ConnectionId {
    let (kind, legacy_id) = connection_legacy_for_account(provider_id, account_id);
    connection_id_for_legacy(kind, &legacy_id)
}

pub(crate) fn has_legacy_subscription(provider_id: &str, setup_step: AccountSetupStep) -> bool {
    provider_id != CUSTOM_PROVIDER_ID
        && provider_id != OPENCODE_ZEN_FREE_PROVIDER_ID
        && provider_id != CPA_PROVIDER_ID
        && builtin_provider(provider_id).is_some()
        && setup_step == AccountSetupStep::Ready
}

pub(crate) fn platform_group_label(group: &crate::platform::PlatformGroup) -> String {
    group
        .id
        .clone()
        .or_else(|| group.platform.clone())
        .or_else(|| group.auto_groups.first().cloned())
        .unwrap_or_default()
}

fn identity_model_writable(conn: &Connection) -> Result<bool> {
    let version = schema_version_on(conn)?;
    if version < IDENTITY_MODEL_VERSION {
        return Ok(false);
    }
    anyhow::ensure!(
        table_exists(conn, "upstream_identities")?,
        "schema version {version} is missing identity model tables"
    );
    Ok(true)
}

pub(crate) fn persist_account_identity_model(
    conn: &Connection,
    account: &Account,
    purchase_date: &str,
    verification_status: ConnectionVerificationStatus,
    sort_order: i64,
    declared: Option<&DeclaredPlatformRelation>,
    now: DateTime<Utc>,
) -> Result<()> {
    persist_account_identity_model_on(
        conn,
        account,
        purchase_date,
        verification_status,
        sort_order,
        declared,
        now,
        None,
    )
}

pub(crate) fn persist_account_identity_model_on(
    conn: &Connection,
    account: &Account,
    purchase_date: &str,
    verification_status: ConnectionVerificationStatus,
    sort_order: i64,
    declared: Option<&DeclaredPlatformRelation>,
    now: DateTime<Utc>,
    identity_override: Option<&str>,
) -> Result<()> {
    if !identity_model_writable(conn)? {
        return Ok(());
    }
    let connection_id = connection_id_for_account(&account.provider_id, &account.id);
    let facts = LegacyAccountFacts {
        account_id: account.id.clone(),
        name: account.name.clone(),
        notes: account.notes.clone(),
        enabled: account.enabled,
        sort_order: u32::try_from(sort_order).unwrap_or(0),
        has_auth_error: account.auth_error.is_some(),
        verified: verification_status == ConnectionVerificationStatus::Verified,
        anonymous: account.credential_kind == ocg_domain::catalog::CredentialKind::None,
        declared_relation: declared.cloned(),
    };
    let (identity, credential, binding) = legacy_account_objects(facts, &connection_id, &[]);
    let (legacy_kind, legacy_id) = connection_legacy_for_account(&account.provider_id, &account.id);
    let now_rfc = now.to_rfc3339();
    let identity_id = match identity_override {
        Some(existing) => {
            let exists: i64 = conn.query_row(
                "SELECT COUNT(*) FROM upstream_identities WHERE id = ?1",
                [existing],
                |row| row.get(0),
            )?;
            anyhow::ensure!(exists == 1, "identity {existing} not found");
            existing.to_string()
        }
        None => {
            upsert_identity(
                conn,
                identity.id.as_str(),
                &identity.label,
                identity.identity_confidence,
                identity
                    .authority_ref
                    .as_ref()
                    .map(|authority| authority.issuer_or_site.as_str()),
                identity.enabled,
                identity.notes.as_deref(),
                &now_rfc,
            )?;
            identity.id.to_string()
        }
    };
    conn.execute(
        "UPDATE accounts SET identity_id = ?2 WHERE id = ?1",
        params![account.id, identity_id.as_str()],
    )?;
    conn.execute(
        "INSERT OR IGNORE INTO credential_state (
            account_id, credential_id, version, auth_state_version, rotated_at
         ) VALUES (?1, ?2, 1, 1, NULL)",
        params![account.id, credential.id.as_str()],
    )?;
    let model_scope = serde_json::to_string(&ModelScope::All)?;
    conn.execute(
        "INSERT OR IGNORE INTO credential_bindings (
            id, account_id, connection_legacy_kind, connection_legacy_id,
            model_scope, enabled, created_at, updated_at
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?7)",
        params![
            binding.id.as_str(),
            account.id,
            legacy_kind.as_str(),
            legacy_id,
            model_scope,
            account.enabled as i32,
            now_rfc,
        ],
    )?;
    insert_legacy_map(
        conn,
        LEGACY_KIND_ACCOUNT,
        &account.id,
        NEW_KIND_IDENTITY,
        identity_id.as_str(),
    )?;
    insert_legacy_map(
        conn,
        LEGACY_KIND_ACCOUNT,
        &account.id,
        NEW_KIND_CREDENTIAL,
        credential.id.as_str(),
    )?;
    insert_legacy_map(
        conn,
        LEGACY_KIND_ACCOUNT,
        &account.id,
        NEW_KIND_BINDING,
        binding.id.as_str(),
    )?;
    if account.account_type == AccountType::Managed && !account.setup_step.is_ready() {
        let task_id = onboarding_task_id_for_legacy_account(&account.id);
        conn.execute(
            "INSERT OR IGNORE INTO onboarding_tasks (
                id, account_id, kind, step, state, created_at, updated_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?6)",
            params![
                task_id.as_str(),
                account.id,
                OnboardingTaskKind::ManagedRegistration.as_str(),
                account.setup_step.as_str(),
                OnboardingTaskState::InProgress.as_str(),
                now_rfc,
            ],
        )?;
    }
    if has_legacy_subscription(&account.provider_id, account.setup_step) {
        let expires_on = purchase_expires_on(purchase_date)?;
        conn.execute(
            "INSERT OR IGNORE INTO subscription_records (
                account_id, source, purchase_date, expires_on, recorded_at
             ) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                account.id,
                SubscriptionSource::LegacyManual.as_str(),
                purchase_date,
                expires_on,
                now_rfc,
            ],
        )?;
    }
    ensure_identity_quota_pool(conn, &identity_id, &account.id, &now_rfc)?;
    Ok(())
}

fn parse_stored_model_scope(raw: &str) -> ModelScope {
    serde_json::from_str(raw).unwrap_or(ModelScope::All)
}

fn ensure_identity_quota_pool(
    conn: &Connection,
    identity_id: &str,
    account_id: &str,
    now_rfc: &str,
) -> Result<()> {
    let pool_id = quota_pool_id_for_identity(identity_id);
    conn.execute(
        "INSERT INTO quota_pools (
            id, subject_kind, subject_ref, relation_confidence, policy_mode, created_at
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)
         ON CONFLICT(id) DO NOTHING",
        params![
            pool_id.as_str(),
            QuotaSubject::Credential.as_str(),
            identity_id,
            RelationConfidence::Unknown.as_str(),
            QuotaPolicyMode::AuthoritativeLimit.as_str(),
            now_rfc,
        ],
    )?;
    conn.execute(
        "INSERT OR IGNORE INTO quota_pool_members (pool_id, account_id) VALUES (?1, ?2)",
        params![pool_id.as_str(), account_id],
    )?;
    let members: i64 = conn.query_row(
        "SELECT COUNT(*) FROM quota_pool_members WHERE pool_id = ?1",
        [pool_id.as_str()],
        |row| row.get(0),
    )?;
    if members > 1 {
        conn.execute(
            "UPDATE quota_pools SET relation_confidence = ?2 WHERE id = ?1",
            params![pool_id.as_str(), RelationConfidence::Declared.as_str()],
        )?;
    }
    Ok(())
}

pub(crate) fn fanout_shared_pool_cooldown(
    conn: &Connection,
    source_account_id: &str,
) -> Result<()> {
    if !identity_model_writable(conn)? {
        return Ok(());
    }
    if !table_exists(conn, "quota_pool_members")? {
        return Ok(());
    }
    let now_rfc = Utc::now().to_rfc3339();
    let siblings = shared_pool_siblings_on(conn, source_account_id)?;
    for sibling in siblings {
        conn.execute(
            "UPDATE accounts SET
                cooldown_until = (SELECT cooldown_until FROM accounts WHERE id = ?1),
                cooldown_generic_until = (SELECT cooldown_generic_until FROM accounts WHERE id = ?1),
                cooldown_5h_until = (SELECT cooldown_5h_until FROM accounts WHERE id = ?1),
                cooldown_week_until = (SELECT cooldown_week_until FROM accounts WHERE id = ?1),
                cooldown_month_until = (SELECT cooldown_month_until FROM accounts WHERE id = ?1),
                cooldown_free_until = (SELECT cooldown_free_until FROM accounts WHERE id = ?1),
                last_error = (SELECT last_error FROM accounts WHERE id = ?1),
                updated_at = ?3
             WHERE id = ?2",
            params![source_account_id, sibling, now_rfc],
        )?;
    }
    Ok(())
}

fn shared_pool_siblings_on(conn: &Connection, account_id: &str) -> Result<Vec<String>> {
    let mut stmt = conn.prepare(
        "SELECT DISTINCT m2.account_id
         FROM quota_pool_members m1
         JOIN quota_pool_members m2 ON m2.pool_id = m1.pool_id
         WHERE m1.account_id = ?1 AND m2.account_id <> ?1",
    )?;
    let rows = stmt
        .query_map([account_id], |row| row.get(0))?
        .collect::<rusqlite::Result<Vec<String>>>()?;
    Ok(rows)
}

fn upsert_identity(
    conn: &Connection,
    id: &str,
    label: &str,
    confidence: IdentityConfidence,
    authority_site: Option<&str>,
    enabled: bool,
    notes: Option<&str>,
    now_rfc: &str,
) -> Result<()> {
    conn.execute(
        "INSERT INTO upstream_identities (
            id, label, identity_confidence, authority_site, authority_subject,
            enabled, notes, created_at, updated_at
         ) VALUES (?1, ?2, ?3, ?4, NULL, ?5, ?6, ?7, ?7)
         ON CONFLICT(id) DO NOTHING",
        params![
            id,
            label,
            confidence.as_str(),
            authority_site,
            enabled as i32,
            notes,
            now_rfc,
        ],
    )?;
    Ok(())
}

fn insert_legacy_map(
    conn: &Connection,
    legacy_kind: &str,
    legacy_id: &str,
    new_kind: &str,
    new_id: &str,
) -> Result<()> {
    conn.execute(
        "INSERT OR IGNORE INTO legacy_identity_map (
            legacy_kind, legacy_id, new_kind, new_id, migration_version
         ) VALUES (?1, ?2, ?3, ?4, ?5)",
        params![
            legacy_kind,
            legacy_id,
            new_kind,
            new_id,
            IDENTITY_MODEL_VERSION,
        ],
    )?;
    Ok(())
}

pub(crate) fn persist_platform_identity(
    conn: &Connection,
    platform_id: &str,
    name: &str,
    base_url: &str,
    now: DateTime<Utc>,
) -> Result<()> {
    if !identity_model_writable(conn)? {
        return Ok(());
    }
    let identity_id = identity_id_for_platform_account(platform_id);
    let now_rfc = now.to_rfc3339();
    upsert_identity(
        conn,
        identity_id.as_str(),
        name,
        IdentityConfidence::Declared,
        Some(base_url),
        true,
        None,
        &now_rfc,
    )?;
    insert_legacy_map(
        conn,
        LEGACY_KIND_PLATFORM,
        platform_id,
        NEW_KIND_IDENTITY,
        identity_id.as_str(),
    )?;
    Ok(())
}

pub(crate) fn update_account_identity_declaration(
    conn: &Connection,
    account_id: &str,
    declared: Option<&DeclaredPlatformRelation>,
    now: DateTime<Utc>,
) -> Result<()> {
    if !identity_model_writable(conn)? {
        return Ok(());
    }
    let identity_id: Option<String> = conn
        .query_row(
            "SELECT identity_id FROM accounts WHERE id = ?1",
            [account_id],
            |row| row.get(0),
        )
        .optional()?
        .flatten();
    let Some(identity_id) = identity_id else {
        return Ok(());
    };
    match declared {
        Some(relation) => {
            conn.execute(
                "UPDATE upstream_identities
                 SET identity_confidence = ?2, authority_site = ?3, authority_subject = NULL,
                     updated_at = ?4
                 WHERE id = ?1",
                params![
                    identity_id,
                    IdentityConfidence::Declared.as_str(),
                    relation.parent_base_url,
                    now.to_rfc3339(),
                ],
            )?;
        }
        None => {
            conn.execute(
                "UPDATE upstream_identities
                 SET identity_confidence = ?2, authority_site = NULL, authority_subject = NULL,
                     updated_at = ?3
                 WHERE id = ?1",
                params![
                    identity_id,
                    IdentityConfidence::Opaque.as_str(),
                    now.to_rfc3339(),
                ],
            )?;
        }
    }
    Ok(())
}

pub(crate) fn delete_account_identity_satellites(
    conn: &Connection,
    account_id: &str,
) -> Result<()> {
    if !identity_model_writable(conn)? {
        return Ok(());
    }
    conn.execute(
        "DELETE FROM legacy_identity_map WHERE legacy_kind = ?1 AND legacy_id = ?2",
        params![LEGACY_KIND_ACCOUNT, account_id],
    )?;
    conn.execute(
        "DELETE FROM credential_state WHERE account_id = ?1",
        [account_id],
    )?;
    conn.execute(
        "DELETE FROM credential_bindings WHERE account_id = ?1",
        [account_id],
    )?;
    conn.execute(
        "DELETE FROM onboarding_tasks WHERE account_id = ?1",
        [account_id],
    )?;
    conn.execute(
        "DELETE FROM subscription_records WHERE account_id = ?1",
        [account_id],
    )?;
    conn.execute(
        "DELETE FROM quota_pool_members WHERE account_id = ?1",
        [account_id],
    )?;
    conn.execute(
        "DELETE FROM quota_pools
         WHERE NOT EXISTS (
            SELECT 1 FROM quota_pool_members m WHERE m.pool_id = quota_pools.id
         )",
        [],
    )?;
    Ok(())
}

pub(crate) fn delete_orphan_identity_for_account(
    conn: &Connection,
    account_id: &str,
    identity_id: Option<&str>,
) -> Result<()> {
    if !identity_model_writable(conn)? {
        return Ok(());
    }
    if let Some(identity_id) = identity_id {
        delete_orphan_identity(conn, identity_id)?;
        return Ok(());
    }
    let derived = identity_id_for_legacy_account(account_id);
    delete_orphan_identity(conn, derived.as_str())
}

pub(crate) fn account_identity_id(conn: &Connection, account_id: &str) -> Result<Option<String>> {
    if !identity_model_writable(conn)? {
        return Ok(None);
    }
    Ok(conn
        .query_row(
            "SELECT identity_id FROM accounts WHERE id = ?1",
            [account_id],
            |row| row.get(0),
        )
        .optional()?
        .flatten())
}

fn delete_orphan_identity(conn: &Connection, identity_id: &str) -> Result<()> {
    let remaining: i64 = conn.query_row(
        "SELECT COUNT(*) FROM accounts WHERE identity_id = ?1",
        [identity_id],
        |row| row.get(0),
    )?;
    if remaining > 0 {
        return Ok(());
    }
    let platform_owned: i64 = conn.query_row(
        "SELECT COUNT(*) FROM legacy_identity_map
         WHERE new_kind = ?1 AND new_id = ?2 AND legacy_kind = ?3",
        params![NEW_KIND_IDENTITY, identity_id, LEGACY_KIND_PLATFORM],
        |row| row.get(0),
    )?;
    if platform_owned > 0 {
        return Ok(());
    }
    conn.execute(
        "DELETE FROM upstream_identities WHERE id = ?1",
        [identity_id],
    )?;
    conn.execute(
        "DELETE FROM legacy_identity_map WHERE new_kind = ?1 AND new_id = ?2",
        params![NEW_KIND_IDENTITY, identity_id],
    )?;
    Ok(())
}

pub(crate) fn delete_platform_identity(conn: &Connection, platform_id: &str) -> Result<()> {
    if !identity_model_writable(conn)? {
        return Ok(());
    }
    let identity_id = identity_id_for_platform_account(platform_id);
    conn.execute(
        "DELETE FROM legacy_identity_map WHERE legacy_kind = ?1 AND legacy_id = ?2",
        params![LEGACY_KIND_PLATFORM, platform_id],
    )?;
    conn.execute(
        "DELETE FROM upstream_identities WHERE id = ?1",
        [identity_id.as_str()],
    )?;
    Ok(())
}

pub(crate) fn migrate_to_v45(conn: &Connection) -> Result<()> {
    if schema_version_on(conn)? >= 45 {
        return Ok(());
    }
    let tx = Transaction::new_unchecked(conn, TransactionBehavior::Immediate)?;
    let version = schema_version_on(&tx)?;
    if version >= 45 {
        return Ok(());
    }
    anyhow::ensure!(version == 44, "v45 requires schema v44");
    create_identity_tables(&tx)?;
    migrate_v45_body(&tx)?;
    tx.execute_batch("INSERT OR REPLACE INTO schema_version(version) VALUES (45);")?;
    tx.commit()?;
    Ok(())
}

fn identity_account_violations(conn: &Connection) -> Result<i64> {
    let missing_identity: i64 = conn.query_row(
        "SELECT COUNT(*) FROM accounts WHERE identity_id IS NULL OR identity_id = ''",
        [],
        |row| row.get(0),
    )?;
    let missing_credential: i64 = conn.query_row(
        "SELECT COUNT(*) FROM accounts a
         WHERE NOT EXISTS (SELECT 1 FROM credential_state c WHERE c.account_id = a.id)",
        [],
        |row| row.get(0),
    )?;
    let missing_binding: i64 = conn.query_row(
        "SELECT COUNT(*) FROM accounts a
         WHERE NOT EXISTS (SELECT 1 FROM credential_bindings b WHERE b.account_id = a.id)",
        [],
        |row| row.get(0),
    )?;
    let missing_pool: i64 = conn.query_row(
        "SELECT COUNT(*) FROM accounts a
         WHERE a.identity_id IS NOT NULL AND a.identity_id <> ''
           AND NOT EXISTS (SELECT 1 FROM quota_pool_members m WHERE m.account_id = a.id)",
        [],
        |row| row.get(0),
    )?;
    Ok(missing_identity + missing_credential + missing_binding + missing_pool)
}

pub(crate) fn ensure_identity_model_consistent(conn: &Connection) -> Result<()> {
    let version = schema_version_on(conn)?;
    if version < IDENTITY_MODEL_VERSION {
        return Ok(());
    }
    anyhow::ensure!(
        table_exists(conn, "upstream_identities")?
            && table_exists(conn, "credential_state")?
            && table_exists(conn, "credential_bindings")?,
        "schema version {version} is missing identity model tables"
    );
    if !accounts_ready_for_identity_backfill(conn)? {
        return Ok(());
    }
    if identity_account_violations(conn)? == 0 {
        return Ok(());
    }
    eprintln!(
        "warning: identity model satellites are incomplete on schema v{version}; repairing with deterministic backfill"
    );
    let tx = Transaction::new_unchecked(conn, TransactionBehavior::Immediate)?;
    migrate_v45_body(&tx)?;
    tx.commit()?;
    let remaining = identity_account_violations(conn)?;
    anyhow::ensure!(
        remaining == 0,
        "identity model remains inconsistent after repair ({remaining} account satellite violations)"
    );
    Ok(())
}

fn create_identity_tables(tx: &Transaction<'_>) -> Result<()> {
    tx.execute_batch(
        "CREATE TABLE IF NOT EXISTS upstream_identities (
            id TEXT PRIMARY KEY,
            label TEXT NOT NULL,
            identity_confidence TEXT NOT NULL CHECK(identity_confidence IN ('opaque','declared')),
            authority_site TEXT,
            authority_subject TEXT,
            enabled INTEGER NOT NULL DEFAULT 1,
            notes TEXT,
            created_at TEXT NOT NULL,
            updated_at TEXT NOT NULL
        );
        CREATE TABLE IF NOT EXISTS credential_state (
            account_id TEXT PRIMARY KEY REFERENCES accounts(id) ON DELETE CASCADE,
            credential_id TEXT NOT NULL UNIQUE,
            version INTEGER NOT NULL DEFAULT 1,
            auth_state_version INTEGER NOT NULL DEFAULT 1,
            rotated_at TEXT
        );
        CREATE TABLE IF NOT EXISTS credential_bindings (
            id TEXT PRIMARY KEY,
            account_id TEXT NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
            connection_legacy_kind TEXT NOT NULL,
            connection_legacy_id TEXT NOT NULL,
            model_scope TEXT NOT NULL,
            enabled INTEGER NOT NULL DEFAULT 1,
            created_at TEXT NOT NULL,
            updated_at TEXT NOT NULL
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
            account_id TEXT NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
            kind TEXT NOT NULL,
            step TEXT NOT NULL,
            state TEXT NOT NULL,
            created_at TEXT NOT NULL,
            updated_at TEXT NOT NULL
        );
        CREATE TABLE IF NOT EXISTS subscription_records (
            account_id TEXT PRIMARY KEY REFERENCES accounts(id) ON DELETE CASCADE,
            source TEXT NOT NULL,
            purchase_date TEXT NOT NULL,
            expires_on TEXT NOT NULL,
            recorded_at TEXT NOT NULL
        );
        CREATE TABLE IF NOT EXISTS quota_pools (
            id TEXT PRIMARY KEY,
            subject_kind TEXT NOT NULL,
            subject_ref TEXT NOT NULL,
            relation_confidence TEXT NOT NULL,
            policy_mode TEXT NOT NULL,
            created_at TEXT NOT NULL
        );
        CREATE TABLE IF NOT EXISTS quota_pool_members (
            pool_id TEXT NOT NULL REFERENCES quota_pools(id) ON DELETE CASCADE,
            account_id TEXT NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
            PRIMARY KEY (pool_id, account_id)
        );",
    )?;
    ensure_column(tx, "accounts", "identity_id", "TEXT")?;
    Ok(())
}

fn accounts_ready_for_identity_backfill(conn: &Connection) -> Result<bool> {
    // Historical stub fixtures (v14+) only add columns each step introduces.
    // Skip the account walk when the modern accounts shape is not present so
    // those opens still reach schema 45.
    for column in [
        "name",
        "username",
        "password_cipher",
        "key_cipher",
        "enabled",
        "referral_code",
        "recharge_date",
        "cooldown_until",
        "cooldown_generic_until",
        "cooldown_5h_until",
        "cooldown_week_until",
        "cooldown_month_until",
        "cooldown_free_until",
        "last_error",
        "created_at",
        "updated_at",
        "auth_error",
        "account_type",
        "setup_step",
        "notes",
        "provider_id",
        "credential_kind",
        "quota_scope",
        "sort_order",
        "verification_status",
    ] {
        if !table_has_column(conn, "accounts", column)? {
            return Ok(false);
        }
    }
    Ok(true)
}

pub(crate) fn migrate_v45_body(tx: &Transaction<'_>) -> Result<()> {
    create_identity_tables(tx)?;
    let now = Utc::now();
    let rows = if accounts_ready_for_identity_backfill(tx)? {
        let mut stmt = tx.prepare(
            "SELECT id, name, username, password_cipher, key_cipher, enabled, referral_code,
                    recharge_date, cooldown_until, cooldown_generic_until, cooldown_5h_until,
                    cooldown_week_until, cooldown_month_until, cooldown_free_until, last_error,
                    created_at, updated_at, auth_error, account_type, setup_step, notes,
                    provider_id, credential_kind, quota_scope, sort_order, verification_status,
                    identity_id
             FROM accounts
             ORDER BY sort_order ASC, created_at ASC, id ASC",
        )?;
        stmt.query_map([], |row| {
            let account = account_from_row(row)?;
            Ok((
                account,
                row.get::<_, i64>(24)?,
                row.get::<_, String>(25)?,
                row.get::<_, Option<String>>(26)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?
    } else {
        Vec::new()
    };

    let mut links = Vec::new();
    if table_exists(tx, "platform_links")? {
        let mut link_stmt = tx.prepare(
            "SELECT l.account_id, l.platform_account_id, l.group_json, p.base_url
             FROM platform_links l
             JOIN platform_accounts p ON p.id = l.platform_account_id",
        )?;
        links = link_stmt
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
    }
    let mut declared_by_account = std::collections::HashMap::new();
    for (account_id, platform_account_id, group_json, base_url) in &links {
        let group: crate::platform::PlatformGroup =
            serde_json::from_str(group_json).unwrap_or_default();
        declared_by_account.insert(
            account_id.clone(),
            DeclaredPlatformRelation {
                platform_account_id: platform_account_id.clone(),
                group: platform_group_label(&group),
                parent_base_url: base_url.clone(),
            },
        );
    }

    for (account, sort_order, verification_status, _identity_id) in &rows {
        let status = ConnectionVerificationStatus::try_from(verification_status.as_str())
            .unwrap_or(ConnectionVerificationStatus::NotRequired);
        persist_account_identity_model(
            tx,
            account,
            &account.purchase_date,
            status,
            *sort_order,
            declared_by_account.get(&account.id),
            now,
        )?;
    }

    if table_exists(tx, "platform_accounts")? {
        let mut parent_stmt =
            tx.prepare("SELECT id, name, base_url FROM platform_accounts ORDER BY rowid")?;
        let parents = parent_stmt
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        drop(parent_stmt);
        for (id, name, base_url) in parents {
            persist_platform_identity(tx, &id, &name, &base_url, now)?;
        }
    }
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RotatedCredential {
    pub account_id: String,
    pub credential_id: String,
    pub version: u64,
    pub auth_state_version: u64,
}

impl Database {
    pub fn list_identity_model(&self) -> Result<IdentityModelSnapshot> {
        list_identity_model_on(&self.conn)
    }

    /// Replace the Key on one legacy account and bump both credential versions
    /// in the same SQLite transaction. A missing `credential_state` row is
    /// repaired with the deterministic id (version 1) then incremented.
    pub fn rotate_account_credential(
        &self,
        account_id: &str,
        key_cipher: &str,
    ) -> Result<RotatedCredential> {
        rotate_account_credential_on(&self.conn, account_id, key_cipher)
    }

    pub fn list_inference_bindings(&self) -> Result<Vec<StoredInferenceBinding>> {
        list_inference_bindings_on(&self.conn)
    }

    pub fn shared_pool_account_ids(&self, account_id: &str) -> Result<Vec<String>> {
        if !identity_model_writable(&self.conn)? {
            return Ok(Vec::new());
        }
        let mut ids = shared_pool_siblings_on(&self.conn, account_id)?;
        ids.push(account_id.to_string());
        ids.sort();
        ids.dedup();
        Ok(ids)
    }

    pub fn update_credential_binding(
        &self,
        binding_id: &str,
        model_scope: Option<&ModelScope>,
        enabled: Option<bool>,
    ) -> Result<StoredInferenceBinding> {
        update_credential_binding_on(&self.conn, binding_id, model_scope, enabled)
    }

    pub fn create_account_for_identity(
        &self,
        identity_id: &str,
        account: &Account,
        purchase_date: &str,
        verification_status: ConnectionVerificationStatus,
    ) -> Result<CreatedIdentityCredential> {
        create_account_for_identity_on(
            &self.conn,
            identity_id,
            account,
            purchase_date,
            verification_status,
        )
    }
}

fn rotate_account_credential_on(
    conn: &Connection,
    account_id: &str,
    key_cipher: &str,
) -> Result<RotatedCredential> {
    if !identity_model_writable(conn)? {
        anyhow::bail!("identity model is not writable");
    }
    let credential_id = credential_id_for_legacy_account(account_id);
    let now_rfc = Utc::now().to_rfc3339();
    let tx = Transaction::new_unchecked(conn, TransactionBehavior::Immediate)?;
    let provider_id: String = tx
        .query_row(
            "SELECT provider_id FROM accounts WHERE id = ?1",
            [account_id],
            |row| row.get(0),
        )
        .optional()?
        .ok_or_else(|| anyhow::anyhow!("account not found"))?;
    let requires_verification = builtin_provider(&provider_id)
        .is_some_and(|plan| plan.verification_policy == VerificationPolicy::Required);
    let verification_status = if requires_verification {
        ConnectionVerificationStatus::Pending
    } else {
        ConnectionVerificationStatus::NotRequired
    };
    let account_updated = tx.execute(
        "UPDATE accounts SET
            key_cipher = ?2,
            auth_error = NULL,
            last_error = NULL,
            verification_status = ?3,
            connection_verified_at = NULL,
            verification_error = NULL,
            updated_at = ?4
         WHERE id = ?1",
        params![
            account_id,
            key_cipher,
            verification_status.as_str(),
            now_rfc
        ],
    )?;
    anyhow::ensure!(account_updated == 1, "account {account_id} was not updated");
    tx.execute(
        "INSERT OR IGNORE INTO credential_state (
            account_id, credential_id, version, auth_state_version, rotated_at
         ) VALUES (?1, ?2, 1, 1, NULL)",
        params![account_id, credential_id.as_str()],
    )?;
    let updated = tx.execute(
        "UPDATE credential_state
         SET version = version + 1,
             auth_state_version = auth_state_version + 1,
             rotated_at = ?2
         WHERE account_id = ?1",
        params![account_id, now_rfc],
    )?;
    anyhow::ensure!(
        updated == 1,
        "account {account_id} is missing credential_state after repair"
    );
    invalidate_rotated_probe_evidence(&tx, account_id, &provider_id)?;
    let (stored_id, version, auth_state_version): (String, i64, i64) = tx.query_row(
        "SELECT credential_id, version, auth_state_version
         FROM credential_state WHERE account_id = ?1",
        [account_id],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
    )?;
    tx.commit()?;
    Ok(RotatedCredential {
        account_id: account_id.to_string(),
        credential_id: stored_id,
        version: version as u64,
        auth_state_version: auth_state_version as u64,
    })
}

fn invalidate_rotated_probe_evidence(
    conn: &Connection,
    account_id: &str,
    provider_id: &str,
) -> Result<()> {
    // Configurable HTTP (Custom + user-defined) stores probe rows on the
    // account's custom_endpoint scope. Builtin catalog scopes stay.
    let now = Utc::now();
    if provider_id == CUSTOM_PROVIDER_ID {
        return invalidate_probe_evidence_on(
            conn,
            &ContractScope::custom_endpoint(account_id),
            now,
        );
    }
    let dynamic: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM providers
             WHERE id = ?1 AND origin IS NOT NULL AND origin != 'builtin'",
            [provider_id],
            |row| row.get(0),
        )
        .optional()?
        .unwrap_or(0);
    if dynamic > 0 {
        invalidate_probe_evidence_on(conn, &ContractScope::custom_endpoint(account_id), now)?;
    }
    Ok(())
}

fn list_inference_bindings_on(conn: &Connection) -> Result<Vec<StoredInferenceBinding>> {
    if !identity_model_writable(conn)? {
        return Ok(Vec::new());
    }
    let mut stmt =
        conn.prepare("SELECT account_id, id, enabled, model_scope FROM credential_bindings")?;
    let rows = stmt
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, i32>(2)?,
                row.get::<_, String>(3)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows
        .into_iter()
        .map(
            |(account_id, binding_id, enabled, model_scope)| StoredInferenceBinding {
                account_id,
                binding_id,
                enabled: enabled != 0,
                model_scope: parse_stored_model_scope(&model_scope),
            },
        )
        .collect())
}

fn update_credential_binding_on(
    conn: &Connection,
    binding_id: &str,
    model_scope: Option<&ModelScope>,
    enabled: Option<bool>,
) -> Result<StoredInferenceBinding> {
    if !identity_model_writable(conn)? {
        anyhow::bail!("identity model is not writable");
    }
    let tx = Transaction::new_unchecked(conn, TransactionBehavior::Immediate)?;
    let existing: Option<(String, i32, String)> = tx
        .query_row(
            "SELECT account_id, enabled, model_scope FROM credential_bindings WHERE id = ?1",
            [binding_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()?;
    let Some((account_id, current_enabled, current_scope)) = existing else {
        anyhow::bail!("binding not found");
    };
    let next_scope = match model_scope {
        Some(scope) => serde_json::to_string(scope)?,
        None => current_scope,
    };
    let next_enabled = enabled.map(|value| value as i32).unwrap_or(current_enabled);
    let now_rfc = Utc::now().to_rfc3339();
    tx.execute(
        "UPDATE credential_bindings
         SET model_scope = ?2, enabled = ?3, updated_at = ?4
         WHERE id = ?1",
        params![binding_id, next_scope, next_enabled, now_rfc],
    )?;
    tx.commit()?;
    Ok(StoredInferenceBinding {
        account_id,
        binding_id: binding_id.to_string(),
        enabled: next_enabled != 0,
        model_scope: parse_stored_model_scope(&next_scope),
    })
}

fn create_account_for_identity_on(
    conn: &Connection,
    identity_id: &str,
    account: &Account,
    purchase_date: &str,
    verification_status: ConnectionVerificationStatus,
) -> Result<CreatedIdentityCredential> {
    if !identity_model_writable(conn)? {
        anyhow::bail!("identity model is not writable");
    }
    let tx = Transaction::new_unchecked(conn, TransactionBehavior::Immediate)?;
    super::insert_account_columns(&tx, account, purchase_date, verification_status)?;
    let sort_order: i64 = tx.query_row(
        "SELECT sort_order FROM accounts WHERE id = ?1",
        [&account.id],
        |row| row.get(0),
    )?;
    persist_account_identity_model_on(
        &tx,
        account,
        purchase_date,
        verification_status,
        sort_order,
        None,
        Utc::now(),
        Some(identity_id),
    )?;
    let (credential_id, version, auth_state_version, binding_id): (String, i64, i64, String) = tx
        .query_row(
        "SELECT c.credential_id, c.version, c.auth_state_version, b.id
             FROM credential_state c
             JOIN credential_bindings b ON b.account_id = c.account_id
             WHERE c.account_id = ?1",
        [&account.id],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
    )?;
    tx.commit()?;
    Ok(CreatedIdentityCredential {
        account_id: account.id.clone(),
        identity_id: identity_id.to_string(),
        credential_id,
        binding_id,
        version: version as u64,
        auth_state_version: auth_state_version as u64,
    })
}

pub(crate) fn list_identity_model_on(conn: &Connection) -> Result<IdentityModelSnapshot> {
    let version = schema_version_on(conn)?;
    if version < IDENTITY_MODEL_VERSION {
        return Ok(IdentityModelSnapshot {
            accounts: Vec::new(),
            identities: Vec::new(),
            platform_parents: Vec::new(),
        });
    }
    anyhow::ensure!(
        table_exists(conn, "upstream_identities")?,
        "schema version {version} is missing identity model tables"
    );

    let mut account_stmt = conn.prepare(
        "SELECT id, name, username, password_cipher, key_cipher, enabled, referral_code,
                recharge_date, cooldown_until, cooldown_generic_until, cooldown_5h_until,
                cooldown_week_until, cooldown_month_until, cooldown_free_until, last_error,
                created_at, updated_at, auth_error, account_type, setup_step, notes,
                provider_id, credential_kind, quota_scope, sort_order, verification_status,
                identity_id, key_cipher
         FROM accounts
         ORDER BY sort_order ASC, created_at ASC, id ASC",
    )?;
    let account_rows = account_stmt
        .query_map([], |row| {
            let account = account_from_row(row)?;
            let key_cipher: String = row.get(27)?;
            Ok((
                account,
                row.get::<_, i64>(24)?,
                row.get::<_, String>(25)?,
                row.get::<_, Option<String>>(26)?,
                !key_cipher.is_empty(),
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    drop(account_stmt);

    let mut credential_map = std::collections::HashMap::new();
    let mut cred_stmt = conn.prepare(
        "SELECT account_id, credential_id, version, auth_state_version FROM credential_state",
    )?;
    for row in cred_stmt.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, i64>(2)?,
            row.get::<_, i64>(3)?,
        ))
    })? {
        let (account_id, credential_id, version, auth_state_version) = row?;
        credential_map.insert(account_id, (credential_id, version, auth_state_version));
    }
    drop(cred_stmt);

    let mut binding_map = std::collections::HashMap::new();
    let mut bind_stmt =
        conn.prepare("SELECT account_id, id, enabled, model_scope FROM credential_bindings")?;
    for row in bind_stmt.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, i32>(2)?,
            row.get::<_, String>(3)?,
        ))
    })? {
        let (account_id, binding_id, enabled, model_scope) = row?;
        binding_map.insert(
            account_id,
            (
                binding_id,
                enabled != 0,
                parse_stored_model_scope(&model_scope),
            ),
        );
    }
    drop(bind_stmt);

    let mut onboarding_map = std::collections::HashMap::new();
    let mut onb_stmt =
        conn.prepare("SELECT account_id, id, kind, step, state FROM onboarding_tasks")?;
    for row in onb_stmt.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            StoredOnboarding {
                id: row.get(1)?,
                kind: row.get(2)?,
                step: row.get(3)?,
                state: row.get(4)?,
            },
        ))
    })? {
        let (account_id, task) = row?;
        onboarding_map.insert(account_id, task);
    }
    drop(onb_stmt);

    let mut subscription_map = std::collections::HashMap::new();
    let mut sub_stmt = conn.prepare(
        "SELECT account_id, source, purchase_date, expires_on FROM subscription_records",
    )?;
    for row in sub_stmt.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            StoredSubscription {
                source: row.get(1)?,
                purchase_date: row.get(2)?,
                expires_on: row.get(3)?,
            },
        ))
    })? {
        let (account_id, record) = row?;
        subscription_map.insert(account_id, record);
    }
    drop(sub_stmt);

    let mut declared_by_account = std::collections::HashMap::new();
    if table_exists(conn, "platform_links")? {
        let mut link_stmt = conn.prepare(
            "SELECT l.account_id, l.platform_account_id, l.group_json, p.base_url
             FROM platform_links l
             JOIN platform_accounts p ON p.id = l.platform_account_id",
        )?;
        for row in link_stmt.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
            ))
        })? {
            let (account_id, platform_account_id, group_json, base_url) = row?;
            let group: crate::platform::PlatformGroup =
                serde_json::from_str(&group_json).unwrap_or_default();
            declared_by_account.insert(
                account_id,
                DeclaredPlatformRelation {
                    platform_account_id,
                    group: platform_group_label(&group),
                    parent_base_url: base_url,
                },
            );
        }
    }

    let mut identities = Vec::new();
    let mut id_stmt = conn.prepare(
        "SELECT id, label, identity_confidence, authority_site, authority_subject,
                enabled, notes
         FROM upstream_identities",
    )?;
    for row in id_stmt.query_map([], |row| {
        Ok(StoredIdentity {
            id: row.get(0)?,
            label: row.get(1)?,
            identity_confidence: row.get(2)?,
            authority_site: row.get(3)?,
            authority_subject: row.get(4)?,
            enabled: row.get::<_, i32>(5)? != 0,
            notes: row.get(6)?,
        })
    })? {
        identities.push(row?);
    }
    drop(id_stmt);
    let identities_by_id: std::collections::HashMap<_, _> = identities
        .iter()
        .map(|identity| (identity.id.clone(), identity.clone()))
        .collect();

    let mut accounts = Vec::new();
    for (account, sort_order, verification_status, identity_id, has_key_material) in account_rows {
        let status = ConnectionVerificationStatus::try_from(verification_status.as_str())
            .unwrap_or(ConnectionVerificationStatus::NotRequired);
        let identity_id = identity_id.ok_or_else(|| {
            anyhow::anyhow!(
                "account {} is missing identity_id on schema v{version}",
                account.id
            )
        })?;
        let (credential_id, credential_version, auth_state_version) =
            credential_map.get(&account.id).cloned().ok_or_else(|| {
                anyhow::anyhow!(
                    "account {} is missing credential_state on schema v{version}",
                    account.id
                )
            })?;
        let (binding_id, binding_enabled, binding_model_scope) =
            binding_map.get(&account.id).cloned().ok_or_else(|| {
                anyhow::anyhow!(
                    "account {} is missing credential_bindings on schema v{version}",
                    account.id
                )
            })?;
        accounts.push(IdentityAccountRecord {
            declared_relation: declared_by_account.get(&account.id).cloned(),
            onboarding: onboarding_map.remove(&account.id),
            subscription: subscription_map.remove(&account.id),
            account,
            sort_order,
            identity_id,
            verification_status: status,
            has_key_material,
            credential_id,
            credential_version: credential_version as u64,
            auth_state_version: auth_state_version as u64,
            binding_id,
            binding_enabled,
            binding_model_scope,
        });
    }

    let mut platform_parents = Vec::new();
    if table_exists(conn, "platform_accounts")? {
        let mut parent_stmt = conn.prepare(
            "SELECT id, name, base_url, credential_cipher IS NOT NULL
             FROM platform_accounts ORDER BY rowid",
        )?;
        for row in parent_stmt.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, bool>(3)?,
            ))
        })? {
            let (platform_id, name, base_url, has_credential) = row?;
            let identity_id = identity_id_for_platform_account(&platform_id);
            let identity = identities_by_id.get(identity_id.as_str()).cloned().ok_or_else(|| {
                anyhow::anyhow!(
                    "platform account {platform_id} is missing its identity row on schema v{version}"
                )
            })?;
            platform_parents.push(PlatformIdentityRecord {
                platform_id,
                name,
                base_url,
                has_credential,
                identity,
            });
        }
    }

    Ok(IdentityModelSnapshot {
        accounts,
        identities,
        platform_parents,
    })
}

pub(crate) fn cooldown_facts_for(account: &Account) -> CooldownFacts {
    CooldownFacts {
        account_id: account.id.clone(),
        generic: account.cooldown_generic_until,
        five_hours: account.cooldown_5h_until,
        week: account.cooldown_week_until,
        month: account.cooldown_month_until,
        free: account.cooldown_free_until,
    }
}
