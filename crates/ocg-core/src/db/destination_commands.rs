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
use ocg_domain::connection::ConnectionId;
use ocg_domain::credential::{RouteSpec, assigned_endpoints_for_routes, normalize_origin};
use ocg_domain::destination::{
    AdapterKind, AuthScheme, CatalogModel, Destination, HttpProtocolRoute, Protocol,
    http_configured_routes,
};
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
    crate::gateway::policy::strip_destination_rules(db, destination_id)?;
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
    replace_http_destination_with_routes_on(
        db,
        destination_id,
        definition,
        authorize_credential_ids,
        None,
    )
}

/// Set declared routes before the first Key's grants are seeded. The enclosing
/// onboarding transaction owns rollback, including an empty saved draft.
pub(crate) fn configure_new_http_routes_on(
    conn: &Connection,
    runtime: &DynamicProviderRuntime,
    routes: &[HttpProtocolRoute],
) -> Result<()> {
    let routes = normalize_http_protocol_routes(routes)?;
    let first = &routes[0];
    ensure!(
        first.endpoint_url == runtime.endpoint_url
            && first.protocol == runtime.upstream_protocol
            && first.auth_scheme == AuthScheme::from(runtime.auth_kind),
        "default endpoint, protocol and authentication must match the first protocol route"
    );
    let id = ocg_domain::destination::destination_id_for_dynamic(&runtime.id);
    let protocols: Vec<_> = routes.iter().map(|route| route.protocol).collect();
    let previous_routes = super::destination_store::load_protocol_routes(conn, &id)?;
    let previous_protocols: Vec<_> = if previous_routes.is_empty() {
        vec![runtime.upstream_protocol]
    } else {
        previous_routes.iter().map(|route| route.protocol).collect()
    };
    let mut catalog = super::destination_store::load_destination_catalog(conn, &id)?;
    for model in &mut catalog {
        if model.upstream_override.is_some() {
            continue;
        }
        model
            .protocols
            .retain(|protocol| protocols.contains(protocol));
        for protocol in &protocols {
            if !previous_protocols.contains(protocol) && !model.protocols.contains(protocol) {
                model.protocols.push(*protocol);
            }
        }
        if model.protocols.is_empty() {
            model.enabled = false;
        }
        if model.preferred.is_none_or(|protocol| {
            !protocols.contains(&protocol)
                || (model.enabled && !model.protocols.contains(&protocol))
        }) {
            model.preferred = model.protocols.first().or(protocols.first()).copied();
        }
    }
    conn.execute(
        "UPDATE destinations SET protocol_routes_json = ?2, protocols_json = ?3 WHERE id = ?1",
        params![
            id,
            serde_json::to_string(&routes)?,
            serde_json::to_string(&protocols)?
        ],
    )?;
    super::destination_store::replace_destination_catalog(conn, &id, &catalog)?;
    Ok(())
}

pub(crate) fn normalize_http_protocol_routes(
    routes: &[HttpProtocolRoute],
) -> Result<Vec<HttpProtocolRoute>> {
    ensure!(
        !routes.is_empty() && routes.len() <= 3,
        "configure one to three HTTP protocol routes"
    );
    let mut seen = HashSet::new();
    let mut normalized = Vec::with_capacity(routes.len());
    let keyless = routes[0].auth_scheme == AuthScheme::None;
    for route in routes {
        ensure!(
            seen.insert(route.protocol),
            "each HTTP protocol may have only one route"
        );
        ensure!(
            (route.auth_scheme == AuthScheme::None) == keyless,
            "keyless and keyed routes cannot share one connection"
        );
        normalized.push(HttpProtocolRoute {
            protocol: route.protocol,
            endpoint_url: validate_custom_endpoint_url(&route.endpoint_url)?,
            auth_scheme: route.auth_scheme,
        });
    }
    Ok(normalized)
}

/// Explicit routes are replaced only by a route-aware caller. Legacy metadata
/// updates must not silently collapse a multi-protocol connection.
pub(crate) fn replace_http_destination_with_routes_on(
    db: &Database,
    destination_id: &str,
    definition: &DynamicProviderDefinition,
    authorize_credential_ids: &[String],
    routes: Option<&[HttpProtocolRoute]>,
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
    let protocol_routes = match routes {
        Some(routes) => {
            let normalized = normalize_http_protocol_routes(routes)?;
            let default = &normalized[0];
            ensure!(
                default.protocol == next_protocol
                    && default.endpoint_url == endpoint_url
                    && default.auth_scheme == next_auth,
                "default endpoint, protocol and authentication must match the first protocol route"
            );
            normalized
        }
        None => {
            ensure!(
                existing.protocol_routes.is_empty()
                    || (existing.base_url.as_deref() == Some(endpoint_url.as_str())
                        && existing.protocols.first().copied() == Some(next_protocol)
                        && existing.auth_scheme == next_auth),
                "changing a multi-protocol connection requires explicit protocolRoutes"
            );
            existing.protocol_routes.clone()
        }
    };
    let protocols: Vec<_> = if protocol_routes.is_empty() {
        vec![next_protocol]
    } else {
        protocol_routes.iter().map(|route| route.protocol).collect()
    };
    let substantive = existing.base_url.as_deref() != Some(endpoint_url.as_str())
        || existing.protocols.first().copied() != Some(next_protocol)
        || existing.auth_scheme != next_auth
        || existing.protocol_routes != protocol_routes
        || mappings_changed(&existing.catalog, &definition);

    let mut catalog = catalog_from_definition(&existing.catalog, &definition, true);
    if existing.protocol_routes != protocol_routes || existing.protocols != protocols {
        let old_available: HashSet<_> = ocg_domain::destination::http_protocol_routes(&existing)
            .into_iter()
            .map(|route| route.protocol)
            .collect();
        for model in &mut catalog {
            if model.upstream_override.is_some() {
                continue;
            }
            model
                .protocols
                .retain(|protocol| protocols.contains(protocol));
            for protocol in &protocols {
                if !old_available.contains(protocol) && !model.protocols.contains(protocol) {
                    model.protocols.push(*protocol);
                }
            }
            if model
                .preferred
                .is_none_or(|protocol| !protocols.contains(&protocol))
            {
                model.preferred = protocols.first().copied();
            }
        }
    }
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
             max_credentials = ?6, updated_at = ?7, protocol_routes_json = ?8
         WHERE id = ?1",
        params![
            destination_id,
            definition.name,
            endpoint_url,
            serde_json::to_string(&protocols)?,
            next_auth.as_str(),
            max_credentials,
            now_rfc,
            serde_json::to_string(&protocol_routes)?,
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
    let mut configured = existing.clone();
    configured.base_url = Some(endpoint_url.clone());
    configured.auth_scheme = next_auth;
    configured.protocols = protocols;
    configured.protocol_routes = protocol_routes;
    configured.catalog = catalog;
    remap_http_grants_on(&db.conn, &existing, &configured)?;
    union_authorized_grants(
        &db.conn,
        &bindings,
        authorize_credential_ids,
        &http_configured_routes(&configured),
    )?;
    for binding in &bindings {
        account_store::sync_inference_credential_projection_on(
            &db.conn,
            &binding.legacy_account_id,
        )?;
    }
    Ok(())
}

pub(crate) fn load_http_destination(db: &Database, destination_id: &str) -> Result<Destination> {
    load_runtime(db)?
        .destinations
        .into_iter()
        .find(|destination| destination.id == destination_id)
        .ok_or_else(|| anyhow!("destination `{destination_id}` not found"))
}

pub(crate) fn remap_http_grants_on(
    conn: &Connection,
    before: &Destination,
    after: &Destination,
) -> Result<()> {
    let old_routes = http_configured_routes(before);
    let new_routes = http_configured_routes(after);
    for binding in load_inference_bindings(conn, &before.id)? {
        let connection = connection_id_for_binding(&binding)?;
        let (ids, origins) = identity::load_credential_grants(conn, &binding.credential_id)?;
        let remapped = ocg_domain::credential::remap_route_grant_ids(
            &connection,
            &old_routes,
            &new_routes,
            &ids,
        );
        identity::replace_binding_grants_for_account_on(
            conn,
            &binding.legacy_account_id,
            &remapped,
            &origins,
        )?;
    }
    Ok(())
}

pub(crate) fn replace_http_catalog_on(
    conn: &Connection,
    before: &Destination,
    catalog: &[CatalogModel],
) -> Result<()> {
    let mut after = before.clone();
    after.catalog = catalog.to_vec();
    destination_store::replace_destination_catalog(conn, &before.id, catalog)?;
    remap_http_grants_on(conn, before, &after)
}

/// Import may merge two different route orderings. Resolve each credential's
/// consent against its own source catalog. Explicit imported grants replace
/// target grants, including empty arrays; target-only credentials retain theirs.
pub(crate) fn reconcile_imported_http_grants_on(
    db: &Database,
    record: &NodeImportRecord,
    before: &crate::destination_projection::DestinationProjection,
) -> Result<()> {
    let after = crate::destination_projection::load_persisted(db)?;
    for destination in after
        .destinations
        .iter()
        .filter(|d| d.adapter == AdapterKind::Http)
    {
        let mut source = record
            .destination_controls
            .iter()
            .find(|d| d.id == destination.id)
            .cloned();
        if source.is_none() {
            if let Some(runtime) = record.dynamic_providers.iter().find(|r| {
                ocg_domain::destination::destination_id_for_dynamic(&r.id) == destination.id
            }) {
                source = Some(ocg_domain::destination::destination_from_legacy(
                    &ocg_domain::destination::LegacyDestinationFacts::Dynamic {
                        definition: runtime.definition(),
                    },
                )?);
            } else if let Some(custom) = record
                .custom_destinations
                .iter()
                .find(|d| d.id == destination.id)
            {
                let mut legacy = destination.clone();
                legacy.base_url = Some(custom.endpoint_url.clone());
                legacy.protocol_routes.clear();
                legacy.protocols = vec![custom.protocol];
                legacy.auth_scheme = custom.auth_scheme;
                legacy.catalog = custom
                    .models
                    .iter()
                    .map(|model| CatalogModel {
                        public_model: model.public_model.clone(),
                        upstream_model: model.upstream_model.clone(),
                        protocols: vec![
                            model
                                .upstream_override
                                .as_ref()
                                .map_or(custom.protocol, |o| o.protocol),
                        ],
                        preferred: None,
                        enabled: true,
                        upstream_override: model.upstream_override.clone(),
                    })
                    .collect();
                source = Some(legacy);
            }
        }
        let Some(source) = source else {
            continue;
        };
        let old_destination = before.destinations.iter().find(|d| d.id == destination.id);
        let new_routes = http_configured_routes(destination);
        for binding in load_inference_bindings(&db.conn, &destination.id)? {
            let connection = connection_id_for_binding(&binding)?;
            let old_key = before.credentials.iter().find(|c| {
                c.legacy_account_id == binding.legacy_account_id
                    && c.destination_id == destination.id
            });
            let imported = record.identity_snapshot.as_ref().and_then(|snapshot| {
                snapshot
                    .accounts
                    .iter()
                    .find(|a| a.account_id == binding.legacy_account_id)
            });
            let mut ids = Vec::new();
            let mut origins = Vec::new();
            if imported.is_none()
                && let (Some(old_destination), Some(old_key)) = (old_destination, old_key)
            {
                ids = ocg_domain::credential::remap_route_grant_ids(
                    &connection,
                    &http_configured_routes(old_destination),
                    &new_routes,
                    &old_key.grants.allowed_endpoint_ids,
                );
                origins = old_key.grants.allowed_origins.clone();
            }
            let source_grants = if let Some(imported) = imported {
                Some((
                    imported.allowed_endpoint_ids.clone(),
                    imported.allowed_origins.clone(),
                ))
            } else if old_key.is_none() {
                // Old portable formats have no explicit grants. Only new Keys
                // receive defaults, derived from the source declaration.
                Some(ocg_domain::credential::safe_default_grants(
                    &assigned_endpoints_for_routes(&connection, &http_configured_routes(&source)),
                ))
            } else {
                None
            };
            if let Some((source_ids, source_origins)) = source_grants {
                for id in ocg_domain::credential::remap_route_grant_ids(
                    &connection,
                    &http_configured_routes(&source),
                    &new_routes,
                    &source_ids,
                ) {
                    if !ids.contains(&id) {
                        ids.push(id);
                    }
                }
                for origin in source_origins {
                    if !origins.contains(&origin) {
                        origins.push(origin);
                    }
                }
            }
            identity::replace_binding_grants_for_account_on(
                &db.conn,
                &binding.legacy_account_id,
                &ids,
                &origins,
            )?;
        }
    }
    Ok(())
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
    preserve_protocols: bool,
) -> Vec<CatalogModel> {
    definition
        .mappings
        .iter()
        .map(|mapping| {
            catalog_model_for_mapping(
                existing,
                mapping,
                definition.upstream_protocol,
                preserve_protocols,
            )
        })
        .collect()
}

fn catalog_model_for_mapping(
    existing: &[CatalogModel],
    mapping: &DynamicModelMapping,
    default_protocol: Protocol,
    preserve_protocols: bool,
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
                && (preserve_protocols
                    || mapping.upstream_override.is_some()
                    || previous.protocols.contains(&route_protocol)
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
    routes: &[RouteSpec],
) -> Result<()> {
    if authorize_credential_ids.is_empty() {
        return Ok(());
    }
    for credential_id in authorize_credential_ids {
        let binding = bindings
            .iter()
            .find(|binding| binding.credential_id == *credential_id)
            .expect("authorize list was validated");
        let connection_id = connection_id_for_binding(binding)?;
        let assigned = assigned_endpoints_for_routes(&connection_id, routes);
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
