//! Additive identity / credential / binding satellites (schema v45).
use super::*;
use ocg_domain::account::DEFAULT_INFERENCE_BINDING_ENABLED;
use ocg_domain::connection::{
    ConnectionId, EndpointOperation, LegacyConnectionKind, connection_id_for_legacy,
};
use ocg_domain::credential::{
    AssignedEndpoint, CooldownFacts, DeclaredPlatformRelation, IdentityConfidence,
    LegacyAccountFacts, ModelScope, OnboardingTaskKind, OnboardingTaskState, QuotaPolicyMode,
    QuotaSubject, RelationConfidence, RouteSpec, SubscriptionSource, assigned_endpoints_for_routes,
    credential_id_for_legacy_account, identity_id_for_legacy_account,
    identity_id_for_platform_account, legacy_account_objects,
    onboarding_task_id_for_legacy_account, quota_pool_id_for_accounts, quota_pool_id_for_identity,
    safe_default_grants,
};
use ocg_domain::dynamic::DynamicModelUpstreamOverride;

pub(crate) const IDENTITY_MODEL_VERSION: i32 = 45;
pub(crate) const BINDING_GRANT_VERSION: i32 = 46;

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
    pub allowed_endpoint_ids: Vec<String>,
    pub allowed_origins: Vec<String>,
    pub quota_pool_id: Option<String>,
    pub quota_relation_confidence: Option<String>,
    pub quota_policy_mode: Option<String>,
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
    pub allowed_endpoint_ids: Vec<String>,
    pub allowed_origins: Vec<String>,
    pub credential_version: u64,
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

/// How a newly created identity credential joins quota.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum QuotaSharingJoin {
    Independent,
    Shared { source_credential_id: String },
}

/// Validated V6 portable identity graph applied inside the node-import
/// transaction. Account ids are already remapped to destination ids.
#[derive(Debug, Clone)]
pub struct IdentityImportSnapshot {
    pub identities: Vec<ImportedIdentity>,
    pub accounts: Vec<ImportedAccountIdentity>,
    pub quota_pools: Vec<ImportedQuotaPool>,
}

#[derive(Debug, Clone)]
pub struct ImportedIdentity {
    pub id: String,
    pub label: String,
    pub identity_confidence: String,
    pub authority_site: Option<String>,
    pub authority_subject: Option<String>,
    pub enabled: bool,
    pub notes: Option<String>,
}

#[derive(Debug, Clone)]
pub struct ImportedAccountIdentity {
    pub account_id: String,
    pub identity_id: String,
    pub credential_id: String,
    pub credential_version: u64,
    pub auth_state_version: u64,
    pub binding_id: String,
    pub binding_enabled: bool,
    pub binding_model_scope: ModelScope,
    pub allowed_endpoint_ids: Vec<String>,
    pub allowed_origins: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct ImportedQuotaPool {
    pub id: String,
    pub subject_kind: String,
    pub subject_ref: String,
    pub relation_confidence: String,
    pub policy_mode: String,
    pub member_account_ids: Vec<String>,
}

impl IdentityImportSnapshot {
    pub fn remap_account_ids(&self, map: &std::collections::HashMap<String, String>) -> Self {
        let remap = |account_id: &str| {
            map.get(account_id)
                .cloned()
                .unwrap_or_else(|| account_id.to_string())
        };
        Self {
            identities: self.identities.clone(),
            accounts: self
                .accounts
                .iter()
                .map(|row| ImportedAccountIdentity {
                    account_id: remap(&row.account_id),
                    ..row.clone()
                })
                .collect(),
            quota_pools: self
                .quota_pools
                .iter()
                .map(|pool| ImportedQuotaPool {
                    member_account_ids: pool
                        .member_account_ids
                        .iter()
                        .map(|id| remap(id))
                        .collect(),
                    ..pool.clone()
                })
                .collect(),
        }
    }

    pub fn filter_account_ids(&self, keep: &HashSet<String>) -> Self {
        let accounts: Vec<_> = self
            .accounts
            .iter()
            .filter(|row| keep.contains(&row.account_id))
            .cloned()
            .collect();
        let identity_ids: HashSet<_> = accounts.iter().map(|row| row.identity_id.clone()).collect();
        let identities = self
            .identities
            .iter()
            .filter(|identity| identity_ids.contains(&identity.id))
            .cloned()
            .collect();
        let quota_pools = self
            .quota_pools
            .iter()
            .filter_map(|pool| {
                let members: Vec<_> = pool
                    .member_account_ids
                    .iter()
                    .filter(|id| keep.contains(*id))
                    .cloned()
                    .collect();
                if members.is_empty() {
                    None
                } else {
                    Some(ImportedQuotaPool {
                        member_account_ids: members,
                        ..pool.clone()
                    })
                }
            })
            .collect();
        Self {
            identities,
            accounts,
            quota_pools,
        }
    }
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

/// One write covers identity, credential, binding, cooldown, and optional override.
#[allow(clippy::too_many_arguments)]
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
    let saved_identity: Option<String> = conn
        .query_row(
            "SELECT identity_id FROM accounts WHERE id = ?1",
            [&account.id],
            |row| row.get(0),
        )
        .optional()?
        .flatten();
    let identity_id = match identity_override.or(saved_identity.as_deref()) {
        Some(existing) => {
            let exists: i64 = conn.query_row(
                "SELECT COUNT(*) FROM upstream_identities WHERE id = ?1",
                [existing],
                |row| row.get(0),
            )?;
            if exists == 0 && identity_override.is_none() {
                upsert_identity(
                    conn,
                    existing,
                    &identity.label,
                    identity.identity_confidence,
                    identity
                        .authority_ref
                        .as_ref()
                        .map(|a| a.issuer_or_site.as_str()),
                    identity.enabled,
                    identity.notes.as_deref(),
                    &now_rfc,
                )?;
            } else {
                anyhow::ensure!(exists == 1, "identity {existing} not found");
            }
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
    let existing_credential_id: String = conn.query_row(
        "SELECT credential_id FROM credential_state WHERE account_id = ?1",
        [&account.id],
        |row| row.get(0),
    )?;
    let existing_binding_id: Option<String> = conn
        .query_row(
            "SELECT id FROM credential_bindings WHERE account_id = ?1",
            [&account.id],
            |row| row.get(0),
        )
        .optional()?;
    let model_scope = serde_json::to_string(&ModelScope::All)?;
    if existing_binding_id.is_none() {
        if binding_grant_columns_ready(conn)? {
            conn.execute(
                "INSERT OR IGNORE INTO credential_bindings (
                id, account_id, connection_legacy_kind, connection_legacy_id,
                model_scope, enabled, created_at, updated_at,
                allowed_endpoint_ids, allowed_origins
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?7, NULL, NULL)",
                params![
                    binding.id.as_str(),
                    account.id,
                    legacy_kind.as_str(),
                    legacy_id,
                    model_scope,
                    DEFAULT_INFERENCE_BINDING_ENABLED as i32,
                    now_rfc,
                ],
            )?;
        } else {
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
                    DEFAULT_INFERENCE_BINDING_ENABLED as i32,
                    now_rfc,
                ],
            )?;
        }
    }
    fill_uninitialized_binding_grants_on(conn, Some(&account.id))?;
    let existing_binding_id = existing_binding_id.unwrap_or_else(|| binding.id.to_string());
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
        &existing_credential_id,
    )?;
    insert_legacy_map(
        conn,
        LEGACY_KIND_ACCOUNT,
        &account.id,
        NEW_KIND_BINDING,
        &existing_binding_id,
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

fn parse_stored_grant_list(raw: Option<&str>) -> Vec<String> {
    raw.and_then(|value| serde_json::from_str(value).ok())
        .unwrap_or_default()
}

fn binding_grant_columns_ready(conn: &Connection) -> Result<bool> {
    Ok(schema_version_on(conn)? >= BINDING_GRANT_VERSION
        && table_has_column(conn, "credential_bindings", "allowed_endpoint_ids")?
        && table_has_column(conn, "credential_bindings", "allowed_origins")?)
}

fn configured_endpoints_for_provider(
    conn: &Connection,
    provider_id: &str,
    account_id: &str,
) -> Result<Vec<AssignedEndpoint>> {
    if provider_id == CUSTOM_PROVIDER_ID {
        let connection_id =
            connection_id_for_legacy(LegacyConnectionKind::CustomAccount, account_id);
        let Some((url, protocol)) = conn
            .query_row(
                "SELECT endpoint_url, upstream_protocol FROM account_custom_configs WHERE account_id = ?1",
                [account_id],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
            )
            .optional()?
        else {
            return Ok(Vec::new());
        };
        let Ok(kind) = ocg_domain::catalog::UpstreamProtocolKind::try_from(protocol.as_str())
        else {
            return Ok(Vec::new());
        };
        return Ok(assigned_endpoints_for_routes(
            &connection_id,
            &[RouteSpec {
                operation: EndpointOperation::from(kind),
                url: Some(url),
            }],
        ));
    }
    if let Some(plan) = builtin_provider(provider_id) {
        let connection_id =
            connection_id_for_legacy(LegacyConnectionKind::BuiltinProvider, plan.provider_id);
        let routes: Vec<RouteSpec> = plan
            .upstream_protocols
            .iter()
            .copied()
            .map(|protocol| RouteSpec {
                operation: EndpointOperation::from(protocol),
                url: None,
            })
            .collect();
        return Ok(assigned_endpoints_for_routes(&connection_id, &routes));
    }
    let Some((endpoint_url, protocol)) = conn
        .query_row(
            "SELECT endpoint_url, upstream_protocol FROM providers
             WHERE id = ?1 AND origin IN ('preset', 'custom')",
            [provider_id],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
        )
        .optional()?
    else {
        return Ok(Vec::new());
    };
    let Ok(kind) = ocg_domain::catalog::UpstreamProtocolKind::try_from(protocol.as_str()) else {
        return Ok(Vec::new());
    };
    let connection_id =
        connection_id_for_legacy(LegacyConnectionKind::DynamicProvider, provider_id);
    let mut routes = vec![RouteSpec {
        operation: EndpointOperation::from(kind),
        url: Some(endpoint_url.clone()),
    }];
    let mut seen = std::collections::HashSet::from([(protocol.clone(), endpoint_url)]);
    if table_exists(conn, "provider_models")? {
        let mut stmt = conn.prepare(
            "SELECT upstream_override FROM provider_models
             WHERE provider_id = ?1 AND upstream_override IS NOT NULL
             ORDER BY public_model_key ASC",
        )?;
        let rows = stmt
            .query_map([provider_id], |row| row.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        drop(stmt);
        for raw in rows {
            let Ok(override_route) = serde_json::from_str::<DynamicModelUpstreamOverride>(&raw)
            else {
                continue;
            };
            if !seen.insert((
                override_route.protocol.as_str().to_string(),
                override_route.endpoint_url.clone(),
            )) {
                continue;
            }
            routes.push(RouteSpec {
                operation: EndpointOperation::from(override_route.protocol),
                url: Some(override_route.endpoint_url),
            });
        }
    }
    Ok(assigned_endpoints_for_routes(&connection_id, &routes))
}

pub(crate) fn fill_uninitialized_binding_grants_on(
    conn: &Connection,
    account_id: Option<&str>,
) -> Result<()> {
    if !binding_grant_columns_ready(conn)? {
        return Ok(());
    }
    let mut sql = String::from(
        "SELECT b.account_id, a.provider_id
         FROM credential_bindings b
         JOIN accounts a ON a.id = b.account_id
         WHERE b.allowed_endpoint_ids IS NULL OR b.allowed_origins IS NULL",
    );
    if account_id.is_some() {
        sql.push_str(" AND b.account_id = ?1");
    }
    let mut stmt = conn.prepare(&sql)?;
    let rows = if let Some(account_id) = account_id {
        stmt.query_map([account_id], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?
    } else {
        stmt.query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?
    };
    drop(stmt);
    for (account_id, provider_id) in rows {
        if provider_id == CUSTOM_PROVIDER_ID {
            if !table_exists(conn, "account_custom_configs")? {
                continue;
            }
            let has_config: bool = conn.query_row(
                "SELECT EXISTS(SELECT 1 FROM account_custom_configs WHERE account_id = ?1)",
                [&account_id],
                |row| row.get(0),
            )?;
            if !has_config {
                continue;
            }
        }
        let endpoints = configured_endpoints_for_provider(conn, &provider_id, &account_id)?;
        let (ids, origins) = safe_default_grants(&endpoints);
        conn.execute(
            "UPDATE credential_bindings
             SET allowed_endpoint_ids = ?2, allowed_origins = ?3
             WHERE account_id = ?1
               AND (allowed_endpoint_ids IS NULL OR allowed_origins IS NULL)",
            params![
                account_id,
                serde_json::to_string(&ids)?,
                serde_json::to_string(&origins)?,
            ],
        )?;
    }
    Ok(())
}

fn ensure_identity_quota_pool(
    conn: &Connection,
    identity_id: &str,
    account_id: &str,
    now_rfc: &str,
) -> Result<()> {
    let has_membership: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM quota_pool_members WHERE account_id = ?1)",
        [account_id],
        |row| row.get(0),
    )?;
    if has_membership {
        return Ok(());
    }
    let other_members: i64 = conn.query_row(
        "SELECT COUNT(*) FROM accounts WHERE identity_id = ?1 AND id <> ?2",
        params![identity_id, account_id],
        |row| row.get(0),
    )?;
    if other_members > 0 {
        return Ok(());
    }
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
    preserve_maxima: bool,
) -> Result<()> {
    if !identity_model_writable(conn)? {
        return Ok(());
    }
    if !table_exists(conn, "quota_pool_members")? {
        return Ok(());
    }
    if preserve_maxima {
        let mut stmt =
            conn.prepare("SELECT DISTINCT pool_id FROM quota_pool_members WHERE account_id = ?1")?;
        let pools = stmt
            .query_map([source_account_id], |row| row.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        drop(stmt);
        for pool_id in pools {
            merge_pool_cooldown_maxima(conn, &pool_id)?;
        }
    }
    let now_rfc = Utc::now().to_rfc3339();
    let siblings = shared_pool_siblings_on(conn, source_account_id)?;
    for sibling in siblings {
        if preserve_maxima {
            conn.execute(
                "UPDATE accounts SET
                    last_error = (SELECT last_error FROM accounts WHERE id = ?1),
                    updated_at = ?3
                 WHERE id = ?2",
                params![source_account_id, sibling, now_rfc],
            )?;
            continue;
        }
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

/// Identity row columns plus shared created/updated timestamp in one SQL write.
#[allow(clippy::too_many_arguments)]
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
    migrate_v45_body(&tx)?;
    tx.execute_batch("INSERT OR REPLACE INTO schema_version(version) VALUES (45);")?;
    tx.commit()?;
    Ok(())
}

pub(crate) fn migrate_to_v46(conn: &Connection) -> Result<()> {
    if schema_version_on(conn)? >= BINDING_GRANT_VERSION {
        return Ok(());
    }
    let tx = Transaction::new_unchecked(conn, TransactionBehavior::Immediate)?;
    let version = schema_version_on(&tx)?;
    if version >= BINDING_GRANT_VERSION {
        return Ok(());
    }
    anyhow::ensure!(version == 45, "v46 requires schema v45");
    anyhow::ensure!(
        table_exists(&tx, "credential_bindings")?,
        "v46 requires credential_bindings"
    );
    ensure_column(&tx, "credential_bindings", "allowed_endpoint_ids", "TEXT")?;
    ensure_column(&tx, "credential_bindings", "allowed_origins", "TEXT")?;
    tx.execute_batch("INSERT OR REPLACE INTO schema_version(version) VALUES (46);")?;
    fill_uninitialized_binding_grants_on(&tx, None)?;
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
    Ok(missing_identity + missing_credential + missing_binding)
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
    fill_uninitialized_binding_grants_on(conn, None)?;
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

pub(crate) fn migrate_v45_body(tx: &Transaction<'_>) -> Result<()> {
    create_identity_tables(tx)?;
    let now = Utc::now();
    let rows = {
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
        allowed_endpoint_ids: Option<&[String]>,
        allowed_origins: Option<&[String]>,
    ) -> Result<StoredInferenceBinding> {
        update_credential_binding_on(
            &self.conn,
            binding_id,
            model_scope,
            enabled,
            allowed_endpoint_ids,
            allowed_origins,
        )
    }

    pub fn create_account_for_identity(
        &self,
        identity_id: &str,
        account: &Account,
        purchase_date: &str,
        verification_status: ConnectionVerificationStatus,
        quota_sharing: QuotaSharingJoin,
        operation: Option<(&str, &str)>,
    ) -> Result<CreatedIdentityCredential> {
        create_account_for_identity_on(
            &self.conn,
            identity_id,
            account,
            purchase_date,
            verification_status,
            quota_sharing,
            operation,
        )
    }

    pub fn list_quota_pools(&self) -> Result<Vec<ImportedQuotaPool>> {
        list_quota_pools_on(&self.conn)
    }

    pub fn identity_import_conflict(
        &self,
        snapshot: &IdentityImportSnapshot,
        imported_account_ids: &HashSet<String>,
    ) -> Result<Option<String>> {
        identity_import_conflict_on(&self.conn, snapshot, imported_account_ids)
    }
}

fn rotate_account_credential_on(
    conn: &Connection,
    account_id: &str,
    key_cipher: &str,
) -> Result<RotatedCredential> {
    let tx = Transaction::new_unchecked(conn, TransactionBehavior::Immediate)?;
    let rotated = rotate_account_credential_in(&tx, account_id, key_cipher)?;
    tx.commit()?;
    Ok(rotated)
}

pub(crate) fn rotate_account_credential_in(
    conn: &Connection,
    account_id: &str,
    key_cipher: &str,
) -> Result<RotatedCredential> {
    if !identity_model_writable(conn)? {
        anyhow::bail!("identity model is not writable");
    }
    let credential_id = credential_id_for_legacy_account(account_id);
    let now_rfc = Utc::now().to_rfc3339();
    let provider_id: String = conn
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
    let account_updated = conn.execute(
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
    conn.execute(
        "INSERT OR IGNORE INTO credential_state (
            account_id, credential_id, version, auth_state_version, rotated_at
         ) VALUES (?1, ?2, 1, 1, NULL)",
        params![account_id, credential_id.as_str()],
    )?;
    let updated = conn.execute(
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
    invalidate_rotated_probe_evidence(conn, account_id, &provider_id)?;
    let (stored_id, version, auth_state_version): (String, i64, i64) = conn.query_row(
        "SELECT credential_id, version, auth_state_version
         FROM credential_state WHERE account_id = ?1",
        [account_id],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
    )?;
    Ok(RotatedCredential {
        account_id: account_id.to_string(),
        credential_id: stored_id,
        version: version as u64,
        auth_state_version: auth_state_version as u64,
    })
}

pub(crate) fn replace_binding_grants_for_account_on(
    conn: &Connection,
    account_id: &str,
    allowed_endpoint_ids: &[String],
    allowed_origins: &[String],
) -> Result<()> {
    if !binding_grant_columns_ready(conn)? {
        anyhow::bail!("binding grants are not writable");
    }
    let now_rfc = Utc::now().to_rfc3339();
    let updated = conn.execute(
        "UPDATE credential_bindings
         SET allowed_endpoint_ids = ?2, allowed_origins = ?3, updated_at = ?4
         WHERE account_id = ?1",
        params![
            account_id,
            serde_json::to_string(allowed_endpoint_ids)?,
            serde_json::to_string(allowed_origins)?,
            now_rfc,
        ],
    )?;
    anyhow::ensure!(updated == 1, "account {account_id} is missing a binding");
    Ok(())
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
    let grants_ready = binding_grant_columns_ready(conn)?;
    let sql = if grants_ready {
        "SELECT b.account_id, b.id, b.enabled, b.model_scope,
                b.allowed_endpoint_ids, b.allowed_origins, c.version
         FROM credential_bindings b
         LEFT JOIN credential_state c ON c.account_id = b.account_id"
    } else {
        "SELECT b.account_id, b.id, b.enabled, b.model_scope,
                NULL, NULL, c.version
         FROM credential_bindings b
         LEFT JOIN credential_state c ON c.account_id = b.account_id"
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
                row.get::<_, Option<i64>>(6)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows
        .into_iter()
        .map(
            |(
                account_id,
                binding_id,
                enabled,
                model_scope,
                allowed_endpoint_ids,
                allowed_origins,
                version,
            )| StoredInferenceBinding {
                account_id,
                binding_id,
                enabled: enabled != 0,
                model_scope: parse_stored_model_scope(&model_scope),
                allowed_endpoint_ids: parse_stored_grant_list(allowed_endpoint_ids.as_deref()),
                allowed_origins: parse_stored_grant_list(allowed_origins.as_deref()),
                credential_version: version.unwrap_or(1) as u64,
            },
        )
        .collect())
}

fn update_credential_binding_on(
    conn: &Connection,
    binding_id: &str,
    model_scope: Option<&ModelScope>,
    enabled: Option<bool>,
    allowed_endpoint_ids: Option<&[String]>,
    allowed_origins: Option<&[String]>,
) -> Result<StoredInferenceBinding> {
    if !identity_model_writable(conn)? {
        anyhow::bail!("identity model is not writable");
    }
    anyhow::ensure!(
        allowed_endpoint_ids.is_some() == allowed_origins.is_some(),
        "allowedEndpointIds and allowedOrigins must be set together"
    );
    let tx = Transaction::new_unchecked(conn, TransactionBehavior::Immediate)?;
    let grants_ready = binding_grant_columns_ready(&tx)?;
    type BindingGrantRow = (String, i32, String, Option<String>, Option<String>);
    let existing: Option<BindingGrantRow> = if grants_ready {
        tx.query_row(
            "SELECT account_id, enabled, model_scope, allowed_endpoint_ids, allowed_origins
             FROM credential_bindings WHERE id = ?1",
            [binding_id],
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
        .optional()?
    } else {
        tx.query_row(
            "SELECT account_id, enabled, model_scope FROM credential_bindings WHERE id = ?1",
            [binding_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, None, None)),
        )
        .optional()?
    };
    let Some((account_id, current_enabled, current_scope, current_ids, current_origins)) = existing
    else {
        anyhow::bail!("binding not found");
    };
    let next_scope = match model_scope {
        Some(scope) => serde_json::to_string(scope)?,
        None => current_scope,
    };
    let next_enabled = enabled.map(|value| value as i32).unwrap_or(current_enabled);
    let next_ids = match allowed_endpoint_ids {
        Some(ids) => serde_json::to_string(&ids)?,
        None => current_ids.unwrap_or_else(|| "[]".to_string()),
    };
    let next_origins = match allowed_origins {
        Some(origins) => serde_json::to_string(&origins)?,
        None => current_origins.unwrap_or_else(|| "[]".to_string()),
    };
    let now_rfc = Utc::now().to_rfc3339();
    if grants_ready {
        tx.execute(
            "UPDATE credential_bindings
             SET model_scope = ?2, enabled = ?3, updated_at = ?4,
                 allowed_endpoint_ids = ?5, allowed_origins = ?6
             WHERE id = ?1",
            params![
                binding_id,
                next_scope,
                next_enabled,
                now_rfc,
                next_ids,
                next_origins
            ],
        )?;
    } else {
        tx.execute(
            "UPDATE credential_bindings
             SET model_scope = ?2, enabled = ?3, updated_at = ?4
             WHERE id = ?1",
            params![binding_id, next_scope, next_enabled, now_rfc],
        )?;
    }
    let credential_version: i64 = tx
        .query_row(
            "SELECT version FROM credential_state WHERE account_id = ?1",
            [&account_id],
            |row| row.get(0),
        )
        .optional()?
        .unwrap_or(1);
    tx.commit()?;
    Ok(StoredInferenceBinding {
        account_id,
        binding_id: binding_id.to_string(),
        enabled: next_enabled != 0,
        model_scope: parse_stored_model_scope(&next_scope),
        allowed_endpoint_ids: parse_stored_grant_list(Some(&next_ids)),
        allowed_origins: parse_stored_grant_list(Some(&next_origins)),
        credential_version: credential_version as u64,
    })
}

fn create_account_for_identity_on(
    conn: &Connection,
    identity_id: &str,
    account: &Account,
    purchase_date: &str,
    verification_status: ConnectionVerificationStatus,
    quota_sharing: QuotaSharingJoin,
    operation: Option<(&str, &str)>,
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
    if let QuotaSharingJoin::Shared {
        source_credential_id,
    } = &quota_sharing
    {
        join_explicit_quota_share(&tx, identity_id, &account.id, source_credential_id)?;
    }
    let (credential_id, version, auth_state_version, binding_id): (String, i64, i64, String) = tx
        .query_row(
        "SELECT c.credential_id, c.version, c.auth_state_version, b.id
             FROM credential_state c
             JOIN credential_bindings b ON b.account_id = c.account_id
             WHERE c.account_id = ?1",
        [&account.id],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
    )?;
    if let Some((operation_id, digest)) = operation {
        let result_json = serde_json::json!({
            "identityId": identity_id,
            "credentialId": credential_id,
            "bindingId": binding_id,
            "accountId": account.id,
            "connectionId": connection_id_for_account(&account.provider_id, &account.id).to_string(),
            "version": version,
            "authStateVersion": auth_state_version,
        })
        .to_string();
        insert_dashboard_operation_on(
            &tx,
            &NewDashboardOperation {
                operation_id: operation_id.to_string(),
                kind: "identity_credential_create".to_string(),
                payload_digest: digest.to_string(),
                result_json,
            },
        )?;
    }
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

fn join_explicit_quota_share(
    conn: &Connection,
    identity_id: &str,
    new_account_id: &str,
    source_credential_id: &str,
) -> Result<()> {
    let source_account_id: String = conn
        .query_row(
            "SELECT c.account_id
             FROM credential_state c
             JOIN accounts a ON a.id = c.account_id
             WHERE c.credential_id = ?1 AND a.identity_id = ?2",
            params![source_credential_id, identity_id],
            |row| row.get(0),
        )
        .optional()?
        .ok_or_else(|| {
            anyhow::anyhow!("quota sharing requires an inference credential on the same identity")
        })?;
    anyhow::ensure!(
        source_account_id != new_account_id,
        "quota sharing cannot target the newly created credential"
    );
    let now_rfc = Utc::now().to_rfc3339();
    let existing_pool: Option<String> = conn
        .query_row(
            "SELECT pool_id FROM quota_pool_members WHERE account_id = ?1 LIMIT 1",
            [&source_account_id],
            |row| row.get(0),
        )
        .optional()?;
    let pool_id = match existing_pool {
        Some(id) => id,
        None => {
            let pool_id =
                quota_pool_id_for_accounts([&source_account_id, new_account_id]).to_string();
            conn.execute(
                "INSERT INTO quota_pools (
                    id, subject_kind, subject_ref, relation_confidence, policy_mode, created_at
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)
                 ON CONFLICT(id) DO NOTHING",
                params![
                    pool_id,
                    QuotaSubject::Credential.as_str(),
                    identity_id,
                    RelationConfidence::Declared.as_str(),
                    QuotaPolicyMode::AuthoritativeLimit.as_str(),
                    now_rfc,
                ],
            )?;
            conn.execute(
                "INSERT OR IGNORE INTO quota_pool_members (pool_id, account_id) VALUES (?1, ?2)",
                params![pool_id, source_account_id],
            )?;
            pool_id
        }
    };
    conn.execute(
        "INSERT OR IGNORE INTO quota_pool_members (pool_id, account_id) VALUES (?1, ?2)",
        params![pool_id, new_account_id],
    )?;
    let members: i64 = conn.query_row(
        "SELECT COUNT(*) FROM quota_pool_members WHERE pool_id = ?1",
        [&pool_id],
        |row| row.get(0),
    )?;
    if members > 1 {
        conn.execute(
            "UPDATE quota_pools SET relation_confidence = ?2 WHERE id = ?1",
            params![pool_id, RelationConfidence::Declared.as_str()],
        )?;
    }
    merge_pool_cooldown_maxima(conn, &pool_id)?;
    Ok(())
}

fn merge_pool_cooldown_maxima(conn: &Connection, pool_id: &str) -> Result<()> {
    type SharedPoolCooldownRow = (
        String,
        Option<String>,
        Option<String>,
        Option<String>,
        Option<String>,
        Option<String>,
        Option<String>,
    );
    let mut stmt = conn.prepare(
        "SELECT a.id, a.cooldown_until, a.cooldown_generic_until, a.cooldown_5h_until,
                a.cooldown_week_until, a.cooldown_month_until, a.cooldown_free_until
         FROM quota_pool_members m
         JOIN accounts a ON a.id = m.account_id
         WHERE m.pool_id = ?1",
    )?;
    let rows: Vec<SharedPoolCooldownRow> = stmt
        .query_map([pool_id], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, Option<String>>(1)?,
                row.get::<_, Option<String>>(2)?,
                row.get::<_, Option<String>>(3)?,
                row.get::<_, Option<String>>(4)?,
                row.get::<_, Option<String>>(5)?,
                row.get::<_, Option<String>>(6)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    drop(stmt);
    if rows.len() < 2 {
        return Ok(());
    }
    let max_of = |pick: fn(&SharedPoolCooldownRow) -> &Option<String>| {
        rows.iter()
            .filter_map(|row| pick(row).as_ref())
            .max()
            .cloned()
    };
    let until = max_of(|row| &row.1);
    let generic = max_of(|row| &row.2);
    let five = max_of(|row| &row.3);
    let week = max_of(|row| &row.4);
    let month = max_of(|row| &row.5);
    let free = max_of(|row| &row.6);
    let now_rfc = Utc::now().to_rfc3339();
    for (
        account_id,
        current_until,
        current_generic,
        current_five,
        current_week,
        current_month,
        current_free,
    ) in &rows
    {
        if current_until == &until
            && current_generic == &generic
            && current_five == &five
            && current_week == &week
            && current_month == &month
            && current_free == &free
        {
            continue;
        }
        conn.execute(
            "UPDATE accounts SET
                cooldown_until = ?2,
                cooldown_generic_until = ?3,
                cooldown_5h_until = ?4,
                cooldown_week_until = ?5,
                cooldown_month_until = ?6,
                cooldown_free_until = ?7,
                updated_at = ?8
             WHERE id = ?1",
            params![account_id, until, generic, five, week, month, free, now_rfc],
        )?;
    }
    Ok(())
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
    let grants_ready = binding_grant_columns_ready(conn)?;
    let bind_sql = if grants_ready {
        "SELECT account_id, id, enabled, model_scope, allowed_endpoint_ids, allowed_origins
         FROM credential_bindings"
    } else {
        "SELECT account_id, id, enabled, model_scope, NULL, NULL FROM credential_bindings"
    };
    let mut bind_stmt = conn.prepare(bind_sql)?;
    for row in bind_stmt.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, i32>(2)?,
            row.get::<_, String>(3)?,
            row.get::<_, Option<String>>(4)?,
            row.get::<_, Option<String>>(5)?,
        ))
    })? {
        let (account_id, binding_id, enabled, model_scope, ids, origins) = row?;
        binding_map.insert(
            account_id,
            (
                binding_id,
                enabled != 0,
                parse_stored_model_scope(&model_scope),
                parse_stored_grant_list(ids.as_deref()),
                parse_stored_grant_list(origins.as_deref()),
            ),
        );
    }
    drop(bind_stmt);

    let mut pool_by_account = std::collections::HashMap::new();
    if table_exists(conn, "quota_pool_members")? && table_exists(conn, "quota_pools")? {
        let mut pool_stmt = conn.prepare(
            "SELECT m.account_id, p.id, p.relation_confidence, p.policy_mode,
                    (SELECT COUNT(*) FROM quota_pool_members m2 WHERE m2.pool_id = p.id)
             FROM quota_pool_members m
             JOIN quota_pools p ON p.id = m.pool_id",
        )?;
        for row in pool_stmt.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, i64>(4)?,
            ))
        })? {
            let (account_id, pool_id, confidence, policy, members) = row?;
            let replace = match pool_by_account.get(&account_id) {
                Some((_, current_confidence, _, current_members)) => {
                    members > *current_members
                        || (members == *current_members
                            && confidence == RelationConfidence::Declared.as_str()
                            && *current_confidence != RelationConfidence::Declared.as_str())
                }
                None => true,
            };
            if replace {
                pool_by_account.insert(account_id, (pool_id, confidence, policy, members));
            }
        }
    }

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
        let (
            binding_id,
            binding_enabled,
            binding_model_scope,
            allowed_endpoint_ids,
            allowed_origins,
        ) = binding_map.get(&account.id).cloned().ok_or_else(|| {
            anyhow::anyhow!(
                "account {} is missing credential_bindings on schema v{version}",
                account.id
            )
        })?;
        let (quota_pool_id, quota_relation_confidence, quota_policy_mode) = pool_by_account
            .get(&account.id)
            .map(|(pool_id, confidence, policy, _)| {
                (
                    Some(pool_id.clone()),
                    Some(confidence.clone()),
                    Some(policy.clone()),
                )
            })
            .unwrap_or((None, None, None));
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
            allowed_endpoint_ids,
            allowed_origins,
            quota_pool_id,
            quota_relation_confidence,
            quota_policy_mode,
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

pub(crate) fn cooldown_facts_for(account: &Account, credential_id: &str) -> CooldownFacts {
    CooldownFacts {
        account_id: account.id.clone(),
        credential_id: credential_id.to_string(),
        generic: account.cooldown_generic_until,
        five_hours: account.cooldown_5h_until,
        week: account.cooldown_week_until,
        month: account.cooldown_month_until,
        free: account.cooldown_free_until,
    }
}

pub(crate) fn list_quota_pools_on(conn: &Connection) -> Result<Vec<ImportedQuotaPool>> {
    if !identity_model_writable(conn)? {
        return Ok(Vec::new());
    }
    let mut stmt = conn.prepare(
        "SELECT id, subject_kind, subject_ref, relation_confidence, policy_mode
         FROM quota_pools",
    )?;
    let pools = stmt
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
    let mut result = Vec::with_capacity(pools.len());
    for (id, subject_kind, subject_ref, relation_confidence, policy_mode) in pools {
        let mut member_stmt =
            conn.prepare("SELECT account_id FROM quota_pool_members WHERE pool_id = ?1")?;
        let member_account_ids = member_stmt
            .query_map([&id], |row| row.get(0))?
            .collect::<rusqlite::Result<Vec<String>>>()?;
        result.push(ImportedQuotaPool {
            id,
            subject_kind,
            subject_ref,
            relation_confidence,
            policy_mode,
            member_account_ids,
        });
    }
    Ok(result)
}

pub(crate) fn identity_import_conflict_on(
    conn: &Connection,
    snapshot: &IdentityImportSnapshot,
    imported_account_ids: &HashSet<String>,
) -> Result<Option<String>> {
    if !identity_model_writable(conn)? {
        return Ok(None);
    }
    for identity in &snapshot.identities {
        let mut stmt = conn.prepare("SELECT id FROM accounts WHERE identity_id = ?1")?;
        let owners = stmt
            .query_map([&identity.id], |row| row.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        if owners.iter().any(|id| !imported_account_ids.contains(id)) {
            return Ok(Some(format!(
                "imported identity {} is already attached to a destination-only account",
                identity.id
            )));
        }
    }
    for pool in &snapshot.quota_pools {
        let mut stmt =
            conn.prepare("SELECT account_id FROM quota_pool_members WHERE pool_id = ?1")?;
        let members = stmt
            .query_map([&pool.id], |row| row.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        if members.iter().any(|id| !imported_account_ids.contains(id)) {
            return Ok(Some(format!(
                "imported quota pool {} already has destination-only members",
                pool.id
            )));
        }
    }
    for row in &snapshot.accounts {
        let credential_owner: Option<String> = conn
            .query_row(
                "SELECT account_id FROM credential_state WHERE credential_id = ?1",
                [&row.credential_id],
                |row| row.get(0),
            )
            .optional()?;
        if let Some(owner) = credential_owner
            && !imported_account_ids.contains(&owner)
        {
            return Ok(Some(format!(
                "imported credential {} is already owned by destination account {owner}",
                row.credential_id
            )));
        }
        let binding_owner: Option<String> = conn
            .query_row(
                "SELECT account_id FROM credential_bindings WHERE id = ?1",
                [&row.binding_id],
                |row| row.get(0),
            )
            .optional()?;
        if let Some(owner) = binding_owner
            && !imported_account_ids.contains(&owner)
        {
            return Ok(Some(format!(
                "imported binding {} is already owned by destination account {owner}",
                row.binding_id
            )));
        }
    }
    Ok(None)
}

pub(crate) fn restore_imported_identity_snapshot_on(
    conn: &Connection,
    snapshot: &IdentityImportSnapshot,
    imported_account_ids: &HashSet<String>,
) -> Result<()> {
    if !identity_model_writable(conn)? {
        anyhow::bail!("identity model is not writable");
    }
    if let Some(conflict) = identity_import_conflict_on(conn, snapshot, imported_account_ids)? {
        anyhow::bail!("{conflict}");
    }
    let now_rfc = Utc::now().to_rfc3339();
    for identity in &snapshot.identities {
        let confidence = parse_portable_identity_confidence(&identity.identity_confidence)?;
        conn.execute(
            "INSERT INTO upstream_identities (
                id, label, identity_confidence, authority_site, authority_subject,
                enabled, notes, created_at, updated_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?8)
             ON CONFLICT(id) DO UPDATE SET
                label = excluded.label,
                identity_confidence = excluded.identity_confidence,
                authority_site = excluded.authority_site,
                authority_subject = excluded.authority_subject,
                enabled = excluded.enabled,
                notes = excluded.notes,
                updated_at = excluded.updated_at",
            params![
                identity.id,
                identity.label,
                confidence.as_str(),
                identity.authority_site,
                identity.authority_subject,
                identity.enabled as i32,
                identity.notes,
                now_rfc,
            ],
        )?;
    }
    for row in &snapshot.accounts {
        anyhow::ensure!(
            imported_account_ids.contains(&row.account_id),
            "identity snapshot references account {} which was not imported",
            row.account_id
        );
        let exists: i64 = conn.query_row(
            "SELECT COUNT(*) FROM upstream_identities WHERE id = ?1",
            [&row.identity_id],
            |row| row.get(0),
        )?;
        anyhow::ensure!(exists == 1, "identity {} not found", row.identity_id);
        let previous_identity: Option<String> = conn
            .query_row(
                "SELECT identity_id FROM accounts WHERE id = ?1",
                [&row.account_id],
                |row| row.get(0),
            )
            .optional()?
            .flatten();
        conn.execute(
            "UPDATE accounts SET identity_id = ?2 WHERE id = ?1",
            params![row.account_id, row.identity_id],
        )?;
        conn.execute(
            "INSERT INTO credential_state (
                account_id, credential_id, version, auth_state_version, rotated_at
             ) VALUES (?1, ?2, ?3, ?4, NULL)
             ON CONFLICT(account_id) DO UPDATE SET
                credential_id = excluded.credential_id,
                version = excluded.version,
                auth_state_version = excluded.auth_state_version",
            params![
                row.account_id,
                row.credential_id,
                row.credential_version as i64,
                row.auth_state_version as i64,
            ],
        )?;
        conn.execute(
            "DELETE FROM credential_bindings WHERE account_id = ?1",
            [&row.account_id],
        )?;
        let (legacy_kind, legacy_id) = {
            let provider_id: String = conn.query_row(
                "SELECT provider_id FROM accounts WHERE id = ?1",
                [&row.account_id],
                |row| row.get(0),
            )?;
            connection_legacy_for_account(&provider_id, &row.account_id)
        };
        let model_scope = serde_json::to_string(&row.binding_model_scope)?;
        let grant_ids = serde_json::to_string(&row.allowed_endpoint_ids)?;
        let grant_origins = serde_json::to_string(&row.allowed_origins)?;
        if binding_grant_columns_ready(conn)? {
            conn.execute(
                "INSERT INTO credential_bindings (
                    id, account_id, connection_legacy_kind, connection_legacy_id,
                    model_scope, enabled, created_at, updated_at,
                    allowed_endpoint_ids, allowed_origins
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?7, ?8, ?9)",
                params![
                    row.binding_id,
                    row.account_id,
                    legacy_kind.as_str(),
                    legacy_id,
                    model_scope,
                    row.binding_enabled as i32,
                    now_rfc,
                    grant_ids,
                    grant_origins,
                ],
            )?;
        } else {
            conn.execute(
                "INSERT INTO credential_bindings (
                    id, account_id, connection_legacy_kind, connection_legacy_id,
                    model_scope, enabled, created_at, updated_at
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?7)",
                params![
                    row.binding_id,
                    row.account_id,
                    legacy_kind.as_str(),
                    legacy_id,
                    model_scope,
                    row.binding_enabled as i32,
                    now_rfc,
                ],
            )?;
        }
        conn.execute(
            "DELETE FROM legacy_identity_map WHERE legacy_kind = ?1 AND legacy_id = ?2",
            params![LEGACY_KIND_ACCOUNT, row.account_id],
        )?;
        insert_legacy_map(
            conn,
            LEGACY_KIND_ACCOUNT,
            &row.account_id,
            NEW_KIND_IDENTITY,
            &row.identity_id,
        )?;
        insert_legacy_map(
            conn,
            LEGACY_KIND_ACCOUNT,
            &row.account_id,
            NEW_KIND_CREDENTIAL,
            &row.credential_id,
        )?;
        insert_legacy_map(
            conn,
            LEGACY_KIND_ACCOUNT,
            &row.account_id,
            NEW_KIND_BINDING,
            &row.binding_id,
        )?;
        conn.execute(
            "DELETE FROM quota_pool_members WHERE account_id = ?1",
            [&row.account_id],
        )?;
        if let Some(previous) = previous_identity {
            delete_orphan_identity(conn, &previous)?;
        }
    }
    for pool in &snapshot.quota_pools {
        let subject = parse_portable_quota_subject(&pool.subject_kind)?;
        let confidence = parse_portable_relation_confidence(&pool.relation_confidence)?;
        let policy = parse_portable_quota_policy(&pool.policy_mode)?;
        conn.execute(
            "INSERT INTO quota_pools (
                id, subject_kind, subject_ref, relation_confidence, policy_mode, created_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT(id) DO UPDATE SET
                subject_kind = excluded.subject_kind,
                subject_ref = excluded.subject_ref,
                relation_confidence = excluded.relation_confidence,
                policy_mode = excluded.policy_mode",
            params![
                pool.id,
                subject.as_str(),
                pool.subject_ref,
                confidence.as_str(),
                policy.as_str(),
                now_rfc,
            ],
        )?;
        for account_id in &pool.member_account_ids {
            anyhow::ensure!(
                imported_account_ids.contains(account_id),
                "quota pool {} references account {account_id} which was not imported",
                pool.id
            );
            conn.execute(
                "INSERT OR IGNORE INTO quota_pool_members (pool_id, account_id) VALUES (?1, ?2)",
                params![pool.id, account_id],
            )?;
        }
    }
    conn.execute(
        "DELETE FROM quota_pools
         WHERE NOT EXISTS (
            SELECT 1 FROM quota_pool_members m WHERE m.pool_id = quota_pools.id
         )",
        [],
    )?;
    Ok(())
}

fn parse_portable_identity_confidence(raw: &str) -> Result<IdentityConfidence> {
    match raw {
        "opaque" => Ok(IdentityConfidence::Opaque),
        "declared" => Ok(IdentityConfidence::Declared),
        "verified" => anyhow::bail!("portable identity confidence cannot be verified"),
        other => anyhow::bail!("unsupported identity confidence `{other}`"),
    }
}

fn parse_portable_quota_subject(raw: &str) -> Result<QuotaSubject> {
    match raw {
        "credential" => Ok(QuotaSubject::Credential),
        "egress" => Ok(QuotaSubject::Egress),
        other => anyhow::bail!("unsupported quota subject `{other}`"),
    }
}

fn parse_portable_relation_confidence(raw: &str) -> Result<RelationConfidence> {
    match raw {
        "unknown" => Ok(RelationConfidence::Unknown),
        "declared" => Ok(RelationConfidence::Declared),
        "verified" => anyhow::bail!("portable quota relation cannot be verified"),
        other => anyhow::bail!("unsupported quota relation confidence `{other}`"),
    }
}

fn parse_portable_quota_policy(raw: &str) -> Result<QuotaPolicyMode> {
    match raw {
        "observe_only" => Ok(QuotaPolicyMode::ObserveOnly),
        "authoritative_limit" => Ok(QuotaPolicyMode::AuthoritativeLimit),
        other => anyhow::bail!("unsupported quota policy `{other}`"),
    }
}
