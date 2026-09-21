//! User-defined / preset HTTP providers on destinations (schema v56+).
//!
//! After the leftover `providers` / `provider_models` drop, dynamic Provider
//! definitions reconstruct from destinations (`legacy_kind=dynamic`,
//! `legacy_id=provider.id`) and `destination_models`. Sealed builtin adapters
//! stay compiled-in (`BUILTIN_PROVIDERS`); this module never invents adapter
//! rows. V4 GET stays secret-free.

use super::*;
use chrono::{DateTime, Utc};
use ocg_domain::destination::{
    AdapterKind, AuthScheme, LegacyDestinationFacts, destination_from_legacy,
    destination_id_for_dynamic, sealed_capabilities,
};
use ocg_domain::dynamic::{DynamicAuthKind, DynamicModelMapping};
use ocg_domain::provider::{ProviderOrigin, builtin_offering, builtin_provider};
use rusqlite::OptionalExtension;

const CONFIGURABLE_HTTP: &str = "configurable_http";

#[derive(Debug, Clone)]
pub(crate) struct DynamicDestinationExtras {
    pub id: String,
    pub onboarding_draft: i32,
    pub preset_id: Option<String>,
    pub origin: Option<String>,
    pub offering: Option<String>,
    pub created_at: Option<String>,
    pub updated_at: Option<String>,
}

#[derive(Debug, Clone)]
pub(crate) struct DynamicModelOverrideExtra {
    pub destination_id: String,
    pub public_model_key: String,
    pub upstream_override: Option<String>,
}

/// v56: destinations + destination_models become the user-defined Provider store.
/// Backfill leftover `origin IN ('preset','custom')` rows when they can map,
/// refuse if a leftover keyed HTTP URL is empty or the adapter is unknown,
/// then `DROP TABLE provider_models` and `DROP TABLE providers`. Builtin seed
/// rows are not copied — they stay sealed catalog.
pub(crate) fn migrate_v56_backfill_and_drop(conn: &Connection) -> Result<()> {
    ensure_v56_columns(conn)?;
    if table_exists(conn, "providers")? {
        backfill_dynamic_from_leftover(conn)?;
    }
    conn.execute_batch(
        "DROP TABLE IF EXISTS provider_models;
         DROP TABLE IF EXISTS providers;",
    )?;
    Ok(())
}

pub(crate) fn ensure_v56_columns(conn: &Connection) -> Result<()> {
    if table_exists(conn, "destinations")? {
        if !table_has_column(conn, "destinations", "onboarding_draft")? {
            conn.execute_batch(
                "ALTER TABLE destinations ADD COLUMN onboarding_draft INTEGER NOT NULL DEFAULT 0",
            )?;
        }
        if !table_has_column(conn, "destinations", "preset_id")? {
            conn.execute_batch("ALTER TABLE destinations ADD COLUMN preset_id TEXT")?;
        }
        if !table_has_column(conn, "destinations", "origin")? {
            conn.execute_batch("ALTER TABLE destinations ADD COLUMN origin TEXT")?;
        }
        if !table_has_column(conn, "destinations", "offering")? {
            conn.execute_batch("ALTER TABLE destinations ADD COLUMN offering TEXT")?;
        }
        if !table_has_column(conn, "destinations", "created_at")? {
            conn.execute_batch("ALTER TABLE destinations ADD COLUMN created_at TEXT")?;
        }
        if !table_has_column(conn, "destinations", "updated_at")? {
            conn.execute_batch("ALTER TABLE destinations ADD COLUMN updated_at TEXT")?;
        }
    }
    if table_exists(conn, "destination_models")?
        && !table_has_column(conn, "destination_models", "upstream_override")?
    {
        conn.execute_batch("ALTER TABLE destination_models ADD COLUMN upstream_override TEXT")?;
    }
    Ok(())
}

pub(crate) fn list_dynamic_providers_on(conn: &Connection) -> Result<Vec<DynamicProviderRuntime>> {
    list_dynamic_providers_filtered_on(conn, false)
}

pub(crate) fn list_control_plane_dynamic_providers_on(
    conn: &Connection,
) -> Result<Vec<DynamicProviderRuntime>> {
    list_dynamic_providers_filtered_on(conn, true)
}

pub(crate) fn get_dynamic_provider_on(
    conn: &Connection,
    provider_id: &str,
) -> Result<Option<DynamicProviderRuntime>> {
    if leftover_providers_readable(conn)? {
        return leftover_get_dynamic_provider_on(conn, provider_id);
    }
    if !table_exists(conn, "destinations")? {
        return Ok(None);
    }
    let Some(row) = load_dynamic_row(conn, provider_id)? else {
        return Ok(None);
    };
    Ok(Some(runtime_from_row(conn, row)?))
}

pub(crate) fn get_provider_definition_on(
    conn: &Connection,
    provider_id: &str,
) -> Result<Option<DynamicProviderRuntime>> {
    if let Some(runtime) = builtin_runtime(provider_id) {
        return Ok(Some(runtime));
    }
    get_dynamic_provider_on(conn, provider_id)
}

pub(crate) fn onboarding_draft_provider_ids_on(conn: &Connection) -> Result<HashSet<String>> {
    if leftover_providers_readable(conn)?
        && table_has_column(conn, "providers", "onboarding_draft")?
    {
        let mut stmt = conn.prepare(
            "SELECT id FROM providers
             WHERE origin IN ('preset', 'custom') AND COALESCE(onboarding_draft, 0) != 0",
        )?;
        let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
        let mut ids = HashSet::new();
        for id in rows {
            ids.insert(id?);
        }
        return Ok(ids);
    }
    if !dynamic_columns_ready(conn)? {
        return Ok(HashSet::new());
    }
    let mut stmt = conn.prepare(
        "SELECT legacy_id FROM destinations
         WHERE legacy_kind = 'dynamic'
           AND COALESCE(onboarding_draft, 0) != 0",
    )?;
    let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
    let mut ids = HashSet::new();
    for id in rows {
        ids.insert(id?);
    }
    Ok(ids)
}

pub(crate) fn provider_is_onboarding_draft_on(
    conn: &Connection,
    provider_id: &str,
) -> Result<Option<bool>> {
    if leftover_providers_readable(conn)?
        && table_has_column(conn, "providers", "onboarding_draft")?
    {
        return conn
            .query_row(
                "SELECT COALESCE(onboarding_draft, 0) FROM providers
                 WHERE origin IN ('preset', 'custom') AND lower(id) = lower(?1)",
                [provider_id],
                |row| row.get::<_, i32>(0),
            )
            .optional()
            .map(|value| value.map(|flag| flag != 0))
            .map_err(Into::into);
    }
    if !dynamic_columns_ready(conn)? {
        return Ok(None);
    }
    conn.query_row(
        "SELECT COALESCE(onboarding_draft, 0) FROM destinations
         WHERE legacy_kind = 'dynamic' AND lower(legacy_id) = lower(?1)",
        [provider_id],
        |row| row.get::<_, i32>(0),
    )
    .optional()
    .map(|value| value.map(|flag| flag != 0))
    .map_err(Into::into)
}

pub(crate) fn insert_dynamic_provider_on(
    conn: &Connection,
    runtime: &DynamicProviderRuntime,
    onboarding_draft: bool,
) -> Result<()> {
    anyhow::ensure!(
        builtin_provider(&runtime.id).is_none(),
        "dynamic provider id collides with a built-in provider"
    );
    upsert_dynamic_destination_on(conn, runtime, Some(onboarding_draft))
}

pub(crate) fn replace_dynamic_provider_definition_on(
    conn: &Connection,
    runtime: &DynamicProviderRuntime,
    onboarding_draft: Option<bool>,
) -> Result<()> {
    let existing = get_dynamic_provider_on(conn, &runtime.id)?
        .ok_or_else(|| anyhow::anyhow!("unknown provider `{}`", runtime.id))?;
    let mut stored = runtime.clone();
    stored.id = existing.id;
    upsert_dynamic_destination_on(conn, &stored, onboarding_draft)
}

pub(crate) fn upsert_imported_dynamic_provider_on(
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
            let mut statement = conn.prepare(
                "SELECT legacy_account_id FROM credentials WHERE lower(provider_id) = lower(?1)",
            )?;
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
        let mut stored = runtime.clone();
        stored.id = existing.id;
        upsert_dynamic_destination_on(conn, &stored, Some(onboarding_draft))
    } else {
        insert_dynamic_provider_on(conn, runtime, onboarding_draft)
    }
}

pub(crate) fn delete_dynamic_provider_on(conn: &Connection, provider_id: &str) -> Result<()> {
    let dest_id = destination_id_for_dynamic(provider_id);
    conn.execute(
        "DELETE FROM destination_models WHERE destination_id = ?1",
        [&dest_id],
    )?;
    conn.execute(
        "DELETE FROM destinations
         WHERE id = ?1 OR (legacy_kind = 'dynamic' AND lower(legacy_id) = lower(?2))",
        params![dest_id, provider_id],
    )?;
    Ok(())
}

pub(crate) fn snapshot_destination_extras(
    conn: &Connection,
) -> Result<Vec<DynamicDestinationExtras>> {
    if !dynamic_columns_ready(conn)? {
        return Ok(Vec::new());
    }
    let mut stmt = conn.prepare(
        "SELECT id, COALESCE(onboarding_draft, 0), preset_id, origin, offering,
                created_at, updated_at
         FROM destinations
         WHERE legacy_kind = 'dynamic'",
    )?;
    let rows = stmt
        .query_map([], |row| {
            Ok(DynamicDestinationExtras {
                id: row.get(0)?,
                onboarding_draft: row.get(1)?,
                preset_id: row.get(2)?,
                origin: row.get(3)?,
                offering: row.get(4)?,
                created_at: row.get(5)?,
                updated_at: row.get(6)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

pub(crate) fn restore_destination_extras(
    conn: &Connection,
    extras: &[DynamicDestinationExtras],
) -> Result<()> {
    if extras.is_empty() || !dynamic_columns_ready(conn)? {
        return Ok(());
    }
    for extra in extras {
        conn.execute(
            "UPDATE destinations
             SET onboarding_draft = ?2, preset_id = ?3, origin = ?4, offering = ?5,
                 created_at = COALESCE(?6, created_at),
                 updated_at = COALESCE(?7, updated_at)
             WHERE id = ?1",
            params![
                extra.id,
                extra.onboarding_draft,
                extra.preset_id,
                extra.origin,
                extra.offering,
                extra.created_at,
                extra.updated_at,
            ],
        )?;
    }
    Ok(())
}

pub(crate) fn snapshot_model_overrides(
    conn: &Connection,
) -> Result<Vec<DynamicModelOverrideExtra>> {
    if !table_has_column(conn, "destination_models", "upstream_override")? {
        return Ok(Vec::new());
    }
    let mut stmt = conn.prepare(
        "SELECT destination_id, public_model_key, upstream_override
         FROM destination_models
         WHERE upstream_override IS NOT NULL",
    )?;
    let rows = stmt
        .query_map([], |row| {
            Ok(DynamicModelOverrideExtra {
                destination_id: row.get(0)?,
                public_model_key: row.get(1)?,
                upstream_override: row.get(2)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

pub(crate) fn restore_model_overrides(
    conn: &Connection,
    extras: &[DynamicModelOverrideExtra],
) -> Result<()> {
    if extras.is_empty() || !table_has_column(conn, "destination_models", "upstream_override")? {
        return Ok(());
    }
    for extra in extras {
        conn.execute(
            "UPDATE destination_models
             SET upstream_override = ?3
             WHERE destination_id = ?1 AND public_model_key = ?2",
            params![
                extra.destination_id,
                extra.public_model_key,
                extra.upstream_override,
            ],
        )?;
    }
    Ok(())
}

pub(crate) fn configured_dynamic_endpoint_on(
    conn: &Connection,
    provider_id: &str,
) -> Result<Option<(String, String)>> {
    if !table_exists(conn, "destinations")? {
        return Ok(None);
    }
    let row = conn
        .query_row(
            "SELECT base_url, protocols_json FROM destinations
             WHERE legacy_kind = 'dynamic' AND lower(legacy_id) = lower(?1)",
            [provider_id],
            |row| Ok((row.get::<_, Option<String>>(0)?, row.get::<_, String>(1)?)),
        )
        .optional()?;
    let Some((base_url, protocols_json)) = row else {
        return Ok(None);
    };
    let protocol = first_protocol(&protocols_json)?;
    Ok(Some((base_url.unwrap_or_default(), protocol)))
}

pub(crate) fn list_dynamic_model_overrides_on(
    conn: &Connection,
    provider_id: &str,
) -> Result<Vec<String>> {
    if !table_has_column(conn, "destination_models", "upstream_override")? {
        return Ok(Vec::new());
    }
    let dest_id = destination_id_for_dynamic(provider_id);
    let mut stmt = conn.prepare(
        "SELECT upstream_override FROM destination_models
         WHERE destination_id = ?1 AND upstream_override IS NOT NULL
         ORDER BY public_model_key ASC",
    )?;
    let rows = stmt
        .query_map([&dest_id], |row| row.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

pub(crate) fn dynamic_destination_present(conn: &Connection, provider_id: &str) -> Result<bool> {
    if !table_exists(conn, "destinations")? {
        return Ok(false);
    }
    Ok(conn.query_row(
        "SELECT EXISTS(
            SELECT 1 FROM destinations
             WHERE legacy_kind = 'dynamic' AND lower(legacy_id) = lower(?1)
         )",
        [provider_id],
        |row| row.get::<_, i64>(0),
    )? != 0)
}

fn leftover_providers_readable(conn: &Connection) -> Result<bool> {
    Ok(table_exists(conn, "providers")? && table_has_column(conn, "providers", "origin")?)
}

fn leftover_get_dynamic_provider_on(
    conn: &Connection,
    provider_id: &str,
) -> Result<Option<DynamicProviderRuntime>> {
    let row = conn
        .query_row(
            "SELECT id, name, COALESCE(endpoint_url, ''), COALESCE(upstream_protocol, ''),
                    COALESCE(auth_kind, ''), created_at, updated_at, preset_id, origin,
                    COALESCE(offering, 'api')
             FROM providers
             WHERE origin IN ('preset', 'custom') AND lower(id) = lower(?1)",
            [provider_id],
            leftover_row_from_sql,
        )
        .optional()?;
    row.map(|row| leftover_runtime(conn, row)).transpose()
}

fn leftover_list_dynamic_providers_on(
    conn: &Connection,
    include_drafts: bool,
) -> Result<Vec<DynamicProviderRuntime>> {
    let draft_sql = if include_drafts || !table_has_column(conn, "providers", "onboarding_draft")? {
        ""
    } else {
        " AND COALESCE(onboarding_draft, 0) = 0"
    };
    let mut stmt = conn.prepare(&format!(
        "SELECT id, name, COALESCE(endpoint_url, ''), COALESCE(upstream_protocol, ''),
                COALESCE(auth_kind, ''), created_at, updated_at, preset_id, origin,
                COALESCE(offering, 'api')
         FROM providers
         WHERE origin IN ('preset', 'custom'){draft_sql}
         ORDER BY created_at ASC, id ASC"
    ))?;
    let rows = stmt
        .query_map([], leftover_row_from_sql)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let mut providers = Vec::new();
    for row in rows {
        providers.push(leftover_runtime(conn, row)?);
    }
    Ok(providers)
}

fn leftover_row_from_sql(row: &rusqlite::Row<'_>) -> rusqlite::Result<DynamicProviderRow> {
    Ok(DynamicProviderRow {
        id: row.get(0)?,
        name: row.get(1)?,
        endpoint_url: row.get(2)?,
        protocol: row.get(3)?,
        auth_kind: row.get(4)?,
        created_at: row.get(5)?,
        updated_at: row.get(6)?,
        preset_id: row.get(7)?,
        origin: row.get(8)?,
        offering: row.get(9)?,
    })
}

fn leftover_runtime(
    conn: &Connection,
    provider: DynamicProviderRow,
) -> Result<DynamicProviderRuntime> {
    let protocol = ocg_domain::catalog::UpstreamProtocolKind::try_from(provider.protocol.as_str())
        .map_err(|error| anyhow::anyhow!(error.to_string()))?;
    let auth_kind = DynamicAuthKind::try_from(provider.auth_kind.as_str())
        .map_err(|error| anyhow::anyhow!(error.to_string()))?;
    let mappings = load_leftover_models(conn, &provider.id)?;
    let created_at = parse_timestamp(&provider.created_at, &provider.id, "created_at")?;
    let updated_at = parse_timestamp(&provider.updated_at, &provider.id, "updated_at")?;
    let origin = ProviderOrigin::try_from(provider.origin.as_str())
        .map_err(|error| anyhow::anyhow!(error.to_string()))?;
    Ok(DynamicProviderRuntime {
        preset_id: provider.preset_id,
        id: provider.id,
        name: provider.name,
        endpoint_url: provider.endpoint_url,
        upstream_protocol: protocol,
        auth_kind,
        mappings,
        created_at,
        updated_at,
        origin,
        offering: provider.offering,
    })
}

fn list_dynamic_providers_filtered_on(
    conn: &Connection,
    include_drafts: bool,
) -> Result<Vec<DynamicProviderRuntime>> {
    if leftover_providers_readable(conn)? {
        return leftover_list_dynamic_providers_on(conn, include_drafts);
    }
    if !table_exists(conn, "destinations")? {
        return Ok(Vec::new());
    }
    let sql = if include_drafts {
        "SELECT legacy_id, name, base_url, protocols_json, auth_scheme,
                COALESCE(created_at, ''), COALESCE(updated_at, ''),
                preset_id, COALESCE(origin, 'custom'), COALESCE(offering, 'api')
         FROM destinations
         WHERE legacy_kind = 'dynamic'
         ORDER BY COALESCE(created_at, '') ASC, legacy_id ASC"
    } else {
        "SELECT legacy_id, name, base_url, protocols_json, auth_scheme,
                COALESCE(created_at, ''), COALESCE(updated_at, ''),
                preset_id, COALESCE(origin, 'custom'), COALESCE(offering, 'api')
         FROM destinations
         WHERE legacy_kind = 'dynamic'
           AND COALESCE(onboarding_draft, 0) = 0
         ORDER BY COALESCE(created_at, '') ASC, legacy_id ASC"
    };
    let mut stmt = conn.prepare(sql)?;
    let rows = stmt
        .query_map([], |row| {
            Ok(DynamicProviderRow {
                id: row.get(0)?,
                name: row.get(1)?,
                endpoint_url: row.get::<_, Option<String>>(2)?.unwrap_or_default(),
                protocol: first_protocol_sql(row.get(3)?),
                auth_kind: row.get(4)?,
                created_at: row.get(5)?,
                updated_at: row.get(6)?,
                preset_id: row.get(7)?,
                origin: row.get(8)?,
                offering: row.get(9)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let mut providers = Vec::new();
    for row in rows {
        providers.push(runtime_from_row(conn, row)?);
    }
    Ok(providers)
}

fn load_dynamic_row(conn: &Connection, provider_id: &str) -> Result<Option<DynamicProviderRow>> {
    conn.query_row(
        "SELECT legacy_id, name, base_url, protocols_json, auth_scheme,
                COALESCE(created_at, ''), COALESCE(updated_at, ''),
                preset_id, COALESCE(origin, 'custom'), COALESCE(offering, 'api')
         FROM destinations
         WHERE legacy_kind = 'dynamic' AND lower(legacy_id) = lower(?1)",
        [provider_id],
        |row| {
            Ok(DynamicProviderRow {
                id: row.get(0)?,
                name: row.get(1)?,
                endpoint_url: row.get::<_, Option<String>>(2)?.unwrap_or_default(),
                protocol: first_protocol_sql(row.get(3)?),
                auth_kind: row.get(4)?,
                created_at: row.get(5)?,
                updated_at: row.get(6)?,
                preset_id: row.get(7)?,
                origin: row.get(8)?,
                offering: row.get(9)?,
            })
        },
    )
    .optional()
    .map_err(Into::into)
}

fn runtime_from_row(
    conn: &Connection,
    provider: DynamicProviderRow,
) -> Result<DynamicProviderRuntime> {
    let protocol = if provider.protocol.trim().is_empty() {
        ocg_domain::catalog::UpstreamProtocolKind::ChatCompletions
    } else {
        ocg_domain::catalog::UpstreamProtocolKind::try_from(provider.protocol.as_str())
            .map_err(|error| anyhow::anyhow!(error.to_string()))?
    };
    let auth_kind = DynamicAuthKind::try_from(provider.auth_kind.as_str())
        .map_err(|error| anyhow::anyhow!(error.to_string()))?;
    let dest_id = destination_id_for_dynamic(&provider.id);
    let mappings = load_mappings(conn, &dest_id)?;
    let created_at = parse_timestamp(&provider.created_at, &provider.id, "created_at")?;
    let updated_at = parse_timestamp(&provider.updated_at, &provider.id, "updated_at")?;
    let origin = ProviderOrigin::try_from(provider.origin.as_str())
        .map_err(|error| anyhow::anyhow!(error.to_string()))?;
    Ok(DynamicProviderRuntime {
        preset_id: provider.preset_id,
        id: provider.id,
        name: provider.name,
        endpoint_url: provider.endpoint_url,
        upstream_protocol: protocol,
        auth_kind,
        mappings,
        created_at,
        updated_at,
        origin,
        offering: provider.offering,
    })
}

fn load_mappings(conn: &Connection, destination_id: &str) -> Result<Vec<DynamicModelMapping>> {
    if !table_exists(conn, "destination_models")? {
        return Ok(Vec::new());
    }
    let override_sql = if table_has_column(conn, "destination_models", "upstream_override")? {
        "upstream_override"
    } else {
        "NULL"
    };
    let mut stmt = conn.prepare(&format!(
        "SELECT public_model, upstream_model, {override_sql}
         FROM destination_models
         WHERE destination_id = ?1
         ORDER BY public_model_key ASC"
    ))?;
    let rows = stmt.query_map([destination_id], |row| {
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
    Ok(mappings)
}

fn upsert_dynamic_destination_on(
    conn: &Connection,
    runtime: &DynamicProviderRuntime,
    onboarding_draft: Option<bool>,
) -> Result<()> {
    ensure_v56_columns(conn)?;
    let dest_id = destination_id_for_dynamic(&runtime.id);
    let origin = runtime.origin.as_str();
    let offering = runtime.offering.as_str();
    let protocols = serde_json::to_string(&[runtime.upstream_protocol])?;
    let auth_scheme = AuthScheme::from(runtime.auth_kind).as_str();
    let capabilities = serde_json::to_string(&sealed_capabilities(AdapterKind::Http))?;
    let base_url = trimmed_url(&runtime.endpoint_url);
    let existing_draft = if onboarding_draft.is_none() {
        provider_is_onboarding_draft_on(conn, &runtime.id)?.unwrap_or(false)
    } else {
        false
    };
    let draft = onboarding_draft.unwrap_or(existing_draft);
    if destination_exists(conn, &dest_id)? {
        conn.execute(
            "UPDATE destinations
             SET name = ?2, base_url = ?3, protocols_json = ?4, auth_scheme = ?5,
                 adapter = ?6, capabilities_json = ?7,
                 model_resolution = 'public_and_upstream',
                 max_credentials = CASE WHEN ?5 = 'none' THEN 1 ELSE NULL END,
                 preset_id = ?8, origin = ?9, offering = ?10,
                 created_at = COALESCE(created_at, ?11), updated_at = ?12,
                 onboarding_draft = ?13
             WHERE id = ?1",
            params![
                dest_id,
                runtime.name,
                base_url.as_deref(),
                protocols,
                auth_scheme,
                AdapterKind::Http.as_str(),
                capabilities,
                runtime.preset_id,
                origin,
                offering,
                runtime.created_at.to_rfc3339(),
                runtime.updated_at.to_rfc3339(),
                i32::from(draft),
            ],
        )?;
    } else {
        conn.execute(
            "INSERT INTO destinations (
                id, legacy_kind, legacy_id, adapter, name, brand_family, base_url,
                protocols_json, auth_scheme, model_resolution, capabilities_json, plan_json,
                max_credentials, observer_credential_id, enabled,
                onboarding_draft, preset_id, origin, offering, created_at, updated_at
             ) VALUES (?1, 'dynamic', ?2, ?3, ?4, NULL, ?5, ?6, ?7, 'public_and_upstream', ?8, NULL, CASE WHEN ?7 = 'none' THEN 1 ELSE NULL END, NULL, 1,
                       ?9, ?10, ?11, ?12, ?13, ?14)",
            params![
                dest_id,
                runtime.id,
                AdapterKind::Http.as_str(),
                runtime.name,
                base_url.as_deref(),
                protocols,
                auth_scheme,
                capabilities,
                i32::from(draft),
                runtime.preset_id,
                origin,
                offering,
                runtime.created_at.to_rfc3339(),
                runtime.updated_at.to_rfc3339(),
            ],
        )?;
    }
    replace_destination_models(conn, &dest_id, runtime)?;
    Ok(())
}

fn replace_destination_models(
    conn: &Connection,
    dest_id: &str,
    runtime: &DynamicProviderRuntime,
) -> Result<()> {
    let previous = super::destination_store::load_destination_catalog(conn, dest_id)?;
    let catalog =
        super::destination_commands::catalog_from_definition(&previous, &runtime.definition());
    super::destination_store::replace_destination_catalog(conn, dest_id, &catalog)
}

fn backfill_dynamic_from_leftover(conn: &Connection) -> Result<()> {
    if !table_has_column(conn, "providers", "origin")?
        || !table_has_column(conn, "providers", "name")?
    {
        let count: i64 = conn.query_row(
            "SELECT COUNT(*) FROM providers WHERE origin IN ('preset', 'custom')",
            [],
            |row| row.get(0),
        )?;
        anyhow::ensure!(count == 0, "v56 refuses leftover providers that cannot map");
        return Ok(());
    }
    let has_adapter = table_has_column(conn, "providers", "adapter_kind")?;
    let has_endpoint = table_has_column(conn, "providers", "endpoint_url")?;
    let has_protocol = table_has_column(conn, "providers", "upstream_protocol")?;
    let has_auth = table_has_column(conn, "providers", "auth_kind")?;
    let has_preset = table_has_column(conn, "providers", "preset_id")?;
    let has_offering = table_has_column(conn, "providers", "offering")?;
    let has_draft = table_has_column(conn, "providers", "onboarding_draft")?;
    let has_created = table_has_column(conn, "providers", "created_at")?;
    let has_updated = table_has_column(conn, "providers", "updated_at")?;
    let adapter_sql = if has_adapter { "adapter_kind" } else { "NULL" };
    let endpoint_sql = if has_endpoint { "endpoint_url" } else { "NULL" };
    let protocol_sql = if has_protocol {
        "upstream_protocol"
    } else {
        "NULL"
    };
    let auth_sql = if has_auth { "auth_kind" } else { "NULL" };
    let preset_sql = if has_preset { "preset_id" } else { "NULL" };
    let offering_sql = if has_offering { "offering" } else { "NULL" };
    let draft_sql = if has_draft {
        "COALESCE(onboarding_draft, 0)"
    } else {
        "0"
    };
    let created_sql = if has_created { "created_at" } else { "NULL" };
    let updated_sql = if has_updated { "updated_at" } else { "NULL" };
    let mut stmt = conn.prepare(&format!(
        "SELECT id, name, {endpoint_sql}, {protocol_sql}, {auth_sql}, {adapter_sql},
                {preset_sql}, origin, {offering_sql}, {draft_sql}, {created_sql}, {updated_sql}
         FROM providers
         WHERE origin IN ('preset', 'custom')
         ORDER BY id ASC"
    ))?;
    let leftovers = stmt
        .query_map([], |row| {
            Ok(LeftoverProvider {
                id: row.get(0)?,
                name: row.get(1)?,
                endpoint_url: row.get::<_, Option<String>>(2)?.unwrap_or_default(),
                protocol: row.get::<_, Option<String>>(3)?.unwrap_or_default(),
                auth_kind: row.get::<_, Option<String>>(4)?.unwrap_or_default(),
                adapter_kind: row.get::<_, Option<String>>(5)?.unwrap_or_default(),
                preset_id: row.get(6)?,
                origin: row.get(7)?,
                offering: row
                    .get::<_, Option<String>>(8)?
                    .unwrap_or_else(|| "api".into()),
                onboarding_draft: row.get::<_, i32>(9)? != 0,
                created_at: row.get::<_, Option<String>>(10)?.unwrap_or_default(),
                updated_at: row.get::<_, Option<String>>(11)?.unwrap_or_default(),
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    drop(stmt);

    let mut mapped_ids = HashSet::new();
    for leftover in leftovers {
        map_leftover_provider(conn, leftover, &mut mapped_ids)?;
    }
    refuse_orphan_provider_models(conn, &mapped_ids)?;
    Ok(())
}

fn map_leftover_provider(
    conn: &Connection,
    leftover: LeftoverProvider,
    mapped_ids: &mut HashSet<String>,
) -> Result<()> {
    let adapter = leftover.adapter_kind.trim();
    anyhow::ensure!(
        adapter.is_empty() || adapter == CONFIGURABLE_HTTP,
        "v56 refuses leftover provider `{}`: unknown adapter `{adapter}`",
        leftover.id
    );
    let auth_kind = DynamicAuthKind::try_from(leftover.auth_kind.as_str()).map_err(|_| {
        anyhow::anyhow!(
            "v56 refuses leftover provider `{}`: unknown auth `{}`",
            leftover.id,
            leftover.auth_kind
        )
    })?;
    let endpoint = leftover.endpoint_url.trim();
    if auth_kind.requires_key() {
        anyhow::ensure!(
            !endpoint.is_empty(),
            "v56 refuses leftover provider `{}`: empty required URL for keyed HTTP",
            leftover.id
        );
    }
    let protocol = if leftover.protocol.trim().is_empty() {
        ocg_domain::catalog::UpstreamProtocolKind::ChatCompletions
    } else {
        ocg_domain::catalog::UpstreamProtocolKind::try_from(leftover.protocol.as_str()).map_err(
            |_| {
                anyhow::anyhow!(
                    "v56 refuses leftover provider `{}`: unknown protocol `{}`",
                    leftover.id,
                    leftover.protocol
                )
            },
        )?
    };
    let mappings = load_leftover_models(conn, &leftover.id)?;
    if !endpoint.is_empty() {
        let definition = ocg_domain::dynamic::DynamicProviderDefinition {
            preset_id: leftover.preset_id.clone(),
            id: leftover.id.clone(),
            name: leftover.name.clone(),
            endpoint_url: leftover.endpoint_url.clone(),
            upstream_protocol: protocol,
            auth_kind,
            mappings: mappings.clone(),
        };
        destination_from_legacy(&LegacyDestinationFacts::Dynamic { definition }).map_err(
            |error| anyhow::anyhow!("v56 refuses leftover provider `{}`: {error}", leftover.id),
        )?;
    }
    let now = Utc::now().to_rfc3339();
    let created_at = parse_or_now(&leftover.created_at, &now);
    let updated_at = parse_or_now(&leftover.updated_at, &now);
    let runtime = DynamicProviderRuntime {
        preset_id: leftover.preset_id,
        id: leftover.id.clone(),
        name: leftover.name,
        endpoint_url: leftover.endpoint_url,
        upstream_protocol: protocol,
        auth_kind,
        mappings,
        created_at,
        updated_at,
        origin: ProviderOrigin::try_from(leftover.origin.as_str()).map_err(|error| {
            anyhow::anyhow!("v56 refuses leftover provider `{}`: {error}", leftover.id)
        })?,
        offering: leftover.offering,
    };
    upsert_dynamic_destination_on(conn, &runtime, Some(leftover.onboarding_draft))?;
    mapped_ids.insert(leftover.id);
    Ok(())
}

fn load_leftover_models(conn: &Connection, provider_id: &str) -> Result<Vec<DynamicModelMapping>> {
    if !table_exists(conn, "provider_models")? {
        return Ok(Vec::new());
    }
    let mut stmt = conn.prepare(
        "SELECT public_model, upstream_model, upstream_override
         FROM provider_models
         WHERE provider_id = ?1
         ORDER BY public_model_key ASC",
    )?;
    let rows = stmt.query_map([provider_id], |row| {
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
    Ok(mappings)
}

fn refuse_orphan_provider_models(conn: &Connection, mapped_ids: &HashSet<String>) -> Result<()> {
    if !table_exists(conn, "provider_models")? {
        return Ok(());
    }
    let mut stmt = conn.prepare("SELECT DISTINCT provider_id FROM provider_models")?;
    let ids = stmt
        .query_map([], |row| row.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    for id in ids {
        if mapped_ids.contains(&id) || builtin_provider(&id).is_some() {
            continue;
        }
        anyhow::bail!("v56 refuses leftover provider_models for unmapped provider `{id}`");
    }
    Ok(())
}

fn builtin_runtime(provider_id: &str) -> Option<DynamicProviderRuntime> {
    let plan = builtin_provider(provider_id)?;
    let protocol = plan
        .upstream_protocols
        .first()
        .copied()
        .unwrap_or(ocg_domain::catalog::UpstreamProtocolKind::ChatCompletions);
    let auth_kind = if plan.credential_kind == ocg_domain::catalog::CredentialKind::None {
        DynamicAuthKind::None
    } else {
        match plan.auth_schemes.first() {
            Some(ocg_domain::catalog::UpstreamAuthScheme::XApiKey) => DynamicAuthKind::XApiKey,
            _ => DynamicAuthKind::Bearer,
        }
    };
    Some(DynamicProviderRuntime {
        preset_id: None,
        id: plan.provider_id.to_string(),
        name: plan.display_name.to_string(),
        endpoint_url: String::new(),
        upstream_protocol: protocol,
        auth_kind,
        mappings: Vec::new(),
        created_at: DateTime::<Utc>::UNIX_EPOCH,
        updated_at: DateTime::<Utc>::UNIX_EPOCH,
        origin: ProviderOrigin::Builtin,
        offering: builtin_offering(plan.provider_id).to_string(),
    })
}

fn destination_exists(conn: &Connection, destination_id: &str) -> Result<bool> {
    Ok(conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM destinations WHERE id = ?1)",
        [destination_id],
        |row| row.get::<_, i64>(0),
    )? != 0)
}

fn dynamic_columns_ready(conn: &Connection) -> Result<bool> {
    Ok(table_exists(conn, "destinations")?
        && table_has_column(conn, "destinations", "onboarding_draft")?)
}

fn first_protocol(protocols_json: &str) -> Result<String> {
    let protocols: Vec<String> = serde_json::from_str(protocols_json).unwrap_or_default();
    Ok(protocols
        .into_iter()
        .next()
        .unwrap_or_else(|| "chat_completions".into()))
}

fn first_protocol_sql(protocols_json: String) -> String {
    first_protocol(&protocols_json).unwrap_or_else(|_| "chat_completions".into())
}

fn trimmed_url(value: &str) -> Option<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

fn parse_timestamp(raw: &str, provider_id: &str, field: &str) -> Result<DateTime<Utc>> {
    if raw.trim().is_empty() {
        return Ok(Utc::now());
    }
    DateTime::parse_from_rfc3339(raw)
        .map(|value| value.with_timezone(&Utc))
        .map_err(|error| {
            anyhow::anyhow!("dynamic provider {provider_id} has invalid {field}: {error}")
        })
}

fn parse_or_now(raw: &str, fallback: &str) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(raw)
        .or_else(|_| DateTime::parse_from_rfc3339(fallback))
        .map(|value| value.with_timezone(&Utc))
        .unwrap_or_else(|_| Utc::now())
}

struct LeftoverProvider {
    id: String,
    name: String,
    endpoint_url: String,
    protocol: String,
    auth_kind: String,
    adapter_kind: String,
    preset_id: Option<String>,
    origin: String,
    offering: String,
    onboarding_draft: bool,
    created_at: String,
    updated_at: String,
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
