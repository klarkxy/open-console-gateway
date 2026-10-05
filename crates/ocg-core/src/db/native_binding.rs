//! Owned native CPA rows on the existing destination and credential tables.
//!
//! One builtin destination, legacy id `cpa-owned-native`, is not the remote
//! CPA singleton. Each native provider plus one relative path has a stable
//! account id. This module does not read token files or open a second database.

use super::insert_account_row_for_destination;
use crate::models::{Account, AccountSetupStep, AccountType};
use crate::provider::{CPA_PROVIDER_ID, ConnectionVerificationStatus};
use anyhow::{Result, anyhow};
use chrono::Utc;
use ocg_domain::catalog::{CredentialKind, QuotaScope};
use ocg_domain::connection::CONNECTION_ID_NAMESPACE;
use ocg_domain::credential::credential_id_for_legacy_account;
use ocg_domain::destination::{
    AdapterKind, AuthScheme, Destination, LegacyDestinationRef, ModelResolution,
    destination_id_for_builtin, sealed_capabilities,
};
use rusqlite::{Connection, OptionalExtension, params};
use uuid::Uuid;

pub(crate) const OWNED_NATIVE_LEGACY_ID: &str = "cpa-owned-native";

pub(crate) fn owned_destination_id() -> String {
    destination_id_for_builtin(OWNED_NATIVE_LEGACY_ID)
}

pub(crate) fn account_id_for(native_provider: &str, relative_path: &str) -> Result<String> {
    let provider = normalize_label(native_provider)?;
    let path = single_relative_path(relative_path)?;
    Ok(Uuid::new_v5(
        &CONNECTION_ID_NAMESPACE,
        format!("cpa-owned-native:{provider}:{path}").as_bytes(),
    )
    .to_string())
}

pub(crate) fn ensure_owned_destination(conn: &Connection) -> Result<String> {
    let destination_id = owned_destination_id();
    let exists: i64 = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM destinations WHERE id = ?1)",
        [&destination_id],
        |row| row.get(0),
    )?;
    if exists == 0 {
        let mut capabilities = sealed_capabilities(AdapterKind::Cpa);
        capabilities.external_integration = false;
        capabilities.observer = false;
        let destination = Destination {
            id: destination_id.clone(),
            legacy: LegacyDestinationRef::Builtin(OWNED_NATIVE_LEGACY_ID.to_string()),
            adapter: AdapterKind::Cpa,
            name: "Owned native CPA".to_string(),
            brand_family: Some("cpa".to_string()),
            base_url: None,
            protocols: Vec::new(),
            protocol_routes: Vec::new(),
            auth_scheme: AuthScheme::None,
            model_resolution: ModelResolution::AdapterDefined,
            catalog: Vec::new(),
            capabilities,
            plan: None,
            max_credentials: None,
            observer_credential_id: None,
            enabled: true,
        };
        super::destination_store::insert_destination_row(conn, &destination)?;
    }
    Ok(destination_id)
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct BoundNative {
    pub account_id: String,
    pub credential_id: String,
    pub binding_id: String,
    pub destination_id: String,
    pub credential_version: u64,
    pub auth_state_version: u64,
    pub created: bool,
}

/// Insert the deterministic account when it is missing.
/// An existing row keeps its rank, enabled flag, scope, and grants.
pub(crate) fn bind_native_account(
    conn: &Connection,
    native_provider: &str,
    relative_path: &str,
) -> Result<BoundNative> {
    let account_id = account_id_for(native_provider, relative_path)?;
    let destination_id = ensure_owned_destination(conn)?;
    let credential_id = credential_id_for_legacy_account(&account_id).to_string();
    let existing: Option<String> = conn
        .query_row(
            "SELECT id FROM credentials WHERE legacy_account_id = ?1",
            [&account_id],
            |row| row.get(0),
        )
        .optional()?;
    let created = existing.is_none();
    if created {
        let now = Utc::now();
        let account = Account {
            id: account_id.clone(),
            provider_id: CPA_PROVIDER_ID.to_string(),
            credential_kind: CredentialKind::None,
            quota_scope: QuotaScope::Key,
            name: format!("{native_provider}:{relative_path}"),
            username: None,
            password_cipher: None,
            key_cipher: String::new(),
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
            created_at: now,
            updated_at: now,
        };
        insert_account_row_for_destination(
            conn,
            &account,
            &destination_id,
            "",
            ConnectionVerificationStatus::NotRequired,
        )?;
        conn.execute(
            "UPDATE credentials
             SET credential_version = CASE
                     WHEN credential_version IS NULL OR credential_version = 0 THEN 1
                     ELSE credential_version
                 END,
                 auth_state_version = CASE
                     WHEN auth_state_version IS NULL OR auth_state_version = 0 THEN 1
                     ELSE auth_state_version
                 END,
                 provider_id = ?2,
                 credential_kind = 'none',
                 key_cipher = ''
             WHERE id = ?1",
            params![credential_id, CPA_PROVIDER_ID],
        )?;
        // Identity insert fills provider-default grants. A new native row stays
        // uninitialized so the first sealed write can install its pins once.
        // An existing row is not cleared.
        reset_new_native_grants(conn, &account_id, &credential_id)?;
    }
    load_bound(conn, &account_id, created)
}

fn reset_new_native_grants(conn: &Connection, account_id: &str, credential_id: &str) -> Result<()> {
    if super::table_exists(conn, "credential_bindings")? {
        if super::table_has_column(conn, "credential_bindings", "allowed_endpoint_ids")? {
            conn.execute(
                "UPDATE credential_bindings
                 SET allowed_endpoint_ids = NULL, allowed_origins = NULL
                 WHERE account_id = ?1",
                [account_id],
            )?;
        }
        return Ok(());
    }
    if !super::table_exists(conn, "credential_grants")? {
        return Ok(());
    }
    conn.execute(
        "DELETE FROM credential_grants WHERE credential_id = ?1",
        [credential_id],
    )?;
    if super::table_has_column(conn, "credentials", "grants_initialized")? {
        conn.execute(
            "UPDATE credentials SET grants_initialized = 0 WHERE legacy_account_id = ?1",
            [account_id],
        )?;
    }
    Ok(())
}

pub(crate) fn write_initial_grants(
    conn: &Connection,
    account_id: &str,
    endpoint_ids: &[String],
    origins: &[String],
) -> Result<bool> {
    if endpoint_ids.is_empty() || origins.is_empty() {
        return Ok(false);
    }
    if super::table_exists(conn, "credential_bindings")? {
        return write_initial_binding_columns(conn, account_id, endpoint_ids, origins);
    }
    write_initial_credential_grants(conn, account_id, endpoint_ids, origins)
}

fn write_initial_binding_columns(
    conn: &Connection,
    account_id: &str,
    endpoint_ids: &[String],
    origins: &[String],
) -> Result<bool> {
    let current: Option<(Option<String>, Option<String>)> = conn
        .query_row(
            "SELECT allowed_endpoint_ids, allowed_origins
             FROM credential_bindings WHERE account_id = ?1",
            [account_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    let Some((ids, origins_stored)) = current else {
        return Ok(false);
    };
    if !grant_column_empty(ids.as_deref()) || !grant_column_empty(origins_stored.as_deref()) {
        return Ok(false);
    }
    let updated = conn.execute(
        "UPDATE credential_bindings
         SET allowed_endpoint_ids = ?2, allowed_origins = ?3
         WHERE account_id = ?1",
        params![
            account_id,
            serde_json::to_string(endpoint_ids)?,
            serde_json::to_string(origins)?,
        ],
    )?;
    Ok(updated == 1)
}

/// v57 stores grants on `credential_grants`. An initialized empty set stays
/// empty. A new credential with no rows receives the supplied pins once.
fn write_initial_credential_grants(
    conn: &Connection,
    account_id: &str,
    endpoint_ids: &[String],
    origins: &[String],
) -> Result<bool> {
    let credential_id = credential_id_for_legacy_account(account_id).to_string();
    let initialized: Option<i64> = conn
        .query_row(
            "SELECT COALESCE(grants_initialized, 0) FROM credentials WHERE legacy_account_id = ?1",
            [account_id],
            |row| row.get(0),
        )
        .optional()?;
    let Some(initialized) = initialized else {
        return Ok(false);
    };
    let (stored_ids, stored_origins) =
        super::identity::load_credential_grants(conn, &credential_id)?;
    if initialized != 0 || !stored_ids.is_empty() || !stored_origins.is_empty() {
        return Ok(false);
    }
    for value in endpoint_ids {
        conn.execute(
            "INSERT OR IGNORE INTO credential_grants (credential_id, kind, value)
             VALUES (?1, 'endpoint_id', ?2)",
            params![credential_id, value],
        )?;
    }
    for value in origins {
        conn.execute(
            "INSERT OR IGNORE INTO credential_grants (credential_id, kind, value)
             VALUES (?1, 'origin', ?2)",
            params![credential_id, value],
        )?;
    }
    let updated = conn.execute(
        "UPDATE credentials SET grants_initialized = 1 WHERE legacy_account_id = ?1",
        [account_id],
    )?;
    Ok(updated == 1)
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct NativeModelInsert {
    pub public_model: String,
    pub upstream_model: String,
    pub protocols: Vec<String>,
}

/// Insert destination models that are not already stored.
/// Existing rows keep their enabled flag. Missing models stay in history.
pub(crate) fn insert_native_models_if_new(
    conn: &Connection,
    destination_id: &str,
    models: &[NativeModelInsert],
) -> Result<usize> {
    if destination_id != owned_destination_id() {
        return Err(anyhow!("native models are destination scoped"));
    }
    let mut inserted = 0;
    for model in models {
        if model.public_model.trim().is_empty()
            || model.protocols.is_empty()
            || model
                .protocols
                .iter()
                .any(|protocol| protocol.trim().is_empty())
        {
            continue;
        }
        let preferred = model.protocols[0].clone();
        let key = model.public_model.to_ascii_lowercase();
        let exists: i64 = conn.query_row(
            "SELECT EXISTS(
                SELECT 1 FROM destination_models
                 WHERE destination_id = ?1 AND public_model_key = ?2
             )",
            params![destination_id, key],
            |row| row.get(0),
        )?;
        if exists != 0 {
            continue;
        }
        conn.execute(
            "INSERT INTO destination_models (
                destination_id, public_model, public_model_key, upstream_model,
                protocols_json, preferred, enabled, upstream_override
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, 1, NULL)",
            params![
                destination_id,
                model.public_model,
                key,
                model.upstream_model,
                serde_json::to_string(&model.protocols)?,
                preferred,
            ],
        )?;
        inserted += 1;
    }
    Ok(inserted)
}

/// Replace a stored `generate_content` protocol list with the three callable hops.
/// Mixed or user-edited protocol lists and the enabled flag stay as stored.
pub(crate) fn repair_retired_generate_content_protocols(
    conn: &Connection,
    destination_id: &str,
) -> Result<usize> {
    if destination_id != owned_destination_id() {
        return Err(anyhow!("native models are destination scoped"));
    }
    let mut statement = conn.prepare(
        "SELECT public_model_key, protocols_json, preferred
         FROM destination_models WHERE destination_id = ?1",
    )?;
    let rows = statement.query_map(params![destination_id], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
        ))
    })?;
    let mut retired = Vec::new();
    for row in rows {
        let (key, protocols_json, preferred) = row?;
        if retired_generate_content(&protocols_json, &preferred) {
            retired.push(key);
        }
    }
    drop(statement);
    let protocols = ["chat_completions", "responses", "messages"];
    let encoded = serde_json::to_string(&protocols)?;
    let mut updated = 0;
    for key in retired {
        updated += conn.execute(
            "UPDATE destination_models
             SET protocols_json = ?3, preferred = ?4
             WHERE destination_id = ?1 AND public_model_key = ?2",
            params![destination_id, key, encoded, protocols[0]],
        )?;
    }
    Ok(updated)
}

fn retired_generate_content(protocols_json: &str, preferred: &str) -> bool {
    let Ok(protocols) = serde_json::from_str::<Vec<String>>(protocols_json) else {
        return false;
    };
    if protocols.is_empty() {
        return preferred == "generate_content";
    }
    protocols
        .iter()
        .all(|protocol| protocol == "generate_content")
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct OwnedCredentialRow {
    pub credential_id: String,
    pub account_id: String,
    pub destination_id: String,
    pub binding_id: String,
    pub provider_id: String,
    pub credential_kind: String,
    pub key_empty: bool,
    pub credential_version: u64,
    pub auth_state_version: u64,
    pub enabled: bool,
    pub binding_enabled: bool,
    pub scope_json: String,
    pub destination_enabled: bool,
    pub routing_rank: u32,
    pub allowed_endpoint_ids: Vec<String>,
    pub allowed_origins: Vec<String>,
}

pub(crate) fn load_owned_credential(
    conn: &Connection,
    credential_id: &str,
) -> Result<Option<OwnedCredentialRow>> {
    let bindings = super::table_exists(conn, "credential_bindings")?;
    let sql = if bindings {
        "SELECT c.id, c.legacy_account_id, COALESCE(c.destination_id, ''),
                COALESCE(NULLIF(c.binding_id, ''), b.id, ''),
                COALESCE(c.provider_id, ''), COALESCE(c.credential_kind, ''),
                COALESCE(c.key_cipher, ''),
                COALESCE(c.credential_version, 0), COALESCE(c.auth_state_version, 0),
                COALESCE(c.enabled, 0), COALESCE(c.binding_enabled, 1),
                COALESCE(c.scope_json, ''),
                COALESCE(d.enabled, 0), COALESCE(c.routing_rank, 0),
                b.allowed_endpoint_ids, b.allowed_origins
         FROM credentials c
         LEFT JOIN destinations d ON d.id = c.destination_id
         LEFT JOIN credential_bindings b ON b.account_id = c.legacy_account_id
         WHERE c.id = ?1"
    } else {
        "SELECT c.id, c.legacy_account_id, COALESCE(c.destination_id, ''),
                COALESCE(c.binding_id, ''),
                COALESCE(c.provider_id, ''), COALESCE(c.credential_kind, ''),
                COALESCE(c.key_cipher, ''),
                COALESCE(c.credential_version, 0), COALESCE(c.auth_state_version, 0),
                COALESCE(c.enabled, 0), COALESCE(c.binding_enabled, 1),
                COALESCE(c.scope_json, ''),
                COALESCE(d.enabled, 0), COALESCE(c.routing_rank, 0),
                NULL, NULL
         FROM credentials c
         LEFT JOIN destinations d ON d.id = c.destination_id
         WHERE c.id = ?1"
    };
    let row = conn
        .query_row(sql, [credential_id], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, String>(5)?,
                row.get::<_, String>(6)?,
                row.get::<_, i64>(7)?,
                row.get::<_, i64>(8)?,
                row.get::<_, i64>(9)?,
                row.get::<_, i64>(10)?,
                row.get::<_, String>(11)?,
                row.get::<_, i64>(12)?,
                row.get::<_, i64>(13)?,
                row.get::<_, Option<String>>(14)?,
                row.get::<_, Option<String>>(15)?,
            ))
        })
        .optional()?;
    let Some((
        credential_id,
        account_id,
        destination_id,
        binding_id,
        provider_id,
        credential_kind,
        key_cipher,
        version,
        auth_version,
        enabled,
        binding_enabled,
        scope_json,
        destination_enabled,
        rank,
        endpoint_ids,
        origins,
    )) = row
    else {
        return Ok(None);
    };
    if version < 0 || auth_version < 0 || rank < 0 {
        return Err(anyhow!("native credential version is not usable"));
    }
    let (allowed_endpoint_ids, allowed_origins) = if bindings {
        (
            parse_string_list(endpoint_ids.as_deref()),
            parse_string_list(origins.as_deref()),
        )
    } else {
        super::identity::load_credential_grants(conn, &credential_id)?
    };
    Ok(Some(OwnedCredentialRow {
        credential_id,
        account_id,
        destination_id,
        binding_id,
        provider_id,
        credential_kind,
        key_empty: key_cipher.trim().is_empty(),
        credential_version: version as u64,
        auth_state_version: auth_version as u64,
        enabled: enabled != 0,
        binding_enabled: binding_enabled != 0,
        scope_json,
        destination_enabled: destination_enabled != 0,
        routing_rank: rank as u32,
        allowed_endpoint_ids,
        allowed_origins,
    }))
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct FenceVersions {
    pub credential_version: u64,
    pub auth_state_version: u64,
}

/// Bump credential and auth-state versions together. Does not restore presence.
pub(crate) fn bump_auth_fence(conn: &Connection, credential_id: &str) -> Result<FenceVersions> {
    let updated = conn.execute(
        "UPDATE credentials
         SET credential_version = COALESCE(credential_version, 1) + 1,
             auth_state_version = COALESCE(auth_state_version, 1) + 1,
             auth_state = 'unknown'
         WHERE id = ?1",
        [credential_id],
    )?;
    if updated != 1 {
        return Err(anyhow!("native credential fence did not match a row"));
    }
    if super::table_exists(conn, "credential_state")? {
        let account_id: Option<String> = conn
            .query_row(
                "SELECT legacy_account_id FROM credentials WHERE id = ?1",
                [credential_id],
                |row| row.get(0),
            )
            .optional()?;
        if let Some(account_id) = account_id {
            conn.execute(
                "UPDATE credential_state
                 SET version = version + 1,
                     auth_state_version = auth_state_version + 1
                 WHERE account_id = ?1",
                [account_id],
            )?;
        }
    }
    let (version, auth_version): (i64, i64) = conn.query_row(
        "SELECT COALESCE(credential_version, 0), COALESCE(auth_state_version, 0)
         FROM credentials WHERE id = ?1",
        [credential_id],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    if version <= 0 || auth_version <= 0 {
        return Err(anyhow!("native credential fence version is empty"));
    }
    Ok(FenceVersions {
        credential_version: version as u64,
        auth_state_version: auth_version as u64,
    })
}

/// Product enabled preference for one owned keyless credential.
/// Does not bump version, rank, scope, or grants. Returns false when the row
/// is not that owned destination credential.
pub(crate) fn set_owned_enabled(
    conn: &Connection,
    credential_id: &str,
    enabled: bool,
) -> Result<bool> {
    let updated = conn.execute(
        "UPDATE credentials
         SET enabled = ?2
         WHERE id = ?1
           AND destination_id = ?3
           AND provider_id = ?4
           AND credential_kind = 'none'
           AND TRIM(COALESCE(key_cipher, '')) = ''",
        params![
            credential_id,
            if enabled { 1 } else { 0 },
            owned_destination_id(),
            CPA_PROVIDER_ID,
        ],
    )?;
    Ok(updated == 1)
}

fn load_bound(conn: &Connection, account_id: &str, created: bool) -> Result<BoundNative> {
    let credential_id = credential_id_for_legacy_account(account_id).to_string();
    let row = load_owned_credential(conn, &credential_id)?
        .ok_or_else(|| anyhow!("native credential row is missing"))?;
    if row.account_id != account_id {
        return Err(anyhow!("native credential account id drifted"));
    }
    Ok(BoundNative {
        account_id: account_id.to_string(),
        credential_id: row.credential_id,
        binding_id: row.binding_id,
        destination_id: row.destination_id,
        credential_version: row.credential_version,
        auth_state_version: row.auth_state_version,
        created,
    })
}

fn normalize_label(value: &str) -> Result<String> {
    let label = value.trim();
    if !matches!(
        label,
        "codex" | "anthropic" | "antigravity" | "kimi" | "xai"
    ) {
        return Err(anyhow!("native provider is not a known executor"));
    }
    Ok(label.to_string())
}

pub(crate) fn single_relative_path(value: &str) -> Result<String> {
    let path = value.trim();
    if path.is_empty()
        || path.contains('/')
        || path.contains('\\')
        || path == "."
        || path == ".."
        || path.starts_with('.')
    {
        return Err(anyhow!("native auth path is not a single relative segment"));
    }
    Ok(path.to_string())
}

fn grant_column_empty(value: Option<&str>) -> bool {
    match value.map(str::trim).filter(|item| !item.is_empty()) {
        None => true,
        Some("[]") => true,
        Some(raw) => serde_json::from_str::<Vec<String>>(raw)
            .ok()
            .is_some_and(|items| items.is_empty()),
    }
}

fn parse_string_list(value: Option<&str>) -> Vec<String> {
    value
        .and_then(|raw| serde_json::from_str::<Vec<String>>(raw).ok())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests;
