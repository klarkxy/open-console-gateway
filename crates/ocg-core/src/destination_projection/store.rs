//! Schema v50 shadow of [`super::project`]. No Key material.

use std::collections::HashMap;

use anyhow::Context;
use chrono::{DateTime, Utc};
use ocg_domain::credential::{AuthState, ModelScope};
use ocg_domain::destination::{
    AdapterKind, AuthScheme, Capabilities, CatalogModel, Cooldowns, Credential, Destination,
    Grants, LegacyDestinationRef, ModelResolution, OnboardingTaskRef, Plan, Protocol,
};
use rusqlite::{Connection, params};

use crate::db::Database;

use super::DestinationProjection;

const GRANT_ENDPOINT: &str = "endpoint_id";
const GRANT_ORIGIN: &str = "origin";

/// Empty the four v50 tables. Caller owns the transaction.
pub(super) fn empty_all_on(conn: &Connection) -> anyhow::Result<()> {
    delete_shadow_rows(conn)
}

/// DELETE + INSERT the full projection. Caller owns the transaction.
pub(super) fn replace_all_on(
    conn: &Connection,
    projection: &DestinationProjection,
) -> anyhow::Result<()> {
    let extras = crate::db::account_store::snapshot_credential_extras(conn)?;
    let dest_extras = crate::db::platform::snapshot_destination_platform_extras(conn)?;
    let cpa_extras = crate::db::cpa::snapshot_destination_extras(conn)?;
    let dynamic_extras = crate::db::dynamic_store::snapshot_destination_extras(conn)?;
    let model_overrides = crate::db::dynamic_store::snapshot_model_overrides(conn)?;
    let observers = crate::db::platform::snapshot_observer_credentials(conn)?;
    delete_shadow_rows(conn)?;
    for destination in &projection.destinations {
        insert_destination(conn, destination)?;
    }
    for credential in &projection.credentials {
        insert_credential(conn, credential)?;
    }
    crate::db::account_store::restore_credential_extras(conn, &extras)?;
    crate::db::platform::restore_destination_platform_extras(conn, &dest_extras)?;
    crate::db::cpa::restore_destination_extras(conn, &cpa_extras)?;
    crate::db::dynamic_store::restore_destination_extras(conn, &dynamic_extras)?;
    crate::db::dynamic_store::restore_model_overrides(conn, &model_overrides)?;
    crate::db::platform::restore_observer_credentials(conn, &observers)?;
    Ok(())
}

pub(super) fn load_all(db: &Database) -> anyhow::Result<DestinationProjection> {
    let catalogs = load_catalogs(&db.conn)?;
    let grants = load_grants(&db.conn)?;
    let destinations = load_destinations(&db.conn, catalogs)?;
    let credentials = load_credentials(&db.conn, grants)?;
    Ok(DestinationProjection {
        destinations,
        credentials,
    })
}

fn delete_shadow_rows(conn: &Connection) -> anyhow::Result<()> {
    conn.execute_batch(
        "DELETE FROM credential_grants;
         DELETE FROM credentials;
         DELETE FROM destination_models;
         DELETE FROM destinations;",
    )?;
    Ok(())
}

fn insert_destination(conn: &Connection, destination: &Destination) -> anyhow::Result<()> {
    let (legacy_kind, legacy_id) = legacy_parts(&destination.legacy);
    conn.execute(
        "INSERT INTO destinations (
            id, legacy_kind, legacy_id, adapter, name, brand_family, base_url,
            protocols_json, auth_scheme, model_resolution, capabilities_json, plan_json,
            max_credentials, observer_credential_id, enabled
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)",
        params![
            destination.id,
            legacy_kind,
            legacy_id,
            destination.adapter.as_str(),
            destination.name,
            destination.brand_family,
            destination.base_url,
            serde_json::to_string(&destination.protocols)?,
            destination.auth_scheme.as_str(),
            destination.model_resolution.as_str(),
            serde_json::to_string(&destination.capabilities)?,
            destination
                .plan
                .as_ref()
                .map(serde_json::to_string)
                .transpose()?,
            destination.max_credentials.map(i64::from),
            destination.observer_credential_id,
            i64::from(destination.enabled),
        ],
    )?;
    for model in &destination.catalog {
        conn.execute(
            "INSERT INTO destination_models (
                destination_id, public_model, public_model_key, upstream_model,
                protocols_json, preferred, enabled
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                destination.id,
                model.public_model,
                model.public_model.to_ascii_lowercase(),
                model.upstream_model,
                serde_json::to_string(&model.protocols)?,
                model
                    .preferred
                    .map(|protocol| protocol.as_str().to_string()),
                i64::from(model.enabled),
            ],
        )?;
    }
    Ok(())
}

fn insert_credential(conn: &Connection, credential: &Credential) -> anyhow::Result<()> {
    conn.execute(
        "INSERT INTO credentials (
            id, legacy_account_id, destination_id, name, notes, has_secret,
            enabled, routing_rank, scope_json, auth_state, last_error,
            cooldown_generic_until, cooldown_5h_until, cooldown_week_until,
            cooldown_month_until, cooldown_free_until, quota_pool_id,
            onboarding_json, purchase_date
         ) VALUES (
            ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15,
            ?16, ?17, ?18, ?19
         )",
        params![
            credential.id,
            credential.legacy_account_id,
            credential.destination_id,
            credential.name,
            credential.notes,
            i64::from(credential.has_secret),
            i64::from(credential.enabled),
            i64::from(credential.routing_rank),
            serde_json::to_string(&credential.scope)?,
            credential.auth_state.as_str(),
            credential.last_error,
            credential
                .cooldowns
                .generic_until
                .map(|value| value.to_rfc3339()),
            credential
                .cooldowns
                .five_hour_until
                .map(|value| value.to_rfc3339()),
            credential
                .cooldowns
                .week_until
                .map(|value| value.to_rfc3339()),
            credential
                .cooldowns
                .month_until
                .map(|value| value.to_rfc3339()),
            credential
                .cooldowns
                .free_until
                .map(|value| value.to_rfc3339()),
            credential.quota_pool_id,
            credential
                .onboarding_task
                .as_ref()
                .map(serde_json::to_string)
                .transpose()?,
            credential.purchase_date,
        ],
    )?;
    if crate::db::credentials_store_secrets(conn) && crate::db::table_exists(conn, "accounts")? {
        conn.execute(
            "UPDATE credentials
             SET key_cipher = COALESCE(
                    (SELECT key_cipher FROM accounts WHERE id = ?1),
                    key_cipher
                 ),
                 password_cipher = COALESCE(
                    (SELECT password_cipher FROM accounts WHERE id = ?1),
                    password_cipher
                 )
             WHERE legacy_account_id = ?1
               AND EXISTS (SELECT 1 FROM accounts WHERE id = ?1)",
            params![credential.legacy_account_id],
        )?;
    }
    for value in &credential.grants.allowed_endpoint_ids {
        conn.execute(
            "INSERT INTO credential_grants (credential_id, kind, value) VALUES (?1, ?2, ?3)",
            params![credential.id, GRANT_ENDPOINT, value],
        )?;
    }
    for value in &credential.grants.allowed_origins {
        conn.execute(
            "INSERT INTO credential_grants (credential_id, kind, value) VALUES (?1, ?2, ?3)",
            params![credential.id, GRANT_ORIGIN, value],
        )?;
    }
    Ok(())
}

fn load_destinations(
    conn: &Connection,
    mut catalogs: HashMap<String, Vec<CatalogModel>>,
) -> anyhow::Result<Vec<Destination>> {
    let mut stmt = conn.prepare(
        "SELECT id, legacy_kind, legacy_id, adapter, name, brand_family, base_url,
                protocols_json, auth_scheme, model_resolution, capabilities_json, plan_json,
                max_credentials, observer_credential_id, enabled
         FROM destinations
         ORDER BY rowid",
    )?;
    let rows = stmt.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, String>(3)?,
            row.get::<_, String>(4)?,
            row.get::<_, Option<String>>(5)?,
            row.get::<_, Option<String>>(6)?,
            row.get::<_, String>(7)?,
            row.get::<_, String>(8)?,
            row.get::<_, String>(9)?,
            row.get::<_, String>(10)?,
            row.get::<_, Option<String>>(11)?,
            row.get::<_, Option<i64>>(12)?,
            row.get::<_, Option<String>>(13)?,
            row.get::<_, i64>(14)?,
        ))
    })?;
    let mut destinations = Vec::new();
    for row in rows {
        let (
            id,
            legacy_kind,
            legacy_id,
            adapter,
            name,
            brand_family,
            base_url,
            protocols_json,
            auth_scheme,
            model_resolution,
            capabilities_json,
            plan_json,
            max_credentials,
            observer_credential_id,
            enabled,
        ) = row?;
        let catalog = catalogs.remove(&id).unwrap_or_default();
        destinations.push(Destination {
            id,
            legacy: legacy_ref(&legacy_kind, legacy_id)?,
            adapter: adapter_from_str(&adapter)?,
            name,
            brand_family,
            base_url,
            protocols: serde_json::from_str(&protocols_json)
                .with_context(|| "invalid destinations.protocols_json")?,
            auth_scheme: auth_scheme_from_str(&auth_scheme)?,
            model_resolution: model_resolution_from_str(&model_resolution)?,
            catalog,
            capabilities: serde_json::from_str::<Capabilities>(&capabilities_json)
                .with_context(|| "invalid destinations.capabilities_json")?,
            plan: plan_json
                .as_deref()
                .map(serde_json::from_str::<Plan>)
                .transpose()
                .with_context(|| "invalid destinations.plan_json")?,
            max_credentials: max_credentials
                .map(|value| u32::try_from(value).context("invalid destinations.max_credentials"))
                .transpose()?,
            observer_credential_id,
            enabled: enabled != 0,
        });
    }
    Ok(destinations)
}

fn model_resolution_from_str(value: &str) -> anyhow::Result<ModelResolution> {
    match value {
        "adapter_defined" => Ok(ModelResolution::AdapterDefined),
        "public_only" => Ok(ModelResolution::PublicOnly),
        "public_and_upstream" => Ok(ModelResolution::PublicAndUpstream),
        other => anyhow::bail!("unknown destinations.model_resolution `{other}`"),
    }
}

fn load_credentials(
    conn: &Connection,
    mut grants: HashMap<String, Grants>,
) -> anyhow::Result<Vec<Credential>> {
    let purpose_filter = if crate::db::table_has_column(conn, "credentials", "credential_purpose")?
    {
        "WHERE COALESCE(credential_purpose, 'inference') = 'inference'"
    } else {
        ""
    };
    let mut stmt = conn.prepare(&format!(
        "SELECT id, legacy_account_id, destination_id, name, notes, has_secret,
                enabled, routing_rank, scope_json, auth_state, last_error,
                cooldown_generic_until, cooldown_5h_until, cooldown_week_until,
                cooldown_month_until, cooldown_free_until, quota_pool_id,
                onboarding_json, purchase_date
         FROM credentials
         {purpose_filter}
         ORDER BY routing_rank ASC, created_at ASC, legacy_account_id ASC, id ASC"
    ))?;
    let rows = stmt.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, String>(3)?,
            row.get::<_, Option<String>>(4)?,
            row.get::<_, i64>(5)?,
            row.get::<_, i64>(6)?,
            row.get::<_, i64>(7)?,
            row.get::<_, String>(8)?,
            row.get::<_, String>(9)?,
            row.get::<_, Option<String>>(10)?,
            row.get::<_, Option<String>>(11)?,
            row.get::<_, Option<String>>(12)?,
            row.get::<_, Option<String>>(13)?,
            row.get::<_, Option<String>>(14)?,
            row.get::<_, Option<String>>(15)?,
            row.get::<_, Option<String>>(16)?,
            row.get::<_, Option<String>>(17)?,
            row.get::<_, Option<String>>(18)?,
        ))
    })?;
    let mut credentials = Vec::new();
    for row in rows {
        let (
            id,
            legacy_account_id,
            destination_id,
            name,
            notes,
            has_secret,
            enabled,
            routing_rank,
            scope_json,
            auth_state,
            last_error,
            cooldown_generic_until,
            cooldown_5h_until,
            cooldown_week_until,
            cooldown_month_until,
            cooldown_free_until,
            quota_pool_id,
            onboarding_json,
            purchase_date,
        ) = row?;
        let grant_set = grants.remove(&id).unwrap_or(Grants {
            allowed_endpoint_ids: Vec::new(),
            allowed_origins: Vec::new(),
        });
        credentials.push(Credential {
            id,
            legacy_account_id,
            destination_id,
            name,
            notes,
            has_secret: has_secret != 0,
            enabled: enabled != 0,
            routing_rank: u32::try_from(routing_rank)
                .context("invalid credentials.routing_rank")?,
            scope: serde_json::from_str::<ModelScope>(&scope_json)
                .with_context(|| "invalid credentials.scope_json")?,
            grants: grant_set,
            auth_state: auth_state_from_str(&auth_state)?,
            last_error,
            cooldowns: Cooldowns {
                generic_until: parse_rfc3339_opt(cooldown_generic_until)?,
                five_hour_until: parse_rfc3339_opt(cooldown_5h_until)?,
                week_until: parse_rfc3339_opt(cooldown_week_until)?,
                month_until: parse_rfc3339_opt(cooldown_month_until)?,
                free_until: parse_rfc3339_opt(cooldown_free_until)?,
            },
            quota_pool_id,
            onboarding_task: onboarding_json
                .as_deref()
                .map(serde_json::from_str::<OnboardingTaskRef>)
                .transpose()
                .with_context(|| "invalid credentials.onboarding_json")?,
            purchase_date,
        });
    }
    Ok(credentials)
}

fn load_catalogs(conn: &Connection) -> anyhow::Result<HashMap<String, Vec<CatalogModel>>> {
    let mut stmt = conn.prepare(
        "SELECT destination_id, public_model, upstream_model, protocols_json,
                preferred, enabled, upstream_override
         FROM destination_models
         ORDER BY rowid",
    )?;
    let rows = stmt.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, String>(3)?,
            row.get::<_, Option<String>>(4)?,
            row.get::<_, i64>(5)?,
            row.get::<_, Option<String>>(6)?,
        ))
    })?;
    let mut catalogs = HashMap::new();
    for row in rows {
        let (
            destination_id,
            public_model,
            upstream_model,
            protocols_json,
            preferred,
            enabled,
            upstream_override,
        ) = row?;
        catalogs
            .entry(destination_id)
            .or_insert_with(Vec::new)
            .push(CatalogModel {
                public_model,
                upstream_model,
                protocols: serde_json::from_str(&protocols_json)
                    .with_context(|| "invalid destination_models.protocols_json")?,
                preferred: preferred.as_deref().map(protocol_from_str).transpose()?,
                enabled: enabled != 0,
                upstream_override: upstream_override
                    .as_deref()
                    .map(serde_json::from_str)
                    .transpose()
                    .with_context(|| "invalid destination_models.upstream_override")?,
            });
    }
    Ok(catalogs)
}

fn load_grants(conn: &Connection) -> anyhow::Result<HashMap<String, Grants>> {
    let mut stmt =
        conn.prepare("SELECT credential_id, kind, value FROM credential_grants ORDER BY rowid")?;
    let rows = stmt.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
        ))
    })?;
    let mut grants = HashMap::new();
    for row in rows {
        let (credential_id, kind, value) = row?;
        let entry = grants.entry(credential_id).or_insert_with(|| Grants {
            allowed_endpoint_ids: Vec::new(),
            allowed_origins: Vec::new(),
        });
        match kind.as_str() {
            GRANT_ENDPOINT => entry.allowed_endpoint_ids.push(value),
            GRANT_ORIGIN => entry.allowed_origins.push(value),
            other => anyhow::bail!("unknown credential_grants.kind `{other}`"),
        }
    }
    Ok(grants)
}

fn legacy_parts(legacy: &LegacyDestinationRef) -> (&'static str, &str) {
    match legacy {
        LegacyDestinationRef::Builtin(id) => ("builtin", id.as_str()),
        LegacyDestinationRef::Dynamic(id) => ("dynamic", id.as_str()),
        LegacyDestinationRef::CustomAccount(id) => ("custom_account", id.as_str()),
        LegacyDestinationRef::PlatformParent(id) => ("platform_parent", id.as_str()),
    }
}

fn legacy_ref(kind: &str, id: String) -> anyhow::Result<LegacyDestinationRef> {
    Ok(match kind {
        "builtin" => LegacyDestinationRef::Builtin(id),
        "dynamic" => LegacyDestinationRef::Dynamic(id),
        "custom_account" => LegacyDestinationRef::CustomAccount(id),
        "platform_parent" => LegacyDestinationRef::PlatformParent(id),
        other => anyhow::bail!("unknown destinations.legacy_kind `{other}`"),
    })
}

fn adapter_from_str(value: &str) -> anyhow::Result<AdapterKind> {
    AdapterKind::ALL
        .into_iter()
        .find(|kind| kind.as_str() == value)
        .ok_or_else(|| anyhow::anyhow!("unknown destinations.adapter `{value}`"))
}

fn auth_scheme_from_str(value: &str) -> anyhow::Result<AuthScheme> {
    match value {
        "none" => Ok(AuthScheme::None),
        "bearer" => Ok(AuthScheme::Bearer),
        "x_api_key" => Ok(AuthScheme::XApiKey),
        other => anyhow::bail!("unknown destinations.auth_scheme `{other}`"),
    }
}

fn auth_state_from_str(value: &str) -> anyhow::Result<AuthState> {
    match value {
        "unknown" => Ok(AuthState::Unknown),
        "valid" => Ok(AuthState::Valid),
        "invalid" => Ok(AuthState::Invalid),
        other => anyhow::bail!("unknown credentials.auth_state `{other}`"),
    }
}

fn protocol_from_str(value: &str) -> anyhow::Result<Protocol> {
    Protocol::try_from(value)
        .map_err(|error| anyhow::anyhow!("invalid destination_models.preferred: {error}"))
}

fn parse_rfc3339_opt(value: Option<String>) -> anyhow::Result<Option<DateTime<Utc>>> {
    value
        .map(|text| {
            DateTime::parse_from_rfc3339(&text)
                .map(|parsed| parsed.with_timezone(&Utc))
                .map_err(|error| anyhow::anyhow!("invalid RFC3339 timestamp: {error}"))
        })
        .transpose()
}
