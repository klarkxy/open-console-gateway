//! Custom HTTP facts on `destinations` / `destination_models` (schema v53+).
//!
//! After the leftover `account_custom_configs` / `account_model_capabilities`
//! drop, Custom endpoint/protocol/model mappings reconstruct from the Custom
//! destination (`legacy_kind=custom_account`) or, for linked platform Keys,
//! the platform parent destination catalog.

use super::*;
use crate::custom::validate_custom_endpoint_url;
use crate::models::{AccountCustomConfig, AccountModelCapability, AccountModelCapabilityInput};
use crate::provider::{UpstreamProtocolKind, validate_custom_model_id};
use anyhow::Result;
use chrono::{DateTime, Utc};
use ocg_domain::credential::{ModelScope, model_scope_allows};
use ocg_domain::destination::{
    AuthScheme, CatalogModel, LegacyDestinationFacts, destination_from_legacy,
    destination_id_for_custom_account, destination_id_for_platform_account,
};
use ocg_domain::dynamic::DynamicModelMapping;
use ocg_domain::ids::{CUSTOM_PROVIDER_ID, normalize_model_name};
use rusqlite::{Connection, OptionalExtension, params};
use std::collections::{HashMap, HashSet};

#[derive(Debug, Clone)]
pub(crate) struct CustomDestinationRecord {
    pub legacy_id: String,
    pub endpoint_url: String,
    pub protocol: UpstreamProtocolKind,
    pub auth_scheme: AuthScheme,
    pub models: Vec<DynamicModelMapping>,
}

/// Resolve the connection-owned Custom destination for one standalone Key.
/// Linked platform Keys deliberately return `None`: their route identity is
/// still account-scoped and owned by the platform destination.
pub(crate) fn custom_destination_for_account_on(
    conn: &Connection,
    account_id: &str,
) -> Result<Option<CustomDestinationRecord>> {
    let Some((destination_id, _, _)) = standalone_custom_destination_for_account(conn, account_id)?
    else {
        return Ok(None);
    };
    custom_destination_for_update_on(conn, &destination_id)
}

/// Copy leftover Custom facts onto destinations, then drop the two leftover
/// tables. Linked platform Keys are not mapped to Custom destinations.
pub(crate) fn migrate_v53_backfill_and_drop(conn: &Connection) -> Result<()> {
    let linked = linked_account_ids(conn)?;
    if table_exists(conn, "account_custom_configs")? {
        backfill_custom_destinations_from_leftover(conn, &linked)?;
    }
    if table_exists(conn, "account_model_capabilities")? {
        backfill_destination_models_from_leftover(conn, &linked)?;
    }
    conn.execute_batch(
        "DROP TABLE IF EXISTS account_model_capabilities;
         DROP TABLE IF EXISTS account_custom_configs;
         DROP INDEX IF EXISTS idx_account_model_capabilities_account;",
    )?;
    Ok(())
}

fn backfill_custom_destinations_from_leftover(
    conn: &Connection,
    linked: &HashSet<String>,
) -> Result<()> {
    if !table_has_column(conn, "account_custom_configs", "endpoint_url")?
        || !table_has_column(conn, "account_custom_configs", "upstream_protocol")?
    {
        let count: i64 =
            conn.query_row("SELECT COUNT(*) FROM account_custom_configs", [], |row| {
                row.get(0)
            })?;
        anyhow::ensure!(
            count == 0,
            "v53 refuses leftover custom configs that still use the pre-v32 columns"
        );
        return Ok(());
    }
    let mut stmt = conn.prepare(
        "SELECT account_id, endpoint_url, upstream_protocol
         FROM account_custom_configs",
    )?;
    let rows = stmt
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    drop(stmt);
    for (account_id, endpoint_url, protocol_value) in rows {
        if linked.contains(&account_id) {
            continue;
        }
        let provider = credential_provider_id(conn, &account_id)?;
        if provider.as_deref() != Some(CUSTOM_PROVIDER_ID) {
            continue;
        }
        let protocol = UpstreamProtocolKind::try_from(protocol_value.as_str()).map_err(|_| {
            anyhow::anyhow!(
                "v53 refuses leftover custom config `{account_id}`: unknown protocol `{protocol_value}`"
            )
        })?;
        let endpoint = endpoint_url.trim();
        anyhow::ensure!(
            !endpoint.is_empty(),
            "v53 refuses leftover custom config `{account_id}`: empty endpoint_url"
        );
        let name = credential_name(conn, &account_id)?.unwrap_or_else(|| account_id.clone());
        let destination_id = destination_id_for_custom_account(&account_id);
        upsert_custom_destination(
            conn,
            &account_id,
            &destination_id,
            &account_id,
            &name,
            endpoint,
            protocol,
            &[],
        )?;
    }
    Ok(())
}

fn backfill_destination_models_from_leftover(
    conn: &Connection,
    linked: &HashSet<String>,
) -> Result<()> {
    if !table_has_column(conn, "account_model_capabilities", "model_id")?
        || !table_has_column(conn, "account_model_capabilities", "protocol")?
    {
        let count: i64 = conn.query_row(
            "SELECT COUNT(*) FROM account_model_capabilities",
            [],
            |row| row.get(0),
        )?;
        anyhow::ensure!(
            count == 0,
            "v53 refuses leftover capabilities that cannot map"
        );
        return Ok(());
    }
    let upstream_sql = if table_has_column(conn, "account_model_capabilities", "upstream_model")? {
        "upstream_model"
    } else {
        "model_id"
    };
    let source_sql = if table_has_column(conn, "account_model_capabilities", "source")? {
        "source"
    } else {
        "'manual'"
    };
    let mut stmt = conn.prepare(&format!(
        "SELECT account_id, model_id, {upstream_sql}, protocol, {source_sql}
         FROM account_model_capabilities
         ORDER BY rowid ASC"
    ))?;
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

    // Start from Custom credentials, not leftover rows: a Key with an empty
    // leftover list must still intersect (`All ∩ []` / `Only[x] ∩ []` → `Only[]`).
    // A missing or unreadable leftover table never reaches this loop.
    let mut leftover_by_account: HashMap<String, Vec<String>> = HashMap::new();
    for account_id in custom_inference_account_ids(conn)? {
        leftover_by_account.entry(account_id).or_default();
    }

    // Aggregate by destination so two Keys on one platform parent write one
    // catalog. HashMap iteration must not decide which Key wins.
    let mut dest_order: Vec<String> = Vec::new();
    let mut by_destination: HashMap<String, Vec<AccountModelCapabilityInput>> = HashMap::new();
    for (account_id, public_model, upstream_model, protocol_value, source) in rows {
        let provider = credential_provider_id(conn, &account_id)?;
        if provider.as_deref() != Some(CUSTOM_PROVIDER_ID) {
            continue;
        }
        let protocol = UpstreamProtocolKind::try_from(protocol_value.as_str()).map_err(|_| {
            anyhow::anyhow!(
                "v53 refuses leftover capability `{account_id}` / `{public_model}`: unknown protocol `{protocol_value}`"
            )
        })?;
        let dest_id = if linked.contains(&account_id) {
            let Some(parent_id) = platform_parent_id(conn, &account_id)? else {
                anyhow::bail!(
                    "v53 refuses leftover linked capability `{account_id}`: platform_links row missing"
                );
            };
            destination_id_for_platform_account(&parent_id)
        } else {
            let dest_id = destination_id_for_custom_account(&account_id);
            anyhow::ensure!(
                destination_exists(conn, &dest_id)?,
                "v53 refuses leftover capability `{account_id}`: Custom destination is missing"
            );
            dest_id
        };
        if !by_destination.contains_key(&dest_id) {
            dest_order.push(dest_id.clone());
        }
        let account_models = leftover_by_account.entry(account_id.clone()).or_default();
        if !account_models
            .iter()
            .any(|model| model.eq_ignore_ascii_case(&public_model))
        {
            account_models.push(public_model.clone());
        }
        by_destination
            .entry(dest_id)
            .or_default()
            .push(AccountModelCapabilityInput {
                public_model,
                upstream_model,
                protocol,
                source: Some(source),
            });
    }

    for dest_id in dest_order {
        let capabilities = by_destination
            .remove(&dest_id)
            .expect("destination order tracks aggregated leftovers");
        let merged = merge_destination_models_refuse_conflict(&dest_id, capabilities)?;
        replace_destination_models(conn, &dest_id, &merged)?;
    }
    for (account_id, models) in leftover_by_account {
        narrow_credential_scope_intersect(conn, &account_id, &models)?;
    }
    Ok(())
}

fn merge_destination_models_refuse_conflict(
    destination_id: &str,
    capabilities: Vec<AccountModelCapabilityInput>,
) -> Result<Vec<AccountModelCapabilityInput>> {
    let mut merged: Vec<AccountModelCapabilityInput> = Vec::new();
    for capability in capabilities {
        if let Some(existing) = merged.iter().find(|row| {
            row.public_model
                .eq_ignore_ascii_case(&capability.public_model)
        }) {
            anyhow::ensure!(
                existing
                    .upstream_model
                    .eq_ignore_ascii_case(&capability.upstream_model),
                "destination `{destination_id}` refuses model `{}`: conflicting upstream mappings",
                capability.public_model
            );
        }
        if merged.iter().any(|row| {
            row.public_model
                .eq_ignore_ascii_case(&capability.public_model)
                && row.protocol == capability.protocol
        }) {
            continue;
        }
        merged.push(capability);
    }
    Ok(merged)
}

fn union_destination_models_keep_existing(
    existing: Vec<AccountModelCapabilityInput>,
    incoming: &[AccountModelCapabilityInput],
) -> Vec<AccountModelCapabilityInput> {
    let existing_names: HashSet<_> = existing
        .iter()
        .map(|row| row.public_model.to_ascii_lowercase())
        .collect();
    let mut merged = existing;
    for capability in incoming {
        if merged.iter().any(|row| {
            row.public_model
                .eq_ignore_ascii_case(&capability.public_model)
                && row.protocol == capability.protocol
        }) {
            continue;
        }
        if existing_names.contains(&capability.public_model.to_ascii_lowercase()) {
            continue;
        }
        merged.push(capability.clone());
    }
    merged
}

pub(crate) fn persist_custom_config_on(
    conn: &Connection,
    account_id: &str,
    input: &AccountCustomConfigInput,
) -> Result<bool> {
    reject_explicit_route_account_edit(conn, account_id)?;
    let endpoint_url = validate_custom_endpoint_url(&input.endpoint_url)?;
    if let Some(parent_id) = platform_parent_id(conn, account_id)? {
        merge_custom_models_onto_platform_parent(conn, account_id, &parent_id)?;
        return Ok(false);
    }
    let existing_row = standalone_custom_destination_for_account(conn, account_id)?;
    let existing = load_custom_destination_endpoint(conn, account_id)?;
    let endpoint_changed = existing.as_ref().is_some_and(|(url, protocol)| {
        url != &endpoint_url || *protocol != input.upstream_protocol
    });
    let (destination_id, legacy_owner_id, name) = match existing_row {
        Some((destination_id, legacy_owner_id, destination_name)) => {
            (destination_id, legacy_owner_id, destination_name)
        }
        None => (
            destination_id_for_custom_account(account_id),
            account_id.to_string(),
            credential_name(conn, account_id)?.unwrap_or_else(|| account_id.to_string()),
        ),
    };
    let existing_models = load_destination_model_inputs(conn, &destination_id, true)?;
    upsert_custom_destination(
        conn,
        account_id,
        &destination_id,
        &legacy_owner_id,
        &name,
        &endpoint_url,
        input.upstream_protocol,
        &existing_models,
    )?;
    Ok(endpoint_changed || existing.is_none())
}

pub(crate) fn persist_custom_capabilities_on(
    conn: &Connection,
    account_id: &str,
    capabilities: &[AccountModelCapabilityInput],
) -> Result<()> {
    reject_explicit_route_account_edit(conn, account_id)?;
    if !capabilities.is_empty() {
        let expected = expected_protocol_for_capabilities(conn, account_id)?.ok_or_else(|| {
            anyhow::anyhow!(
                "Custom model capabilities require a persisted custom_config.upstream_protocol"
            )
        })?;
        crate::custom::validate_custom_capability_expansion(expected, capabilities)
            .map_err(|message| anyhow::anyhow!(message))?;
    }
    let dest_id = destination_id_for_account_custom_facts(conn, account_id)?;
    if dest_id.is_none() && capabilities.is_empty() {
        return Ok(());
    }
    let dest_id = dest_id.ok_or_else(|| {
        anyhow::anyhow!(
            "Custom model capabilities require a persisted custom_config.upstream_protocol"
        )
    })?;
    if platform_parent_id(conn, account_id)?.is_some() {
        persist_linked_custom_capabilities(conn, account_id, &dest_id, capabilities)?;
    } else {
        // A credential edits its scope; sibling credentials still own their
        // references to the shared destination catalog.
        let count = custom_connection_credential_count_on(conn, account_id)?;
        if count > 1 {
            let mut merged = load_destination_model_inputs(conn, &dest_id, true)?;
            for incoming in capabilities {
                if let Some(existing) = merged.iter().find(|model| {
                    model
                        .public_model
                        .eq_ignore_ascii_case(&incoming.public_model)
                }) {
                    anyhow::ensure!(
                        existing.upstream_model == incoming.upstream_model
                            && existing.protocol == incoming.protocol,
                        "shared destination model `{}` has conflicting configuration",
                        incoming.public_model
                    );
                } else {
                    merged.push(incoming.clone());
                }
            }
            replace_destination_models(conn, &dest_id, &merged)?;
        } else {
            replace_destination_models(conn, &dest_id, capabilities)?;
        }
        persist_credential_model_scope_on(
            conn,
            account_id,
            &ModelScope::Only {
                models: unique_public_models_from_inputs(capabilities),
            },
        )?;
    }
    Ok(())
}

fn reject_explicit_route_account_edit(conn: &Connection, account_id: &str) -> Result<()> {
    if let Some(destination_id) = destination_id_for_account_custom_facts(conn, account_id)? {
        anyhow::ensure!(
            super::destination_store::load_protocol_routes(conn, &destination_id)?.is_empty(),
            "this connection has explicit protocol routes; edit its configuration and models in Providers"
        );
    }
    Ok(())
}

// Platform onboarding declares the site's protocol set for each new model.
// replace_destination_models retains controls already saved on surviving rows.
fn platform_capabilities(
    conn: &Connection,
    destination_id: &str,
    capabilities: &[AccountModelCapabilityInput],
) -> Result<Vec<AccountModelCapabilityInput>> {
    let raw: String = conn.query_row(
        "SELECT protocols_json FROM destinations WHERE id = ?1",
        [destination_id],
        |row| row.get(0),
    )?;
    let protocols: Vec<UpstreamProtocolKind> = serde_json::from_str(&raw)?;
    let mut seen = HashSet::new();
    let mut declared = Vec::new();
    for model in capabilities {
        for protocol in &protocols {
            if seen.insert((model.public_model.to_ascii_lowercase(), *protocol)) {
                declared.push(AccountModelCapabilityInput {
                    protocol: *protocol,
                    ..model.clone()
                });
            }
        }
    }
    Ok(declared)
}

fn persist_linked_custom_capabilities(
    conn: &Connection,
    account_id: &str,
    parent_dest_id: &str,
    capabilities: &[AccountModelCapabilityInput],
) -> Result<()> {
    let discovered = unique_public_models_from_inputs(capabilities);
    let owned_id = destination_id_for_custom_account(account_id);
    if destination_exists(conn, &owned_id)? {
        replace_destination_models(conn, &owned_id, capabilities)?;
    }
    let next_scope = ModelScope::Only { models: discovered };
    let declared = platform_capabilities(conn, parent_dest_id, capabilities)?;
    let mut catalog = union_destination_models_keep_existing(
        load_destination_model_inputs(conn, parent_dest_id, true)?,
        &declared,
    );
    if let Some(referenced) =
        referenced_catalog_models(conn, parent_dest_id, account_id, &next_scope)?
    {
        catalog.retain(|row| referenced.contains(&normalize_model_name(&row.public_model)));
    }
    replace_destination_models_with_controls(conn, parent_dest_id, &catalog, true)?;
    persist_credential_model_scope_on(conn, account_id, &next_scope)?;
    Ok(())
}

pub(crate) fn account_custom_config_on(
    conn: &Connection,
    account_id: &str,
) -> Result<Option<AccountCustomConfig>> {
    if let Some(parent_id) = platform_parent_id(conn, account_id)? {
        return linked_custom_config(conn, account_id, &parent_id);
    }
    let Some((endpoint_url, protocol)) = load_custom_destination_endpoint(conn, account_id)? else {
        return Ok(None);
    };
    let (created_at, updated_at) = credential_timestamps(conn, account_id)?;
    Ok(Some(AccountCustomConfig {
        account_id: account_id.to_string(),
        endpoint_url,
        upstream_protocol: protocol,
        created_at,
        updated_at,
    }))
}

pub(crate) fn list_capabilities_on(
    conn: &Connection,
    account_id: &str,
    declared_order: bool,
    skip_unknown_protocols: bool,
) -> Result<Vec<AccountModelCapability>> {
    let Some(dest_id) = destination_id_for_account_custom_facts(conn, account_id)? else {
        return Ok(Vec::new());
    };
    let mut stmt = conn.prepare(
        "SELECT m.public_model, m.upstream_model,
             CASE WHEN m.protocols_json != '[]' THEN m.protocols_json
                  WHEN m.preferred IS NOT NULL THEN json_array(m.preferred)
                  ELSE d.protocols_json END, m.upstream_override
         FROM destination_models m JOIN destinations d ON d.id = m.destination_id
         WHERE m.destination_id = ?1
         ORDER BY m.rowid ASC",
    )?;
    let rows = stmt
        .query_map([&dest_id], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, Option<String>>(3)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    drop(stmt);
    let mut capabilities = Vec::new();
    for (public_model, upstream_model, protocols_json, upstream_override) in rows {
        let mut protocols: Vec<UpstreamProtocolKind> = match serde_json::from_str(&protocols_json) {
            Ok(value) => value,
            Err(error) if skip_unknown_protocols => {
                let _ = error;
                continue;
            }
            Err(error) => {
                return Err(anyhow::anyhow!(
                    "invalid destination_models.protocols_json: {error}"
                ));
            }
        };
        if let Some(raw) = upstream_override {
            let route: ocg_domain::dynamic::DynamicModelUpstreamOverride =
                serde_json::from_str(&raw)?;
            protocols = vec![route.protocol];
        }
        for protocol in protocols {
            capabilities.push(AccountModelCapability {
                account_id: account_id.to_string(),
                public_model: public_model.clone(),
                upstream_model: upstream_model.clone(),
                protocol,
                verified_at: None,
                source: "manual".into(),
            });
        }
    }
    let scope = credential_model_scope_on(conn, account_id)?;
    capabilities.retain(|capability| model_scope_allows(&scope, &capability.public_model));
    if !declared_order {
        capabilities.sort_by(|left, right| {
            left.public_model
                .cmp(&right.public_model)
                .then_with(|| left.protocol.as_str().cmp(right.protocol.as_str()))
        });
    }
    Ok(capabilities)
}

pub(crate) fn custom_auth_kind_on(
    conn: &Connection,
    account_id: &str,
) -> Result<Option<ocg_domain::dynamic::DynamicAuthKind>> {
    let value = conn
        .query_row(
            "SELECT d.auth_scheme
             FROM credentials c
             JOIN destinations d ON d.id = c.destination_id
             WHERE c.legacy_account_id = ?1",
            [account_id],
            |row| row.get::<_, String>(0),
        )
        .optional()?;
    value
        .map(|value| match value.as_str() {
            "bearer" => Ok(ocg_domain::dynamic::DynamicAuthKind::Bearer),
            "x_api_key" | "x-api-key" => Ok(ocg_domain::dynamic::DynamicAuthKind::XApiKey),
            "none" => Ok(ocg_domain::dynamic::DynamicAuthKind::None),
            other => anyhow::bail!("unknown destinations.auth_scheme `{other}`"),
        })
        .transpose()
}

pub(crate) fn custom_route_overrides_on(
    conn: &Connection,
    account_id: &str,
) -> Result<Vec<(String, ocg_domain::dynamic::DynamicModelUpstreamOverride)>> {
    let Some(destination_id) = destination_id_for_account_custom_facts(conn, account_id)? else {
        return Ok(Vec::new());
    };
    let scope = credential_model_scope_on(conn, account_id)?;
    let mut stmt = conn.prepare(
        "SELECT public_model, upstream_override
         FROM destination_models
         WHERE destination_id = ?1 AND upstream_override IS NOT NULL
         ORDER BY rowid ASC",
    )?;
    let rows = stmt
        .query_map([destination_id], |row| {
            let public_model = row.get::<_, String>(0)?;
            let raw = row.get::<_, String>(1)?;
            let route = serde_json::from_str(&raw).map_err(|error| {
                rusqlite::Error::FromSqlConversionFailure(
                    1,
                    rusqlite::types::Type::Text,
                    Box::new(error),
                )
            })?;
            Ok((public_model, route))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows
        .into_iter()
        .filter(|(public_model, _)| model_scope_allows(&scope, public_model))
        .collect())
}

pub(crate) fn capability_triples_on(
    conn: &Connection,
    account_id: &str,
) -> Result<Vec<(String, String, UpstreamProtocolKind)>> {
    Ok(list_capabilities_on(conn, account_id, true, false)?
        .into_iter()
        .map(|row| (row.public_model, row.upstream_model, row.protocol))
        .collect())
}

pub(crate) fn custom_endpoint_protocol_on(
    conn: &Connection,
    account_id: &str,
) -> Result<Option<(String, UpstreamProtocolKind)>> {
    Ok(account_custom_config_on(conn, account_id)?
        .map(|config| (config.endpoint_url, config.upstream_protocol)))
}

pub(crate) fn has_custom_config_on(conn: &Connection, account_id: &str) -> Result<bool> {
    Ok(account_custom_config_on(conn, account_id)?.is_some())
}

pub(crate) fn delete_custom_destination_facts(conn: &Connection, account_id: &str) -> Result<()> {
    if platform_parent_id(conn, account_id)?.is_some() {
        return Ok(());
    }
    // Destination configuration is connection-owned after v58. Deleting or
    // replacing the last Key deliberately leaves the connection available so
    // another credential can be attached later.
    Ok(())
}

pub(crate) fn persist_custom_destination_after_unlink(
    conn: &Connection,
    account_id: &str,
    parent_id: &str,
) -> Result<()> {
    let parent_dest = destination_id_for_platform_account(parent_id);
    let Some(base_url) = crate::db::platform::platform_parent_base_url(conn, parent_id)? else {
        return Ok(());
    };
    let endpoint = crate::platform::hosted_endpoint(&base_url)?;
    let models = load_destination_model_inputs(conn, &parent_dest, true)?;
    let protocol = models
        .first()
        .map(|row| row.protocol)
        .or_else(|| {
            load_destination_first_protocol(conn, &parent_dest)
                .ok()
                .flatten()
        })
        .unwrap_or(UpstreamProtocolKind::ChatCompletions);
    // Preserve explicit revocation. A new connection identity may inherit
    // authorization only when this Key had both the old endpoint id and the
    // exact new origin authorized before unlink.
    let credential_id: String = conn.query_row(
        "SELECT id FROM credentials
         WHERE legacy_account_id = ?1
           AND COALESCE(credential_purpose, 'inference') = 'inference'",
        [account_id],
        |row| row.get(0),
    )?;
    let old_connection_id =
        connection_id_for_legacy(LegacyConnectionKind::CustomAccount, account_id);
    let old_endpoint_id = endpoint_id_for(&old_connection_id, EndpointOperation::from(protocol));
    let new_origin = normalize_origin(&endpoint);
    let mut had_old_endpoint = false;
    let mut had_new_origin = false;
    {
        let mut stmt =
            conn.prepare("SELECT kind, value FROM credential_grants WHERE credential_id = ?1")?;
        for row in stmt.query_map([&credential_id], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })? {
            let (kind, value) = row?;
            if kind == "endpoint_id" && value == old_endpoint_id.as_str() {
                had_old_endpoint = true;
            } else if kind == "origin"
                && new_origin
                    .as_ref()
                    .is_some_and(|expected| normalize_origin(&value).as_ref() == Some(expected))
            {
                had_new_origin = true;
            }
        }
    }
    let name = credential_name(conn, account_id)?.unwrap_or_else(|| account_id.to_string());
    // Return to the original connection when it is empty and still points to
    // the same route. This is the normal Add Key -> link -> unlink path and
    // avoids creating a second identical zero-Key connection. A shared or
    // edited source stays untouched; in that case allocate a new identity.
    let mut legacy_owner_id = account_id.to_string();
    let mut destination_id = destination_id_for_custom_account(&legacy_owner_id);
    let reuse_source =
        reusable_unlinked_custom_destination(conn, &destination_id, &endpoint, protocol, &models)?;
    if reuse_source {
        conn.execute(
            "UPDATE credentials SET destination_id = ?2 WHERE legacy_account_id = ?1",
            params![account_id, destination_id],
        )?;
    } else {
        while destination_exists(conn, &destination_id)? {
            legacy_owner_id = uuid::Uuid::new_v4().to_string();
            destination_id = destination_id_for_custom_account(&legacy_owner_id);
        }
        upsert_custom_destination(
            conn,
            account_id,
            &destination_id,
            &legacy_owner_id,
            &name,
            &endpoint,
            protocol,
            &models,
        )?;
    }
    // Platform endpoint ids are not valid for the Custom connection, whether
    // it is the reusable source or a fresh identity. Rebuild the grant from
    // the saved authorization before committing.
    let connection_id =
        connection_id_for_legacy(LegacyConnectionKind::CustomAccount, &legacy_owner_id);
    let (ids, origins) = if had_old_endpoint && had_new_origin {
        let assigned = assigned_endpoints_for_routes(
            &connection_id,
            &[RouteSpec {
                operation: EndpointOperation::from(protocol),
                url: Some(endpoint),
            }],
        );
        safe_default_grants(&assigned)
    } else {
        (Vec::new(), Vec::new())
    };
    identity::replace_binding_grants_for_account_on(conn, account_id, &ids, &origins)?;
    Ok(())
}

fn reusable_unlinked_custom_destination(
    conn: &Connection,
    destination_id: &str,
    endpoint: &str,
    protocol: UpstreamProtocolKind,
    models: &[AccountModelCapabilityInput],
) -> Result<bool> {
    let source: Option<(Option<String>, String, String)> = conn
        .query_row(
            "SELECT base_url, protocols_json, auth_scheme FROM destinations
             WHERE id = ?1 AND legacy_kind = 'custom_account' AND adapter = 'http'",
            [destination_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()?;
    let Some((base_url, protocols_json, auth_scheme)) = source else {
        return Ok(false);
    };
    let protocols: Vec<UpstreamProtocolKind> = serde_json::from_str(&protocols_json)?;
    let expected_auth_scheme = if protocol == UpstreamProtocolKind::Messages {
        AuthScheme::XApiKey
    } else {
        AuthScheme::Bearer
    };
    if base_url.as_deref() != Some(endpoint)
        || protocols.as_slice() != [protocol]
        || auth_scheme != expected_auth_scheme.as_str()
        || !super::destination_store::load_protocol_routes(conn, destination_id)?.is_empty()
    {
        return Ok(false);
    }
    let override_count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM destination_models
         WHERE destination_id = ?1 AND upstream_override IS NOT NULL",
        [destination_id],
        |row| row.get(0),
    )?;
    if override_count != 0 {
        return Ok(false);
    }
    let source_models = load_destination_model_inputs(conn, destination_id, true)?;
    let pairs = |rows: &[AccountModelCapabilityInput]| -> HashSet<(String, String)> {
        rows.iter()
            .map(|row| (row.public_model.clone(), row.upstream_model.clone()))
            .collect()
    };
    if pairs(&source_models) != pairs(models) {
        return Ok(false);
    }
    let count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM credentials WHERE destination_id = ?1",
        [destination_id],
        |row| row.get(0),
    )?;
    Ok(count == 0)
}

fn linked_custom_config(
    conn: &Connection,
    account_id: &str,
    parent_id: &str,
) -> Result<Option<AccountCustomConfig>> {
    let Some(base_url) = crate::db::platform::platform_parent_base_url(conn, parent_id)? else {
        return Ok(None);
    };
    let endpoint_url = crate::platform::hosted_endpoint(&base_url)?;
    let dest_id = destination_id_for_platform_account(parent_id);
    let protocol = list_capabilities_on(conn, account_id, true, true)?
        .first()
        .map(|row| row.protocol)
        .or_else(|| {
            load_destination_first_protocol(conn, &dest_id)
                .ok()
                .flatten()
        })
        .unwrap_or(UpstreamProtocolKind::ChatCompletions);
    let (created_at, updated_at) = credential_timestamps(conn, account_id)?;
    Ok(Some(AccountCustomConfig {
        account_id: account_id.to_string(),
        endpoint_url,
        upstream_protocol: protocol,
        created_at,
        updated_at,
    }))
}

fn expected_protocol_for_capabilities(
    conn: &Connection,
    account_id: &str,
) -> Result<Option<UpstreamProtocolKind>> {
    Ok(account_custom_config_on(conn, account_id)?.map(|config| config.upstream_protocol))
}

fn destination_id_for_account_custom_facts(
    conn: &Connection,
    account_id: &str,
) -> Result<Option<String>> {
    if let Some(parent_id) = platform_parent_id(conn, account_id)? {
        return Ok(Some(destination_id_for_platform_account(&parent_id)));
    }
    Ok(standalone_custom_destination_for_account(conn, account_id)?
        .map(|(destination_id, _, _)| destination_id))
}

#[allow(clippy::too_many_arguments)]
fn upsert_custom_destination(
    conn: &Connection,
    account_id: &str,
    destination_id: &str,
    legacy_owner_id: &str,
    name: &str,
    endpoint_url: &str,
    protocol: UpstreamProtocolKind,
    capabilities: &[AccountModelCapabilityInput],
) -> Result<()> {
    let pairs: Vec<(String, String)> = capabilities
        .iter()
        .map(|row| (row.public_model.clone(), row.upstream_model.clone()))
        .collect();
    let destination = destination_from_legacy(&LegacyDestinationFacts::CustomAccount {
        account_id: legacy_owner_id.to_string(),
        name: name.to_string(),
        endpoint_url: endpoint_url.to_string(),
        protocol,
        model_capabilities: pairs,
    })
    .map_err(|error| anyhow::anyhow!("{error}"))?;
    anyhow::ensure!(
        destination.id == destination_id,
        "custom destination identity changed while updating account `{account_id}`"
    );
    let dest_id = destination_id.to_string();
    if destination_exists(conn, &dest_id)? {
        conn.execute(
            "UPDATE destinations
             SET name = ?2, base_url = ?3, protocols_json = ?4, auth_scheme = ?5,
                 adapter = ?6, capabilities_json = ?7,
                 model_resolution = 'public_only', max_credentials = NULL
             WHERE id = ?1",
            params![
                dest_id,
                destination.name,
                destination.base_url,
                serde_json::to_string(&destination.protocols)?,
                destination.auth_scheme.as_str(),
                destination.adapter.as_str(),
                serde_json::to_string(&destination.capabilities)?,
            ],
        )?;
    } else {
        conn.execute(
            "INSERT INTO destinations (
                id, legacy_kind, legacy_id, adapter, name, brand_family, base_url,
                protocols_json, auth_scheme, model_resolution, capabilities_json, plan_json,
                max_credentials, observer_credential_id, enabled
             ) VALUES (?1, 'custom_account', ?2, ?3, ?4, NULL, ?5, ?6, ?7, 'public_only', ?8, NULL, NULL, NULL, ?9)",
            params![
                dest_id,
                legacy_owner_id,
                destination.adapter.as_str(),
                destination.name,
                destination.base_url,
                serde_json::to_string(&destination.protocols)?,
                destination.auth_scheme.as_str(),
                serde_json::to_string(&destination.capabilities)?,
                i64::from(destination.enabled),
            ],
        )?;
    }
    if !capabilities.is_empty() {
        replace_destination_models(conn, &dest_id, capabilities)?;
    }
    conn.execute(
        "UPDATE credentials SET destination_id = ?2 WHERE legacy_account_id = ?1",
        params![account_id, dest_id],
    )?;
    Ok(())
}

fn replace_destination_models(
    conn: &Connection,
    destination_id: &str,
    capabilities: &[AccountModelCapabilityInput],
) -> Result<()> {
    replace_destination_models_with_controls(conn, destination_id, capabilities, false)
}

fn replace_destination_models_with_controls(
    conn: &Connection,
    destination_id: &str,
    capabilities: &[AccountModelCapabilityInput],
    preserve_controls: bool,
) -> Result<()> {
    let previous = super::destination_store::load_destination_catalog(conn, destination_id)?;
    let mut seen = HashSet::new();
    let mut models: Vec<CatalogModel> = Vec::new();
    for capability in capabilities {
        let public_model = validate_custom_model_id(&capability.public_model)?;
        let upstream_model = validate_custom_model_id(&capability.upstream_model)?;
        let key = (
            public_model.to_ascii_lowercase(),
            capability.protocol.as_str().to_string(),
        );
        anyhow::ensure!(
            seen.insert(key),
            "duplicate model capability `{public_model}` / {}",
            capability.protocol.as_str()
        );
        if let Some(existing) = models
            .iter_mut()
            .find(|model| model.public_model.eq_ignore_ascii_case(&public_model))
        {
            anyhow::ensure!(
                existing.upstream_model == upstream_model,
                "model `{public_model}` has conflicting upstream identities"
            );
            if !existing.protocols.contains(&capability.protocol) {
                existing.protocols.push(capability.protocol);
            }
            continue;
        }
        models.push(CatalogModel {
            public_model,
            upstream_model,
            protocols: vec![capability.protocol],
            preferred: Some(capability.protocol),
            enabled: true,
            upstream_override: None,
        });
    }
    conn.execute(
        "DELETE FROM destination_models WHERE destination_id = ?1",
        [destination_id],
    )?;
    for mut model in models {
        if let Some(saved) = previous
            .iter()
            .find(|saved| saved.public_model.eq_ignore_ascii_case(&model.public_model))
        {
            model.enabled = saved.enabled;
            if saved.upstream_model == model.upstream_model {
                model.upstream_override = saved.upstream_override.clone();
                if preserve_controls
                    || saved
                        .preferred
                        .is_some_and(|preferred| model.protocols.contains(&preferred))
                {
                    model.preferred = saved.preferred;
                    model.protocols = saved.protocols.clone();
                }
            }
        }
        conn.execute(
            "INSERT INTO destination_models (
                destination_id, public_model, public_model_key, upstream_model,
                protocols_json, preferred, enabled, upstream_override
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                destination_id,
                model.public_model,
                model.public_model.to_ascii_lowercase(),
                model.upstream_model,
                serde_json::to_string(&model.protocols)?,
                model
                    .preferred
                    .map(|protocol| protocol.as_str().to_string()),
                i64::from(model.enabled),
                model
                    .upstream_override
                    .as_ref()
                    .map(serde_json::to_string)
                    .transpose()?,
            ],
        )?;
    }
    Ok(())
}

pub(crate) fn merge_custom_models_onto_platform_parent(
    conn: &Connection,
    account_id: &str,
    parent_id: &str,
) -> Result<()> {
    let Some((custom_id, _, _)) = standalone_custom_destination_for_account(conn, account_id)?
    else {
        return Ok(());
    };
    let parent_id = destination_id_for_platform_account(parent_id);
    let custom_models = load_destination_model_inputs(conn, &custom_id, true)?;
    if custom_models.is_empty() {
        return Ok(());
    }
    let custom_models = platform_capabilities(conn, &parent_id, &custom_models)?;
    let merged = union_destination_models_keep_existing(
        load_destination_model_inputs(conn, &parent_id, true)?,
        &custom_models,
    );
    if destination_exists(conn, &parent_id)? {
        replace_destination_models_with_controls(conn, &parent_id, &merged, true)?;
    }
    Ok(())
}

/// Copy each linked Key's owned Custom catalog onto its platform parent.
/// Conflicting public→upstream maps refuse so import does not pick a winner.
pub(crate) fn merge_linked_custom_models_for_import(
    conn: &Connection,
    links: &[(String, String)],
) -> Result<()> {
    let mut dest_order: Vec<String> = Vec::new();
    let mut by_destination: HashMap<String, Vec<AccountModelCapabilityInput>> = HashMap::new();
    let mut owned_scopes: Vec<(String, Vec<String>)> = Vec::new();
    for (account_id, parent_id) in links {
        let custom_id = destination_id_for_custom_account(account_id);
        let models = load_destination_model_inputs(conn, &custom_id, true)?;
        if models.is_empty() {
            continue;
        }
        owned_scopes.push((
            account_id.clone(),
            unique_public_models_from_inputs(&models),
        ));
        let dest_id = destination_id_for_platform_account(parent_id);
        if !by_destination.contains_key(&dest_id) {
            dest_order.push(dest_id.clone());
        }
        by_destination.entry(dest_id).or_default().extend(models);
    }
    for dest_id in dest_order {
        let incoming = by_destination
            .remove(&dest_id)
            .expect("destination order tracks imported catalogs");
        let mut combined = load_destination_model_inputs(conn, &dest_id, true)?;
        combined.extend(incoming);
        let merged = merge_destination_models_refuse_conflict(&dest_id, combined)?;
        if destination_exists(conn, &dest_id)? {
            replace_destination_models(conn, &dest_id, &merged)?;
        }
    }
    for (account_id, models) in owned_scopes {
        narrow_credential_scope_intersect(conn, &account_id, &models)?;
    }
    Ok(())
}

/// Re-narrow imported Custom scopes after identity restore may write `All`.
/// Uses the package capability list, including empty lists — an independent
/// Custom destination may be gone after a later merge onto the platform parent.
pub(crate) fn narrow_imported_custom_scopes(
    conn: &Connection,
    imported: &[(&str, &[AccountModelCapabilityInput])],
) -> Result<()> {
    for (account_id, capabilities) in imported {
        narrow_credential_scope_intersect(
            conn,
            account_id,
            &unique_public_models_from_inputs(capabilities),
        )?;
    }
    Ok(())
}

pub(crate) fn narrow_credential_scope_from_custom_destination(
    conn: &Connection,
    account_id: &str,
) -> Result<()> {
    let Some((custom_id, _, _)) = standalone_custom_destination_for_account(conn, account_id)?
    else {
        return Ok(());
    };
    let models = load_destination_model_inputs(conn, &custom_id, true)?;
    if models.is_empty() {
        return Ok(());
    }
    narrow_credential_scope_intersect(conn, account_id, &unique_public_models_from_inputs(&models))
}

fn load_custom_destination_endpoint(
    conn: &Connection,
    account_id: &str,
) -> Result<Option<(String, UpstreamProtocolKind)>> {
    let Some((dest_id, _, _)) = standalone_custom_destination_for_account(conn, account_id)? else {
        return Ok(None);
    };
    let row: Option<(Option<String>, String)> = conn
        .query_row(
            "SELECT base_url, protocols_json FROM destinations
             WHERE id = ?1 AND legacy_kind = 'custom_account'",
            [&dest_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    let Some((base_url, protocols_json)) = row else {
        return Ok(None);
    };
    let protocols: Vec<UpstreamProtocolKind> = serde_json::from_str(&protocols_json)
        .map_err(|error| anyhow::anyhow!("invalid destinations.protocols_json: {error}"))?;
    let protocol = protocols
        .into_iter()
        .next()
        .unwrap_or(UpstreamProtocolKind::ChatCompletions);
    Ok(Some((base_url.unwrap_or_default(), protocol)))
}

/// Resolve the connection-owned Custom destination for one credential.
/// The destination's legacy id remains the first pre-v58 account id, while
/// later credentials point at the same destination through `destination_id`.
fn standalone_custom_destination_for_account(
    conn: &Connection,
    account_id: &str,
) -> Result<Option<(String, String, String)>> {
    conn.query_row(
        "SELECT d.id, d.legacy_id, d.name
         FROM credentials c
         JOIN destinations d ON d.id = c.destination_id
         WHERE c.legacy_account_id = ?1
           AND d.legacy_kind = 'custom_account'",
        [account_id],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
    )
    .optional()
    .map_err(Into::into)
}

pub(crate) fn custom_connection_credential_count_on(
    conn: &Connection,
    account_id: &str,
) -> Result<i64> {
    let Some((destination_id, _, _)) = standalone_custom_destination_for_account(conn, account_id)?
    else {
        return Ok(0);
    };
    conn.query_row(
        "SELECT COUNT(*) FROM credentials
         WHERE destination_id = ?1
           AND COALESCE(credential_purpose, 'inference') = 'inference'",
        [destination_id],
        |row| row.get(0),
    )
    .map_err(Into::into)
}

pub(crate) fn custom_destination_for_update_on(
    conn: &Connection,
    destination_id: &str,
) -> Result<Option<CustomDestinationRecord>> {
    let row: Option<(String, String, String, String)> = conn
        .query_row(
            "SELECT legacy_id, COALESCE(base_url, ''), protocols_json, auth_scheme
             FROM destinations
             WHERE id = ?1 AND legacy_kind = 'custom_account' AND adapter = 'http'",
            [destination_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .optional()?;
    let Some((legacy_id, endpoint_url, protocols_json, auth_scheme)) = row else {
        return Ok(None);
    };
    let protocols: Vec<UpstreamProtocolKind> = serde_json::from_str(&protocols_json)?;
    let protocol = protocols
        .first()
        .copied()
        .unwrap_or(UpstreamProtocolKind::ChatCompletions);
    let auth_scheme = match auth_scheme.as_str() {
        "bearer" => AuthScheme::Bearer,
        "x_api_key" | "x-api-key" => AuthScheme::XApiKey,
        "none" => AuthScheme::None,
        other => anyhow::bail!("unknown destinations.auth_scheme `{other}`"),
    };
    let mut stmt = conn.prepare(
        "SELECT public_model, upstream_model, upstream_override
         FROM destination_models
         WHERE destination_id = ?1
         ORDER BY rowid ASC",
    )?;
    let models = stmt
        .query_map([destination_id], |row| {
            Ok(DynamicModelMapping {
                public_model: row.get(0)?,
                upstream_model: row.get(1)?,
                upstream_override: row
                    .get::<_, Option<String>>(2)?
                    .map(|raw| {
                        serde_json::from_str(&raw).map_err(|error| {
                            rusqlite::Error::FromSqlConversionFailure(
                                2,
                                rusqlite::types::Type::Text,
                                Box::new(error),
                            )
                        })
                    })
                    .transpose()?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(Some(CustomDestinationRecord {
        legacy_id,
        endpoint_url,
        protocol,
        auth_scheme,
        models,
    }))
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn upsert_imported_custom_destination_on(
    conn: &Connection,
    destination_id: &str,
    legacy_id: &str,
    name: &str,
    endpoint_url: &str,
    protocol: UpstreamProtocolKind,
    auth_scheme: AuthScheme,
    models: &[DynamicModelMapping],
    enabled: bool,
) -> Result<()> {
    anyhow::ensure!(
        destination_id_for_custom_account(legacy_id) == destination_id,
        "Custom destination `{destination_id}` has an incompatible stable identity"
    );
    let endpoint_url = validate_custom_endpoint_url(endpoint_url)?;
    let definition = ocg_domain::dynamic::DynamicProviderDefinition {
        preset_id: None,
        id: legacy_id.to_string(),
        name: name.to_string(),
        endpoint_url: endpoint_url.clone(),
        upstream_protocol: protocol,
        auth_kind: match auth_scheme {
            AuthScheme::Bearer => ocg_domain::dynamic::DynamicAuthKind::Bearer,
            AuthScheme::XApiKey => ocg_domain::dynamic::DynamicAuthKind::XApiKey,
            AuthScheme::None => ocg_domain::dynamic::DynamicAuthKind::None,
        },
        mappings: models.to_vec(),
    };
    let definition = crate::dynamic::validate_definition(definition)
        .map_err(|error| anyhow::anyhow!(error.to_string()))?;
    let name = definition.name.as_str();
    let endpoint_url = definition.endpoint_url;
    let protocol = definition.upstream_protocol;
    let models = definition.mappings;
    let pairs = models
        .iter()
        .map(|model| (model.public_model.clone(), model.upstream_model.clone()))
        .collect::<Vec<_>>();
    let mapped = destination_from_legacy(&LegacyDestinationFacts::CustomAccount {
        account_id: legacy_id.to_string(),
        name: name.to_string(),
        endpoint_url: endpoint_url.clone(),
        protocol,
        model_capabilities: pairs,
    })
    .map_err(|error| anyhow::anyhow!(error.to_string()))?;
    let collision = conn
        .query_row(
            "SELECT legacy_kind, legacy_id, adapter FROM destinations WHERE id = ?1",
            [destination_id],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                ))
            },
        )
        .optional()?;
    if let Some((kind, existing_legacy_id, adapter)) = collision {
        anyhow::ensure!(
            kind == "custom_account" && existing_legacy_id == legacy_id && adapter == "http",
            "Custom destination stable id collides with an incompatible destination"
        );
        conn.execute(
            "UPDATE destinations SET
                 name = ?2, base_url = ?3, protocols_json = ?4,
                 auth_scheme = ?5, model_resolution = 'public_only',
                 capabilities_json = ?6, max_credentials = CASE WHEN ?5 = 'none' THEN 1 ELSE NULL END, enabled = ?7
             WHERE id = ?1",
            params![
                destination_id,
                name,
                endpoint_url,
                serde_json::to_string(&[protocol])?,
                auth_scheme.as_str(),
                serde_json::to_string(&mapped.capabilities)?,
                i64::from(enabled),
            ],
        )?;
    } else {
        let conflicting_id: Option<String> = conn
            .query_row(
                "SELECT id FROM destinations
                 WHERE legacy_kind = 'custom_account' AND legacy_id = ?1",
                [legacy_id],
                |row| row.get(0),
            )
            .optional()?;
        anyhow::ensure!(
            conflicting_id.is_none(),
            "Custom destination legacy identity collides with another stable id"
        );
        conn.execute(
            "INSERT INTO destinations (
                 id, legacy_kind, legacy_id, adapter, name, brand_family,
                 base_url, protocols_json, auth_scheme, model_resolution,
                 capabilities_json, plan_json, max_credentials,
                 observer_credential_id, enabled
             ) VALUES (?1, 'custom_account', ?2, 'http', ?3, NULL, ?4, ?5,
                       ?6, 'public_only', ?7, NULL, CASE WHEN ?6 = 'none' THEN 1 ELSE NULL END, NULL, ?8)",
            params![
                destination_id,
                legacy_id,
                name,
                endpoint_url,
                serde_json::to_string(&[protocol])?,
                auth_scheme.as_str(),
                serde_json::to_string(&mapped.capabilities)?,
                i64::from(enabled),
            ],
        )?;
    }
    replace_custom_destination_definition_on(
        conn,
        destination_id,
        name,
        &endpoint_url,
        protocol,
        auth_scheme,
        &models,
        Utc::now(),
    )?;
    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn replace_custom_destination_definition_on(
    conn: &Connection,
    destination_id: &str,
    name: &str,
    endpoint_url: &str,
    protocol: UpstreamProtocolKind,
    auth_scheme: AuthScheme,
    models: &[DynamicModelMapping],
    updated_at: DateTime<Utc>,
) -> Result<Vec<(String, String)>> {
    let _existing = custom_destination_for_update_on(conn, destination_id)?
        .ok_or_else(|| anyhow::anyhow!("custom destination not found"))?;
    let endpoint_url = validate_custom_endpoint_url(endpoint_url)?;
    let protocols_json = serde_json::to_string(&[protocol])?;
    conn.execute(
        "UPDATE destinations
         SET name = ?2, base_url = ?3, protocols_json = ?4, auth_scheme = ?5,
             model_resolution = 'public_only', max_credentials = CASE WHEN ?5 = 'none' THEN 1 ELSE NULL END, updated_at = ?6
         WHERE id = ?1 AND legacy_kind = 'custom_account' AND adapter = 'http'",
        params![
            destination_id,
            name,
            endpoint_url,
            protocols_json,
            auth_scheme.as_str(),
            updated_at.to_rfc3339(),
        ],
    )?;
    conn.execute(
        "DELETE FROM destination_models WHERE destination_id = ?1",
        [destination_id],
    )?;
    let mut insert = conn.prepare(
        "INSERT INTO destination_models (
            destination_id, public_model, public_model_key, upstream_model,
            protocols_json, preferred, enabled, upstream_override
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, 1, ?7)",
    )?;
    for model in models {
        let effective_protocol = model
            .upstream_override
            .as_ref()
            .map(|route| route.protocol)
            .unwrap_or(protocol);
        insert.execute(params![
            destination_id,
            model.public_model,
            model.public_model.to_ascii_lowercase(),
            model.upstream_model,
            serde_json::to_string(&[effective_protocol])?,
            effective_protocol.as_str(),
            model
                .upstream_override
                .as_ref()
                .map(serde_json::to_string)
                .transpose()?,
        ])?;
    }
    drop(insert);
    let mut stmt = conn.prepare(
        "SELECT legacy_account_id, id FROM credentials
         WHERE destination_id = ?1
           AND COALESCE(credential_purpose, 'inference') = 'inference'
         ORDER BY routing_rank ASC, created_at ASC, legacy_account_id ASC",
    )?;
    let credentials = stmt
        .query_map([destination_id], |row| Ok((row.get(0)?, row.get(1)?)))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(credentials)
}

pub(crate) fn delete_empty_custom_destination_on(
    conn: &Connection,
    destination_id: &str,
) -> Result<()> {
    let exists: i64 = conn.query_row(
        "SELECT COUNT(*) FROM destinations
         WHERE id = ?1 AND legacy_kind = 'custom_account' AND adapter = 'http'",
        [destination_id],
        |row| row.get(0),
    )?;
    anyhow::ensure!(exists == 1, "custom destination not found");
    let credentials: i64 = conn.query_row(
        "SELECT COUNT(*) FROM credentials WHERE destination_id = ?1",
        [destination_id],
        |row| row.get(0),
    )?;
    anyhow::ensure!(
        credentials == 0,
        "custom destination still has {credentials} credential(s)"
    );
    conn.execute(
        "DELETE FROM destination_models WHERE destination_id = ?1",
        [destination_id],
    )?;
    conn.execute("DELETE FROM destinations WHERE id = ?1", [destination_id])?;
    Ok(())
}

fn load_destination_first_protocol(
    conn: &Connection,
    destination_id: &str,
) -> Result<Option<UpstreamProtocolKind>> {
    let protocols_json: Option<String> = conn
        .query_row(
            "SELECT protocols_json FROM destinations WHERE id = ?1",
            [destination_id],
            |row| row.get(0),
        )
        .optional()?;
    let Some(protocols_json) = protocols_json else {
        return Ok(None);
    };
    let protocols: Vec<UpstreamProtocolKind> = serde_json::from_str(&protocols_json)
        .map_err(|error| anyhow::anyhow!("invalid destinations.protocols_json: {error}"))?;
    Ok(protocols.into_iter().next())
}

fn load_destination_model_inputs(
    conn: &Connection,
    destination_id: &str,
    skip_unknown: bool,
) -> Result<Vec<AccountModelCapabilityInput>> {
    if !destination_exists(conn, destination_id)? {
        return Ok(Vec::new());
    }
    let mut stmt = conn.prepare(
        "SELECT public_model, upstream_model,
                CASE WHEN protocols_json = '[]' THEN (SELECT protocols_json FROM destinations WHERE id = ?1) ELSE protocols_json END
         FROM destination_models
         WHERE destination_id = ?1
         ORDER BY rowid ASC",
    )?;
    let rows = stmt
        .query_map([destination_id], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    drop(stmt);
    let mut inputs = Vec::new();
    for (public_model, upstream_model, protocols_json) in rows {
        let protocols: Vec<UpstreamProtocolKind> = match serde_json::from_str(&protocols_json) {
            Ok(value) => value,
            Err(_) if skip_unknown => continue,
            Err(error) => {
                return Err(anyhow::anyhow!(
                    "invalid destination_models.protocols_json: {error}"
                ));
            }
        };
        for protocol in protocols {
            inputs.push(AccountModelCapabilityInput {
                public_model: public_model.clone(),
                upstream_model: upstream_model.clone(),
                protocol,
                source: Some("manual".into()),
            });
        }
    }
    Ok(inputs)
}

fn unique_public_models_from_inputs(capabilities: &[AccountModelCapabilityInput]) -> Vec<String> {
    let mut models = Vec::new();
    for capability in capabilities {
        if !models
            .iter()
            .any(|model: &String| model.eq_ignore_ascii_case(&capability.public_model))
        {
            models.push(capability.public_model.clone());
        }
    }
    models
}

fn credential_model_scope_on(conn: &Connection, account_id: &str) -> Result<ModelScope> {
    if table_exists(conn, "credentials")?
        && table_has_column(conn, "credentials", "scope_json")?
        && let Some(raw) = conn
            .query_row(
                "SELECT scope_json FROM credentials WHERE legacy_account_id = ?1",
                [account_id],
                |row| row.get::<_, Option<String>>(0),
            )
            .optional()?
            .flatten()
        && !raw.trim().is_empty()
    {
        return Ok(parse_scope_json(&raw));
    }
    if table_exists(conn, "credential_bindings")?
        && table_has_column(conn, "credential_bindings", "model_scope")?
        && let Some(raw) = conn
            .query_row(
                "SELECT model_scope FROM credential_bindings WHERE account_id = ?1",
                [account_id],
                |row| row.get::<_, Option<String>>(0),
            )
            .optional()?
            .flatten()
        && !raw.trim().is_empty()
    {
        return Ok(parse_scope_json(&raw));
    }
    Ok(ModelScope::All)
}

fn persist_credential_model_scope_on(
    conn: &Connection,
    account_id: &str,
    scope: &ModelScope,
) -> Result<()> {
    let json = serde_json::to_string(scope)?;
    if table_exists(conn, "credentials")? && table_has_column(conn, "credentials", "scope_json")? {
        conn.execute(
            "UPDATE credentials SET scope_json = ?2 WHERE legacy_account_id = ?1",
            params![account_id, json],
        )?;
    }
    if table_exists(conn, "credential_bindings")?
        && table_has_column(conn, "credential_bindings", "model_scope")?
    {
        conn.execute(
            "UPDATE credential_bindings SET model_scope = ?2 WHERE account_id = ?1",
            params![account_id, json],
        )?;
    }
    Ok(())
}

fn narrow_credential_scope_intersect(
    conn: &Connection,
    account_id: &str,
    models: &[String],
) -> Result<()> {
    let existing = credential_model_scope_on(conn, account_id)?;
    persist_credential_model_scope_on(
        conn,
        account_id,
        &intersect_scope_with_models(&existing, models),
    )
}

fn intersect_scope_with_models(existing: &ModelScope, models: &[String]) -> ModelScope {
    let discovered = unique_public_models_from_names(models);
    match existing {
        ModelScope::All => ModelScope::Only { models: discovered },
        ModelScope::Only { models: current } => ModelScope::Only {
            models: current
                .iter()
                .filter(|model| {
                    discovered.iter().any(|discovered| {
                        normalize_model_name(model) == normalize_model_name(discovered)
                    })
                })
                .cloned()
                .collect(),
        },
    }
}

fn unique_public_models_from_names(models: &[String]) -> Vec<String> {
    let mut unique = Vec::new();
    for model in models {
        if !unique
            .iter()
            .any(|existing: &String| existing.eq_ignore_ascii_case(model))
        {
            unique.push(model.clone());
        }
    }
    unique
}

fn parse_scope_json(raw: &str) -> ModelScope {
    serde_json::from_str(raw).unwrap_or(ModelScope::All)
}

fn referenced_catalog_models(
    conn: &Connection,
    destination_id: &str,
    this_account: &str,
    this_scope: &ModelScope,
) -> Result<Option<HashSet<String>>> {
    let mut referenced = HashSet::new();
    if !push_scope_models(this_scope, &mut referenced) {
        return Ok(None);
    }
    for sibling in linked_account_ids_for_destination(conn, destination_id)? {
        if sibling == this_account {
            continue;
        }
        let scope = credential_model_scope_on(conn, &sibling)?;
        if !push_scope_models(&scope, &mut referenced) {
            return Ok(None);
        }
    }
    Ok(Some(referenced))
}

fn push_scope_models(scope: &ModelScope, into: &mut HashSet<String>) -> bool {
    match scope {
        ModelScope::All => false,
        ModelScope::Only { models } => {
            for model in models {
                let key = normalize_model_name(model);
                if !key.is_empty() {
                    into.insert(key);
                }
            }
            true
        }
    }
}

fn linked_account_ids_for_destination(
    conn: &Connection,
    destination_id: &str,
) -> Result<Vec<String>> {
    if !table_exists(conn, "credentials")?
        || !table_has_column(conn, "credentials", "destination_id")?
    {
        return Ok(Vec::new());
    }
    let purpose_filter = if table_has_column(conn, "credentials", "credential_purpose")? {
        "AND COALESCE(credential_purpose, 'inference') = 'inference'"
    } else {
        ""
    };
    let mut stmt = conn.prepare(&format!(
        "SELECT legacy_account_id FROM credentials
         WHERE destination_id = ?1
           {purpose_filter}"
    ))?;
    stmt.query_map([destination_id], |row| row.get(0))?
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(Into::into)
}

fn destination_exists(conn: &Connection, destination_id: &str) -> Result<bool> {
    Ok(conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM destinations WHERE id = ?1)",
        [destination_id],
        |row| row.get::<_, i64>(0),
    )? != 0)
}

fn custom_inference_account_ids(conn: &Connection) -> Result<Vec<String>> {
    if table_exists(conn, "credentials")? && table_has_column(conn, "credentials", "provider_id")? {
        let purpose_filter = if table_has_column(conn, "credentials", "credential_purpose")? {
            "AND COALESCE(credential_purpose, 'inference') = 'inference'"
        } else {
            ""
        };
        let mut stmt = conn.prepare(&format!(
            "SELECT legacy_account_id FROM credentials
             WHERE provider_id = ?1
               {purpose_filter}
             ORDER BY COALESCE(routing_rank, 0) ASC, COALESCE(created_at, '') ASC, legacy_account_id ASC"
        ))?;
        return stmt
            .query_map([CUSTOM_PROVIDER_ID], |row| row.get(0))?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(Into::into);
    }
    if table_exists(conn, "accounts")? && table_has_column(conn, "accounts", "provider_id")? {
        let mut stmt = conn.prepare(
            "SELECT id FROM accounts
             WHERE provider_id = ?1
             ORDER BY id ASC",
        )?;
        return stmt
            .query_map([CUSTOM_PROVIDER_ID], |row| row.get(0))?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(Into::into);
    }
    Ok(Vec::new())
}

fn linked_account_ids(conn: &Connection) -> Result<HashSet<String>> {
    if table_exists(conn, "credentials")?
        && table_exists(conn, "destinations")?
        && table_has_column(conn, "credentials", "group_json")?
    {
        let purpose_filter = if table_has_column(conn, "credentials", "credential_purpose")? {
            "AND COALESCE(c.credential_purpose, 'inference') = 'inference'"
        } else {
            ""
        };
        let mut stmt = conn.prepare(&format!(
            "SELECT c.legacy_account_id
             FROM credentials c
             JOIN destinations d ON d.id = c.destination_id
             WHERE d.legacy_kind = 'platform_parent'
               AND c.group_json IS NOT NULL
               {purpose_filter}"
        ))?;
        let rows = stmt
            .query_map([], |row| row.get::<_, String>(0))?
            .collect::<rusqlite::Result<HashSet<_>>>()?;
        if !rows.is_empty() || !table_exists(conn, "platform_links")? {
            return Ok(rows);
        }
    }
    if !table_exists(conn, "platform_links")? {
        return Ok(HashSet::new());
    }
    let mut stmt = conn.prepare("SELECT account_id FROM platform_links")?;
    let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
    rows.collect::<rusqlite::Result<HashSet<_>>>()
        .map_err(Into::into)
}

pub(crate) fn platform_parent_id(conn: &Connection, account_id: &str) -> Result<Option<String>> {
    if table_exists(conn, "credentials")? && table_exists(conn, "destinations")? {
        let purpose_filter = if table_has_column(conn, "credentials", "credential_purpose")? {
            "AND COALESCE(c.credential_purpose, 'inference') = 'inference'"
        } else {
            ""
        };
        let from_dest: Option<String> = conn
            .query_row(
                &format!(
                    "SELECT d.legacy_id
                     FROM credentials c
                     JOIN destinations d ON d.id = c.destination_id
                     WHERE c.legacy_account_id = ?1
                       AND d.legacy_kind = 'platform_parent'
                       {purpose_filter}"
                ),
                [account_id],
                |row| row.get(0),
            )
            .optional()?;
        if from_dest.is_some() {
            return Ok(from_dest);
        }
    }
    if table_exists(conn, "platform_links")? {
        return Ok(conn
            .query_row(
                "SELECT platform_account_id FROM platform_links WHERE account_id = ?1",
                [account_id],
                |row| row.get(0),
            )
            .optional()?);
    }
    Ok(None)
}

fn credential_provider_id(conn: &Connection, account_id: &str) -> Result<Option<String>> {
    if table_exists(conn, "credentials")?
        && let Some(value) = conn
            .query_row(
                "SELECT provider_id FROM credentials WHERE legacy_account_id = ?1",
                [account_id],
                |row| row.get::<_, Option<String>>(0),
            )
            .optional()?
            .flatten()
    {
        return Ok(Some(value));
    }
    if table_exists(conn, "accounts")? {
        return Ok(conn
            .query_row(
                "SELECT provider_id FROM accounts WHERE id = ?1",
                [account_id],
                |row| row.get(0),
            )
            .optional()?);
    }
    Ok(None)
}

fn credential_name(conn: &Connection, account_id: &str) -> Result<Option<String>> {
    Ok(conn
        .query_row(
            "SELECT name FROM credentials WHERE legacy_account_id = ?1",
            [account_id],
            |row| row.get(0),
        )
        .optional()?)
}

fn credential_timestamps(
    conn: &Connection,
    account_id: &str,
) -> Result<(DateTime<Utc>, DateTime<Utc>)> {
    let row: Option<(Option<String>, Option<String>)> = conn
        .query_row(
            "SELECT created_at, updated_at FROM credentials WHERE legacy_account_id = ?1",
            [account_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    let now = Utc::now();
    let Some((created, updated)) = row else {
        return Ok((now, now));
    };
    Ok((
        created.map(parse_stored_datetime).unwrap_or(now),
        updated.map(parse_stored_datetime).unwrap_or(now),
    ))
}

fn parse_stored_datetime(value: String) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(&value)
        .map(|parsed| parsed.with_timezone(&Utc))
        .unwrap_or_else(|_| Utc::now())
}
