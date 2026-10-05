//! Assemble one auth per enabled credential. No selector and no transport.

use super::auth_id::auth_id;
use super::digest::refresh_logical_digest;
use super::endpoints::{
    Origin, check_route_grant, origin_of, owned_listener_origin, remote_origin, resolve_model,
    sealed_grant_base,
};
use super::types::{
    CredentialRouteSet, NormalizedRoute, OAuthFileRef, Omission, PendingApprovedRoute,
    ProductProjection, ProjectedAuth, ProjectedModel, ProjectedRoute, ProjectionError,
    ProjectionInput, RemoteInner, RequestIdentityFact, RuntimeEnvelope, SecretMaterial,
    SourceProvenance, ValidationCandidate, ValidationCandidateKind, ValidationRow, WireFact,
};
use crate::models::ProxyMode;
use crate::provider::builtin_provider;
use crate::routing_snapshot::ExecutionCredential;
use ocg_domain::catalog::UpstreamProtocolKind;
use ocg_domain::connection::ConnectionId;
use ocg_domain::credential::ModelScope;
use ocg_domain::destination::{AdapterKind, CatalogModel, Destination, LegacyDestinationRef};
use ocg_domain::ids::{
    COMMAND_CODE_PROVIDER_ID, KIMI_PROVIDER_ID, MINIMAX_PROVIDER_ID, OLLAMA_PROVIDER_ID,
    OPENCODE_PROVIDER_ID, OPENCODE_ZEN_FREE_PROVIDER_ID, model_identity_key, model_ids_match,
};
use ocg_gateway::selector::CONVERSATION_TTL;
use std::collections::{BTreeMap, BTreeSet};

pub(crate) fn project(input: ProjectionInput<'_>) -> Result<ProductProjection, ProjectionError> {
    let runtime = accept_runtime(&input.runtime, &input.config.gateway_key)?;
    let owned = exact_owned_origins(input.owned_listener, input.owned_origins)?;
    let proxy_url = normalize_proxy(input.config)?;
    let oauth_refs = validate_oauth(input.oauth_refs)?;
    let zen_ids = zen_model_ids(input.snapshot);
    let mut auths = Vec::new();
    let mut omissions = Vec::new();
    for credential in &input.snapshot.credentials {
        match project_credential(&input, credential, &owned, &zen_ids, &mut omissions)? {
            Some(auth) => auths.push(auth),
            None => {}
        }
    }
    for (sequence, auth) in auths.iter_mut().enumerate() {
        auth.sequence = u32::try_from(sequence).unwrap_or(u32::MAX);
    }
    if auths.iter().any(|auth| {
        auth.material.as_ref().is_some_and(|secret| {
            let plain = secret.expose();
            plain == runtime.hop_secret.expose()
                || plain == runtime.ready_key.expose()
                || plain == runtime.policy_token.expose()
        })
    }) {
        return Err(ProjectionError::new(
            "invalid_runtime_envelope",
            "runtime secret matches an upstream credential",
        ));
    }
    let route_sets = route_sets_from_auths(&auths);
    let mut projection = ProductProjection {
        revisions: input.revisions,
        digest: String::new(),
        routing_mode: input.config.routing_mode,
        conversation_sticky: input.config.conversation_sticky,
        conversation_ttl_secs: if input.config.conversation_sticky {
            CONVERSATION_TTL.as_secs()
        } else {
            0
        },
        proxy_mode: input.config.proxy_mode,
        proxy_url,
        proxy_list_direction: input.config.proxy_list_direction,
        proxy_list_models: input.config.proxy_list_models.clone(),
        auths,
        omissions,
        oauth_refs,
        route_sets,
        process_generation: runtime.process_generation,
        listen_port: runtime.port,
        auth_dir: runtime.auth_dir,
        policy_url: runtime.policy_url,
        policy_origin: runtime.policy_origin,
        ready_key: runtime.ready_key,
        hop_secret: runtime.hop_secret,
        policy_token: runtime.policy_token,
    };
    refresh_logical_digest(&mut projection)?;
    Ok(projection)
}

fn project_credential(
    input: &ProjectionInput<'_>,
    credential: &ExecutionCredential,
    owned: &[Origin],
    zen_ids: &[String],
    omissions: &mut Vec<Omission>,
) -> Result<Option<ProjectedAuth>, ProjectionError> {
    if credential.credential_version == 0 {
        return Err(ProjectionError::new(
            "invalid_version",
            format!(
                "credential `{}` has no version fence",
                credential.credential_id
            ),
        ));
    }
    let Some(destination) = input
        .snapshot
        .projection
        .destinations
        .iter()
        .find(|destination| destination.id == credential.destination_id)
    else {
        return Err(ProjectionError::new(
            "missing_destination",
            format!(
                "credential `{}` has no destination",
                credential.credential_id
            ),
        ));
    };
    if !destination.enabled {
        return omit(omissions, credential, "destination_disabled", None);
    }
    if !credential.binding_enabled {
        return omit(omissions, credential, "binding_disabled", None);
    }
    let candidate = accepted_candidate(input, credential, destination, zen_ids);
    let client_credential = credential.enabled && credential.ready;
    if !client_credential && candidate.is_none() {
        if !credential.enabled {
            return omit(omissions, credential, "credential_disabled", None);
        }
        return omit(omissions, credential, "draft", None);
    }
    if let ModelScope::Only { models } = &credential.scope
        && models.is_empty()
    {
        return omit(omissions, credential, "scope_empty", None);
    }
    if destination.adapter == AdapterKind::Cpa {
        let base = destination.base_url.as_deref().unwrap_or("").trim();
        match cpa_placement(base, &destination.id, owned, input.owned_destination_ids)? {
            CpaPlacement::OwnedPool => {
                return omit(omissions, credential, "owned_oauth_pool", None);
            }
            CpaPlacement::Remote => {
                return omit(omissions, credential, "migration_required", None);
            }
        }
    }
    let connection = parse_connection(&credential.authorization_connection_id)?;
    let (models, needs_material) = assemble_models(
        credential,
        destination,
        &connection,
        candidate,
        client_credential,
        zen_ids,
        omissions,
        input.endpoints,
    )?;
    if models.is_empty() {
        return omit(omissions, credential, "no_positive_models", None);
    }
    let material = decrypt_material(input, credential, needs_material, omissions)?;
    let Some(material) = material else {
        return Ok(None);
    };
    let fingerprint = material.as_ref().map(|secret| secret.fingerprint());
    let auth = ProjectedAuth {
        auth_id: auth_id(
            &credential.credential_id,
            credential.credential_version,
            &credential.binding_id,
            fingerprint.as_deref(),
        ),
        credential_id: credential.credential_id.clone(),
        legacy_account_id: credential.id.clone(),
        credential_version: credential.credential_version,
        binding_id: credential.binding_id.clone(),
        routing_rank: routing_rank(input, credential)?,
        sequence: 0,
        provenance: provenance(input, credential, destination),
        scope: credential.scope.clone(),
        material,
        models,
        remote_inner: RemoteInner::NotRemote,
        request_identity: match destination.adapter {
            AdapterKind::OpencodeGo | AdapterKind::Zen => RequestIdentityFact::OpenCodeSession,
            _ => RequestIdentityFact::None,
        },
        wire: if destination.adapter == AdapterKind::Ollama {
            WireFact::OllamaReasoning
        } else {
            WireFact::None
        },
    };
    Ok(Some(auth))
}

fn decrypt_material(
    input: &ProjectionInput<'_>,
    credential: &ExecutionCredential,
    needs_material: bool,
    omissions: &mut Vec<Omission>,
) -> Result<Option<Option<SecretMaterial>>, ProjectionError> {
    if credential.key_cipher.is_empty() {
        if needs_material {
            omit(omissions, credential, "missing_material", None)?;
            return Ok(None);
        }
        return Ok(Some(None));
    }
    let plain = input.cipher.decrypt(&credential.key_cipher).map_err(|_| {
        ProjectionError::new(
            "decrypt_failed",
            format!(
                "credential `{}` could not be decrypted",
                credential.credential_id
            ),
        )
    })?;
    if plain.is_empty() {
        if needs_material {
            omit(omissions, credential, "missing_material", None)?;
            return Ok(None);
        }
        return Ok(Some(None));
    }
    Ok(Some(Some(SecretMaterial::new(plain))))
}

fn omit(
    omissions: &mut Vec<Omission>,
    credential: &ExecutionCredential,
    reason: &'static str,
    model: Option<String>,
) -> Result<Option<ProjectedAuth>, ProjectionError> {
    omissions.push(Omission {
        credential_id: credential.credential_id.clone(),
        reason,
        model,
    });
    Ok(None)
}

pub(crate) fn note_native_route_unmapped(
    projection: &mut ProductProjection,
    credential_id: String,
) {
    if projection
        .omissions
        .iter()
        .any(|item| item.credential_id == credential_id && item.reason == "native_route_unmapped")
    {
        return;
    }
    projection.omissions.push(Omission {
        credential_id,
        reason: "native_route_unmapped",
        model: None,
    });
}

fn routing_rank(
    input: &ProjectionInput<'_>,
    credential: &ExecutionCredential,
) -> Result<u32, ProjectionError> {
    input
        .snapshot
        .projection
        .credentials
        .iter()
        .find(|row| row.id == credential.credential_id)
        .map(|row| row.routing_rank)
        .ok_or_else(|| {
            ProjectionError::new(
                "missing_rank",
                format!(
                    "credential `{}` has no routing rank",
                    credential.credential_id
                ),
            )
        })
}

fn provenance(
    input: &ProjectionInput<'_>,
    credential: &ExecutionCredential,
    destination: &Destination,
) -> SourceProvenance {
    let (legacy_kind, legacy_id) = match &destination.legacy {
        LegacyDestinationRef::Builtin(id) => ("builtin", id.clone()),
        LegacyDestinationRef::Dynamic(id) => ("dynamic", id.clone()),
        LegacyDestinationRef::CustomAccount(id) => ("custom_account", id.clone()),
        LegacyDestinationRef::PlatformParent(id) => ("platform_parent", id.clone()),
    };
    SourceProvenance {
        provider_id: credential.provider_id.clone(),
        adapter: destination.adapter.as_str().to_string(),
        legacy_kind: legacy_kind.to_string(),
        legacy_id,
        preset_id: input.preset_ids.get(&destination.id).cloned(),
        destination_id: destination.id.clone(),
        quota_scope: builtin_provider(&credential.provider_id)
            .map(|plan| plan.quota_scope.as_str().to_string())
            .unwrap_or_else(|| "unknown".to_string()),
    }
}

pub(crate) fn registration_is_complete(account_type: &str, setup_step: &str) -> bool {
    let step = setup_step.trim();
    step.is_empty()
        || step == "ready"
        || (account_type.trim() == "managed" && step == "key_verification")
}

pub(crate) fn classify_validation(row: &ValidationRow) -> Option<ValidationCandidateKind> {
    classify_validation_with_auth(row, row.material_present)
}

/// `authentication_satisfied` replaces only the ciphertext gate.
/// Binding, destination, registration, setup, and enablement stay the same.
pub(crate) fn classify_validation_with_auth(
    row: &ValidationRow,
    authentication_satisfied: bool,
) -> Option<ValidationCandidateKind> {
    if !row.binding_enabled
        || !row.destination_enabled
        || !authentication_satisfied
        || !row.registration_complete
    {
        return None;
    }
    let step = row.setup_step.trim();
    let managed = row.account_type.trim() == "managed";
    if managed && step == "key_verification" && !row.enabled && !row.destination_draft {
        return Some(ValidationCandidateKind::ManagedKeyVerification);
    }
    if row.enabled && (step.is_empty() || step == "ready") {
        return Some(ValidationCandidateKind::CompletePending);
    }
    None
}

pub(crate) fn native_inclusion(row: &ValidationRow) -> Option<super::types::NativeInclusion> {
    match classify_validation(row) {
        Some(ValidationCandidateKind::ManagedKeyVerification) => {
            return Some(super::types::NativeInclusion::ValidationOnly);
        }
        Some(ValidationCandidateKind::CompletePending) if row.destination_draft => {
            return Some(super::types::NativeInclusion::ValidationOnly);
        }
        _ => {}
    }
    let step = row.setup_step.trim();
    let ready = step.is_empty() || step == "ready";
    if row.enabled
        && row.binding_enabled
        && row.destination_enabled
        && !row.destination_draft
        && ready
    {
        Some(super::types::NativeInclusion::Client)
    } else {
        None
    }
}

fn accepted_candidate<'a>(
    input: &'a ProjectionInput<'_>,
    credential: &ExecutionCredential,
    destination: &Destination,
    zen_ids: &[String],
) -> Option<&'a ValidationCandidate> {
    let mut matched = input
        .validation
        .candidates
        .iter()
        .filter(|candidate| candidate.credential_id == credential.credential_id);
    let candidate = matched.next()?;
    if matched.next().is_some() || candidate.credential_version != credential.credential_version {
        return None;
    }
    let material_present = !credential.key_cipher.trim().is_empty() && candidate.material_present;
    let satisfied = sidecar_authentication_satisfied(
        credential,
        destination,
        candidate,
        zen_ids,
        material_present,
        input.endpoints,
    );
    let row = ValidationRow {
        credential_id: credential.credential_id.clone(),
        credential_version: credential.credential_version,
        enabled: credential.enabled,
        binding_enabled: credential.binding_enabled,
        setup_step: candidate.setup_step.clone(),
        account_type: candidate.account_type.clone(),
        material_present,
        destination_enabled: destination.enabled,
        destination_draft: candidate.destination_draft,
        registration_complete: registration_is_complete(
            &candidate.account_type,
            &candidate.setup_step,
        ),
    };
    (classify_validation_with_auth(&row, satisfied) == Some(candidate.kind)).then_some(candidate)
}

/// Configured-route authentication for one pending sidecar.
///
/// Every resolved route must be `AuthScheme::None` before a missing key is
/// enough. A keyed route, a failed resolve, or no route uses the ciphertext
/// fact. Empty ciphertext is not CPA metadata.
fn sidecar_authentication_satisfied(
    credential: &ExecutionCredential,
    destination: &Destination,
    candidate: &ValidationCandidate,
    zen_ids: &[String],
    material_present: bool,
    endpoints: &crate::cpa_test_endpoints::EndpointAuthority,
) -> bool {
    let Ok(connection) = parse_connection(&credential.authorization_connection_id) else {
        return material_present;
    };
    let mut ignored = Vec::new();
    let Ok(planned) = plan_models(
        credential,
        destination,
        Some(candidate),
        zen_ids,
        &mut ignored,
    ) else {
        return material_present;
    };
    let mut saw_route = false;
    let mut needs_material = false;
    for plan in planned {
        let model = &destination.catalog[plan.index];
        let mut covered = BTreeSet::new();
        if plan.client {
            let Ok(resolved) = resolve_model(
                destination,
                model,
                &connection,
                &credential.provider_id,
                endpoints,
            ) else {
                return material_present;
            };
            if resolved.routes.is_empty() {
                return material_present;
            }
            saw_route = true;
            needs_material |= resolved.needs_material;
            for route in resolved.routes {
                covered.insert(route.protocol);
            }
        }
        let alias = curated_alias(&credential.provider_id, &model.upstream_model, zen_ids);
        for pending in &candidate.pending_routes {
            if !pending_matches(pending, model, &alias)
                || !pending_in_scope(&credential.scope, pending, model, &alias)
                || covered.contains(&pending.protocol)
            {
                continue;
            }
            let Some(protocol) = protocol_kind(&pending.protocol) else {
                return material_present;
            };
            let synthetic = synthetic_model(model, protocol);
            let Ok(resolved) = resolve_model(
                destination,
                &synthetic,
                &connection,
                &credential.provider_id,
                endpoints,
            ) else {
                return material_present;
            };
            if resolved.routes.is_empty() {
                return material_present;
            }
            saw_route = true;
            needs_material |= resolved.needs_material;
            for route in resolved.routes {
                covered.insert(route.protocol);
            }
        }
    }
    if !saw_route || needs_material {
        material_present
    } else {
        true
    }
}

struct PlannedModel {
    index: usize,
    names: Vec<String>,
    client: bool,
}

fn assemble_models(
    credential: &ExecutionCredential,
    destination: &Destination,
    connection: &ConnectionId,
    candidate: Option<&ValidationCandidate>,
    client_credential: bool,
    zen_ids: &[String],
    omissions: &mut Vec<Omission>,
    endpoints: &crate::cpa_test_endpoints::EndpointAuthority,
) -> Result<(Vec<ProjectedModel>, bool), ProjectionError> {
    let planned = plan_models(credential, destination, candidate, zen_ids, omissions)?;
    let mut models = Vec::new();
    let mut needs_material = false;
    let mut emitted = BTreeSet::new();
    for plan in planned {
        let catalog_model = &destination.catalog[plan.index];
        let mut names = Vec::new();
        for name in plan.names {
            if emitted.insert(model_identity_key(&name)) {
                names.push(name);
            }
        }
        if names.is_empty() {
            continue;
        }
        let mut routes = Vec::new();
        if plan.client {
            let resolved = resolve_model(
                destination,
                catalog_model,
                connection,
                &credential.provider_id,
                endpoints,
            )?;
            needs_material |= resolved.needs_material;
            routes = checked_routes(credential, destination, resolved.routes, endpoints)?;
            if !client_credential {
                for route in &mut routes {
                    route.validation_only = true;
                }
            }
        }
        append_pending(
            credential,
            destination,
            catalog_model,
            connection,
            candidate,
            zen_ids,
            omissions,
            &mut routes,
            &mut needs_material,
            endpoints,
        )?;
        if routes.is_empty() {
            continue;
        }
        for name in names {
            models.push(ProjectedModel {
                public_alias: name,
                upstream_name: catalog_model.upstream_model.clone(),
                routes: routes.clone(),
            });
        }
    }
    if let ModelScope::Only { models: names } = &credential.scope {
        for name in names {
            let matched = destination.catalog.iter().any(|model| {
                model.enabled && scope_name_matches(name, model, &credential.provider_id, zen_ids)
            });
            if !matched {
                return Err(ProjectionError::new(
                    "invalid_scope",
                    format!(
                        "scope name on credential `{}` matches no enabled model",
                        credential.credential_id
                    ),
                ));
            }
        }
    }
    Ok((models, needs_material))
}

fn plan_models(
    credential: &ExecutionCredential,
    destination: &Destination,
    candidate: Option<&ValidationCandidate>,
    zen_ids: &[String],
    omissions: &mut Vec<Omission>,
) -> Result<Vec<PlannedModel>, ProjectionError> {
    let mut owners: BTreeMap<String, String> = BTreeMap::new();
    let mut planned = Vec::new();
    for (index, model) in destination.catalog.iter().enumerate() {
        let pending = has_scoped_pending(
            candidate,
            model,
            &credential.scope,
            &credential.provider_id,
            zen_ids,
        );
        if !model.enabled {
            omissions.push(Omission {
                credential_id: credential.credential_id.clone(),
                reason: "model_disabled",
                model: Some(model.public_model.clone()),
            });
            if !pending {
                continue;
            }
        }
        let names = public_names(&credential.provider_id, model, &credential.scope, zen_ids);
        if names.is_empty() {
            continue;
        }
        for name in &names {
            let key = model_identity_key(name);
            if let Some(existing) = owners.get(&key) {
                if !model_ids_match(existing, &model.upstream_model) {
                    return Err(ProjectionError::new(
                        "invalid_alias",
                        "one public alias maps to two upstream names",
                    ));
                }
            } else {
                owners.insert(key, model.upstream_model.clone());
            }
        }
        planned.push(PlannedModel {
            index,
            names,
            client: model.enabled,
        });
    }
    Ok(planned)
}

fn append_pending(
    credential: &ExecutionCredential,
    destination: &Destination,
    model: &CatalogModel,
    connection: &ConnectionId,
    candidate: Option<&ValidationCandidate>,
    zen_ids: &[String],
    omissions: &mut Vec<Omission>,
    routes: &mut Vec<ProjectedRoute>,
    needs_material: &mut bool,
    endpoints: &crate::cpa_test_endpoints::EndpointAuthority,
) -> Result<(), ProjectionError> {
    let Some(candidate) = candidate else {
        return Ok(());
    };
    let alias = curated_alias(&credential.provider_id, &model.upstream_model, zen_ids);
    for pending in &candidate.pending_routes {
        if !pending_matches(pending, model, &alias) {
            continue;
        }
        if !pending_in_scope(&credential.scope, pending, model, &alias) {
            continue;
        }
        if routes
            .iter()
            .any(|route| route.protocol == pending.protocol)
        {
            continue;
        }
        let Some(protocol) = protocol_kind(&pending.protocol) else {
            return Err(ProjectionError::new(
                "unsupported_protocol",
                "unknown protocol",
            ));
        };
        let synthetic = synthetic_model(model, protocol);
        let resolved = match resolve_model(
            destination,
            &synthetic,
            connection,
            &credential.provider_id,
            endpoints,
        ) {
            Ok(resolved) => resolved,
            Err(error) => {
                record_pending_omission(omissions, credential, pending, error)?;
                continue;
            }
        };
        *needs_material |= resolved.needs_material;
        let mut extra = match checked_routes(credential, destination, resolved.routes, endpoints) {
            Ok(extra) => extra,
            Err(error) => {
                record_pending_omission(omissions, credential, pending, error)?;
                continue;
            }
        };
        for route in &mut extra {
            route.validation_only = true;
        }
        routes.extend(extra);
    }
    Ok(())
}

fn record_pending_omission(
    omissions: &mut Vec<Omission>,
    credential: &ExecutionCredential,
    pending: &PendingApprovedRoute,
    error: ProjectionError,
) -> Result<(), ProjectionError> {
    let reason = match error.code {
        "invalid_grant" => "unapproved_grant",
        "unsupported_protocol" | "invalid_endpoint" => "unsupported_protocol",
        _ => return Err(error),
    };
    omissions.push(Omission {
        credential_id: credential.credential_id.clone(),
        reason,
        model: Some(pending.public_model.clone()),
    });
    Ok(())
}

fn checked_routes(
    credential: &ExecutionCredential,
    destination: &Destination,
    routes: Vec<ProjectedRoute>,
    endpoints: &crate::cpa_test_endpoints::EndpointAuthority,
) -> Result<Vec<ProjectedRoute>, ProjectionError> {
    for route in &routes {
        if destination.adapter != AdapterKind::Http {
            let protocol = protocol_kind(&route.protocol)
                .ok_or_else(|| ProjectionError::new("unsupported_protocol", "unknown protocol"))?;
            let sealed = sealed_grant_base(destination, protocol, endpoints)?;
            check_route_grant(
                route,
                &credential.grants.allowed_endpoint_ids,
                &credential.grants.allowed_origins,
                Some(sealed.as_str()),
            )?;
        } else {
            check_route_grant(
                route,
                &credential.grants.allowed_endpoint_ids,
                &credential.grants.allowed_origins,
                None,
            )?;
        }
    }
    Ok(routes)
}

fn synthetic_model(model: &CatalogModel, protocol: UpstreamProtocolKind) -> CatalogModel {
    let mut synthetic = model.clone();
    synthetic.enabled = true;
    synthetic.protocols = vec![protocol];
    synthetic.preferred = Some(protocol);
    if synthetic
        .upstream_override
        .as_ref()
        .is_some_and(|route| route.protocol != protocol)
    {
        synthetic.upstream_override = None;
    }
    synthetic
}

fn public_names(
    provider_id: &str,
    model: &CatalogModel,
    scope: &ModelScope,
    zen_ids: &[String],
) -> Vec<String> {
    let alias = curated_alias(provider_id, &model.upstream_model, zen_ids);
    let mut names = Vec::new();
    match scope {
        ModelScope::All => {
            names.push(model.public_model.clone());
            if !alias.is_empty() && !model_ids_match(&alias, &model.public_model) {
                names.push(alias);
            }
        }
        ModelScope::Only { models } => {
            if models
                .iter()
                .any(|name| model_ids_match(name, &model.public_model))
            {
                names.push(model.public_model.clone());
            }
            if !alias.is_empty()
                && !model_ids_match(&alias, &model.public_model)
                && models.iter().any(|name| model_ids_match(name, &alias))
            {
                names.push(alias.clone());
            }
            let listed_upstream = models
                .iter()
                .any(|name| model_ids_match(name, &model.upstream_model));
            let already = names
                .iter()
                .any(|name| model_ids_match(name, &model.upstream_model));
            if listed_upstream && !already {
                names.push(model.upstream_model.clone());
            }
        }
    }
    names
}

fn scope_name_matches(
    name: &str,
    model: &CatalogModel,
    provider_id: &str,
    zen_ids: &[String],
) -> bool {
    let alias = curated_alias(provider_id, &model.upstream_model, zen_ids);
    model_ids_match(name, &model.public_model)
        || model_ids_match(name, &model.upstream_model)
        || (!alias.is_empty() && model_ids_match(name, &alias))
}

fn curated_alias(provider_id: &str, upstream: &str, zen_ids: &[String]) -> String {
    if !curated_provider(provider_id) {
        return String::new();
    }
    ocg_gateway::alias::canonical_alias_for_provider_model(provider_id, upstream, &[], zen_ids)
}

fn curated_provider(provider_id: &str) -> bool {
    provider_id == OPENCODE_PROVIDER_ID
        || provider_id == OPENCODE_ZEN_FREE_PROVIDER_ID
        || provider_id == COMMAND_CODE_PROVIDER_ID
        || provider_id == MINIMAX_PROVIDER_ID
        || provider_id == KIMI_PROVIDER_ID
        || provider_id == OLLAMA_PROVIDER_ID
}

fn pending_matches(pending: &PendingApprovedRoute, model: &CatalogModel, alias: &str) -> bool {
    let targets = [
        model.public_model.as_str(),
        model.upstream_model.as_str(),
        alias,
    ];
    targets.iter().any(|name| {
        !name.is_empty()
            && (model_ids_match(&pending.public_model, name)
                || model_ids_match(&pending.upstream_model, name))
    })
}

fn pending_in_scope(
    scope: &ModelScope,
    pending: &PendingApprovedRoute,
    model: &CatalogModel,
    alias: &str,
) -> bool {
    match scope {
        ModelScope::All => true,
        ModelScope::Only { models } => models.iter().any(|name| {
            model_ids_match(name, &pending.public_model)
                || model_ids_match(name, &pending.upstream_model)
                || model_ids_match(name, &model.public_model)
                || model_ids_match(name, &model.upstream_model)
                || (!alias.is_empty() && model_ids_match(name, alias))
        }),
    }
}

fn has_scoped_pending(
    candidate: Option<&ValidationCandidate>,
    model: &CatalogModel,
    scope: &ModelScope,
    provider_id: &str,
    zen_ids: &[String],
) -> bool {
    let Some(candidate) = candidate else {
        return false;
    };
    let alias = curated_alias(provider_id, &model.upstream_model, zen_ids);
    candidate.pending_routes.iter().any(|pending| {
        pending_matches(pending, model, &alias) && pending_in_scope(scope, pending, model, &alias)
    })
}

fn route_sets_from_auths(auths: &[ProjectedAuth]) -> Vec<CredentialRouteSet> {
    auths
        .iter()
        .map(|auth| {
            let mut routes = Vec::new();
            for model in &auth.models {
                for route in &model.routes {
                    routes.push(NormalizedRoute {
                        public_model: model.public_alias.clone(),
                        upstream_model: model.upstream_name.clone(),
                        protocol: route.protocol.clone(),
                        endpoint_id: route.endpoint_id.clone(),
                        origin: route_origin(&route.endpoint_url),
                        endpoint_fingerprint: super::digest::endpoint_fingerprint(
                            &route.endpoint_url,
                        ),
                        validation_only: route.validation_only,
                        native_targets: Vec::new(),
                    });
                }
            }
            routes.sort_by(route_order);
            let material_fingerprint = auth
                .material
                .as_ref()
                .map(|secret| secret.fingerprint())
                .unwrap_or_else(|| "no-material".to_string());
            let fingerprint = super::digest::credential_route_fingerprint(
                &auth.auth_id,
                &auth.credential_id,
                auth.credential_version,
                &auth.binding_id,
                &material_fingerprint,
                auth.routing_rank,
                &routes,
            );
            CredentialRouteSet {
                auth_id: auth.auth_id.clone(),
                credential_id: auth.credential_id.clone(),
                credential_version: auth.credential_version,
                binding_id: auth.binding_id.clone(),
                material_fingerprint,
                routing_rank: auth.routing_rank,
                routes,
                fingerprint,
            }
        })
        .collect()
}

fn route_order(left: &NormalizedRoute, right: &NormalizedRoute) -> std::cmp::Ordering {
    left.public_model
        .cmp(&right.public_model)
        .then(left.protocol.cmp(&right.protocol))
        .then(left.endpoint_id.cmp(&right.endpoint_id))
        .then(left.endpoint_fingerprint.cmp(&right.endpoint_fingerprint))
        .then(left.upstream_model.cmp(&right.upstream_model))
}

fn route_origin(url: &str) -> String {
    match origin_of(url) {
        Some(origin) => format!("{}://{}:{}", origin.scheme, origin.host, origin.port),
        None => String::new(),
    }
}

fn zen_model_ids(snapshot: &crate::routing_snapshot::RoutingSnapshot) -> Vec<String> {
    snapshot
        .projection
        .destinations
        .iter()
        .filter(|destination| destination.adapter == AdapterKind::Zen)
        .flat_map(|destination| {
            destination
                .catalog
                .iter()
                .map(|model| model.upstream_model.clone())
        })
        .collect()
}

fn parse_connection(raw: &str) -> Result<ConnectionId, ProjectionError> {
    if raw.trim().is_empty() {
        return Err(ProjectionError::new(
            "invalid_grant",
            "credential has no authorization connection",
        ));
    }
    serde_json::from_value(serde_json::Value::String(raw.to_string())).map_err(|_| {
        ProjectionError::new("invalid_grant", "authorization connection id is invalid")
    })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CpaPlacement {
    OwnedPool,
    Remote,
}

fn exact_owned_origins(listener: &str, extras: &[&str]) -> Result<Vec<Origin>, ProjectionError> {
    let mut origins = vec![owned_listener_origin(listener)?];
    for extra in extras {
        let origin = owned_listener_origin(extra)?;
        if !origins.iter().any(|saved| saved == &origin) {
            origins.push(origin);
        }
    }
    Ok(origins)
}

/// Empty base, or an origin the caller named exactly, is the owned pool.
/// Any other parseable base is historical remote configuration. It is omitted
/// with `migration_required` and is not upstream authority. An unparseable
/// base stays `invalid_endpoint`. A named destination whose base is a
/// different origin is an error, not an omission.
fn cpa_placement(
    base: &str,
    destination_id: &str,
    owned: &[Origin],
    owned_destination_ids: &[&str],
) -> Result<CpaPlacement, ProjectionError> {
    let named = owned_destination_ids.iter().any(|id| *id == destination_id);
    if base.is_empty() {
        return Ok(CpaPlacement::OwnedPool);
    }
    let origin = remote_origin(base)?;
    let exact = owned.iter().any(|item| item == &origin);
    if named && !exact {
        return Err(ProjectionError::new(
            "ambiguous_owned_pool",
            "owned destination reference does not match a known owned origin",
        ));
    }
    if exact {
        return Ok(CpaPlacement::OwnedPool);
    }
    Ok(CpaPlacement::Remote)
}

/// Parse caller-supplied owned origins, then use the private classifier.
/// An empty origin list is not parsed. `Origin` stays inside this module.
pub(crate) fn cpa_placement_from_origins(
    base: &str,
    destination_id: &str,
    owned_origins: &[&str],
    owned_destination_ids: &[&str],
) -> Result<CpaPlacement, ProjectionError> {
    let mut owned = Vec::with_capacity(owned_origins.len());
    for origin in owned_origins {
        let parsed = owned_listener_origin(origin)?;
        if !owned.iter().any(|saved| saved == &parsed) {
            owned.push(parsed);
        }
    }
    cpa_placement(base, destination_id, &owned, owned_destination_ids)
}

fn protocol_kind(value: &str) -> Option<ocg_domain::catalog::UpstreamProtocolKind> {
    match value {
        "chat_completions" => Some(ocg_domain::catalog::UpstreamProtocolKind::ChatCompletions),
        "responses" => Some(ocg_domain::catalog::UpstreamProtocolKind::Responses),
        "messages" => Some(ocg_domain::catalog::UpstreamProtocolKind::Messages),
        _ => None,
    }
}

fn normalize_proxy(config: &crate::models::AppConfig) -> Result<String, ProjectionError> {
    if config.proxy_url.contains('@') {
        return Err(ProjectionError::new(
            "invalid_proxy",
            "proxy URL must not include credentials",
        ));
    }
    if config.proxy_mode == ProxyMode::List && config.proxy_list_models.is_empty() {
        return Err(ProjectionError::new(
            "invalid_proxy",
            "list proxy mode has no public models",
        ));
    }
    crate::models::normalize_proxy_url(config.proxy_mode, &config.proxy_url)
        .map_err(|_| ProjectionError::new("invalid_proxy", "proxy URL is not valid"))
}

fn validate_oauth(refs: &[OAuthFileRef]) -> Result<Vec<OAuthFileRef>, ProjectionError> {
    let mut seen = Vec::new();
    let mut out = Vec::new();
    for item in refs {
        let path = item.relative_path.trim();
        if path.is_empty()
            || path.starts_with('/')
            || path.starts_with('\\')
            || path
                .split(['/', '\\'])
                .any(|part| part == ".." || part.is_empty())
        {
            return Err(ProjectionError::new(
                "invalid_oauth_ref",
                "OAuth reference path is not a relative file under auth-dir",
            ));
        }
        let generation = item.material_revision.trim().to_ascii_lowercase();
        if generation.len() != 64 || !generation.chars().all(|ch| ch.is_ascii_hexdigit()) {
            return Err(ProjectionError::new(
                "invalid_oauth_ref",
                "OAuth material revision must be the reported 64-digit generation",
            ));
        }
        if seen.iter().any(|saved: &String| saved == path) {
            return Err(ProjectionError::new(
                "invalid_oauth_ref",
                "duplicate OAuth reference path",
            ));
        }
        let auth_id = item.auth_id.trim();
        let credential_id = item.credential_id.trim();
        if auth_id.is_empty() || credential_id.is_empty() || item.credential_version == 0 {
            return Err(ProjectionError::new(
                "invalid_oauth_ref",
                "OAuth reference is missing its auth id, credential id, or version",
            ));
        }
        if item.models.iter().any(|model| model.trim().is_empty()) {
            return Err(ProjectionError::new(
                "invalid_oauth_ref",
                "OAuth reference model name is empty",
            ));
        }
        if item.product_provider_id.trim() != crate::provider::CPA_PROVIDER_ID {
            return Err(ProjectionError::new(
                "invalid_oauth_ref",
                "OAuth product provider must be cpa",
            ));
        }
        if !super::native_targets::native_mode_ok(item.native_mode.trim()) {
            return Err(ProjectionError::new(
                "invalid_oauth_ref",
                "OAuth native mode is not a supported fact",
            ));
        }
        seen.push(path.to_string());
        out.push(OAuthFileRef {
            provider: item.provider,
            product_provider_id: crate::provider::CPA_PROVIDER_ID.to_string(),
            relative_path: path.to_string(),
            material_revision: generation,
            auth_id: auth_id.to_string(),
            credential_id: credential_id.to_string(),
            credential_version: item.credential_version,
            routing_rank: item.routing_rank,
            models: item
                .models
                .iter()
                .map(|model| model.trim().to_string())
                .collect(),
            raw_provider_label: item.raw_provider_label.trim().to_string(),
            native_mode: item.native_mode.trim().to_string(),
            reported_base: item.reported_base.trim().to_string(),
        });
    }
    Ok(out)
}

struct AcceptedRuntime {
    process_generation: u64,
    port: u16,
    auth_dir: String,
    policy_url: String,
    policy_origin: String,
    ready_key: SecretMaterial,
    hop_secret: SecretMaterial,
    policy_token: SecretMaterial,
}

fn accept_runtime(
    runtime: &RuntimeEnvelope<'_>,
    gateway_key: &str,
) -> Result<AcceptedRuntime, ProjectionError> {
    let invalid = |detail: &str| ProjectionError::new("invalid_runtime_envelope", detail);
    if runtime.port == 0 {
        return Err(invalid("runtime port is missing"));
    }
    let auth_dir = runtime.auth_dir.trim();
    if auth_dir.is_empty()
        || auth_dir.contains('\0')
        || auth_dir.split(['/', '\\']).any(|part| part == "..")
    {
        return Err(invalid("runtime auth-dir is not an owned path"));
    }
    let hop = runtime.hop_secret.trim();
    let ready = runtime.ready_key.trim();
    let token = runtime.policy_token.trim();
    if hop.is_empty() || ready.is_empty() || token.is_empty() {
        return Err(invalid("runtime secrets are incomplete"));
    }
    if hop == ready || hop == token || ready == token {
        return Err(invalid("runtime secrets must be distinct"));
    }
    let gateway = gateway_key.trim();
    if !gateway.is_empty() && (hop == gateway || ready == gateway || token == gateway) {
        return Err(invalid("runtime secret matches the gateway client key"));
    }
    let policy = reqwest::Url::parse(runtime.policy_url.trim())
        .map_err(|_| invalid("policy URL is invalid"))?;
    crate::custom_http::inspect_custom_url(&policy)
        .map_err(|_| invalid("policy URL is invalid"))?;
    if policy.fragment().is_some()
        || policy.query().is_some()
        || policy.path() != "/_internal/ocg/cpa-policy"
    {
        return Err(invalid("policy URL is not the owned callback"));
    }
    if !crate::custom_http::is_loopback_inference_origin(policy.as_str()) {
        return Err(invalid("policy URL is not a loopback origin"));
    }
    if !crate::custom_http::origins_match(runtime.policy_origin.trim(), policy.as_str()) {
        return Err(invalid("policy origin does not match the policy URL"));
    }
    Ok(AcceptedRuntime {
        process_generation: runtime.process_generation,
        port: runtime.port,
        auth_dir: auth_dir.to_string(),
        policy_url: policy.as_str().trim_end_matches('/').to_string(),
        policy_origin: runtime
            .policy_origin
            .trim()
            .trim_end_matches('/')
            .to_string(),
        ready_key: SecretMaterial::new(ready.to_string()),
        hop_secret: SecretMaterial::new(hop.to_string()),
        policy_token: SecretMaterial::new(token.to_string()),
    })
}

#[cfg(test)]
mod placement_tests;
