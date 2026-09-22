//! Read-only V4 connection projection. Built from in-memory CoreState and
//! SQLite reads only — handlers must not touch reqwest or issue outbound
//! requests.

use axum::Json;
use axum::extract::State;
use chrono::Utc;
use std::collections::{HashMap, HashSet};

use crate::dashboard_v3::{AccountUpstreamProtocol, ControlRevision, V3ApiError};
use crate::dynamic::DynamicProviderRuntime;
use crate::models::Account;
use crate::provider::{
    BUILTIN_PROVIDERS, ConnectionVerificationStatus, ProviderAdapterKind, builtin_offering,
    preset_offering,
};
use crate::provider_contracts::EffectiveContractSet;
use crate::state::CoreState;
use ocg_domain::catalog::CredentialKind;
use ocg_domain::connection::{
    ConnectionId, ConnectionLifecycle as DomainLifecycle, ConnectionOrigin, CredentialFacts,
    EndpointAuthScheme, EndpointOperation, LegacyConnectionKind, connection_id_for_legacy,
    cooling_all_usable, derive_authorization, derive_eligibility, endpoint_id_for, target_id_for,
};
use ocg_domain::credential::{RouteSpec, assigned_endpoints_for_routes};
use ocg_domain::destination::{Destination, LegacyDestinationRef};
use ocg_domain::dynamic::DynamicAuthKind;
use ocg_domain::ids::CUSTOM_PROVIDER_ID;
use ocg_domain::provider::provider_origin_from_preset;

use super::identities::{CredentialCreateFacts, credential_create_capability};
use super::templates::{is_cpa_id, offering_kind};
use super::types::{
    ConnectionEndpoint, ConnectionLifecycle, ConnectionList, ConnectionSummary, ConnectionTarget,
    CredentialCreateCapabilityDto, Eligibility, LegacyIdentity, OfferingKind, TemplateRef,
};

const TEMPLATE_VERSION: u32 = 1;

pub(super) async fn list_connections(
    State(state): State<CoreState>,
) -> Result<Json<ConnectionList>, V3ApiError> {
    list_connections_locked(&state).map(Json)
}

fn list_connections_locked(state: &CoreState) -> Result<ConnectionList, V3ApiError> {
    let _settings_update = state.settings_update.lock();
    let now = Utc::now();
    let contracts = state.provider_contracts();
    let (accounts, custom_runtimes, dynamic_providers, draft_ids, projection) = {
        let db = state.db.lock();
        let accounts = db.list_accounts().map_err(V3ApiError::internal)?;
        let custom_runtimes = db
            .list_custom_account_runtimes()
            .map_err(V3ApiError::internal)?;
        let dynamic_providers = db
            .list_control_plane_dynamic_providers()
            .map_err(V3ApiError::internal)?;
        let draft_ids = db
            .onboarding_draft_provider_ids()
            .map_err(V3ApiError::internal)?;
        let projection = crate::destination_projection::read_v4_projection(&db)
            .map_err(V3ApiError::internal)?
            .map_err(|_| V3ApiError::conflict_at(&state, "destination projection refused"))?;
        let mut verification = HashMap::new();
        for account in &accounts {
            if let Some(row) = db
                .account_verification_state(&account.id)
                .map_err(V3ApiError::internal)?
            {
                verification.insert(account.id.clone(), row.status);
            }
        }
        (
            accounts
                .into_iter()
                .map(|account| {
                    let status = verification
                        .get(&account.id)
                        .copied()
                        .unwrap_or(ConnectionVerificationStatus::NotRequired);
                    (account, status)
                })
                .collect::<Vec<_>>(),
            custom_runtimes,
            dynamic_providers,
            draft_ids,
            projection,
        )
    };

    let mut by_provider: HashMap<String, Vec<usize>> = HashMap::new();
    for (index, (account, _)) in accounts.iter().enumerate() {
        by_provider
            .entry(account.provider_id.clone())
            .or_default()
            .push(index);
    }

    let mut connections = Vec::new();
    for plan in BUILTIN_PROVIDERS {
        if plan.product_surface.is_external_integration() || is_cpa_id(plan.provider_id) {
            continue;
        }
        if plan.provider_id == CUSTOM_PROVIDER_ID {
            continue;
        }
        let Some(indexes) = by_provider.get(plan.provider_id) else {
            continue;
        };
        if indexes.is_empty() {
            continue;
        }
        let group: Vec<_> = indexes
            .iter()
            .map(|index| (&accounts[*index].0, accounts[*index].1))
            .collect();
        connections.push(project_builtin(&plan, &group, &contracts, now));
    }

    let destination_by_dynamic: HashMap<&str, &Destination> = projection
        .destinations
        .iter()
        .filter_map(|destination| match &destination.legacy {
            LegacyDestinationRef::Dynamic(id) => Some((id.as_str(), destination)),
            _ => None,
        })
        .collect();
    for runtime in dynamic_providers.iter() {
        let empty = Vec::new();
        let indexes = by_provider.get(&runtime.id).unwrap_or(&empty);
        let group: Vec<_> = indexes
            .iter()
            .map(|index| (&accounts[*index].0, accounts[*index].1))
            .collect();
        let mut summary = project_dynamic(runtime, &group, now, draft_ids.contains(&runtime.id));
        if let Some(destination) = destination_by_dynamic.get(runtime.id.as_str()) {
            let connection_id =
                connection_id_for_legacy(LegacyConnectionKind::DynamicProvider, &runtime.id);
            let (endpoints, targets) = http_connection_members(destination, &connection_id);
            summary.endpoints = endpoints;
            summary.targets = targets;
        }
        connections.push(summary);
    }

    let accounts_by_id: HashMap<&str, &(Account, ConnectionVerificationStatus)> = accounts
        .iter()
        .map(|row| (row.0.id.as_str(), row))
        .collect();
    let destination_by_account = projection
        .credentials
        .iter()
        .map(|credential| {
            (
                credential.legacy_account_id.as_str(),
                credential.destination_id.as_str(),
            )
        })
        .collect::<HashMap<_, _>>();
    let mut custom_indexes_by_destination: HashMap<&str, Vec<usize>> = HashMap::new();
    for (index, (account, _)) in accounts.iter().enumerate() {
        if account.provider_id != CUSTOM_PROVIDER_ID {
            continue;
        }
        let Some(destination_id) = destination_by_account.get(account.id.as_str()).copied() else {
            continue;
        };
        custom_indexes_by_destination
            .entry(destination_id)
            .or_default()
            .push(index);
    }
    let mut projected_custom_accounts = HashSet::new();
    for destination in projection
        .destinations
        .iter()
        .filter(|destination| matches!(destination.legacy, LegacyDestinationRef::CustomAccount(_)))
    {
        let empty = Vec::new();
        let indexes = custom_indexes_by_destination
            .get(destination.id.as_str())
            .unwrap_or(&empty);
        let group: Vec<_> = indexes
            .iter()
            .map(|index| (&accounts[*index].0, accounts[*index].1))
            .collect();
        projected_custom_accounts.extend(group.iter().map(|(account, _)| account.id.as_str()));
        connections.push(project_custom_destination(destination, &group, now));
    }
    // Platform-linked Custom Keys retain their pre-unification projection;
    // their service definition is owned by the platform parent, not editable
    // through the Custom destination route.
    for runtime in custom_runtimes {
        if projected_custom_accounts.contains(runtime.account_id.as_str()) {
            continue;
        }
        let Some((account, status)) = accounts_by_id.get(runtime.account_id.as_str()) else {
            continue;
        };
        connections.push(project_custom(account, *status, &runtime, now));
    }

    Ok(ConnectionList {
        revision: ControlRevision::from_state(state),
        connections,
    })
}

fn http_connection_members(
    destination: &Destination,
    connection_id: &ocg_domain::connection::ConnectionId,
) -> (Vec<ConnectionEndpoint>, Vec<ConnectionTarget>) {
    use ocg_domain::destination::{http_configured_routes, http_model_route, http_protocol_routes};
    let routes = http_configured_routes(destination);
    let declared = http_protocol_routes(destination);
    let endpoints: Vec<_> = assigned_endpoints_for_routes(connection_id, &routes)
        .into_iter()
        .zip(routes)
        .map(|(assigned, route)| {
            let protocol = ocg_domain::catalog::UpstreamProtocolKind::from(route.operation);
            let auth = declared
                .iter()
                .find(|entry| entry.protocol == protocol)
                .map(|entry| entry.auth_scheme)
                .unwrap_or(destination.auth_scheme);
            let auth = match auth {
                ocg_domain::destination::AuthScheme::Bearer => EndpointAuthScheme::Bearer,
                ocg_domain::destination::AuthScheme::XApiKey => EndpointAuthScheme::XApiKey,
                ocg_domain::destination::AuthScheme::None => EndpointAuthScheme::None,
            };
            endpoint_dto(
                connection_id,
                assigned.id,
                route.operation,
                protocol,
                assigned.url,
                auth,
                false,
            )
        })
        .collect();
    let targets = destination
        .catalog
        .iter()
        .map(|model| {
            let endpoint_ids = model
                .protocols
                .iter()
                .filter_map(|protocol| {
                    let route = http_model_route(destination, model, *protocol)?;
                    endpoints
                        .iter()
                        .find(|endpoint| {
                            endpoint.operation == EndpointOperation::from(*protocol)
                                && endpoint.url.as_deref() == Some(route.endpoint_url.as_str())
                        })
                        .map(|endpoint| endpoint.id.clone())
                })
                .collect();
            ConnectionTarget {
                id: target_id_for(connection_id, &model.public_model).to_string(),
                connection_id: connection_id.to_string(),
                public_name: model.public_model.clone(),
                upstream_model_id: model.upstream_model.clone(),
                endpoint_ids,
                enabled: model.enabled,
            }
        })
        .collect();
    (endpoints, targets)
}

fn project_custom_destination(
    destination: &Destination,
    accounts: &[(&Account, ConnectionVerificationStatus)],
    now: chrono::DateTime<Utc>,
) -> ConnectionSummary {
    let LegacyDestinationRef::CustomAccount(legacy_id) = &destination.legacy else {
        unreachable!("filtered to legacy Custom destinations")
    };
    let connection_id = connection_id_for_legacy(LegacyConnectionKind::CustomAccount, legacy_id);
    let (endpoints, targets) = http_connection_members(destination, &connection_id);
    let facts = credential_facts(accounts, now);
    finish_summary(SummaryDraft {
        connection_id,
        name: destination.name.clone(),
        origin: ConnectionOrigin::CustomAccount,
        template_ref: Some(TemplateRef {
            id: CUSTOM_PROVIDER_ID.to_string(),
            version: TEMPLATE_VERSION,
        }),
        adapter_kind: ProviderAdapterKind::ConfigurableHttp.as_str().to_string(),
        credential_kind: CredentialKind::ApiKey,
        facts: &facts,
        accounts,
        endpoints,
        targets,
        legacy: LegacyIdentity {
            kind: LegacyConnectionKind::CustomAccount,
            id: legacy_id.clone(),
        },
        display_family: Some("Custom".to_string()),
        offering: OfferingKind::Api,
        onboarding_draft: false,
        credential_create: credential_create_capability(&CredentialCreateFacts::CustomAccount),
    })
}

fn project_builtin(
    plan: &crate::provider::BuiltinProvider,
    accounts: &[(&Account, ConnectionVerificationStatus)],
    contracts: &EffectiveContractSet,
    now: chrono::DateTime<Utc>,
) -> ConnectionSummary {
    let connection_id =
        connection_id_for_legacy(LegacyConnectionKind::BuiltinProvider, plan.provider_id);
    let endpoints: Vec<ConnectionEndpoint> = plan
        .upstream_protocols
        .iter()
        .copied()
        .map(|protocol| {
            let operation = EndpointOperation::from(protocol);
            endpoint_dto(
                &connection_id,
                endpoint_id_for(&connection_id, operation),
                operation,
                protocol,
                None,
                EndpointAuthScheme::Sealed,
                true,
            )
        })
        .collect();
    let endpoint_ids: Vec<String> = endpoints
        .iter()
        .map(|endpoint| endpoint.id.clone())
        .collect();
    let targets = builtin_targets(&connection_id, plan.provider_id, contracts, &endpoint_ids);
    let facts = credential_facts(accounts, now);
    finish_summary(SummaryDraft {
        connection_id,
        name: plan.display_name.to_string(),
        origin: ConnectionOrigin::Builtin,
        template_ref: Some(TemplateRef {
            id: plan.provider_id.to_string(),
            version: TEMPLATE_VERSION,
        }),
        adapter_kind: ProviderAdapterKind::from_provider_id(plan.provider_id)
            .expect("builtin catalog rows have a sealed adapter")
            .as_str()
            .to_string(),
        credential_kind: plan.credential_kind,
        facts: &facts,
        accounts,
        endpoints,
        targets,
        legacy: LegacyIdentity {
            kind: LegacyConnectionKind::BuiltinProvider,
            id: plan.provider_id.to_string(),
        },
        display_family: Some(plan.display_family.to_string()),
        offering: offering_kind(builtin_offering(plan.provider_id)),
        onboarding_draft: false,
        credential_create: credential_create_capability(&CredentialCreateFacts::Builtin(plan)),
    })
}

fn project_dynamic(
    runtime: &DynamicProviderRuntime,
    accounts: &[(&Account, ConnectionVerificationStatus)],
    now: chrono::DateTime<Utc>,
    onboarding_draft: bool,
) -> ConnectionSummary {
    let connection_id =
        connection_id_for_legacy(LegacyConnectionKind::DynamicProvider, &runtime.id);
    let (endpoints, targets) = dynamic_routes(&connection_id, runtime);
    let facts = credential_facts(accounts, now);
    let origin = match provider_origin_from_preset(runtime.preset_id.as_deref()) {
        ocg_domain::provider::ProviderOrigin::Preset => ConnectionOrigin::Preset,
        _ => ConnectionOrigin::Custom,
    };
    let offering = runtime
        .preset_id
        .as_deref()
        .map(preset_offering)
        .unwrap_or(runtime.offering.as_str());
    finish_summary(SummaryDraft {
        connection_id,
        name: runtime.name.clone(),
        origin,
        template_ref: runtime.preset_id.as_ref().map(|id| TemplateRef {
            id: id.clone(),
            version: TEMPLATE_VERSION,
        }),
        adapter_kind: ProviderAdapterKind::ConfigurableHttp.as_str().to_string(),
        credential_kind: runtime.auth_kind.credential_kind(),
        facts: &facts,
        accounts,
        endpoints,
        targets,
        legacy: LegacyIdentity {
            kind: LegacyConnectionKind::DynamicProvider,
            id: runtime.id.clone(),
        },
        display_family: None,
        offering: offering_kind(offering),
        onboarding_draft,
        credential_create: credential_create_capability(&CredentialCreateFacts::Dynamic {
            runtime,
            onboarding_draft,
        }),
    })
}

fn project_custom(
    account: &Account,
    verification: ConnectionVerificationStatus,
    runtime: &crate::custom::CustomAccountRuntime,
    now: chrono::DateTime<Utc>,
) -> ConnectionSummary {
    let connection_id = connection_id_for_legacy(LegacyConnectionKind::CustomAccount, &account.id);
    let operation = EndpointOperation::from(runtime.config.upstream_protocol);
    let endpoint_id = endpoint_id_for(&connection_id, operation);
    let endpoints = vec![endpoint_dto(
        &connection_id,
        endpoint_id.clone(),
        operation,
        runtime.config.upstream_protocol,
        Some(runtime.config.endpoint_url.clone()),
        match runtime.auth_kind {
            DynamicAuthKind::Bearer => EndpointAuthScheme::Bearer,
            DynamicAuthKind::XApiKey => EndpointAuthScheme::XApiKey,
            DynamicAuthKind::None => EndpointAuthScheme::None,
        },
        false,
    )];
    let targets: Vec<ConnectionTarget> = runtime
        .capabilities
        .iter()
        .map(|capability| {
            let id = target_id_for(&connection_id, &capability.public_model);
            ConnectionTarget {
                id: id.to_string(),
                connection_id: connection_id.to_string(),
                public_name: capability.public_model.clone(),
                upstream_model_id: capability.upstream_model.clone(),
                endpoint_ids: vec![endpoint_id.to_string()],
                enabled: true,
            }
        })
        .collect();
    let facts = credential_facts(&[(account, verification)], now);
    finish_summary(SummaryDraft {
        connection_id,
        name: account.name.clone(),
        origin: ConnectionOrigin::CustomAccount,
        template_ref: Some(TemplateRef {
            id: CUSTOM_PROVIDER_ID.to_string(),
            version: TEMPLATE_VERSION,
        }),
        adapter_kind: ProviderAdapterKind::ConfigurableHttp.as_str().to_string(),
        credential_kind: account.credential_kind,
        facts: &facts,
        accounts: &[(account, verification)],
        endpoints,
        targets,
        legacy: LegacyIdentity {
            kind: LegacyConnectionKind::CustomAccount,
            id: account.id.clone(),
        },
        display_family: Some("Custom".to_string()),
        offering: OfferingKind::Api,
        onboarding_draft: false,
        credential_create: credential_create_capability(&CredentialCreateFacts::CustomAccount),
    })
}

fn dynamic_routes(
    connection_id: &ConnectionId,
    runtime: &DynamicProviderRuntime,
) -> (Vec<ConnectionEndpoint>, Vec<ConnectionTarget>) {
    let default_operation = EndpointOperation::from(runtime.upstream_protocol);
    let auth = match runtime.auth_kind {
        DynamicAuthKind::Bearer => EndpointAuthScheme::Bearer,
        DynamicAuthKind::XApiKey => EndpointAuthScheme::XApiKey,
        DynamicAuthKind::None => EndpointAuthScheme::None,
    };
    let mut routes = vec![RouteSpec {
        operation: default_operation,
        url: Some(runtime.endpoint_url.clone()),
    }];
    let mut seen_routes =
        HashSet::from([(runtime.upstream_protocol, runtime.endpoint_url.clone())]);
    for mapping in &runtime.mappings {
        let Some(override_route) = &mapping.upstream_override else {
            continue;
        };
        if !seen_routes.insert((override_route.protocol, override_route.endpoint_url.clone())) {
            continue;
        }
        routes.push(RouteSpec {
            operation: EndpointOperation::from(override_route.protocol),
            url: Some(override_route.endpoint_url.clone()),
        });
    }
    let assigned = assigned_endpoints_for_routes(connection_id, &routes);
    let endpoints: Vec<ConnectionEndpoint> = assigned
        .into_iter()
        .zip(routes)
        .map(|(assigned, route)| {
            let protocol = ocg_domain::catalog::UpstreamProtocolKind::from(route.operation);
            endpoint_dto(
                connection_id,
                assigned.id,
                route.operation,
                protocol,
                assigned.url,
                auth,
                false,
            )
        })
        .collect();

    let targets = runtime
        .mappings
        .iter()
        .map(|mapping| {
            let route = runtime.effective_route(mapping);
            let operation = EndpointOperation::from(route.protocol);
            let endpoint_id = endpoints
                .iter()
                .find(|endpoint| {
                    endpoint.operation == operation
                        && endpoint.url.as_deref() == Some(route.endpoint_url.as_str())
                })
                .map(|endpoint| endpoint.id.clone())
                .unwrap_or_else(|| endpoint_id_for(connection_id, operation).to_string());
            let id = target_id_for(connection_id, &mapping.public_model);
            ConnectionTarget {
                id: id.to_string(),
                connection_id: connection_id.to_string(),
                public_name: mapping.public_model.clone(),
                upstream_model_id: mapping.upstream_model.clone(),
                endpoint_ids: vec![endpoint_id],
                enabled: true,
            }
        })
        .collect();
    (endpoints, targets)
}

fn builtin_targets(
    connection_id: &ConnectionId,
    provider_id: &str,
    contracts: &EffectiveContractSet,
    endpoint_ids: &[String],
) -> Vec<ConnectionTarget> {
    let Some(scope) = contracts.providers.get(provider_id) else {
        return Vec::new();
    };
    let model_ids = if scope.catalog.models.is_empty() {
        scope
            .models
            .values()
            .map(|model| model.model_id.clone())
            .collect::<Vec<_>>()
    } else {
        scope.catalog.models.clone()
    };
    model_ids
        .into_iter()
        .map(|model_id| {
            let enabled = scope
                .model(&model_id)
                .map(|model| model.routable || model.has_enabled_protocol())
                .unwrap_or(true);
            let id = target_id_for(connection_id, &model_id);
            ConnectionTarget {
                id: id.to_string(),
                connection_id: connection_id.to_string(),
                public_name: model_id.clone(),
                upstream_model_id: model_id,
                endpoint_ids: endpoint_ids.to_vec(),
                enabled,
            }
        })
        .collect()
}

fn credential_facts(
    accounts: &[(&Account, ConnectionVerificationStatus)],
    now: chrono::DateTime<Utc>,
) -> Vec<CredentialFacts> {
    accounts
        .iter()
        .map(|(account, status)| CredentialFacts {
            enabled: account.enabled,
            has_auth_error: account.auth_error.is_some(),
            verified: *status == ConnectionVerificationStatus::Verified,
            cooling: account.is_cooling_at(now),
        })
        .collect()
}

struct SummaryDraft<'a> {
    connection_id: ConnectionId,
    name: String,
    origin: ConnectionOrigin,
    template_ref: Option<TemplateRef>,
    adapter_kind: String,
    credential_kind: CredentialKind,
    facts: &'a [CredentialFacts],
    accounts: &'a [(&'a Account, ConnectionVerificationStatus)],
    endpoints: Vec<ConnectionEndpoint>,
    targets: Vec<ConnectionTarget>,
    legacy: LegacyIdentity,
    display_family: Option<String>,
    offering: OfferingKind,
    onboarding_draft: bool,
    credential_create: CredentialCreateCapabilityDto,
}

fn finish_summary(draft: SummaryDraft<'_>) -> ConnectionSummary {
    let SummaryDraft {
        connection_id,
        name,
        origin,
        template_ref,
        adapter_kind,
        credential_kind,
        facts,
        accounts,
        endpoints,
        targets,
        legacy,
        display_family,
        offering,
        onboarding_draft,
        credential_create,
    } = draft;
    let domain_lifecycle =
        if !accounts.is_empty() && accounts.iter().all(|(account, _)| !account.enabled) {
            DomainLifecycle::Disabled
        } else {
            DomainLifecycle::Configured
        };
    let authorization = derive_authorization(credential_kind, facts);
    let enabled_target_count = targets.iter().filter(|target| target.enabled).count();
    let enabled_credential_count = facts.iter().filter(|fact| fact.enabled).count();
    let (eligibility_state, eligibility_reason) = if onboarding_draft {
        (
            ocg_domain::connection::EligibilityState::Ineligible,
            ocg_domain::connection::EligibilityReason::ConnectionDisabled,
        )
    } else {
        derive_eligibility(
            domain_lifecycle,
            authorization,
            enabled_target_count,
            cooling_all_usable(facts),
            enabled_credential_count,
        )
    };
    let lifecycle = if onboarding_draft {
        ConnectionLifecycle::Draft
    } else {
        ConnectionLifecycle::from(domain_lifecycle)
    };
    let credit_presets = if matches!(
        legacy.kind,
        LegacyConnectionKind::DynamicProvider | LegacyConnectionKind::CustomAccount
    ) {
        Some(
            endpoints
                // The first route is the destination's base endpoint. Model
                // overrides must not change the Key's billing setup.
                .first()
                .and_then(|endpoint| endpoint.url.as_deref())
                .and_then(|url| crate::billing::stepfun_plan_credits(url, Utc::now()))
                .unwrap_or_default(),
        )
    } else {
        None
    };
    ConnectionSummary {
        credential_create,
        id: connection_id.to_string(),
        name,
        origin,
        template_ref,
        adapter_kind,
        lifecycle,
        authorization,
        eligibility: Eligibility {
            state: eligibility_state,
            reason: eligibility_reason,
        },
        credential_count: facts.len() as u32,
        enabled_credential_count: enabled_credential_count as u32,
        target_count: targets.len() as u32,
        endpoints,
        targets,
        legacy,
        display_family,
        offering,
        credit_presets,
    }
}

fn endpoint_dto(
    connection_id: &ConnectionId,
    id: impl AsRef<str>,
    operation: EndpointOperation,
    protocol: ocg_domain::catalog::UpstreamProtocolKind,
    url: Option<String>,
    auth_scheme: EndpointAuthScheme,
    locked: bool,
) -> ConnectionEndpoint {
    ConnectionEndpoint {
        official_balance: url
            .as_deref()
            .is_some_and(crate::official_service::has_official_balance),
        id: id.as_ref().to_string(),
        connection_id: connection_id.to_string(),
        operation,
        wire_protocol: AccountUpstreamProtocol::from(protocol),
        url,
        auth_scheme,
        locked,
    }
}

#[cfg(test)]
mod tests;
