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
use ocg_domain::destination::{
    CatalogModel, LegacyDestinationFacts, destination_from_legacy,
    destination_id_for_custom_account, destination_id_for_platform_account,
};
use ocg_domain::ids::CUSTOM_PROVIDER_ID;
use rusqlite::{Connection, OptionalExtension, params};
use std::collections::{HashMap, HashSet};

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
        upsert_custom_destination(conn, &account_id, &name, endpoint, protocol, &[])?;
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
        let merged = merge_leftover_destination_models(&dest_id, capabilities)?;
        replace_destination_models(conn, &dest_id, &merged)?;
    }
    Ok(())
}

/// Union leftover Key catalogs that share a destination. Identical rows
/// collapse; the same public model with a different upstream mapping is a
/// hard refusal so upgrade does not pick a winner by walk order.
fn merge_leftover_destination_models(
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
                "v53 refuses leftover capability for `{destination_id}`: `{}` maps to conflicting upstream models",
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

pub(crate) fn persist_custom_config_on(
    conn: &Connection,
    account_id: &str,
    input: &AccountCustomConfigInput,
) -> Result<bool> {
    let endpoint_url = validate_custom_endpoint_url(&input.endpoint_url)?;
    if let Some(parent_id) = platform_parent_id(conn, account_id)? {
        merge_custom_models_onto_platform_parent(conn, account_id, &parent_id)?;
        return Ok(false);
    }
    let existing = load_custom_destination_endpoint(conn, account_id)?;
    let endpoint_changed = existing.as_ref().is_some_and(|(url, protocol)| {
        url != &endpoint_url || *protocol != input.upstream_protocol
    });
    let name = credential_name(conn, account_id)?.unwrap_or_else(|| account_id.to_string());
    let existing_models =
        load_destination_model_inputs(conn, &destination_id_for_custom_account(account_id), true)?;
    upsert_custom_destination(
        conn,
        account_id,
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
    replace_destination_models(conn, &dest_id, capabilities)?;
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
        "SELECT public_model, upstream_model, protocols_json
         FROM destination_models
         WHERE destination_id = ?1
         ORDER BY rowid ASC",
    )?;
    let rows = stmt
        .query_map([&dest_id], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    drop(stmt);
    let mut capabilities = Vec::new();
    for (public_model, upstream_model, protocols_json) in rows {
        let protocols: Vec<UpstreamProtocolKind> = match serde_json::from_str(&protocols_json) {
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
    if !declared_order {
        capabilities.sort_by(|left, right| {
            left.public_model
                .cmp(&right.public_model)
                .then_with(|| left.protocol.as_str().cmp(right.protocol.as_str()))
        });
    }
    Ok(capabilities)
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
    let dest_id = destination_id_for_custom_account(account_id);
    conn.execute(
        "DELETE FROM destination_models WHERE destination_id = ?1",
        [&dest_id],
    )?;
    conn.execute("DELETE FROM destinations WHERE id = ?1", [&dest_id])?;
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
    let name = credential_name(conn, account_id)?.unwrap_or_else(|| account_id.to_string());
    upsert_custom_destination(conn, account_id, &name, &endpoint, protocol, &models)?;
    Ok(())
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
    let dest_id = destination_id_for_custom_account(account_id);
    if destination_exists(conn, &dest_id)? {
        Ok(Some(dest_id))
    } else {
        Ok(None)
    }
}

fn upsert_custom_destination(
    conn: &Connection,
    account_id: &str,
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
        account_id: account_id.to_string(),
        name: name.to_string(),
        endpoint_url: endpoint_url.to_string(),
        protocol,
        model_capabilities: pairs,
    })
    .map_err(|error| anyhow::anyhow!("{error}"))?;
    let dest_id = destination.id.clone();
    if destination_exists(conn, &dest_id)? {
        conn.execute(
            "UPDATE destinations
             SET name = ?2, base_url = ?3, protocols_json = ?4, auth_scheme = ?5,
                 adapter = ?6, capabilities_json = ?7, max_credentials = ?8, enabled = ?9
             WHERE id = ?1",
            params![
                dest_id,
                destination.name,
                destination.base_url,
                serde_json::to_string(&destination.protocols)?,
                destination.auth_scheme.as_str(),
                destination.adapter.as_str(),
                serde_json::to_string(&destination.capabilities)?,
                destination.max_credentials.map(i64::from),
                i64::from(destination.enabled),
            ],
        )?;
    } else {
        conn.execute(
            "INSERT INTO destinations (
                id, legacy_kind, legacy_id, adapter, name, brand_family, base_url,
                protocols_json, auth_scheme, capabilities_json, plan_json,
                max_credentials, observer_credential_id, enabled
             ) VALUES (?1, 'custom_account', ?2, ?3, ?4, NULL, ?5, ?6, ?7, ?8, NULL, ?9, NULL, ?10)",
            params![
                dest_id,
                account_id,
                destination.adapter.as_str(),
                destination.name,
                destination.base_url,
                serde_json::to_string(&destination.protocols)?,
                destination.auth_scheme.as_str(),
                serde_json::to_string(&destination.capabilities)?,
                destination.max_credentials.map(i64::from),
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
        });
    }
    conn.execute(
        "DELETE FROM destination_models WHERE destination_id = ?1",
        [destination_id],
    )?;
    for model in models {
        conn.execute(
            "INSERT INTO destination_models (
                destination_id, public_model, public_model_key, upstream_model,
                protocols_json, preferred, enabled
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
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
    let custom_id = destination_id_for_custom_account(account_id);
    let parent_id = destination_id_for_platform_account(parent_id);
    if !destination_exists(conn, &custom_id)? {
        return Ok(());
    }
    let custom_models = load_destination_model_inputs(conn, &custom_id, true)?;
    if custom_models.is_empty() {
        return Ok(());
    }
    let mut merged = load_destination_model_inputs(conn, &parent_id, true)?;
    let mut seen: HashSet<String> = merged
        .iter()
        .map(|row| row.public_model.to_ascii_lowercase())
        .collect();
    for model in custom_models {
        if seen.insert(model.public_model.to_ascii_lowercase()) {
            merged.push(model);
        }
    }
    if destination_exists(conn, &parent_id)? {
        replace_destination_models(conn, &parent_id, &merged)?;
    }
    Ok(())
}

fn load_custom_destination_endpoint(
    conn: &Connection,
    account_id: &str,
) -> Result<Option<(String, UpstreamProtocolKind)>> {
    let dest_id = destination_id_for_custom_account(account_id);
    let row: Option<(Option<String>, String)> = conn
        .query_row(
            "SELECT base_url, protocols_json FROM destinations
             WHERE legacy_kind = 'custom_account' AND legacy_id = ?1",
            [account_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    let Some((base_url, protocols_json)) = row else {
        let _ = dest_id;
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
        "SELECT public_model, upstream_model, protocols_json
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

fn destination_exists(conn: &Connection, destination_id: &str) -> Result<bool> {
    Ok(conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM destinations WHERE id = ?1)",
        [destination_id],
        |row| row.get::<_, i64>(0),
    )? != 0)
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
    if table_exists(conn, "credentials")? {
        if let Some(value) = conn
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
