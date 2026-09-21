//! Incremental destination and catalog writes for the current schema.
//!
//! Runtime writers persist `destinations` / `destination_models` in place.
//! `project()` / `replace_all_on` stay on leftover-table migration and V4–V6
//! backup conversion.

use super::*;
use crate::provider_contracts::{EffectiveScopeContract, build_effective_contracts};
use ocg_domain::destination::{
    CatalogModel, Destination, LegacyDestinationFacts, LegacyDestinationRef,
    destination_from_legacy, destination_id_for_builtin,
};
use ocg_domain::ids::OPENCODE_ZEN_FREE_PROVIDER_ID;
use rusqlite::{Connection, OptionalExtension, params};
use std::collections::HashSet;

pub(crate) fn destination_exists(conn: &Connection, destination_id: &str) -> Result<bool> {
    Ok(conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM destinations WHERE id = ?1)",
        [destination_id],
        |row| row.get::<_, i64>(0),
    )? != 0)
}

/// Insert the sealed builtin destination row when missing. Existing extras
/// columns are left untouched.
pub(crate) fn ensure_builtin_destination(conn: &Connection, provider_id: &str) -> Result<String> {
    let destination = destination_from_legacy(&LegacyDestinationFacts::Builtin {
        provider_id: provider_id.to_string(),
    })
    .map_err(|error| anyhow::anyhow!("{error}"))?;
    let dest_id = destination.id.clone();
    if !destination_exists(conn, &dest_id)? {
        insert_destination_row(conn, &destination)?;
    }
    Ok(dest_id)
}

pub(crate) fn ensure_zen_destination(conn: &Connection) -> Result<String> {
    ensure_builtin_destination(conn, OPENCODE_ZEN_FREE_PROVIDER_ID)
}

/// Rewrite `destination_models` for one destination. Caller owns the transaction.
pub(crate) fn replace_destination_catalog(
    conn: &Connection,
    destination_id: &str,
    models: &[CatalogModel],
) -> Result<()> {
    conn.execute(
        "DELETE FROM destination_models WHERE destination_id = ?1",
        [destination_id],
    )?;
    for model in models {
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

/// Merge a portable platform catalog into an existing destination without
/// deleting target-only models. Matching public names must keep one upstream
/// identity; protocol evidence is unioned and enabled state is monotonic.
pub(crate) fn merge_destination_catalog_refuse_conflict(
    conn: &Connection,
    destination_id: &str,
    incoming: &[CatalogModel],
) -> Result<()> {
    let mut merged = load_destination_catalog(conn, destination_id)?;
    for model in incoming {
        if let Some(existing) = merged.iter_mut().find(|existing| {
            existing
                .public_model
                .eq_ignore_ascii_case(&model.public_model)
        }) {
            anyhow::ensure!(
                existing
                    .upstream_model
                    .eq_ignore_ascii_case(&model.upstream_model),
                "destination `{destination_id}` refuses model `{}`: conflicting upstream mappings",
                model.public_model
            );
            for protocol in &model.protocols {
                if !existing.protocols.contains(protocol) {
                    existing.protocols.push(*protocol);
                }
            }
            if existing.preferred.is_none() {
                existing.preferred = model.preferred;
            }
            existing.enabled |= model.enabled;
            if existing.upstream_override.is_none() {
                existing.upstream_override = model.upstream_override.clone();
            } else if model.upstream_override.is_some() {
                anyhow::ensure!(
                    existing.upstream_override == model.upstream_override,
                    "destination `{destination_id}` refuses model `{}`: conflicting upstream route overrides",
                    model.public_model
                );
            }
        } else {
            merged.push(model.clone());
        }
    }
    replace_destination_catalog(conn, destination_id, &merged)
}

pub(crate) fn load_destination_catalog(
    conn: &Connection,
    destination_id: &str,
) -> Result<Vec<CatalogModel>> {
    let mut stmt = conn.prepare(
        "SELECT public_model, upstream_model, protocols_json, preferred, enabled, upstream_override
         FROM destination_models WHERE destination_id = ?1 ORDER BY rowid ASC",
    )?;
    let rows = stmt.query_map([destination_id], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, Option<String>>(3)?,
            row.get::<_, i64>(4)?,
            row.get::<_, Option<String>>(5)?,
        ))
    })?;
    let mut catalog = Vec::new();
    for row in rows {
        let (public_model, upstream_model, protocols_json, preferred, enabled, upstream_override) =
            row?;
        catalog.push(CatalogModel {
            public_model,
            upstream_model,
            protocols: serde_json::from_str(&protocols_json)?,
            preferred: preferred
                .as_deref()
                .map(ocg_domain::catalog::UpstreamProtocolKind::try_from)
                .transpose()?,
            enabled: enabled != 0,
            upstream_override: upstream_override
                .as_deref()
                .map(serde_json::from_str)
                .transpose()?,
        });
    }
    Ok(catalog)
}

/// Seed newly available catalog rows. Existing routing choices belong to the
/// destination and are never rewritten by unrelated account writes or refresh.
pub(crate) fn sync_builtin_catalogs(db: &Database) -> Result<()> {
    let zen = db.zen_free_model_catalog()?.unwrap_or_default();
    let persisted = db.load_persisted_contracts()?;
    let contracts = build_effective_contracts(&zen, &[], persisted);
    for scope in contracts.providers.values() {
        let dest_id = destination_id_for_builtin(&scope.provider_id);
        if !destination_exists(&db.conn, &dest_id)? {
            continue;
        }
        let mut catalog = load_destination_catalog(&db.conn, &dest_id)?;
        for model in catalog_from_persisted_scope(scope) {
            if !catalog.iter().any(|existing| {
                existing
                    .public_model
                    .eq_ignore_ascii_case(&model.public_model)
            }) {
                catalog.push(model);
            }
        }
        replace_destination_catalog(&db.conn, &dest_id, &catalog)?;
    }
    Ok(())
}

/// Explicit catalog replacement preserves controls for surviving models while
/// removing upstream models that are no longer in the refreshed directory.
pub(crate) fn refresh_builtin_catalog(
    db: &Database,
    scope: &crate::provider_contracts::ContractScope,
) -> Result<()> {
    let crate::provider_contracts::ContractScope::Provider(id) = scope else {
        return Ok(());
    };
    if crate::provider::builtin_provider(id).is_none() {
        return Ok(());
    }
    let dest_id = ensure_builtin_destination(&db.conn, id)?;
    let zen = db.zen_free_model_catalog()?.unwrap_or_default();
    let contracts = build_effective_contracts(&zen, &[], db.load_persisted_contracts()?);
    let Some(scope) = contracts.scope(scope) else {
        return Ok(());
    };
    let previous = load_destination_catalog(&db.conn, &dest_id)?;
    let catalog = catalog_from_persisted_scope(scope)
        .into_iter()
        .map(|next| {
            previous
                .iter()
                .find(|model| model.public_model.eq_ignore_ascii_case(&next.public_model))
                .cloned()
                .unwrap_or(next)
        })
        .collect::<Vec<_>>();
    replace_destination_catalog(&db.conn, &dest_id, &catalog)
}

pub(crate) fn seed_missing_builtin_catalogs(db: &Database) -> Result<()> {
    let missing: bool = db.conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM destinations d
         JOIN provider_contract_scopes s ON s.scope_kind = 'provider' AND s.scope_id = d.legacy_id
         WHERE d.legacy_kind = 'builtin' AND s.catalog_models_json != '[]'
           AND NOT EXISTS(SELECT 1 FROM destination_models m WHERE m.destination_id = d.id))",
        [],
        |row| row.get(0),
    )?;
    if missing {
        sync_builtin_catalogs(db)?;
    }
    Ok(())
}

/// Explicit protocol-control writes update their destination model rows in the
/// caller's transaction. Evidence remains available for future Auto decisions,
/// but is no longer a second request-time authority.
pub(crate) fn apply_scope_controls(
    db: &Database,
    scope: &crate::provider_contracts::ContractScope,
    model_ids: Option<&[String]>,
) -> Result<()> {
    use crate::provider_contracts::ContractScope;
    let dest_id = match scope {
        ContractScope::Provider(id) => destination_id_for_builtin(id),
        ContractScope::CustomEndpoint(id) => {
            let id: Option<String> = db
                .conn
                .query_row(
                    "SELECT destination_id FROM credentials WHERE legacy_account_id = ?1",
                    [id],
                    |row| row.get(0),
                )
                .optional()?;
            let Some(id) = id else {
                return Ok(());
            };
            id
        }
    };
    if !destination_exists(&db.conn, &dest_id)? {
        return Ok(());
    }
    let zen = db.zen_free_model_catalog()?.unwrap_or_default();
    let custom = db.list_custom_account_runtimes()?;
    let contracts = build_effective_contracts(&zen, &custom, db.load_persisted_contracts()?);
    let Some(contract) = contracts.scope(scope) else {
        return Ok(());
    };
    let mut catalog = load_destination_catalog(&db.conn, &dest_id)?;
    for model in &mut catalog {
        if model_ids.is_some_and(|ids| {
            !ids.iter()
                .any(|id| id.eq_ignore_ascii_case(&model.public_model))
        }) {
            continue;
        }
        if let Some(effective) = contract.model(&model.public_model) {
            model.protocols = effective.enabled_protocols();
            model.preferred = Some(effective.preferred_protocol);
            model.enabled = effective.has_enabled_protocol();
        }
    }
    replace_destination_catalog(&db.conn, &dest_id, &catalog)
}

pub(crate) fn remove_scope_models(
    db: &Database,
    scope: &crate::provider_contracts::ContractScope,
    ids: &[String],
) -> Result<()> {
    let dest_id = match scope {
        crate::provider_contracts::ContractScope::Provider(id) => destination_id_for_builtin(id),
        crate::provider_contracts::ContractScope::CustomEndpoint(id) => {
            let id: Option<String> = db
                .conn
                .query_row(
                    "SELECT destination_id FROM credentials WHERE legacy_account_id = ?1",
                    [id],
                    |row| row.get(0),
                )
                .optional()?;
            let Some(id) = id else {
                return Ok(());
            };
            id
        }
    };
    for id in ids {
        db.conn.execute(
            "DELETE FROM destination_models WHERE destination_id = ?1 AND public_model_key = ?2",
            params![dest_id, id.to_ascii_lowercase()],
        )?;
    }
    Ok(())
}

/// One-time conversion of per-account protocol judgments. Disagreement cannot
/// be silently unioned because it could authorize a sibling Key's protocol.
pub(crate) fn migrate_custom_protocol_controls(db: &Database) -> Result<()> {
    let projection = crate::destination_projection::load_persisted(db)?;
    let zen = db.zen_free_model_catalog()?.unwrap_or_default();
    let runtimes = db.list_custom_account_runtimes()?;
    let contracts = build_effective_contracts(&zen, &runtimes, db.load_persisted_contracts()?);
    for destination in projection
        .destinations
        .iter()
        .filter(|destination| destination.adapter == ocg_domain::destination::AdapterKind::Http)
    {
        let mut catalog = destination.catalog.clone();
        for model in &mut catalog {
            let mut selected: Option<(Vec<UpstreamProtocolKind>, UpstreamProtocolKind)> = None;
            for credential in projection
                .credentials
                .iter()
                .filter(|credential| credential.destination_id == destination.id)
            {
                let Some(effective) = contracts
                    .custom_endpoints
                    .get(&credential.legacy_account_id)
                    .and_then(|scope| scope.model(&model.public_model))
                else {
                    continue;
                };
                let decision = (effective.enabled_protocols(), effective.preferred_protocol);
                if let Some(previous) = &selected {
                    anyhow::ensure!(
                        previous == &decision,
                        "destination `{}` model `{}` has conflicting credential protocol settings; resolve them before upgrading",
                        destination.id,
                        model.public_model
                    );
                } else {
                    selected = Some(decision);
                }
            }
            if let Some((protocols, preferred)) = selected {
                model.enabled &= !protocols.is_empty();
                model.protocols = protocols;
                model.preferred = Some(preferred);
            }
        }
        replace_destination_catalog(&db.conn, &destination.id, &catalog)?;
    }
    if destination_exists(&db.conn, &super::cpa::destination_id())?
        && let Some(catalog) = db.cpa_model_catalog()?
    {
        replace_destination_catalog(
            &db.conn,
            &super::cpa::destination_id(),
            &cpa_catalog(&catalog.models),
        )?;
    }
    Ok(())
}

pub(crate) fn cpa_catalog(models: &[super::CpaCatalogModel]) -> Vec<CatalogModel> {
    models
        .iter()
        .map(|model| CatalogModel {
            public_model: model.id.clone(),
            upstream_model: model.id.clone(),
            enabled: model.enabled,
            protocols: UpstreamProtocolKind::ALL.to_vec(),
            preferred: Some(UpstreamProtocolKind::ChatCompletions),
            upstream_override: None,
        })
        .collect()
}

fn insert_destination_row(conn: &Connection, destination: &Destination) -> Result<()> {
    let (legacy_kind, legacy_id) = match &destination.legacy {
        LegacyDestinationRef::Builtin(id) => ("builtin", id.as_str()),
        LegacyDestinationRef::Dynamic(id) => ("dynamic", id.as_str()),
        LegacyDestinationRef::CustomAccount(id) => ("custom_account", id.as_str()),
        LegacyDestinationRef::PlatformParent(id) => ("platform_parent", id.as_str()),
    };
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
    Ok(())
}

fn catalog_from_persisted_scope(scope: &EffectiveScopeContract) -> Vec<CatalogModel> {
    let mut catalog = Vec::new();
    let mut seen = HashSet::new();
    for model_id in &scope.catalog.models {
        let folded = model_id.to_ascii_lowercase();
        if !seen.insert(folded) {
            continue;
        }
        let Some(model) = scope.model(model_id) else {
            continue;
        };
        catalog.push(CatalogModel {
            public_model: model_id.clone(),
            upstream_model: model.model_id.clone(),
            protocols: model.enabled_protocols(),
            preferred: Some(model.preferred_protocol),
            enabled: model.has_enabled_protocol(),
            upstream_override: None,
        });
    }
    catalog
}
