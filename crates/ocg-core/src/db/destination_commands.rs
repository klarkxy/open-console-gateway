//! Transactional mutation for an existing configurable HTTP destination.
//!
//! The caller already owns the SQLite transaction. This module does not
//! begin or commit, and it does not invent connection identities.

use super::*;
use crate::custom::validate_custom_endpoint_url;
use crate::destination_projection::load_runtime;
use crate::dynamic::validate_definition;
use crate::provider_contracts::ContractScope;
use anyhow::{Context, Result, anyhow, bail, ensure};
use chrono::Utc;
use ocg_domain::connection::{ConnectionId, EndpointOperation};
use ocg_domain::credential::{RouteSpec, assigned_endpoints_for_routes, normalize_origin};
use ocg_domain::destination::{AdapterKind, AuthScheme, CatalogModel, Destination, Protocol};
use ocg_domain::dynamic::{DynamicAuthKind, DynamicModelMapping, DynamicProviderDefinition};
use rusqlite::{Connection, params};
use std::collections::HashSet;

struct InferenceBinding {
    credential_id: String,
    legacy_account_id: String,
    authorization_connection_id: Option<String>,
}

pub(crate) fn delete_http_destination_on(db: &Database, destination_id: &str) -> Result<()> {
    let destination = load_http_destination(db, destination_id)?;
    ensure!(
        destination.adapter == AdapterKind::Http && !destination.capabilities.observer,
        "sealed and platform-managed destinations cannot be deleted"
    );
    let count: i64 = db.conn.query_row(
        "SELECT COUNT(*) FROM credentials WHERE destination_id = ?1",
        [destination_id],
        |row| row.get(0),
    )?;
    ensure!(
        count == 0,
        "destination still has {count} referencing credentials"
    );
    db.conn.execute("DELETE FROM provider_pricing_snapshots WHERE provider_id = (SELECT legacy_id FROM destinations WHERE id = ?1)", [destination_id])?;
    db.conn.execute(
        "DELETE FROM destination_models WHERE destination_id = ?1",
        [destination_id],
    )?;
    db.conn
        .execute("DELETE FROM destinations WHERE id = ?1", [destination_id])?;
    Ok(())
}

/// Replace name, endpoint, protocol, auth, and mappings on one HTTP destination.
///
/// `authorize_credential_ids` is explicit grant consent. Duplicate or foreign
/// ids fail before any write. Connection ids come from
/// `credentials.authorization_connection_id`; they are never derived.
pub(crate) fn replace_http_destination_on(
    db: &Database,
    destination_id: &str,
    definition: &DynamicProviderDefinition,
    authorize_credential_ids: &[String],
) -> Result<()> {
    let definition = validate_definition(definition.clone()).map_err(|error| anyhow!("{error}"))?;
    let endpoint_url = validate_custom_endpoint_url(&definition.endpoint_url)?;
    let existing = load_http_destination(db, destination_id)?;
    ensure!(
        existing.adapter == AdapterKind::Http,
        "destination `{destination_id}` is not a configurable HTTP adapter"
    );
    ensure!(
        !existing.capabilities.observer,
        "managed observer destinations cannot be mutated here"
    );

    let bindings = load_inference_bindings(&db.conn, destination_id)?;
    validate_authorize_list(&bindings, destination_id, authorize_credential_ids)?;
    for credential_id in authorize_credential_ids {
        let binding = bindings
            .iter()
            .find(|binding| binding.credential_id == *credential_id)
            .expect("authorize list was validated");
        connection_id_for_binding(binding)?;
    }
    if definition.auth_kind.is_singleton() && bindings.len() > 1 {
        bail!("no-auth destinations require at most one credential");
    }

    let next_auth = AuthScheme::from(definition.auth_kind);
    let next_protocol = definition.upstream_protocol;
    let substantive = existing.base_url.as_deref() != Some(endpoint_url.as_str())
        || existing.protocols.first().copied() != Some(next_protocol)
        || existing.auth_scheme != next_auth
        || mappings_changed(&existing.catalog, &definition);

    let catalog = catalog_from_definition(&existing.catalog, &definition);
    let now = Utc::now();
    let now_rfc = now.to_rfc3339();
    let max_credentials = if definition.auth_kind.is_singleton() {
        Some(1_i64)
    } else {
        None
    };

    db.conn.execute(
        "UPDATE destinations
         SET name = ?2, base_url = ?3, protocols_json = ?4, auth_scheme = ?5,
             max_credentials = ?6, updated_at = ?7
         WHERE id = ?1",
        params![
            destination_id,
            definition.name,
            endpoint_url,
            serde_json::to_string(&[next_protocol])?,
            next_auth.as_str(),
            max_credentials,
            now_rfc,
        ],
    )?;
    destination_store::replace_destination_catalog(&db.conn, destination_id, &catalog)?;
    apply_auth_transition(
        &db.conn,
        destination_id,
        existing.auth_scheme,
        definition.auth_kind,
        &now_rfc,
    )?;
    if substantive {
        reset_verification_on(&db.conn, destination_id, &now_rfc)?;
        for binding in &bindings {
            invalidate_probe_evidence_on(
                &db.conn,
                &ContractScope::custom_endpoint(&binding.legacy_account_id),
                now,
            )?;
        }
    }
    union_authorized_grants(
        &db.conn,
        &bindings,
        authorize_credential_ids,
        &definition,
        &endpoint_url,
    )?;
    for binding in &bindings {
        account_store::sync_inference_credential_projection_on(
            &db.conn,
            &binding.legacy_account_id,
        )?;
    }
    Ok(())
}

fn load_http_destination(db: &Database, destination_id: &str) -> Result<Destination> {
    load_runtime(db)?
        .destinations
        .into_iter()
        .find(|destination| destination.id == destination_id)
        .ok_or_else(|| anyhow!("destination `{destination_id}` not found"))
}

fn load_inference_bindings(
    conn: &Connection,
    destination_id: &str,
) -> Result<Vec<InferenceBinding>> {
    let purpose = if table_has_column(conn, "credentials", "credential_purpose")? {
        " AND COALESCE(credential_purpose, 'inference') = 'inference'"
    } else {
        ""
    };
    let sql = format!(
        "SELECT id, legacy_account_id, authorization_connection_id
         FROM credentials
         WHERE destination_id = ?1{purpose}
         ORDER BY routing_rank ASC, created_at ASC, legacy_account_id ASC, id ASC"
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map([destination_id], |row| {
        Ok(InferenceBinding {
            credential_id: row.get(0)?,
            legacy_account_id: row.get(1)?,
            authorization_connection_id: row.get(2)?,
        })
    })?;
    rows.collect::<rusqlite::Result<Vec<_>>>()
        .map_err(Into::into)
}

fn validate_authorize_list(
    bindings: &[InferenceBinding],
    destination_id: &str,
    authorize_credential_ids: &[String],
) -> Result<()> {
    let mut seen = HashSet::new();
    for credential_id in authorize_credential_ids {
        ensure!(
            seen.insert(credential_id.as_str()),
            "authorizeCredentialIds contains duplicates"
        );
        ensure!(
            bindings
                .iter()
                .any(|binding| binding.credential_id == *credential_id),
            "credential `{credential_id}` does not belong to destination `{destination_id}`"
        );
    }
    Ok(())
}

fn mappings_changed(catalog: &[CatalogModel], definition: &DynamicProviderDefinition) -> bool {
    if catalog.len() != definition.mappings.len() {
        return true;
    }
    !catalog
        .iter()
        .zip(definition.mappings.iter())
        .all(|(model, mapping)| {
            model
                .public_model
                .eq_ignore_ascii_case(&mapping.public_model)
                && model
                    .upstream_model
                    .eq_ignore_ascii_case(&mapping.upstream_model)
                && model.upstream_override == mapping.upstream_override
        })
}

pub(crate) fn catalog_from_definition(
    existing: &[CatalogModel],
    definition: &DynamicProviderDefinition,
) -> Vec<CatalogModel> {
    definition
        .mappings
        .iter()
        .map(|mapping| catalog_model_for_mapping(existing, mapping, definition.upstream_protocol))
        .collect()
}

fn catalog_model_for_mapping(
    existing: &[CatalogModel],
    mapping: &DynamicModelMapping,
    default_protocol: Protocol,
) -> CatalogModel {
    let route_protocol = mapping
        .upstream_override
        .as_ref()
        .map(|route| route.protocol)
        .unwrap_or(default_protocol);
    let previous = existing.iter().find(|model| {
        model
            .public_model
            .eq_ignore_ascii_case(&mapping.public_model)
    });
    match previous {
        Some(previous)
            if previous
                .upstream_model
                .eq_ignore_ascii_case(&mapping.upstream_model)
                && previous.upstream_override == mapping.upstream_override
                && (mapping.upstream_override.is_some()
                    || existing_route_protocol(previous) == route_protocol) =>
        {
            CatalogModel {
                public_model: mapping.public_model.clone(),
                upstream_model: mapping.upstream_model.clone(),
                protocols: previous.protocols.clone(),
                preferred: previous.preferred,
                enabled: previous.enabled,
                upstream_override: mapping.upstream_override.clone(),
            }
        }
        Some(previous)
            if previous
                .upstream_model
                .eq_ignore_ascii_case(&mapping.upstream_model) =>
        {
            let (protocols, preferred) = protocol_after_route_change(previous, route_protocol);
            CatalogModel {
                public_model: mapping.public_model.clone(),
                upstream_model: mapping.upstream_model.clone(),
                protocols,
                preferred,
                enabled: previous.enabled,
                upstream_override: mapping.upstream_override.clone(),
            }
        }
        Some(_) | None => CatalogModel {
            public_model: mapping.public_model.clone(),
            upstream_model: mapping.upstream_model.clone(),
            protocols: vec![route_protocol],
            preferred: Some(route_protocol),
            enabled: previous.is_none_or(|model| model.enabled),
            upstream_override: mapping.upstream_override.clone(),
        },
    }
}

fn existing_route_protocol(existing: &CatalogModel) -> Protocol {
    existing
        .upstream_override
        .as_ref()
        .map(|route| route.protocol)
        .or(existing.preferred)
        .or_else(|| existing.protocols.first().copied())
        .unwrap_or(Protocol::ChatCompletions)
}

fn protocol_after_route_change(
    existing: &CatalogModel,
    new_protocol: Protocol,
) -> (Vec<Protocol>, Option<Protocol>) {
    let old_protocol = existing
        .upstream_override
        .as_ref()
        .map(|route| route.protocol)
        .or(existing.preferred)
        .or_else(|| existing.protocols.first().copied());
    let mut protocols = existing.protocols.clone();
    if let Some(old_protocol) = old_protocol {
        if old_protocol != new_protocol {
            protocols.retain(|protocol| *protocol != old_protocol);
            if !protocols.contains(&new_protocol) {
                protocols.push(new_protocol);
            }
        }
    } else if !protocols.contains(&new_protocol) {
        protocols.push(new_protocol);
    }
    if protocols.is_empty() {
        protocols.push(new_protocol);
    }
    let preferred = match existing.preferred {
        Some(preferred) if old_protocol == Some(preferred) && preferred != new_protocol => {
            Some(new_protocol)
        }
        Some(preferred) if protocols.contains(&preferred) => Some(preferred),
        _ => Some(new_protocol),
    };
    (protocols, preferred)
}

fn apply_auth_transition(
    conn: &Connection,
    destination_id: &str,
    previous: AuthScheme,
    next: DynamicAuthKind,
    now_rfc: &str,
) -> Result<()> {
    let purpose = inference_predicate(conn)?;
    conn.execute(
        &format!(
            "UPDATE credentials
             SET credential_kind = ?2, quota_scope = ?3, updated_at = ?4
             WHERE destination_id = ?1{purpose}"
        ),
        params![
            destination_id,
            next.credential_kind().as_str(),
            next.quota_scope().as_str(),
            now_rfc,
        ],
    )?;
    if !matches!(previous, AuthScheme::None) && !next.requires_key() {
        conn.execute(
            &format!(
                "UPDATE credentials
                 SET key_cipher = '',
                     has_secret = 0,
                     credential_version = COALESCE(credential_version, 1) + 1,
                     updated_at = ?2
                 WHERE destination_id = ?1{purpose}"
            ),
            params![destination_id, now_rfc],
        )?;
    }
    Ok(())
}

fn reset_verification_on(conn: &Connection, destination_id: &str, now_rfc: &str) -> Result<()> {
    let purpose = inference_predicate(conn)?;
    conn.execute(
        &format!(
            "UPDATE credentials SET
                auth_error = NULL,
                last_error = NULL,
                verification_status = CASE
                    WHEN verification_status = 'not_required' THEN verification_status
                    ELSE 'pending'
                END,
                connection_verified_at = NULL,
                verification_error = NULL,
                updated_at = ?2
             WHERE destination_id = ?1{purpose}"
        ),
        params![destination_id, now_rfc],
    )?;
    Ok(())
}

fn union_authorized_grants(
    conn: &Connection,
    bindings: &[InferenceBinding],
    authorize_credential_ids: &[String],
    definition: &DynamicProviderDefinition,
    endpoint_url: &str,
) -> Result<()> {
    if authorize_credential_ids.is_empty() {
        return Ok(());
    }
    let routes = configured_routes(definition, endpoint_url);
    for credential_id in authorize_credential_ids {
        let binding = bindings
            .iter()
            .find(|binding| binding.credential_id == *credential_id)
            .expect("authorize list was validated");
        let connection_id = connection_id_for_binding(binding)?;
        let assigned = assigned_endpoints_for_routes(&connection_id, &routes);
        let mut ids = Vec::new();
        let mut origins = Vec::new();
        {
            let mut stmt = conn.prepare(
                "SELECT kind, value FROM credential_grants
                 WHERE credential_id = ?1 ORDER BY kind, value",
            )?;
            let rows = stmt
                .query_map([&binding.credential_id], |row| {
                    Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            for (kind, value) in rows {
                if kind == "endpoint_id" {
                    ids.push(value);
                } else if kind == "origin" {
                    origins.push(value);
                }
            }
        }
        for endpoint in &assigned {
            if !ids.contains(&endpoint.id) {
                ids.push(endpoint.id.clone());
            }
            if let Some(origin) = endpoint.url.as_deref().and_then(normalize_origin)
                && !origins.contains(&origin)
            {
                origins.push(origin);
            }
        }
        identity::replace_binding_grants_for_account_on(
            conn,
            &binding.legacy_account_id,
            &ids,
            &origins,
        )?;
    }
    Ok(())
}

fn configured_routes(definition: &DynamicProviderDefinition, endpoint_url: &str) -> Vec<RouteSpec> {
    let mut routes = vec![RouteSpec {
        operation: EndpointOperation::from(definition.upstream_protocol),
        url: Some(endpoint_url.to_string()),
    }];
    let mut seen = HashSet::from([(definition.upstream_protocol, endpoint_url.to_string())]);
    for mapping in &definition.mappings {
        let Some(route) = &mapping.upstream_override else {
            continue;
        };
        if seen.insert((route.protocol, route.endpoint_url.clone())) {
            routes.push(RouteSpec {
                operation: EndpointOperation::from(route.protocol),
                url: Some(route.endpoint_url.clone()),
            });
        }
    }
    routes
}

fn connection_id_for_binding(binding: &InferenceBinding) -> Result<ConnectionId> {
    let stored = binding
        .authorization_connection_id
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| {
            anyhow!(
                "credential `{}` is missing authorization_connection_id",
                binding.credential_id
            )
        })?;
    serde_json::from_value(serde_json::Value::String(stored.to_string())).with_context(|| {
        format!(
            "credential `{}` has an invalid authorization_connection_id",
            binding.credential_id
        )
    })
}

fn inference_predicate(conn: &Connection) -> Result<&'static str> {
    if table_has_column(conn, "credentials", "credential_purpose")? {
        Ok(" AND COALESCE(credential_purpose, 'inference') = 'inference'")
    } else {
        Ok("")
    }
}

#[cfg(test)]
mod tests;
