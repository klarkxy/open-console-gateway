use super::*;
use crate::log_types::OperationOutcome;
use ocg_domain::credential::ModelScope;
use std::collections::{HashMap, HashSet};

#[test]
fn import_receipt_counts_duplicates_separately_from_failures() {
    let clean = transfer_import_receipt(2, 3, 0);
    assert!(clean.outcome.is_none());
    assert_eq!(clean.requested, 5);
    assert_eq!(clean.completed, 2);
    assert!(clean.failed.is_none());

    let partial = transfer_import_receipt(2, 1, 4);
    assert_eq!(
        partial.outcome,
        Some((OperationOutcome::Partial, "internal"))
    );
    assert_eq!(partial.requested, 7);
    assert_eq!(partial.completed, 2);
    assert_eq!(partial.failed, Some(4));

    let failed = transfer_import_receipt(0, 2, 3);
    assert_eq!(failed.outcome, Some((OperationOutcome::Failed, "internal")));
    assert_eq!(failed.requested, 5);
    assert_eq!(failed.completed, 0);
    assert_eq!(failed.failed, Some(3));

    let excluded = transfer_exclusion_receipt(4, 2);
    assert!(excluded.outcome.is_none());
    assert_eq!(excluded.requested, 6);
    assert_eq!(excluded.completed, 4);
    assert!(excluded.failed.is_none());
}

#[test]
fn credit_v10_transfer_restores_new_accounts_but_never_refills_existing_accounts() {
    use crate::billing_types::{
        CreditBalanceCorrection, CreditBucket, CreditBucketKind, CreditConfigurationWrite,
    };
    let account = "00000000-0000-4000-8000-0000000000a1";
    let (source_dir, source) = seed_ab_accounts("credit-transfer-source");
    let now = chrono::Utc::now();
    crate::db::billing::configure_on(
        &source.db.lock().conn,
        account,
        CreditConfigurationWrite {
            name: "credits".into(),
            currency: "CNY".into(),
            monthly: None,
            source_url: None,
        },
        Some(vec![CreditBucket {
            id: "current".into(),
            kind: CreditBucketKind::Manual,
            label: "remaining".into(),
            granted: 100.0,
            remaining: 35.0,
            starts_at: now,
            expires_at: None,
        }]),
        now,
    )
    .unwrap();
    let original = crate::db::billing::load_on(&source.db.lock().conn, account)
        .unwrap()
        .unwrap();
    let (mut payload, _, _) = export_payload(&source).unwrap();
    assert_eq!(payload.version, PAYLOAD_VERSION);
    assert!(
        payload
            .credentials
            .iter()
            .any(|credential| credential.credit_meter.is_some())
    );
    let bundle = encrypt_payload(&payload, "correct horse battery").unwrap();
    let (target_dir, target) = transfer_state("credit-transfer-target");
    local_import(&target, "correct horse battery", &bundle).unwrap();
    let imported = crate::db::billing::load_on(&target.db.lock().conn, account)
        .unwrap()
        .unwrap();
    assert_ne!(original.meter_id, imported.meter_id);
    assert_eq!(imported.project(now, 0).remaining, 35.0);
    crate::db::billing::calibrate_on(
        &target.db.lock().conn,
        account,
        &[CreditBalanceCorrection {
            bucket_id: "current".into(),
            remaining: 3.0,
        }],
        now,
    )
    .unwrap();
    local_import(&target, "correct horse battery", &bundle).unwrap();
    assert_eq!(
        crate::db::billing::load_on(&target.db.lock().conn, account)
            .unwrap()
            .unwrap()
            .project(now, 0)
            .remaining,
        3.0
    );
    payload.version = 9;
    let invalid_old = encrypt_payload(&payload, "correct horse battery").unwrap();
    assert!(decrypt_and_validate(&invalid_old, "correct horse battery").is_err());
    for credential in &mut payload.credentials {
        credential.credit_meter = None;
    }
    let valid_old = encrypt_payload(&payload, "correct horse battery").unwrap();
    local_import(&target, "correct horse battery", &valid_old).unwrap();
    assert_eq!(
        crate::db::billing::load_on(&target.db.lock().conn, account)
            .unwrap()
            .unwrap()
            .project(now, 0)
            .remaining,
        3.0
    );
    drop(source);
    drop(target);
    std::fs::remove_dir_all(source_dir).unwrap();
    std::fs::remove_dir_all(target_dir).unwrap();
}

fn sample_account(name: impl Into<String>) -> PortableAccount {
    PortableAccount {
        id: None,
        provider_id: "opencode".to_string(),

        name: name.into(),
        username: Some("user@example.com".to_string()),
        key: "sk-ocg-test-secret".to_string(),
        enabled: true,
        account_type: "key".to_string(),
        setup_step: "ready".to_string(),
        purchase_date: "2026-08-01".to_string(),
        expires_on: String::new(),
        notes: Some("portable".to_string()),
        verification_status: None,
        connection_verified_at: None,
        custom_config: None,
        model_capabilities: Vec::new(),
        ollama_billing_tier: None,
        identity_id: None,
        credential_id: None,
        credential_version: None,
        auth_state_version: None,
        binding_id: None,
        binding_enabled: None,
        binding_model_scope: None,
        allowed_endpoint_ids: None,
        allowed_origins: None,
        cooldowns: None,
    }
}

fn sample_account_graph() -> PortablePayload {
    let account_id = "00000000-0000-4000-8000-000000000041";
    let mut account = sample_account("Primary");
    account.id = Some(account_id.to_string());
    let mut payload = PortablePayload {
        platform_accounts: Vec::new(),
        platform_links: Vec::new(),
        version: V6_PAYLOAD_VERSION,
        exported_at: "2026-08-29T00:00:00Z".to_string(),
        accounts: vec![account],
        dynamic_providers: Vec::new(),
        identities: Vec::new(),
        quota_pools: Vec::new(),
        destinations: Vec::new(),
        credentials: Vec::new(),
        routing_cards: None,
        node: Some(sample_node(account_id)),
    };
    attach_default_identity_snapshot(&mut payload);
    payload
}

fn sample_v6_payload() -> PortablePayload {
    sample_account_graph()
}

fn sample_payload() -> PortablePayload {
    let mut payload = sample_account_graph();
    payload.version = PAYLOAD_VERSION;
    attach_default_destination_snapshot(&mut payload);
    payload.accounts.clear();
    payload.identities.clear();
    payload.dynamic_providers.clear();
    payload.platform_accounts.clear();
    payload.platform_links.clear();
    payload
}

const V11_CUSTOM_ACCOUNT_ID: &str = "00000000-0000-4000-8000-0000000000c1";
const V11_DYNAMIC_PROVIDER_ID: &str = "00000000-0000-4000-8000-0000000000d1";
const V11_DEFAULT_ENDPOINT: &str = "https://lab.example/v1";
const V11_MESSAGES_ENDPOINT: &str = "https://lab.example/anthropic";
const V11_GRANT_ENDPOINTS: [&str; 2] = [
    "11111111-1111-4111-8111-111111111111",
    "22222222-2222-4222-8222-222222222222",
];
const V11_GRANT_ORIGIN: &str = "https://lab.example";

fn v11_http_catalog() -> Vec<super::portable::PortableCatalogModel> {
    use super::portable::{PortableCatalogModel, PortableCatalogOverride};
    use crate::dashboard_v4::types::ProtocolDto;
    vec![
        PortableCatalogModel {
            public_model: "lab-chat".to_string(),
            upstream_model: "lab-chat-upstream".to_string(),
            protocols: vec![ProtocolDto::ChatCompletions, ProtocolDto::Messages],
            preferred: Some(ProtocolDto::Messages),
            enabled: true,
            upstream_override: Some(PortableCatalogOverride {
                protocol: "messages".to_string(),
                endpoint_url: V11_MESSAGES_ENDPOINT.to_string(),
            }),
        },
        PortableCatalogModel {
            public_model: "lab-off".to_string(),
            upstream_model: "lab-off-upstream".to_string(),
            protocols: vec![ProtocolDto::ChatCompletions],
            preferred: None,
            enabled: false,
            upstream_override: None,
        },
    ]
}

fn v11_http_routes() -> Vec<crate::dashboard_v4::types::HttpProtocolRouteDto> {
    use crate::dashboard_v4::types::{AuthSchemeDto, HttpProtocolRouteDto, ProtocolDto};
    vec![
        HttpProtocolRouteDto {
            protocol: ProtocolDto::ChatCompletions,
            endpoint_url: V11_DEFAULT_ENDPOINT.to_string(),
            auth_scheme: AuthSchemeDto::Bearer,
        },
        HttpProtocolRouteDto {
            protocol: ProtocolDto::Messages,
            endpoint_url: V11_MESSAGES_ENDPOINT.to_string(),
            auth_scheme: AuthSchemeDto::Bearer,
        },
    ]
}

fn v11_http_base_destination(
    id: String,
    kind: crate::dashboard_v4::types::LegacyDestinationKindDto,
    legacy_id: String,
) -> super::portable::PortableDestination {
    use crate::dashboard_v4::types::{
        AdapterKindDto, AuthSchemeDto, CapabilitiesDto, LegacyDestinationRefDto, ProtocolDto,
    };
    use ocg_domain::destination::{AdapterKind, ModelResolution, sealed_capabilities};
    let model_resolution = match kind {
        crate::dashboard_v4::types::LegacyDestinationKindDto::Dynamic => {
            ModelResolution::PublicAndUpstream
        }
        _ => ModelResolution::PublicOnly,
    };
    super::portable::PortableDestination {
        id,
        legacy: LegacyDestinationRefDto {
            kind,
            id: legacy_id,
        },
        adapter: AdapterKindDto::Http,
        name: "Lab HTTP".to_string(),
        brand_family: None,
        base_url: Some(V11_DEFAULT_ENDPOINT.to_string()),
        protocols: vec![ProtocolDto::ChatCompletions, ProtocolDto::Messages],
        protocol_routes: v11_http_routes(),
        auth_scheme: AuthSchemeDto::Bearer,
        model_resolution: Some(model_resolution),
        catalog: v11_http_catalog(),
        capabilities: CapabilitiesDto::from(&sealed_capabilities(AdapterKind::Http)),
        plan: None,
        max_credentials: None,
        observer_credential_id: None,
        enabled: true,
        platform_kind: None,
        platform_version: None,
        platform_snapshot: None,
        onboarding_draft: None,
        preset_id: None,
        origin: None,
        offering: None,
    }
}

fn v11_http_transfer_payload() -> PortablePayload {
    use crate::dashboard_v4::types::{CredentialGrantsDto, LegacyDestinationKindDto, RoutingCard};
    use ocg_domain::connection::{LegacyConnectionKind, connection_id_for_legacy};
    use ocg_domain::credential::{
        binding_id_for, credential_id_for_legacy_account, identity_id_for_legacy_account,
        quota_pool_id_for_identity,
    };
    use ocg_domain::destination::{destination_id_for_custom_account, destination_id_for_dynamic};
    use ocg_domain::provider::{preset_offering, provider_origin_from_preset};

    let mut payload = sample_payload();
    let custom_dest_id = destination_id_for_custom_account(V11_CUSTOM_ACCOUNT_ID);
    payload.destinations.push(v11_http_base_destination(
        custom_dest_id.clone(),
        LegacyDestinationKindDto::CustomAccount,
        V11_CUSTOM_ACCOUNT_ID.to_string(),
    ));

    let mut dynamic = v11_http_base_destination(
        destination_id_for_dynamic(V11_DYNAMIC_PROVIDER_ID),
        LegacyDestinationKindDto::Dynamic,
        V11_DYNAMIC_PROVIDER_ID.to_string(),
    );
    dynamic.name = "Lab Dynamic".to_string();
    dynamic.onboarding_draft = Some(false);
    dynamic.origin = Some(provider_origin_from_preset(None).as_str().to_string());
    dynamic.offering = Some(preset_offering("").to_string());
    payload.destinations.push(dynamic);

    let mut credential = payload.credentials[0].clone();
    let identity_id = identity_id_for_legacy_account(V11_CUSTOM_ACCOUNT_ID);
    let credential_id = credential_id_for_legacy_account(V11_CUSTOM_ACCOUNT_ID);
    let connection_id =
        connection_id_for_legacy(LegacyConnectionKind::CustomAccount, V11_CUSTOM_ACCOUNT_ID);
    let binding_id = binding_id_for(&credential_id, &connection_id);
    credential.id = credential_id.to_string();
    credential.legacy_account_id = V11_CUSTOM_ACCOUNT_ID.to_string();
    credential.destination_id = custom_dest_id.clone();
    credential.name = "Lab HTTP".to_string();
    credential.provider_id = Some(crate::kernel::ids::CUSTOM_PROVIDER_ID.to_string());
    credential.identity_id = Some(identity_id.to_string());
    credential.identity_label = Some("Lab HTTP".to_string());
    credential.binding_id = Some(binding_id.to_string());
    credential.grants = CredentialGrantsDto {
        allowed_endpoint_ids: V11_GRANT_ENDPOINTS
            .iter()
            .map(|id| (*id).to_string())
            .collect(),
        allowed_origins: vec![V11_GRANT_ORIGIN.to_string()],
    };
    credential.routing_rank = payload
        .credentials
        .iter()
        .map(|row| row.routing_rank)
        .max()
        .unwrap_or(0)
        + 1;
    payload.credentials.push(credential.clone());
    payload.quota_pools.push(PortableQuotaPool {
        id: quota_pool_id_for_identity(identity_id.as_str()).to_string(),
        subject_kind: "credential".to_string(),
        subject_ref: identity_id.to_string(),
        relation_confidence: "unknown".to_string(),
        policy_mode: "authoritative_limit".to_string(),
        member_account_ids: vec![V11_CUSTOM_ACCOUNT_ID.to_string()],
    });
    if let Some(cards) = payload.routing_cards.as_mut() {
        cards.push(RoutingCard {
            id: format!("card:{}:custom", custom_dest_id),
            destination_id: custom_dest_id,
            credential_ids: vec![credential.id],
        });
    }
    if let Some(node) = payload.node.as_mut() {
        node.account_order.push(V11_CUSTOM_ACCOUNT_ID.to_string());
    }
    payload
}

fn strip_protocol_routes(payload: &mut PortablePayload) {
    for destination in &mut payload.destinations {
        destination.protocol_routes.clear();
    }
}

fn sample_legacy_payload(version: u32) -> PortablePayload {
    let mut payload = sample_account_graph();
    payload.version = version;
    if version < V6_PAYLOAD_VERSION {
        strip_identity_snapshot(&mut payload);
    }
    payload.destinations.clear();
    payload.credentials.clear();
    payload
}

fn portable_safe_grants(
    account: &PortableAccount,
    dynamics: &[PortableProviderDefinition],
) -> (Vec<String>, Vec<String>) {
    use ocg_domain::connection::{
        EndpointOperation, LegacyConnectionKind, connection_id_for_legacy,
    };
    use ocg_domain::credential::{RouteSpec, assigned_endpoints_for_routes, safe_default_grants};
    let account_id = account.id.as_deref().unwrap_or_default();
    if account.provider_id == crate::kernel::ids::CUSTOM_PROVIDER_ID {
        let connection_id =
            connection_id_for_legacy(LegacyConnectionKind::CustomAccount, account_id);
        let Some(config) = &account.custom_config else {
            return (Vec::new(), Vec::new());
        };
        let Ok(protocol) = UpstreamProtocolKind::try_from(config.upstream_protocol.as_str()) else {
            return (Vec::new(), Vec::new());
        };
        return safe_default_grants(&assigned_endpoints_for_routes(
            &connection_id,
            &[RouteSpec {
                operation: EndpointOperation::from(protocol),
                url: Some(config.endpoint_url.clone()),
            }],
        ));
    }
    if let Some(plan) = builtin_provider(&account.provider_id) {
        let connection_id =
            connection_id_for_legacy(LegacyConnectionKind::BuiltinProvider, plan.provider_id);
        let routes: Vec<_> = plan
            .upstream_protocols
            .iter()
            .copied()
            .map(|protocol| RouteSpec {
                operation: EndpointOperation::from(protocol),
                url: None,
            })
            .collect();
        return safe_default_grants(&assigned_endpoints_for_routes(&connection_id, &routes));
    }
    if let Some(runtime) = dynamics.iter().find(|row| row.id == account.provider_id) {
        let connection_id =
            connection_id_for_legacy(LegacyConnectionKind::DynamicProvider, &runtime.id);
        let Ok(protocol) = UpstreamProtocolKind::try_from(runtime.upstream_protocol.as_str())
        else {
            return (Vec::new(), Vec::new());
        };
        let mut routes = vec![RouteSpec {
            operation: EndpointOperation::from(protocol),
            url: Some(runtime.endpoint_url.clone()),
        }];
        let mut seen = HashSet::from([(
            runtime.upstream_protocol.clone(),
            runtime.endpoint_url.clone(),
        )]);
        for model in &runtime.models {
            let Some(override_route) = &model.upstream_override else {
                continue;
            };
            if !seen.insert((
                override_route.protocol.clone(),
                override_route.endpoint_url.clone(),
            )) {
                continue;
            }
            let Ok(kind) = UpstreamProtocolKind::try_from(override_route.protocol.as_str()) else {
                continue;
            };
            routes.push(RouteSpec {
                operation: EndpointOperation::from(kind),
                url: Some(override_route.endpoint_url.clone()),
            });
        }
        return safe_default_grants(&assigned_endpoints_for_routes(&connection_id, &routes));
    }
    (Vec::new(), Vec::new())
}

fn attach_default_identity_snapshot(payload: &mut PortablePayload) {
    use ocg_domain::connection::{LegacyConnectionKind, connection_id_for_legacy};
    use ocg_domain::credential::{
        binding_id_for, credential_id_for_legacy_account, identity_id_for_legacy_account,
        quota_pool_id_for_identity,
    };
    payload.identities.clear();
    payload.quota_pools.clear();
    let grants: Vec<_> = payload
        .accounts
        .iter()
        .map(|account| portable_safe_grants(account, &payload.dynamic_providers))
        .collect();
    let mut seen_identities = HashSet::new();
    for (account, (grant_ids, grant_origins)) in payload.accounts.iter_mut().zip(grants) {
        let Some(account_id) = account.id.clone() else {
            continue;
        };
        let identity_id = identity_id_for_legacy_account(&account_id);
        let credential_id = credential_id_for_legacy_account(&account_id);
        let connection_id = if account.provider_id == crate::kernel::ids::CUSTOM_PROVIDER_ID {
            connection_id_for_legacy(LegacyConnectionKind::CustomAccount, &account_id)
        } else {
            connection_id_for_legacy(LegacyConnectionKind::BuiltinProvider, &account.provider_id)
        };
        let binding_id = binding_id_for(&credential_id, &connection_id);
        account.identity_id = Some(identity_id.to_string());
        account.credential_id = Some(credential_id.to_string());
        account.credential_version = Some(1);
        account.auth_state_version = Some(1);
        account.binding_id = Some(binding_id.to_string());
        account.binding_enabled = Some(account.enabled);
        account.binding_model_scope = Some(ModelScope::All);
        account.cooldowns = Some(PortableCooldowns::default());
        account.allowed_endpoint_ids = Some(grant_ids);
        account.allowed_origins = Some(grant_origins);
        if seen_identities.insert(identity_id.to_string()) {
            payload.identities.push(PortableIdentity {
                id: identity_id.to_string(),
                label: account.name.clone(),
                identity_confidence: "opaque".to_string(),
                authority_site: None,
                authority_subject: None,
                enabled: true,
                notes: None,
            });
        }
        payload.quota_pools.push(PortableQuotaPool {
            id: quota_pool_id_for_identity(identity_id.as_str()).to_string(),
            subject_kind: "credential".to_string(),
            subject_ref: identity_id.to_string(),
            relation_confidence: "unknown".to_string(),
            policy_mode: "authoritative_limit".to_string(),
            member_account_ids: vec![account_id],
        });
    }
}

fn attach_default_destination_snapshot(payload: &mut PortablePayload) {
    use super::portable::{PURPOSE_INFERENCE, PortableCredential, PortableDestination};
    use ocg_domain::credential::{ModelScope, credential_id_for_legacy_account};
    use ocg_domain::destination::{
        LegacyCredentialFacts, LegacyDestinationFacts, LegacyIdentityFacts, LegacyPlatformLink,
        credential_from_legacy, destination_from_legacy,
    };
    use ocg_domain::dynamic::{DynamicAuthKind, DynamicModelMapping, DynamicProviderDefinition};

    payload.destinations.clear();
    payload.credentials.clear();
    payload.routing_cards = None;
    if payload.version < V7_PAYLOAD_VERSION {
        return;
    }
    let dynamics: HashMap<_, _> = payload
        .dynamic_providers
        .iter()
        .map(|provider| (provider.id.clone(), provider))
        .collect();
    let link_by_account: HashMap<_, _> = payload
        .platform_links
        .iter()
        .map(|link| (link.account_id.clone(), link.clone()))
        .collect();
    let mut domain_destinations = Vec::new();
    let mut seen_destinations = HashSet::new();
    for account in &payload.accounts {
        let Some(account_id) = account.id.as_deref() else {
            continue;
        };
        let facts = if account.provider_id == crate::kernel::ids::CUSTOM_PROVIDER_ID {
            let Some(config) = account.custom_config.as_ref() else {
                continue;
            };
            let Ok(protocol) = UpstreamProtocolKind::try_from(config.upstream_protocol.as_str())
            else {
                continue;
            };
            LegacyDestinationFacts::CustomAccount {
                account_id: account_id.to_string(),
                name: account.name.clone(),
                endpoint_url: config.endpoint_url.clone(),
                protocol,
                model_capabilities: account
                    .model_capabilities
                    .iter()
                    .filter_map(|capability| match capability {
                        PortableModelCapability::Canonical(row) => {
                            Some((row.public_model.clone(), row.upstream_model.clone()))
                        }
                        PortableModelCapability::Legacy(_) => None,
                    })
                    .collect(),
            }
        } else if let Some(provider) = dynamics.get(&account.provider_id) {
            let Ok(protocol) = UpstreamProtocolKind::try_from(provider.upstream_protocol.as_str())
            else {
                continue;
            };
            let Ok(auth_kind) = DynamicAuthKind::try_from(provider.auth_kind.as_str()) else {
                continue;
            };
            LegacyDestinationFacts::Dynamic {
                definition: DynamicProviderDefinition {
                    preset_id: provider.preset_id.clone(),
                    id: provider.id.clone(),
                    name: provider.name.clone(),
                    endpoint_url: provider.endpoint_url.clone(),
                    upstream_protocol: protocol,
                    auth_kind,
                    mappings: provider
                        .models
                        .iter()
                        .map(|model| DynamicModelMapping {
                            public_model: model.public_model.clone(),
                            upstream_model: model.upstream_model.clone(),
                            upstream_override: None,
                        })
                        .collect(),
                },
            }
        } else {
            LegacyDestinationFacts::Builtin {
                provider_id: account.provider_id.clone(),
            }
        };
        let Ok(mapped) = destination_from_legacy(&facts) else {
            continue;
        };
        if !seen_destinations.insert(mapped.id.clone()) {
            continue;
        }
        let mut portable = PortableDestination::from(&mapped);
        if let Some(provider) = dynamics.get(&account.provider_id) {
            portable.onboarding_draft = provider.onboarding_draft;
            portable.preset_id = provider.preset_id.clone();
            portable.origin = Some(
                ocg_domain::provider::provider_origin_from_preset(provider.preset_id.as_deref())
                    .as_str()
                    .to_string(),
            );
            portable.offering = Some(
                ocg_domain::provider::preset_offering(provider.preset_id.as_deref().unwrap_or(""))
                    .to_string(),
            );
        }
        domain_destinations.push(mapped);
        payload.destinations.push(portable);
    }
    for (rank, account) in payload.accounts.iter().enumerate() {
        let Some(account_id) = account.id.as_deref() else {
            continue;
        };
        let facts = LegacyCredentialFacts {
            id: account_id.to_string(),
            provider_id: account.provider_id.clone(),
            name: account.name.clone(),
            notes: account.notes.clone(),
            has_key: !account.key.is_empty(),
            enabled: account.enabled,
            order_index: rank as u32,
            setup_step: match account.setup_step.as_str() {
                "ready" => ModelSetupStep::Ready,
                "payment" => ModelSetupStep::Payment,
                other => ModelSetupStep::try_from(other).unwrap_or(ModelSetupStep::Ready),
            },
            account_type: match account.account_type.as_str() {
                "managed" => ModelAccountType::Managed,
                _ => ModelAccountType::Key,
            },
            auth_error: None,
            last_error: None,
            purchase_date: (!account.purchase_date.is_empty())
                .then(|| account.purchase_date.clone()),
            cooldown_generic_until: account.cooldowns.as_ref().and_then(|row| row.generic),
            cooldown_5h_until: account.cooldowns.as_ref().and_then(|row| row.five_hours),
            cooldown_week_until: account.cooldowns.as_ref().and_then(|row| row.week),
            cooldown_month_until: account.cooldowns.as_ref().and_then(|row| row.month),
            cooldown_free_until: account.cooldowns.as_ref().and_then(|row| row.free),
            verified: account.verification_status.as_deref() == Some("verified"),
            platform_link: link_by_account
                .get(account_id)
                .map(|link| LegacyPlatformLink {
                    parent_id: link.platform_account_id.clone(),
                }),
            identity: account.identity_id.as_ref().map(|_| LegacyIdentityFacts {
                quota_pool_id: None,
                model_scope: account
                    .binding_model_scope
                    .clone()
                    .unwrap_or(ModelScope::All),
                allowed_endpoint_ids: account.allowed_endpoint_ids.clone().unwrap_or_default(),
                allowed_origins: account.allowed_origins.clone().unwrap_or_default(),
                binding_enabled: account.binding_enabled.unwrap_or(true),
            }),
        };
        let Ok(mapped) = credential_from_legacy(&facts, &domain_destinations) else {
            continue;
        };
        let mut portable = PortableCredential::from(&mapped);
        portable.id = credential_id_for_legacy_account(account_id).to_string();
        portable.key = account.key.clone();
        portable.username = account.username.clone();
        portable.purpose = Some(PURPOSE_INFERENCE.to_string());
        portable.provider_id = Some(account.provider_id.clone());
        portable.account_type = Some(account.account_type.clone());
        portable.setup_step = Some(account.setup_step.clone());
        portable.verification_status = account.verification_status.clone();
        portable.identity_id = account.identity_id.clone();
        portable.identity_label = Some(account.name.clone());
        portable.identity_confidence = Some("opaque".to_string());
        portable.identity_enabled = Some(true);
        portable.credential_version = account.credential_version;
        portable.auth_state_version = account.auth_state_version;
        portable.binding_id = account.binding_id.clone();
        portable.binding_enabled = account.binding_enabled;
        portable.scope = account
            .binding_model_scope
            .clone()
            .unwrap_or(ModelScope::All);
        if let Some(link) = link_by_account.get(account_id) {
            portable.link_group = Some(link.group.clone());
        }
        payload.credentials.push(portable);
    }
    attach_default_routing_cards(payload);
}

fn attach_default_routing_cards(payload: &mut PortablePayload) {
    use super::portable::{credential_purpose, is_observer_purpose};

    payload.routing_cards = None;
    if payload.version < V9_PAYLOAD_VERSION {
        return;
    }
    let mut inference: Vec<&PortableCredential> = payload
        .credentials
        .iter()
        .filter(|credential| !is_observer_purpose(credential_purpose(credential)))
        .collect();
    inference.sort_by_key(|credential| credential.routing_rank);
    let mut cards: Vec<crate::dashboard_v4::types::RoutingCard> = Vec::new();
    for credential in inference {
        if let Some(last) = cards.last_mut()
            && last.destination_id == credential.destination_id
        {
            last.credential_ids.push(credential.id.clone());
            continue;
        }
        cards.push(crate::dashboard_v4::types::RoutingCard {
            id: format!("card:{}:{}", credential.destination_id, cards.len()),
            credential_ids: vec![credential.id.clone()],
            destination_id: credential.destination_id.clone(),
        });
    }
    payload.routing_cards = Some(cards);
}

fn strip_identity_snapshot(payload: &mut PortablePayload) {
    payload.identities.clear();
    payload.quota_pools.clear();
    for account in &mut payload.accounts {
        account.identity_id = None;
        account.credential_id = None;
        account.credential_version = None;
        account.auth_state_version = None;
        account.binding_id = None;
        account.binding_enabled = None;
        account.binding_model_scope = None;
        account.allowed_endpoint_ids = None;
        account.allowed_origins = None;
        account.cooldowns = None;
    }
}

fn sample_custom_account() -> PortableAccount {
    PortableAccount {
        id: None,
        provider_id: crate::kernel::ids::CUSTOM_PROVIDER_ID.to_string(),

        name: "Mapped Custom".to_string(),
        username: None,
        key: "sk-custom-test-secret".to_string(),
        enabled: true,
        account_type: "key".to_string(),
        setup_step: "ready".to_string(),
        purchase_date: String::new(),
        expires_on: String::new(),
        notes: None,
        verification_status: Some("pending".to_string()),
        connection_verified_at: None,
        custom_config: Some(PortableCustomConfig {
            endpoint_url: "https://api.example.com/v1/chat/completions".to_string(),
            upstream_protocol: "chat_completions".to_string(),
        }),
        model_capabilities: Vec::new(),
        ollama_billing_tier: None,
        identity_id: None,
        credential_id: None,
        credential_version: None,
        auth_state_version: None,
        binding_id: None,
        binding_enabled: None,
        binding_model_scope: None,
        allowed_endpoint_ids: None,
        allowed_origins: None,
        cooldowns: None,
    }
}

fn sample_node(account_id: &str) -> PortableNodeState {
    let config = AppConfig {
        gateway_key: "ocg-transfer-primary-key".to_string(),
        ..AppConfig::default()
    };
    PortableNodeState {
        config,
        access_keys: Vec::new(),
        zen_free: PortableZenFree {
            enabled: false,
            models: Vec::new(),
            refreshed_at: None,
            source_url: String::new(),
        },
        account_order: vec![
            crate::kernel::ids::ZEN_FREE_ACCOUNT_ID.to_string(),
            account_id.to_string(),
        ],
        provider_contracts: Vec::new(),
    }
}

#[test]
fn encrypted_bundle_round_trips_without_plaintext_secret() {
    let payload = sample_payload();
    let bundle = encrypt_payload(&payload, "correct horse battery").unwrap();
    let second = encrypt_payload(&payload, "correct horse battery").unwrap();
    assert_ne!(
        bundle, second,
        "OS randomness must produce a fresh envelope"
    );
    assert!(!bundle.contains("sk-ocg-test-secret"));
    let migration = decrypt_and_validate(&bundle, "correct horse battery").unwrap();
    assert_eq!(migration.accounts.len(), 1);
    assert_eq!(migration.accounts[0].key.as_str(), "sk-ocg-test-secret");
}

#[test]
fn payload_v1_v2_and_v3_are_rejected_without_a_legacy_offering_parser() {
    for version in [LEGACY_PAYLOAD_VERSION, NODE_PAYLOAD_VERSION, 3] {
        let mut account = sample_custom_account();
        let node = if version >= NODE_PAYLOAD_VERSION {
            let account_id = "00000000-0000-4000-8000-000000000042";
            account.id = Some(account_id.to_string());
            Some(sample_node(account_id))
        } else {
            None
        };
        let error = validate_payload(PortablePayload {
            platform_accounts: Vec::new(),
            platform_links: Vec::new(),
            version,
            exported_at: "2026-08-29T00:00:00Z".to_string(),
            accounts: vec![account],
            dynamic_providers: Vec::new(),
            identities: Vec::new(),
            quota_pools: Vec::new(),
            destinations: Vec::new(),
            credentials: Vec::new(),
            routing_cards: None,
            node,
        })
        .unwrap_err();
        assert!(
            matches!(error, TransferError::UnsupportedVersion(found) if found == version),
            "{error:?}"
        );
    }
}

#[test]
fn unsupported_payload_version_is_not_a_password_or_damage_error() {
    let v4 = sample_legacy_payload(4);
    let bundle = encrypt_payload(&v4, "correct horse battery").unwrap();
    let imported = decrypt_and_validate(&bundle, "correct horse battery").unwrap();
    assert!(imported.unified.platform_accounts.is_empty());
    assert!(imported.unified.platform_links.is_empty());
    assert!(imported.unified.identity_snapshot.is_none());
    let mut payload = sample_payload();
    payload.version = 3;
    let bundle = encrypt_payload(&payload, "correct horse battery").unwrap();
    let error = decrypt_and_validate(&bundle, "correct horse battery").unwrap_err();
    assert!(
        matches!(error, TransferError::UnsupportedVersion(3)),
        "{error:?}"
    );
}

#[test]
fn future_payload_version_is_rejected_as_unsupported_not_wrong_password() {
    use crate::crypto::{KeyCipher, StaticKeyCipher};
    use crate::db::Database;
    use crate::state::CoreStateInner;
    use axum::http::StatusCode;
    use std::fs;
    use std::sync::Arc;

    assert_eq!(PAYLOAD_VERSION, 12);
    let future = PAYLOAD_VERSION + 1;
    let mut payload = sample_payload();
    payload.version = future;
    let bundle = encrypt_payload(&payload, "correct horse battery").unwrap();
    let error = decrypt_and_validate(&bundle, "correct horse battery").unwrap_err();
    assert!(
        matches!(error, TransferError::UnsupportedVersion(version) if version == future),
        "{error:?}"
    );
    assert!(!matches!(error, TransferError::InvalidBundle));

    let dir = std::env::temp_dir().join(format!(
        "ocg-transfer-future-payload-{}",
        uuid::Uuid::new_v4()
    ));
    fs::create_dir_all(&dir).unwrap();
    let cipher: Arc<dyn KeyCipher + Send + Sync> =
        Arc::new(StaticKeyCipher::new("v3-transfer-future-payload"));
    let state = Arc::new(
        CoreStateInner::new(Database::open(dir.clone()).unwrap(), dir.clone(), cipher).unwrap(),
    );
    let mapped = map_transfer_error(&state, error);
    assert_eq!(mapped.status, StatusCode::BAD_REQUEST);
    assert_eq!(mapped.body.code, super::super::ERROR_INVALID_REQUEST);
    assert!(
        mapped
            .body
            .message
            .contains(&format!("payload version {future}")),
        "{}",
        mapped.body.message
    );
    assert!(
        mapped
            .body
            .message
            .contains(&format!("payload version {PAYLOAD_VERSION}")),
        "{}",
        mapped.body.message
    );
    let lower = mapped.body.message.to_ascii_lowercase();
    assert!(!lower.contains("password"), "{}", mapped.body.message);
    assert!(!lower.contains("damaged"), "{}", mapped.body.message);
    drop(state);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn v7_destinations_gain_resolution_defaults_but_v8_requires_the_field() {
    use ocg_domain::destination::ModelResolution;

    let mut v7 = sample_payload();
    v7.version = V7_PAYLOAD_VERSION;
    v7.routing_cards = None;
    for destination in &mut v7.destinations {
        destination.model_resolution = None;
    }
    let validated = validate_payload(v7).unwrap();
    assert!(validated.unified.destinations.iter().all(|destination| {
        destination.model_resolution
            == Some(match destination.legacy.kind {
                crate::dashboard_v4::types::LegacyDestinationKindDto::Dynamic => {
                    ModelResolution::PublicAndUpstream
                }
                crate::dashboard_v4::types::LegacyDestinationKindDto::CustomAccount
                | crate::dashboard_v4::types::LegacyDestinationKindDto::PlatformParent => {
                    ModelResolution::PublicOnly
                }
                crate::dashboard_v4::types::LegacyDestinationKindDto::Builtin => {
                    ModelResolution::AdapterDefined
                }
            })
    }));

    let mut v8 = sample_payload();
    v8.version = V8_PAYLOAD_VERSION;
    v8.routing_cards = None;
    v8.destinations[0].model_resolution = None;
    let error = validate_payload(v8).unwrap_err();
    assert!(
        matches!(error, TransferError::Invalid(ref message) if message.contains("modelResolution")),
        "{error:?}"
    );

    let mut v9 = sample_payload();
    v9.destinations[0].model_resolution = None;
    let error = validate_payload(v9).unwrap_err();
    assert!(
        matches!(error, TransferError::Invalid(ref message) if message.contains("modelResolution")),
        "{error:?}"
    );
}

#[test]
fn preview_rejects_same_platform_id_with_a_different_site() {
    use crate::crypto::{KeyCipher, StaticKeyCipher};
    use crate::db::Database;
    use crate::platform::{PlatformKind, PortablePlatformAccount};
    use crate::state::CoreStateInner;
    use axum::http::StatusCode;
    use std::fs;
    use std::sync::Arc;

    let parent_id = "00000000-0000-4000-8000-0000000000aa";
    let dir = std::env::temp_dir().join(format!(
        "ocg-transfer-platform-site-{}",
        uuid::Uuid::new_v4()
    ));
    fs::create_dir_all(&dir).unwrap();
    let cipher: Arc<dyn KeyCipher + Send + Sync> =
        Arc::new(StaticKeyCipher::new("v3-transfer-platform-site"));
    let state = Arc::new(
        CoreStateInner::new(Database::open(dir.clone()).unwrap(), dir.clone(), cipher).unwrap(),
    );
    state
        .db
        .lock()
        .create_platform_account(
            parent_id,
            PlatformKind::NewApi,
            "Destination site",
            "https://dest.example",
            None,
        )
        .unwrap();

    let mut payload = sample_v6_payload();
    payload.platform_accounts = vec![PortablePlatformAccount {
        id: parent_id.to_string(),
        kind: PlatformKind::NewApi,
        name: "Source site".into(),
        base_url: "https://other.example".into(),
    }];
    let validated = validate_payload(payload).unwrap();
    let error = preview_against_current(&state, &validated).unwrap_err();
    assert_eq!(error.status, StatusCode::CONFLICT);
    assert!(
        error
            .body
            .message
            .contains("imported platform identity conflicts with immutable origin"),
        "{}",
        error.body.message
    );
    assert_eq!(
        state
            .db
            .lock()
            .platform_account(parent_id)
            .unwrap()
            .unwrap()
            .base_url,
        "https://dest.example"
    );
    drop(state);
    fs::remove_dir_all(dir).unwrap();
}

fn sample_dynamic_provider(id: &str, name: &str) -> PortableProviderDefinition {
    PortableProviderDefinition {
        preset_id: None,
        id: id.to_string(),
        name: name.to_string(),
        endpoint_url: "http://127.0.0.1:9/v1".to_string(),
        upstream_protocol: "chat_completions".to_string(),
        auth_kind: "bearer".to_string(),
        models: vec![PortableProviderDefinitionModel {
            public_model: "lab-opus".to_string(),
            upstream_model: "vendor/opus".to_string(),
            upstream_override: None,
        }],
        onboarding_draft: Some(false),
    }
}

#[test]
fn portable_preset_provenance_is_optional_and_survives_validation() {
    let mut provider = sample_dynamic_provider("dc7f6bbf-18a1-458b-845b-54c219c19dba", "Renamed");
    provider.preset_id = Some("azure-openai".into());
    let encoded = serde_json::to_value(&provider).unwrap();
    assert_eq!(encoded["presetId"], "azure-openai");
    let decoded: PortableProviderDefinition = serde_json::from_value(encoded.clone()).unwrap();
    let (validated, _) = validate_portable_dynamic_providers(&[decoded], PAYLOAD_VERSION).unwrap();
    assert_eq!(validated[0].preset_id.as_deref(), Some("azure-openai"));
    let mut legacy = encoded;
    legacy.as_object_mut().unwrap().remove("presetId");
    let decoded: PortableProviderDefinition = serde_json::from_value(legacy).unwrap();
    assert_eq!(
        validate_portable_dynamic_providers(&[decoded], PAYLOAD_VERSION)
            .unwrap()
            .0[0]
            .preset_id,
        None
    );
}

#[test]
fn portable_model_override_roundtrips_and_missing_override_inherits() {
    let mut provider = sample_dynamic_provider("dc7f6bbf-18a1-458b-845b-54c219c19dba", "Lab");
    let legacy = serde_json::to_value(&provider).unwrap();
    assert!(legacy["models"][0].get("upstreamOverride").is_none());
    provider.models[0].upstream_override = Some(PortableProviderDefinitionModelOverride {
        protocol: "messages".into(),
        endpoint_url: "https://example.test/anthropic/v1/messages".into(),
    });
    let encoded = serde_json::to_value(&provider).unwrap();
    assert_eq!(
        encoded["models"][0]["upstreamOverride"]["protocol"],
        "messages"
    );
    let decoded = serde_json::from_value(encoded).unwrap();
    let (validated, _) = validate_portable_dynamic_providers(&[decoded], PAYLOAD_VERSION).unwrap();
    let route = validated[0].mappings[0].upstream_override.as_ref().unwrap();
    assert_eq!(route.protocol, UpstreamProtocolKind::Messages);
    assert_eq!(
        route.endpoint_url,
        "https://example.test/anthropic/v1/messages"
    );
    let old = validate_portable_dynamic_providers(
        &[serde_json::from_value(legacy).unwrap()],
        PAYLOAD_VERSION,
    )
    .unwrap()
    .0;
    assert!(old[0].mappings[0].upstream_override.is_none());
    provider.models[0]
        .upstream_override
        .as_mut()
        .unwrap()
        .protocol = "unknown".into();
    assert!(validate_portable_dynamic_providers(&[provider], PAYLOAD_VERSION).is_err());
}

#[test]
fn dynamic_provider_definitions_are_validated_and_dangling_ids_fail() {
    let provider_id = "00000000-0000-4000-8000-0000000000aa";
    let account_id = "00000000-0000-4000-8000-0000000000ab";
    let mut account = sample_account("Lab");
    account.id = Some(account_id.to_string());
    account.provider_id = provider_id.to_string();
    let mut payload = PortablePayload {
        platform_accounts: Vec::new(),
        platform_links: Vec::new(),
        version: PAYLOAD_VERSION,
        exported_at: "2026-08-29T00:00:00Z".to_string(),
        accounts: vec![account],
        dynamic_providers: vec![sample_dynamic_provider(provider_id, "Lab")],
        identities: Vec::new(),
        quota_pools: Vec::new(),
        destinations: Vec::new(),
        credentials: Vec::new(),
        routing_cards: None,
        node: Some(sample_node(account_id)),
    };
    attach_default_identity_snapshot(&mut payload);
    attach_default_destination_snapshot(&mut payload);
    let mut dangling = PortablePayload {
        platform_accounts: Vec::new(),
        platform_links: Vec::new(),
        version: PAYLOAD_VERSION,
        exported_at: "2026-08-29T00:00:00Z".to_string(),
        accounts: Vec::new(),
        dynamic_providers: Vec::new(),
        identities: Vec::new(),
        quota_pools: Vec::new(),
        destinations: payload.destinations.clone(),
        credentials: payload.credentials.clone(),
        routing_cards: payload.routing_cards.clone(),
        node: Some(sample_node(account_id)),
    };
    dangling.credentials[0].provider_id = Some("not-a-registered-plan".to_string());
    let validated = validate_payload(payload).unwrap();
    assert_eq!(validated.unified.dynamic_providers.len(), 1);
    assert_eq!(validated.unified.dynamic_providers[0].name, "Lab");
    assert_eq!(
        validated.accounts[0].credential_kind,
        crate::provider::CredentialKind::ApiKey
    );
    let error = validate_payload(dangling).unwrap_err();
    assert!(
        matches!(error, TransferError::Invalid(ref message) if message.contains("unknown provider")),
        "{error:?}"
    );
}

#[test]
fn v3_exports_canonical_model_mapping_inside_the_v1_envelope() {
    let account_id = "00000000-0000-4000-8000-000000000043";
    let mut account = sample_custom_account();
    account.id = Some(account_id.to_string());
    account.model_capabilities = vec![PortableModelCapability::Canonical(
        PortableModelCapabilityCanonical {
            public_model: "deepseek-v4-flash".to_string(),
            upstream_model: "deepseek-v4-flash:0731".to_string(),
            protocol: "chat_completions".to_string(),
        },
    )];
    let mut payload = PortablePayload {
        platform_accounts: Vec::new(),
        platform_links: Vec::new(),
        version: PAYLOAD_VERSION,
        exported_at: "2026-08-29T00:00:00Z".to_string(),
        accounts: vec![account],
        dynamic_providers: Vec::new(),
        identities: Vec::new(),
        quota_pools: Vec::new(),
        destinations: Vec::new(),
        credentials: Vec::new(),
        routing_cards: None,
        node: Some(sample_node(account_id)),
    };
    attach_default_identity_snapshot(&mut payload);
    attach_default_destination_snapshot(&mut payload);
    payload.accounts.clear();
    payload.identities.clear();
    let json = serde_json::to_value(&payload).unwrap();
    let capability = &json["destinations"][0]["catalog"][0];
    assert_eq!(capability["publicModel"], "deepseek-v4-flash");
    assert_eq!(capability["upstreamModel"], "deepseek-v4-flash:0731");
    assert!(capability.get("modelId").is_none());

    let bundle = encrypt_payload(&payload, "correct horse battery").unwrap();
    let envelope: EncryptedEnvelope = serde_json::from_str(&bundle).unwrap();
    assert_eq!(envelope.version, ENVELOPE_VERSION);
    let validated = decrypt_and_validate(&bundle, "correct horse battery").unwrap();
    let capability = &validated.accounts[0].capabilities[0];
    assert_eq!(capability.public_model, "deepseek-v4-flash");
    assert_eq!(capability.upstream_model, "deepseek-v4-flash:0731");
}

#[test]
fn wrong_password_and_tampering_share_invalid_bundle_result() {
    let bundle = encrypt_payload(&sample_payload(), "correct horse battery").unwrap();
    assert!(matches!(
        decrypt_and_validate(&bundle, "wrong password value"),
        Err(TransferError::InvalidBundle)
    ));
    let mut envelope: EncryptedEnvelope = serde_json::from_str(&bundle).unwrap();
    let mut ciphertext = STANDARD.decode(&envelope.ciphertext).unwrap();
    ciphertext[0] ^= 1;
    envelope.ciphertext = STANDARD.encode(ciphertext);
    assert!(matches!(
        decrypt_and_validate(
            &serde_json::to_string(&envelope).unwrap(),
            "correct horse battery"
        ),
        Err(TransferError::InvalidBundle)
    ));

    let mut wrong_version: EncryptedEnvelope = serde_json::from_str(&bundle).unwrap();
    wrong_version.version += 1;
    assert!(matches!(
        decrypt_and_validate(
            &serde_json::to_string(&wrong_version).unwrap(),
            "correct horse battery"
        ),
        Err(TransferError::InvalidBundle)
    ));

    let mut wrong_nonce: EncryptedEnvelope = serde_json::from_str(&bundle).unwrap();
    wrong_nonce.nonce = STANDARD.encode([0_u8; NONCE_LEN - 1]);
    assert!(matches!(
        decrypt_and_validate(
            &serde_json::to_string(&wrong_nonce).unwrap(),
            "correct horse battery"
        ),
        Err(TransferError::InvalidBundle)
    ));
}

#[test]
fn duplicate_rows_inside_bundle_fail_closed() {
    let mut payload = sample_v6_payload();
    payload.accounts.push(PortableAccount {
        id: None,
        provider_id: "opencode".to_string(),

        name: "Primary".to_string(),
        username: None,
        key: "sk-ocg-another-secret".to_string(),
        enabled: false,
        account_type: "key".to_string(),
        setup_step: "ready".to_string(),
        purchase_date: String::new(),
        expires_on: String::new(),
        notes: None,
        verification_status: None,
        connection_verified_at: None,
        custom_config: None,
        model_capabilities: Vec::new(),
        ollama_billing_tier: None,
        identity_id: None,
        credential_id: None,
        credential_version: None,
        auth_state_version: None,
        binding_id: None,
        binding_enabled: None,
        binding_model_scope: None,
        allowed_endpoint_ids: None,
        allowed_origins: None,
        cooldowns: None,
    });
    let bundle = encrypt_payload(&payload, "correct horse battery").unwrap();
    assert!(matches!(
        decrypt_and_validate(&bundle, "correct horse battery"),
        Err(TransferError::Invalid(_))
    ));
}

#[test]
fn managed_lifecycle_is_normalized_without_browser_identity() {
    assert!(!migration_exports_key(
        ModelAccountType::Managed,
        ModelSetupStep::Payment
    ));
    assert!(migration_exports_key(
        ModelAccountType::Managed,
        ModelSetupStep::Ready
    ));
    let mut draft = sample_v6_payload();
    draft.accounts[0].account_type = "managed".to_string();
    draft.accounts[0].setup_step = "payment".to_string();
    draft.accounts[0].enabled = true;
    let draft = validate_payload(draft).unwrap();
    assert_eq!(draft.accounts[0].setup_step, ModelSetupStep::GoogleAccount);
    assert!(!draft.accounts[0].enabled);
    assert!(draft.accounts[0].key.is_empty());

    let mut ready = sample_v6_payload();
    ready.accounts[0].account_type = "managed".to_string();
    let ready = validate_payload(ready).unwrap();
    assert_eq!(ready.accounts[0].setup_step, ModelSetupStep::Ready);
    assert!(ready.accounts[0].enabled);
    assert_eq!(ready.accounts[0].key.as_str(), "sk-ocg-test-secret");
}

#[test]
fn account_count_and_decoded_ciphertext_limits_fail_closed() {
    let mut payload = sample_v6_payload();
    let node = payload
        .node
        .as_mut()
        .expect("v4 payload includes node state");
    for index in 1..MAX_ACCOUNTS {
        let id = format!("00000000-0000-4000-8000-{:012x}", 0x41 + index);
        let mut extra = sample_account(format!("Account {index}"));
        extra.id = Some(id.clone());
        payload.accounts.push(extra);
        node.account_order.push(id);
    }
    attach_default_identity_snapshot(&mut payload);
    attach_default_destination_snapshot(&mut payload);
    assert_eq!(
        validate_payload(payload).unwrap().accounts.len(),
        MAX_ACCOUNTS
    );

    let mut oversized = sample_v6_payload();
    for index in 1..=MAX_ACCOUNTS {
        oversized
            .accounts
            .push(sample_account(format!("Account {index}")));
    }
    assert!(matches!(
        validate_payload(oversized),
        Err(TransferError::InvalidBundle)
    ));

    let envelope = EncryptedEnvelope {
        format: ENVELOPE_FORMAT.to_string(),
        version: ENVELOPE_VERSION,
        salt: STANDARD.encode([0_u8; SALT_LEN]),
        nonce: STANDARD.encode([0_u8; NONCE_LEN]),
        ciphertext: STANDARD.encode(vec![0_u8; MAX_PLAINTEXT_BYTES + 33]),
    };
    assert!(matches!(
        decrypt_and_validate(
            &serde_json::to_string(&envelope).unwrap(),
            "correct horse battery"
        ),
        Err(TransferError::InvalidBundle)
    ));
}

#[test]
fn v4_and_v5_payloads_without_identity_snapshot_remain_importable() {
    for version in [4, V5_PAYLOAD_VERSION] {
        let payload = sample_legacy_payload(version);
        let validated = validate_payload(payload).unwrap();
        assert!(validated.unified.identity_snapshot.is_none());
        assert_eq!(validated.unified.platform_links_authoritative, version >= 5);
    }
}

#[test]
fn v6_without_identity_snapshot_is_rejected() {
    let mut payload = sample_v6_payload();
    strip_identity_snapshot(&mut payload);
    payload.accounts[0].cooldowns = Some(PortableCooldowns::default());
    payload.destinations.clear();
    payload.credentials.clear();
    let error = validate_payload(payload).unwrap_err();
    assert!(
        matches!(error, TransferError::Invalid(ref message) if message.contains("identity")),
        "{error:?}"
    );
}

#[test]
fn v5_package_with_identity_semantics_is_not_silently_downgraded() {
    let mut payload = sample_v6_payload();
    payload.version = V5_PAYLOAD_VERSION;
    payload.destinations.clear();
    payload.credentials.clear();
    let error = validate_payload(payload).unwrap_err();
    assert!(
        matches!(error, TransferError::Invalid(ref message) if message.contains("identity semantics")),
        "{error:?}"
    );
}

#[test]
fn v6_dangling_identity_and_verified_relation_are_rejected() {
    let mut dangling = sample_v6_payload();
    dangling.identities[0].id = "00000000-0000-4000-8000-000000000099".to_string();
    let error = validate_payload(dangling).unwrap_err();
    assert!(
        matches!(error, TransferError::Invalid(ref message) if message.contains("identity")),
        "{error:?}"
    );

    let mut verified = sample_v6_payload();
    verified.identities[0].identity_confidence = "verified".to_string();
    let error = validate_payload(verified).unwrap_err();
    assert!(
        matches!(error, TransferError::Invalid(ref message) if message.contains("verified")),
        "{error:?}"
    );

    let mut missing_member = sample_v6_payload();
    missing_member.quota_pools[0].member_account_ids =
        vec!["00000000-0000-4000-8000-000000000098".to_string()];
    let error = validate_payload(missing_member).unwrap_err();
    assert!(
        matches!(error, TransferError::Invalid(ref message) if message.contains("quota pool")),
        "{error:?}"
    );
}

#[test]
fn v6_rejects_versions_that_sqlite_cannot_preserve() {
    for value in [0, i64::MAX as u64 + 1, u64::MAX] {
        for auth_version in [false, true] {
            let mut payload = sample_v6_payload();
            if auth_version {
                payload.accounts[0].auth_state_version = Some(value);
            } else {
                payload.accounts[0].credential_version = Some(value);
            }
            assert!(
                matches!(validate_payload(payload), Err(TransferError::Invalid(message))
                    if message.contains("out-of-range credential version"))
            );
        }
    }
}

#[test]
fn v6_requires_onboarding_draft_and_rejects_blank_targets_unless_draft() {
    let provider_id = "dc7f6bbf-18a1-458b-845b-54c219c19dba";
    let mut missing_flag = sample_dynamic_provider(provider_id, "Lab");
    missing_flag.onboarding_draft = None;
    let error = validate_portable_dynamic_providers(&[missing_flag], PAYLOAD_VERSION).unwrap_err();
    assert!(
        matches!(error, TransferError::Invalid(ref message) if message.contains("onboardingDraft")),
        "{error:?}"
    );

    let mut blank = sample_dynamic_provider(provider_id, "Lab");
    blank.models.clear();
    blank.onboarding_draft = Some(false);
    assert!(validate_portable_dynamic_providers(&[blank], PAYLOAD_VERSION).is_err());

    let mut draft = sample_dynamic_provider(provider_id, "Lab");
    draft.models.clear();
    draft.onboarding_draft = Some(true);
    let (validated, draft_ids) =
        validate_portable_dynamic_providers(&[draft], PAYLOAD_VERSION).unwrap();
    assert!(validated[0].mappings.is_empty());
    assert!(draft_ids.contains(provider_id));
}

#[test]
fn v5_package_with_onboarding_draft_is_rejected() {
    let mut provider = sample_dynamic_provider("dc7f6bbf-18a1-458b-845b-54c219c19dba", "Lab");
    provider.onboarding_draft = Some(false);
    let error = validate_portable_dynamic_providers(&[provider], V5_PAYLOAD_VERSION).unwrap_err();
    assert!(
        matches!(error, TransferError::Invalid(ref message) if message.contains("onboardingDraft")),
        "{error:?}"
    );
}

#[test]
fn v1_encryption_vector_is_stable() {
    use sha2::{Digest, Sha256};

    let bundle = encrypt_payload_with_material(
        &sample_legacy_payload(V5_PAYLOAD_VERSION),
        "correct horse battery",
        [7_u8; SALT_LEN],
        [9_u8; NONCE_LEN],
    )
    .unwrap();
    assert_eq!(
        format!("{:x}", Sha256::digest(bundle.as_bytes())),
        // Lock the deterministic V5 fixture after retired config fields are omitted.
        "158b5e3145c76bd6accc7644f3715a79b9935c5b07c1dcfcf60720e99aa007b0"
    );
}

#[test]
fn v6_package_with_destination_semantics_is_rejected() {
    let mut payload = sample_payload();
    payload.version = V6_PAYLOAD_VERSION;
    let error = validate_payload(payload).unwrap_err();
    assert!(
        matches!(
            error,
            TransferError::Invalid(ref message) if message.contains("destination semantics")
        ),
        "{error:?}"
    );
}

#[test]
fn v7_requires_a_destination_snapshot_for_exported_accounts() {
    let mut payload = sample_account_graph();
    payload.version = PAYLOAD_VERSION;
    payload.destinations.clear();
    payload.credentials.clear();
    let error = validate_payload(payload).unwrap_err();
    assert!(
        matches!(
            error,
            TransferError::Invalid(ref message)
                if message.contains("destination/credential snapshot")
        ),
        "{error:?}"
    );
}

#[test]
fn v7_leftover_account_fields_that_disagree_with_dest_cred_are_rejected() {
    let mut payload = sample_payload();
    let account_id = payload.credentials[0].legacy_account_id.clone();
    let mut leftover = sample_account("Primary");
    leftover.id = Some(account_id);
    leftover.enabled = !payload.credentials[0].enabled;
    leftover.binding_model_scope = Some(ModelScope::Only {
        models: vec!["glm-5.1".to_string()],
    });
    payload.accounts = vec![leftover];
    let error = validate_payload(payload).unwrap_err();
    assert!(
        matches!(
            error,
            TransferError::Invalid(ref message) if message.contains("conflicts with destination/credential")
        ),
        "{error:?}"
    );
}

#[test]
fn v4_v5_and_v6_samples_still_import_through_the_old_decoder() {
    for version in [4, V5_PAYLOAD_VERSION] {
        let payload = sample_legacy_payload(version);
        let validated = validate_payload(payload).unwrap();
        assert_eq!(validated.accounts.len(), 1);
        assert_eq!(validated.accounts[0].key.as_str(), "sk-ocg-test-secret");
        assert!(!validated.unified.destinations.is_empty());
        assert_eq!(validated.unified.credentials.len(), 1);
        assert_eq!(validated.unified.credentials[0].key, "sk-ocg-test-secret");
    }
    let validated = validate_payload(sample_v6_payload()).unwrap();
    assert_eq!(validated.accounts.len(), 1);
    assert!(validated.unified.identity_snapshot.is_some());
    assert_eq!(validated.unified.credentials[0].key, "sk-ocg-test-secret");
}

#[test]
fn v7_export_json_omits_old_account_graph_fields() {
    use crate::crypto::{KeyCipher, StaticKeyCipher};
    use crate::db::Database;
    use crate::state::CoreStateInner;
    use std::fs;
    use std::sync::Arc;

    let dir = std::env::temp_dir().join(format!(
        "ocg-transfer-v7-export-json-{}",
        uuid::Uuid::new_v4()
    ));
    fs::create_dir_all(&dir).unwrap();
    let cipher: Arc<dyn KeyCipher + Send + Sync> =
        Arc::new(StaticKeyCipher::new("v3-transfer-v7-export-json"));
    let state = Arc::new(
        CoreStateInner::new(Database::open(dir.clone()).unwrap(), dir.clone(), cipher).unwrap(),
    );
    let (payload, _, _) = export_payload(&state).unwrap();
    let json = serde_json::to_value(&payload).unwrap();
    let object = json.as_object().expect("export payload is an object");
    for key in [
        "accounts",
        "platformAccounts",
        "platformLinks",
        "dynamicProviders",
        "identities",
    ] {
        assert!(
            !object.contains_key(key),
            "new export still serialized {key}"
        );
    }
    assert!(object.contains_key("node"));
    assert!(object.contains_key("routingCards"));
    drop(state);
    fs::remove_dir_all(dir).unwrap();
}

fn transfer_state(label: &str) -> (std::path::PathBuf, crate::state::CoreState) {
    use crate::crypto::{KeyCipher, StaticKeyCipher};
    use crate::db::Database;
    use crate::state::CoreStateInner;
    use std::sync::Arc;

    let dir = std::env::temp_dir().join(format!("ocg-transfer-{label}-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let cipher: Arc<dyn KeyCipher + Send + Sync> =
        Arc::new(StaticKeyCipher::new("v3-transfer-observer"));
    let state = Arc::new(
        CoreStateInner::new(Database::open(dir.clone()).unwrap(), dir.clone(), cipher).unwrap(),
    );
    (dir, state)
}

fn empty_node_import() -> crate::db::NodeImportRecord {
    let config = crate::models::AppConfig {
        gateway_key: "ocg-transfer-primary-key".into(),
        ..crate::models::AppConfig::default()
    };
    crate::db::NodeImportRecord {
        platform_links_authoritative: true,
        platform_accounts: Vec::new(),
        platform_links: Vec::new(),
        platform_catalogs: HashMap::new(),
        destination_controls: Vec::new(),
        accounts: Vec::new(),
        account_order: vec![crate::provider::ZEN_FREE_ACCOUNT_ID.to_string()],
        config_json: serde_json::to_string(&config).unwrap(),
        sub_keys: Vec::new(),
        zen_free_enabled: false,
        zen_catalog: crate::kernel::zen::ZenFreeModelCatalog::default(),
        provider_contracts: crate::provider_contracts::PersistedContracts::default(),
        dynamic_providers: Vec::new(),
        custom_destinations: Vec::new(),
        custom_credential_destinations: HashMap::new(),
        identity_snapshot: None,
        draft_provider_ids: HashSet::new(),
        platform_observer_ciphers: HashMap::new(),
        platform_snapshots: HashMap::new(),
        platform_versions: HashMap::new(),
        cpa_base_url: None,
        cpa_management_key_cipher: None,
    }
}

fn cpa_account() -> crate::models::Account {
    let now = chrono::Utc::now();
    crate::models::Account {
        id: crate::provider::CPA_ACCOUNT_ID.to_string(),
        provider_id: crate::provider::CPA_PROVIDER_ID.to_string(),
        credential_kind: crate::provider::CredentialKind::ApiKey,
        quota_scope: crate::provider::QuotaScope::Key,
        name: crate::provider::CPA_ACCOUNT_NAME.to_string(),
        username: None,
        password_cipher: None,
        key_cipher: String::new(),
        enabled: false,
        account_type: crate::models::AccountType::Key,
        setup_step: crate::models::AccountSetupStep::Ready,
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
    }
}

#[test]
fn v7_export_restores_platform_and_cpa_observer_management_keys_on_a_new_node() {
    use super::portable::{PURPOSE_CPA_OBSERVER, PURPOSE_PLATFORM_OBSERVER};

    let (source_dir, source) = transfer_state("v7-observer-export");
    let parent_id = "00000000-0000-4000-8000-0000000000aa";
    let pat = "123:platform-pat-secret";
    let pat_cipher = source.encrypt_key(pat).unwrap();
    source
        .db
        .lock()
        .create_platform_account(
            parent_id,
            crate::platform::PlatformKind::NewApi,
            "Site",
            "https://newapi.example",
            Some(&pat_cipher),
        )
        .unwrap();
    let cpa_mgmt = "cpa-management-secret";
    let cpa_cipher = source.encrypt_key(cpa_mgmt).unwrap();
    source
        .db
        .lock()
        .upsert_cpa_integration(&cpa_account(), "http://127.0.0.1:8317", &cpa_cipher)
        .unwrap();

    let (payload, _, _) = export_payload(&source).unwrap();
    let observers: Vec<_> = payload
        .credentials
        .iter()
        .filter(|credential| is_observer_purpose(credential_purpose(credential)))
        .collect();
    let platform_observer = observers
        .iter()
        .find(|credential| credential_purpose(credential) == PURPOSE_PLATFORM_OBSERVER)
        .expect("platform observer must be exported");
    assert_eq!(platform_observer.management_key.as_deref(), Some(pat));
    assert!(platform_observer.key.is_empty());
    let encoded = serde_json::to_value(*platform_observer).unwrap();
    assert!(encoded.get("key").is_none() || encoded["key"] == "");
    assert_eq!(encoded["managementKey"], pat);
    let cpa_observer = observers
        .iter()
        .find(|credential| credential_purpose(credential) == PURPOSE_CPA_OBSERVER)
        .expect("CPA observer must be exported");
    assert_eq!(cpa_observer.management_key.as_deref(), Some(cpa_mgmt));
    assert!(cpa_observer.key.is_empty());
    let encoded = serde_json::to_value(*cpa_observer).unwrap();
    assert!(encoded.get("key").is_none() || encoded["key"] == "");
    assert_eq!(encoded["managementKey"], cpa_mgmt);

    let validated = validate_payload(payload).unwrap();
    let plains = super::new_model::observer_plaintext_by_parent(&validated.unified);
    assert_eq!(plains.get(parent_id).map(String::as_str), Some(pat));
    assert_eq!(
        validated.unified.cpa_management_key.as_deref(),
        Some(cpa_mgmt)
    );

    let (dest_dir, dest) = transfer_state("v7-observer-restore");
    let mut record = empty_node_import();
    record.platform_accounts = validated.unified.platform_accounts.clone();
    record.platform_snapshots = validated.unified.platform_snapshots.clone();
    record.platform_versions = validated.unified.platform_versions.clone();
    record.platform_observer_ciphers = plains
        .into_iter()
        .map(|(id, plain)| (id, dest.encrypt_key(&plain).unwrap()))
        .collect();
    record.cpa_base_url = validated.unified.cpa_base_url.clone();
    record.cpa_management_key_cipher = validated
        .unified
        .cpa_management_key
        .as_deref()
        .map(|plain| dest.encrypt_key(plain).unwrap());
    dest.db
        .lock()
        .import_node_state(&record, |_| -> anyhow::Result<()> { Ok(()) })
        .unwrap();
    assert!(
        dest.db
            .lock()
            .platform_account(parent_id)
            .unwrap()
            .unwrap()
            .has_user_credential
    );
    let restored_plat = dest
        .db
        .lock()
        .platform_credential_cipher(parent_id)
        .unwrap()
        .unwrap();
    assert_eq!(dest.decrypt_key(&restored_plat).unwrap(), pat);
    let restored_cpa = dest.db.lock().cpa_integration().unwrap().unwrap();
    assert_eq!(
        dest.decrypt_key(&restored_cpa.management_key_cipher)
            .unwrap(),
        cpa_mgmt
    );

    drop(source);
    drop(dest);
    std::fs::remove_dir_all(source_dir).unwrap();
    std::fs::remove_dir_all(dest_dir).unwrap();
}

#[test]
fn v7_merge_without_cpa_observer_key_keeps_existing_management_key() {
    use super::portable::PURPOSE_CPA_OBSERVER;

    let (source_dir, source) = transfer_state("v7-cpa-merge-source");
    let source_cipher = source.encrypt_key("source-cpa-secret").unwrap();
    source
        .db
        .lock()
        .upsert_cpa_integration(&cpa_account(), "http://127.0.0.1:8317", &source_cipher)
        .unwrap();
    let (mut payload, _, _) = export_payload(&source).unwrap();
    payload
        .credentials
        .retain(|credential| credential_purpose(credential) != PURPOSE_CPA_OBSERVER);
    let validated = validate_payload(payload).unwrap();
    assert!(
        validated
            .unified
            .cpa_management_key
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .is_none()
    );
    assert!(validated.unified.cpa_base_url.is_some());

    let (dest_dir, dest) = transfer_state("v7-cpa-merge-keep");
    let keep = "keep-existing-cpa-secret";
    let keep_cipher = dest.encrypt_key(keep).unwrap();
    dest.db
        .lock()
        .upsert_cpa_integration(&cpa_account(), "http://127.0.0.1:8317", &keep_cipher)
        .unwrap();
    let mut record = empty_node_import();
    record.account_order = dest
        .db
        .lock()
        .list_accounts()
        .unwrap()
        .into_iter()
        .map(|account| account.id)
        .collect();
    record.cpa_base_url = validated.unified.cpa_base_url.clone();
    record.cpa_management_key_cipher = validated
        .unified
        .cpa_management_key
        .as_deref()
        .filter(|value| !value.is_empty())
        .map(|plain| dest.encrypt_key(plain).unwrap());
    dest.db
        .lock()
        .import_node_state(&record, |_| -> anyhow::Result<()> { Ok(()) })
        .unwrap();
    let after = dest.db.lock().cpa_integration().unwrap().unwrap();
    assert_eq!(after.base_url, "http://127.0.0.1:8317");
    assert_eq!(
        dest.decrypt_key(&after.management_key_cipher).unwrap(),
        keep
    );

    drop(source);
    drop(dest);
    std::fs::remove_dir_all(source_dir).unwrap();
    std::fs::remove_dir_all(dest_dir).unwrap();
}

#[test]
fn v7_empty_only_model_scope_roundtrips_and_rejects_blank_model_ids() {
    let mut payload = sample_payload();
    payload.credentials[0].scope = ModelScope::Only { models: Vec::new() };
    let validated = validate_payload(payload).unwrap();
    assert_eq!(
        validated.unified.credentials[0].scope,
        ModelScope::Only { models: Vec::new() }
    );
    assert_eq!(
        validated
            .unified
            .identity_snapshot
            .as_ref()
            .unwrap()
            .accounts[0]
            .binding_model_scope,
        ModelScope::Only { models: Vec::new() }
    );
    assert!(!matches!(
        validated.unified.credentials[0].scope,
        ModelScope::All
    ));

    let mut payload = sample_payload();
    payload.credentials[0].scope = ModelScope::Only { models: Vec::new() };
    let bundle = encrypt_payload(&payload, "correct horse battery").unwrap();
    let migration = decrypt_and_validate(&bundle, "correct horse battery").unwrap();
    assert_eq!(
        migration.unified.credentials[0].scope,
        ModelScope::Only { models: Vec::new() }
    );
    assert_eq!(
        migration.accounts[0]
            .identity
            .as_ref()
            .unwrap()
            .binding_model_scope,
        ModelScope::Only { models: Vec::new() }
    );

    for models in [
        vec![String::new()],
        vec![" ".to_string()],
        vec!["glm-5".to_string(), String::new()],
    ] {
        let mut payload = sample_payload();
        payload.credentials[0].scope = ModelScope::Only { models };
        let error = validate_payload(payload).unwrap_err();
        assert!(
            matches!(
                error,
                TransferError::Invalid(ref message) if message.contains("model scope")
            ),
            "{error:?}"
        );
    }
}

#[test]
fn v8_http_controls_and_no_key_credentials_survive_encrypted_validation() {
    for is_custom in [false, true] {
        let account_id = "00000000-0000-4000-8000-0000000000cc";
        let provider_id = "00000000-0000-4000-8000-0000000000dd";
        let mut account = if is_custom {
            sample_custom_account()
        } else {
            sample_account("HTTP")
        };
        account.id = Some(account_id.into());
        if is_custom {
            account.model_capabilities = vec![PortableModelCapability::Canonical(
                PortableModelCapabilityCanonical {
                    public_model: "public-model".into(),
                    upstream_model: "upstream-model".into(),
                    protocol: "chat_completions".into(),
                },
            )];
        }
        if !is_custom {
            account.provider_id = provider_id.into();
        }
        let mut payload = PortablePayload {
            version: PAYLOAD_VERSION,
            exported_at: "2026-08-29T00:00:00Z".into(),
            accounts: vec![account],
            dynamic_providers: if is_custom {
                vec![]
            } else {
                vec![sample_dynamic_provider(provider_id, "HTTP")]
            },
            identities: vec![],
            quota_pools: vec![],
            destinations: vec![],
            credentials: vec![],
            routing_cards: None,
            platform_accounts: vec![],
            platform_links: vec![],
            node: Some(sample_node(account_id)),
        };
        attach_default_identity_snapshot(&mut payload);
        attach_default_destination_snapshot(&mut payload);
        payload.accounts.clear();
        payload.identities.clear();
        payload.dynamic_providers.clear();
        payload.destinations[0].auth_scheme = crate::dashboard_v4::types::AuthSchemeDto::None;
        payload.destinations[0].enabled = false;
        payload.destinations[0].catalog[0].enabled = false;
        payload.destinations[0].catalog[0].protocols.clear();
        payload.destinations[0].catalog[0].preferred = None;
        payload.credentials[0].key.clear();
        payload.credentials[0].has_secret = false;
        payload.credentials[0].scope = ModelScope::Only { models: vec![] };
        let bundle = encrypt_payload(&payload, "correct horse battery").unwrap();
        let validated = decrypt_and_validate(&bundle, "correct horse battery").unwrap();
        assert!(validated.accounts[0].key.is_empty());
        let controls = &validated.unified.destination_controls[0];
        assert!(!controls.enabled);
        assert!(!controls.catalog[0].enabled);
        assert!(controls.catalog[0].protocols.is_empty());
        assert!(controls.catalog[0].preferred.is_none());
        assert_eq!(
            validated.unified.credentials[0].scope,
            ModelScope::Only { models: vec![] }
        );
        assert_eq!(
            validated.accounts[0].credential_kind,
            crate::provider::CredentialKind::None
        );
        let (dir, state) = transfer_state(if is_custom {
            "custom-noauth-full"
        } else {
            "dynamic-noauth-full"
        });
        let imported = &validated.accounts[0];
        let mut account = cpa_account();
        account.id = account_id.into();
        account.provider_id = imported.provider_id.clone();
        account.name = imported.name.clone();
        account.credential_kind = imported.credential_kind;
        account.quota_scope = imported.quota_scope;
        account.enabled = imported.enabled;
        let mut record = empty_node_import();
        record.account_order.push(account_id.into());
        record.accounts.push(crate::db::AccountImportRecord {
            account,
            custom_config: imported.custom_config.clone(),
            capabilities: imported.capabilities.clone(),
            verification_status: imported.verification_status,
            connection_verified_at: imported.connection_verified_at,
            ollama_billing_tier: None,
            goat_plan: imported.goat_plan.clone(),
        });
        record.dynamic_providers = validated.unified.dynamic_providers.clone();
        record.custom_destinations = validated.unified.custom_destinations.clone();
        record.custom_credential_destinations =
            validated.unified.custom_credential_destinations.clone();
        record.destination_controls = validated.unified.destination_controls.clone();
        record.identity_snapshot = validated.unified.identity_snapshot.clone();
        for _ in 0..2 {
            state
                .db
                .lock()
                .import_node_state(&record, |db| state.prepare_imported_node_runtime(db))
                .unwrap();
            let db = state.db.lock();
            let account = db.get_account(account_id).unwrap().unwrap();
            assert_eq!(
                account.credential_kind,
                crate::provider::CredentialKind::None
            );
            assert!(account.key_cipher.is_empty());
            let projection = crate::destination_projection::load_persisted(&db).unwrap();
            let credential = projection
                .credentials
                .iter()
                .find(|c| c.legacy_account_id == account_id)
                .unwrap();
            assert_eq!(credential.scope, ModelScope::Only { models: vec![] });
            let destination = projection
                .destinations
                .iter()
                .find(|d| d.id == credential.destination_id)
                .unwrap();
            assert_eq!(destination.max_credentials, Some(1));
            assert!(!destination.enabled);
            assert!(!destination.catalog[0].enabled);
            assert!(destination.catalog[0].protocols.is_empty());
        }
        drop(state);
        std::fs::remove_dir_all(dir).unwrap();
    }
}

#[test]
fn v8_package_with_routing_cards_is_rejected() {
    let mut payload = sample_payload();
    payload.version = V8_PAYLOAD_VERSION;
    let error = validate_payload(payload).unwrap_err();
    assert!(
        matches!(
            error,
            TransferError::Invalid(ref message) if message.contains("routing card semantics")
        ),
        "{error:?}"
    );
}

#[test]
fn v8_without_routing_cards_still_imports_and_v9_requires_the_field() {
    let mut v8 = sample_payload();
    v8.version = V8_PAYLOAD_VERSION;
    v8.routing_cards = None;
    validate_payload(v8).unwrap();

    let mut v9 = sample_payload();
    v9.routing_cards = None;
    let error = validate_payload(v9).unwrap_err();
    assert!(
        matches!(error, TransferError::Invalid(ref message) if message.contains("routingCards")),
        "{error:?}"
    );
}

fn routing_card(
    id: &str,
    destination: &str,
    credentials: &[&str],
) -> crate::dashboard_v4::types::RoutingCard {
    crate::dashboard_v4::types::RoutingCard {
        id: id.into(),
        destination_id: destination.into(),
        credential_ids: credentials.iter().map(|id| (*id).to_string()).collect(),
    }
}

fn load_cards(state: &crate::state::CoreState) -> Vec<crate::dashboard_v4::types::RoutingCard> {
    let db = state.db.lock();
    crate::db::routing_cards::load_on(&db.conn).unwrap()
}

fn rank_ids(state: &crate::state::CoreState) -> Vec<String> {
    load_cards(state)
        .into_iter()
        .flat_map(|card| card.credential_ids)
        .collect()
}

fn save_cards(state: &crate::state::CoreState, cards: &[crate::dashboard_v4::types::RoutingCard]) {
    let db = state.db.lock();
    crate::db::routing_cards::save_on(&db.conn, cards).unwrap();
}

fn credential_for(state: &crate::state::CoreState, account: &str) -> (String, String) {
    let db = state.db.lock();
    let projection = crate::destination_projection::load_persisted(&db).unwrap();
    let row = projection
        .credentials
        .iter()
        .find(|row| row.legacy_account_id == account)
        .unwrap();
    (row.id.clone(), row.destination_id.clone())
}

fn seed_ab_accounts(label: &str) -> (std::path::PathBuf, crate::state::CoreState) {
    use crate::crypto::{KeyCipher, StaticKeyCipher};
    use crate::db::Database;
    use crate::dynamic::DynamicProviderRuntime;
    use crate::models::{Account, AccountSetupStep, AccountType};
    use crate::provider::{CredentialKind, ProviderOrigin, QuotaScope, UpstreamProtocolKind};
    use crate::state::CoreStateInner;
    use chrono::Utc;
    use ocg_domain::dynamic::{DynamicAuthKind, DynamicModelMapping};
    use std::sync::Arc;

    let dir = std::env::temp_dir().join(format!("ocg-transfer-{label}-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let cipher: Arc<dyn KeyCipher + Send + Sync> =
        Arc::new(StaticKeyCipher::new("v9-routing-cards"));
    let db = Database::open(dir.clone()).unwrap();
    let now = Utc::now();
    for (id, url) in [
        (
            "00000000-0000-4000-8000-0000000000aa",
            "https://a.invalid/v1",
        ),
        (
            "00000000-0000-4000-8000-0000000000bb",
            "https://b.invalid/v1",
        ),
    ] {
        db.create_dynamic_provider_definition(&DynamicProviderRuntime {
            preset_id: None,
            id: id.into(),
            name: id.into(),
            endpoint_url: url.into(),
            upstream_protocol: UpstreamProtocolKind::ChatCompletions,
            auth_kind: DynamicAuthKind::Bearer,
            mappings: vec![DynamicModelMapping {
                public_model: "card-test".into(),
                upstream_model: format!("{id}-upstream"),
                upstream_override: None,
            }],
            created_at: now,
            updated_at: now,
            origin: ProviderOrigin::Custom,
            offering: "api".into(),
        })
        .unwrap();
    }
    for (id, provider) in [
        (
            "00000000-0000-4000-8000-0000000000a1",
            "00000000-0000-4000-8000-0000000000aa",
        ),
        (
            "00000000-0000-4000-8000-0000000000a2",
            "00000000-0000-4000-8000-0000000000aa",
        ),
        (
            "00000000-0000-4000-8000-0000000000b1",
            "00000000-0000-4000-8000-0000000000bb",
        ),
    ] {
        db.create_account(&Account {
            id: id.into(),
            provider_id: provider.into(),
            credential_kind: CredentialKind::ApiKey,
            quota_scope: QuotaScope::Key,
            name: id.into(),
            username: None,
            password_cipher: None,
            key_cipher: cipher.encrypt(&format!("dummy-{id}")).unwrap(),
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
        })
        .unwrap();
    }
    let state = Arc::new(CoreStateInner::new(db, dir.clone(), cipher).unwrap());
    (dir, state)
}

fn layout_covering_snapshot(
    state: &crate::state::CoreState,
    desired: Vec<crate::dashboard_v4::types::RoutingCard>,
) -> Vec<crate::dashboard_v4::types::RoutingCard> {
    let owned: HashSet<_> = desired
        .iter()
        .flat_map(|card| card.credential_ids.iter().cloned())
        .collect();
    let mut cards = desired;
    cards.extend(
        load_cards(state)
            .into_iter()
            .filter(|card| !card.credential_ids.iter().any(|id| owned.contains(id))),
    );
    cards
}

fn import_validated_node(
    target: &crate::state::CoreState,
    validated: &ValidatedMigration,
) -> anyhow::Result<()> {
    let mut record = empty_node_import();
    record.account_order = validated.node.account_order.clone();
    record.destination_controls = validated.unified.destination_controls.clone();
    record.dynamic_providers = validated.unified.dynamic_providers.clone();
    record.custom_destinations = validated.unified.custom_destinations.clone();
    record.custom_credential_destinations =
        validated.unified.custom_credential_destinations.clone();
    record.identity_snapshot = validated.unified.identity_snapshot.clone();
    record.draft_provider_ids = validated.unified.draft_provider_ids.clone();
    record.zen_free_enabled = validated.node.zen_free.enabled;
    for account in &validated.accounts {
        let id = account.id.clone().unwrap();
        let key_cipher = if account.key.is_empty() {
            String::new()
        } else {
            target.encrypt_key(account.key.as_str())?
        };
        record.accounts.push(crate::db::AccountImportRecord {
            account: crate::models::Account {
                id,
                provider_id: account.provider_id.clone(),
                credential_kind: account.credential_kind,
                quota_scope: account.quota_scope,
                name: account.name.clone(),
                username: account.username.clone(),
                password_cipher: None,
                key_cipher,
                enabled: account.enabled,
                account_type: account.account_type,
                setup_step: account.setup_step,
                referral_code: None,
                purchase_date: account.purchase_date.clone(),
                expires_on: account.expires_on.clone(),
                cooldown_until: account.cooldowns.until,
                cooldown_generic_until: account.cooldowns.generic,
                cooldown_5h_until: account.cooldowns.five_hours,
                cooldown_week_until: account.cooldowns.week,
                cooldown_month_until: account.cooldowns.month,
                cooldown_free_until: account.cooldowns.free,
                last_error: None,
                auth_error: None,
                notes: account.notes.clone(),
                created_at: chrono::Utc::now(),
                updated_at: chrono::Utc::now(),
            },
            custom_config: account.custom_config.clone(),
            capabilities: account.capabilities.clone(),
            verification_status: account.verification_status,
            connection_verified_at: account.connection_verified_at,
            ollama_billing_tier: account.ollama_billing_tier,
            goat_plan: account.goat_plan.clone(),
        });
    }
    let imported = validated.unified.routing_cards.clone();
    let db = target.db.lock();
    let preimport = crate::db::routing_cards::load_on(&db.conn)?;
    db.import_node_state(&record, |db| {
        if let Some(cards) = imported.as_ref() {
            restore_imported_routing_cards_on(&db.conn, &preimport, cards)?;
        }
        Ok(())
    })?;
    Ok(())
}

fn local_import(
    state: &crate::state::CoreState,
    password: &str,
    bundle: &str,
) -> Result<AccountImportResult, super::super::V3ApiError> {
    static IMPORT_TEST_GATE: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _serial = IMPORT_TEST_GATE.lock().unwrap();
    use axum::body::Bytes;
    use axum::http::HeaderMap;

    state.set_dashboard_local_mode(true);
    let mut headers = HeaderMap::new();
    headers.insert("host", "localhost".parse().unwrap());
    let body = serde_json::to_vec(&serde_json::json!({
        "expectedRevision": state.settings_revision(),
        "processGeneration": state.process_generation(),
        "password": password,
        "bundle": bundle,
    }))
    .unwrap();
    let mut effect = super::super::settings::CommittedEffect::Atomic;
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(import_accounts_inner(
            state.clone(),
            headers,
            Bytes::from(body),
            &mut effect,
        ))
        .map(|axum::Json(result)| result)
}

#[test]
fn imported_routing_cards_do_not_replace_preexisting_membership() {
    let preimport = vec![
        routing_card("current", "dest-a", &["a1", "a2"]),
        routing_card("empty", "dest-a", &[]),
    ];
    let imported = vec![routing_card("imported", "dest-a", &["a2", "a1"])];
    let merged = proposed_imported_routing_cards(preimport.clone(), &imported, &preimport).unwrap();
    assert_eq!(merged, preimport);
}

#[test]
fn v9_routing_cards_must_cover_inference_credentials_and_known_destinations() {
    let mut unknown_dest = sample_payload();
    unknown_dest.routing_cards.as_mut().unwrap()[0].destination_id = "missing-dest".into();
    let error = validate_payload(unknown_dest).unwrap_err();
    assert!(
        matches!(
            error,
            TransferError::Invalid(ref message) if message.contains("unknown destination")
        ),
        "{error:?}"
    );

    let mut incomplete = sample_payload();
    incomplete.routing_cards.as_mut().unwrap()[0]
        .credential_ids
        .clear();
    let error = validate_payload(incomplete).unwrap_err();
    assert!(
        matches!(
            error,
            TransferError::Invalid(ref message)
                if message.contains("every inference credential exactly once")
        ),
        "{error:?}"
    );
}

#[test]
fn v9_routing_cards_contradictory_rank_order_is_rejected() {
    let provider_id = "00000000-0000-4000-8000-0000000000aa";
    let first_id = "00000000-0000-4000-8000-0000000000a1";
    let second_id = "00000000-0000-4000-8000-0000000000a2";
    let mut first = sample_account("A1");
    first.id = Some(first_id.into());
    first.provider_id = provider_id.into();
    let mut second = sample_account("A2");
    second.id = Some(second_id.into());
    second.provider_id = provider_id.into();
    let mut payload = PortablePayload {
        platform_accounts: Vec::new(),
        platform_links: Vec::new(),
        version: PAYLOAD_VERSION,
        exported_at: "2026-08-29T00:00:00Z".into(),
        accounts: vec![first, second],
        dynamic_providers: vec![sample_dynamic_provider(provider_id, "Lab")],
        identities: Vec::new(),
        quota_pools: Vec::new(),
        destinations: Vec::new(),
        credentials: Vec::new(),
        routing_cards: None,
        node: Some({
            let mut node = sample_node(first_id);
            node.account_order.push(second_id.into());
            node
        }),
    };
    attach_default_identity_snapshot(&mut payload);
    attach_default_destination_snapshot(&mut payload);
    payload.accounts.clear();
    payload.identities.clear();
    payload.dynamic_providers.clear();
    payload.routing_cards.as_mut().unwrap()[0]
        .credential_ids
        .reverse();
    let error = validate_payload(payload).unwrap_err();
    assert!(
        matches!(
            error,
            TransferError::Invalid(ref message) if message.contains("routing_rank")
        ),
        "{error:?}"
    );
}

#[test]
fn imported_routing_cards_keep_target_only_credentials_and_empty_cards() {
    let preimport = vec![
        routing_card("target", "dest-t", &["target-only"]),
        routing_card("empty", "dest-t", &[]),
        routing_card("shared", "dest-s", &["shared"]),
    ];
    let imported = vec![routing_card(
        "imported",
        "dest-s",
        &["shared", "new-on-source"],
    )];
    let post = vec![
        routing_card("target", "dest-t", &["target-only"]),
        routing_card("empty", "dest-t", &[]),
        routing_card("shared", "dest-s", &["shared"]),
        routing_card("auto", "dest-s", &["new-on-source"]),
    ];
    let merged = proposed_imported_routing_cards(preimport, &imported, &post).unwrap();
    assert_eq!(
        merged.iter().find(|card| card.id == "target").unwrap(),
        &routing_card("target", "dest-t", &["target-only"])
    );
    assert!(
        merged
            .iter()
            .any(|card| card.id == "empty" && card.credential_ids.is_empty())
    );
    assert_eq!(
        merged.iter().find(|card| card.id == "shared").unwrap(),
        &routing_card("shared", "dest-s", &["shared"])
    );
    assert_eq!(
        merged.iter().find(|card| card.id == "imported").unwrap(),
        &routing_card("imported", "dest-s", &["new-on-source"])
    );
}

#[test]
fn imported_routing_card_id_collision_across_destinations_is_rejected() {
    let preimport = vec![routing_card("same", "dest-a", &["a1"])];
    let imported = vec![routing_card("same", "dest-b", &["b-new"])];
    let post = vec![
        routing_card("same", "dest-a", &["a1"]),
        routing_card("auto", "dest-b", &["b-new"]),
    ];
    let error = proposed_imported_routing_cards(preimport, &imported, &post).unwrap_err();
    assert!(error.to_string().contains("collides"), "{error}");
}

#[test]
fn v9_merge_keeps_preexisting_ranks_and_unrelated_empty_card() {
    let (source_dir, source) = transfer_state("v9-merge-source");
    let (target_dir, target) = seed_ab_accounts("v9-merge-target");
    let (a1, dest_a) = credential_for(&target, "00000000-0000-4000-8000-0000000000a1");
    let (a2, _) = credential_for(&target, "00000000-0000-4000-8000-0000000000a2");
    let (b1, dest_b) = credential_for(&target, "00000000-0000-4000-8000-0000000000b1");
    let empty_id = uuid::Uuid::new_v4().to_string();
    let desired = vec![
        routing_card("card-a1", &dest_a, &[a1.as_str()]),
        routing_card("card-b1", &dest_b, &[b1.as_str()]),
        routing_card("card-a2", &dest_a, &[a2.as_str()]),
        routing_card(&empty_id, &dest_a, &[]),
    ];
    save_cards(&target, &layout_covering_snapshot(&target, desired));
    let ranks_before = rank_ids(&target);
    assert_eq!(
        ranks_before
            .iter()
            .filter(|id| **id == a1 || **id == b1 || **id == a2)
            .cloned()
            .collect::<Vec<_>>(),
        vec![a1.clone(), b1.clone(), a2.clone()]
    );

    let (payload, _, _) = export_payload(&source).unwrap();
    let bundle = encrypt_payload(&payload, "correct horse battery").unwrap();
    let validated = decrypt_and_validate(&bundle, "correct horse battery").unwrap();
    import_validated_node(&target, &validated).unwrap();
    let restored = load_cards(&target);
    assert_eq!(
        rank_ids(&target)
            .into_iter()
            .filter(|id| *id == a1 || *id == b1 || *id == a2)
            .collect::<Vec<_>>(),
        vec![a1, b1, a2]
    );
    assert!(
        restored
            .iter()
            .any(|card| card.id == empty_id && card.credential_ids.is_empty())
    );
    drop(source);
    drop(target);
    std::fs::remove_dir_all(source_dir).unwrap();
    std::fs::remove_dir_all(target_dir).unwrap();
}

#[test]
fn v9_fresh_import_keeps_source_adjacent_cards_and_empty_after_reopen() {
    let (source_dir, source) = seed_ab_accounts("v9-adjacent-source");
    let (a1, dest_a) = credential_for(&source, "00000000-0000-4000-8000-0000000000a1");
    let (a2, _) = credential_for(&source, "00000000-0000-4000-8000-0000000000a2");
    let (b1, dest_b) = credential_for(&source, "00000000-0000-4000-8000-0000000000b1");
    let empty_id = "source-empty-a".to_string();
    let desired = vec![
        routing_card("source-a1", &dest_a, &[a1.as_str()]),
        routing_card(&empty_id, &dest_a, &[]),
        routing_card("source-a2", &dest_a, &[a2.as_str()]),
        routing_card("source-b1", &dest_b, &[b1.as_str()]),
    ];
    save_cards(&source, &layout_covering_snapshot(&source, desired));
    let (payload, _, _) = export_payload(&source).unwrap();
    let exported = payload.routing_cards.clone().unwrap();
    assert!(
        exported
            .iter()
            .any(|card| card.id == empty_id && card.credential_ids.is_empty())
    );
    let bundle = encrypt_payload(&payload, "correct horse battery").unwrap();
    let validated = decrypt_and_validate(&bundle, "correct horse battery").unwrap();

    let (target_dir, target) = transfer_state("v9-adjacent-target");
    import_validated_node(&target, &validated).unwrap();
    let restored = load_cards(&target);
    let a_cards: Vec<_> = restored
        .iter()
        .filter(|card| card.destination_id == dest_a)
        .map(|card| (card.id.as_str(), card.credential_ids.clone()))
        .collect();
    assert!(
        a_cards.windows(3).any(|window| {
            window[0] == ("source-a1", vec![a1.clone()])
                && window[1] == (empty_id.as_str(), Vec::new())
                && window[2] == ("source-a2", vec![a2.clone()])
        }),
        "{a_cards:?}"
    );
    assert!(
        restored
            .iter()
            .any(|card| { card.id == "source-b1" && card.credential_ids == [b1.clone()] })
    );

    drop(target);
    let reopened = crate::db::Database::open(target_dir.clone()).unwrap();
    let again = crate::db::routing_cards::load_on(&reopened.conn).unwrap();
    assert_eq!(again, restored);
    drop(reopened);
    drop(source);
    std::fs::remove_dir_all(source_dir).unwrap();
    std::fs::remove_dir_all(target_dir).unwrap();
}

#[test]
fn v9_repeat_import_does_not_add_extra_cards() {
    let (source_dir, source) = seed_ab_accounts("v9-repeat-source");
    let (payload, _, _) = export_payload(&source).unwrap();
    let bundle = encrypt_payload(&payload, "correct horse battery").unwrap();
    let validated = decrypt_and_validate(&bundle, "correct horse battery").unwrap();
    let (target_dir, target) = transfer_state("v9-repeat-target");
    import_validated_node(&target, &validated).unwrap();
    let first = load_cards(&target);
    import_validated_node(&target, &validated).unwrap();
    assert_eq!(load_cards(&target), first);
    drop(source);
    drop(target);
    std::fs::remove_dir_all(source_dir).unwrap();
    std::fs::remove_dir_all(target_dir).unwrap();
}

#[test]
fn v9_export_import_restores_interleaved_routing_cards_on_fresh_target() {
    let (source_dir, source) = seed_ab_accounts("v9-cards-source");
    let (a1, dest_a) = credential_for(&source, "00000000-0000-4000-8000-0000000000a1");
    let (a2, _) = credential_for(&source, "00000000-0000-4000-8000-0000000000a2");
    let (b1, dest_b) = credential_for(&source, "00000000-0000-4000-8000-0000000000b1");
    let desired = vec![
        routing_card("src-a1", &dest_a, &[a1.as_str()]),
        routing_card("src-b1", &dest_b, &[b1.as_str()]),
        routing_card("src-a2", &dest_a, &[a2.as_str()]),
    ];
    save_cards(&source, &layout_covering_snapshot(&source, desired));
    let (payload, _, _) = export_payload(&source).unwrap();
    assert_eq!(payload.version, PAYLOAD_VERSION);
    let exported = payload.routing_cards.clone().unwrap();
    assert_eq!(
        exported
            .iter()
            .flat_map(|card| card.credential_ids.iter().cloned())
            .filter(|id| *id == a1 || *id == b1 || *id == a2)
            .collect::<Vec<_>>(),
        vec![a1.clone(), b1.clone(), a2.clone()]
    );
    let bundle = encrypt_payload(&payload, "correct horse battery").unwrap();
    let validated = decrypt_and_validate(&bundle, "correct horse battery").unwrap();
    assert_eq!(validated.unified.routing_cards.as_ref().unwrap(), &exported);

    let (target_dir, target) = transfer_state("v9-cards-target");
    import_validated_node(&target, &validated).unwrap();
    let restored = load_cards(&target);
    assert!(
        restored
            .iter()
            .any(|card| { card.id == "src-a1" && card.credential_ids == [a1.clone()] })
    );
    assert!(
        restored
            .iter()
            .any(|card| { card.id == "src-b1" && card.credential_ids == [b1.clone()] })
    );
    assert!(
        restored
            .iter()
            .any(|card| { card.id == "src-a2" && card.credential_ids == [a2.clone()] })
    );
    drop(source);
    drop(target);
    std::fs::remove_dir_all(source_dir).unwrap();
    std::fs::remove_dir_all(target_dir).unwrap();
}

#[test]
fn v9_import_endpoint_rolls_back_when_routing_card_ids_collide() {
    let (source_dir, source) = seed_ab_accounts("v9-collide-source");
    let (a1, dest_a) = credential_for(&source, "00000000-0000-4000-8000-0000000000a1");
    let (a2, _) = credential_for(&source, "00000000-0000-4000-8000-0000000000a2");
    let (b1, dest_b) = credential_for(&source, "00000000-0000-4000-8000-0000000000b1");
    let collision = "collision-card-id";
    let desired = vec![
        routing_card(collision, &dest_a, &[a1.as_str()]),
        routing_card("src-b1", &dest_b, &[b1.as_str()]),
        routing_card("src-a2", &dest_a, &[a2.as_str()]),
    ];
    save_cards(&source, &layout_covering_snapshot(&source, desired));
    let (payload, _, _) = export_payload(&source).unwrap();
    let bundle = encrypt_payload(&payload, "correct horse battery").unwrap();

    let (target_dir, target) = transfer_state("v9-collide-target");
    let before = target
        .db
        .lock()
        .list_accounts()
        .unwrap()
        .into_iter()
        .map(|account| account.id)
        .collect::<Vec<_>>();
    let mut target_cards = load_cards(&target);
    target_cards[0].id = collision.into();
    save_cards(&target, &target_cards);
    let error = local_import(&target, "correct horse battery", &bundle).unwrap_err();
    assert!(
        error.body.message.contains("collides"),
        "{}",
        error.body.message
    );
    let after = target
        .db
        .lock()
        .list_accounts()
        .unwrap()
        .into_iter()
        .map(|account| account.id)
        .collect::<Vec<_>>();
    assert_eq!(after, before);
    assert!(
        after
            .iter()
            .all(|id| id != "00000000-0000-4000-8000-0000000000a1")
    );
    drop(source);
    drop(target);
    std::fs::remove_dir_all(source_dir).unwrap();
    std::fs::remove_dir_all(target_dir).unwrap();
}

#[test]
fn v9_routing_cards_contradictory_node_order_is_rejected() {
    let (source_dir, source) = seed_ab_accounts("v9-node-order");
    let (mut payload, _, _) = export_payload(&source).unwrap();
    payload.node.as_mut().unwrap().account_order.reverse();
    let error = validate_payload(payload).unwrap_err();
    assert!(
        matches!(error, TransferError::Invalid(ref message) if message.contains("account_order")),
        "{error:?}"
    );
    drop(source);
    std::fs::remove_dir_all(source_dir).unwrap();
}

#[test]
fn imported_empty_routing_card_collision_is_rejected() {
    let preimport = vec![routing_card("same", "dest-a", &[])];
    let imported = vec![routing_card("same", "dest-b", &[])];
    let post = vec![
        routing_card("same", "dest-a", &[]),
        routing_card("auto", "dest-b", &[]),
    ];
    let error = proposed_imported_routing_cards(preimport, &imported, &post).unwrap_err();
    assert!(error.to_string().contains("collides"), "{error}");
}

#[test]
fn v11_payload_roundtrip_keeps_http_routes_model_controls_and_grants() {
    use crate::dashboard_v4::types::ProtocolDto;
    use ocg_domain::destination::{
        AuthScheme, Protocol, destination_id_for_custom_account, destination_id_for_dynamic,
    };

    let payload = v11_http_transfer_payload();
    assert_eq!(payload.version, PAYLOAD_VERSION);
    let encoded = serde_json::to_value(&payload).unwrap();
    let custom_json = encoded["destinations"]
        .as_array()
        .unwrap()
        .iter()
        .find(|destination| {
            destination["protocolRoutes"]
                .as_array()
                .map(|routes| routes.len())
                == Some(2)
        })
        .expect("v11 payload must serialize protocolRoutes");
    assert_eq!(
        custom_json["protocolRoutes"][0]["protocol"],
        "chat_completions"
    );
    assert_eq!(custom_json["protocolRoutes"][1]["protocol"], "messages");

    let bundle = encrypt_payload(&payload, "correct horse battery").unwrap();
    assert!(!bundle.contains(V11_DEFAULT_ENDPOINT));
    let restored = decrypt_and_validate(&bundle, "correct horse battery").unwrap();
    let custom_id = destination_id_for_custom_account(V11_CUSTOM_ACCOUNT_ID);
    let dynamic_id = destination_id_for_dynamic(V11_DYNAMIC_PROVIDER_ID);

    let custom = restored
        .unified
        .destinations
        .iter()
        .find(|destination| destination.id == custom_id)
        .expect("custom destination");
    assert_eq!(custom.protocol_routes.len(), 2);
    assert_eq!(custom.protocols.len(), 2);
    assert_eq!(custom.catalog.len(), 2);
    assert!(custom.catalog[0].enabled);
    assert_eq!(
        custom.catalog[0].protocols,
        vec![ProtocolDto::ChatCompletions, ProtocolDto::Messages]
    );
    assert_eq!(custom.catalog[0].preferred, Some(ProtocolDto::Messages));
    assert_eq!(
        custom.catalog[0]
            .upstream_override
            .as_ref()
            .map(|route| route.endpoint_url.as_str()),
        Some(V11_MESSAGES_ENDPOINT)
    );
    assert!(!custom.catalog[1].enabled);

    let custom_controls = restored
        .unified
        .destination_controls
        .iter()
        .find(|destination| destination.id == custom_id)
        .expect("custom destination_controls");
    assert_eq!(custom_controls.protocol_routes.len(), 2);
    assert_eq!(
        custom_controls.protocol_routes[0].protocol,
        Protocol::ChatCompletions
    );
    assert_eq!(
        custom_controls.protocol_routes[0].endpoint_url,
        V11_DEFAULT_ENDPOINT
    );
    assert_eq!(
        custom_controls.protocol_routes[1].protocol,
        Protocol::Messages
    );
    assert_eq!(custom_controls.catalog.len(), 2);
    assert!(custom_controls.catalog[0].enabled);
    assert_eq!(
        custom_controls.catalog[0].protocols,
        vec![Protocol::ChatCompletions, Protocol::Messages]
    );
    assert_eq!(
        custom_controls.catalog[0].preferred,
        Some(Protocol::Messages)
    );
    assert_eq!(
        custom_controls.catalog[0]
            .upstream_override
            .as_ref()
            .map(|route| route.endpoint_url.as_str()),
        Some(V11_MESSAGES_ENDPOINT)
    );
    assert!(!custom_controls.catalog[1].enabled);

    let imported_custom = restored
        .unified
        .custom_destinations
        .iter()
        .find(|destination| destination.id == custom_id)
        .expect("imported custom");
    assert_eq!(imported_custom.protocol, Protocol::ChatCompletions);
    assert_eq!(imported_custom.endpoint_url, V11_DEFAULT_ENDPOINT);
    assert_eq!(imported_custom.auth_scheme, AuthScheme::Bearer);
    assert_eq!(imported_custom.models.len(), 2);
    assert_eq!(
        imported_custom.models[0]
            .upstream_override
            .as_ref()
            .map(|route| (route.protocol, route.endpoint_url.as_str())),
        Some((Protocol::Messages, V11_MESSAGES_ENDPOINT))
    );

    let dynamic_controls = restored
        .unified
        .destination_controls
        .iter()
        .find(|destination| destination.id == dynamic_id)
        .expect("dynamic destination_controls");
    assert_eq!(dynamic_controls.protocol_routes.len(), 2);
    assert_eq!(dynamic_controls.catalog.len(), 2);
    assert_eq!(
        dynamic_controls.catalog[0].preferred,
        Some(Protocol::Messages)
    );
    let imported_dynamic = restored
        .unified
        .dynamic_providers
        .iter()
        .find(|provider| provider.id == V11_DYNAMIC_PROVIDER_ID)
        .expect("imported dynamic");
    assert_eq!(
        imported_dynamic.upstream_protocol,
        Protocol::ChatCompletions
    );
    assert_eq!(imported_dynamic.endpoint_url, V11_DEFAULT_ENDPOINT);
    assert_eq!(imported_dynamic.mappings.len(), 2);
    assert_eq!(
        imported_dynamic.mappings[0]
            .upstream_override
            .as_ref()
            .map(|route| route.protocol),
        Some(Protocol::Messages)
    );

    let credential = restored
        .unified
        .credentials
        .iter()
        .find(|credential| credential.legacy_account_id == V11_CUSTOM_ACCOUNT_ID)
        .expect("custom credential");
    assert_eq!(
        credential.grants.allowed_endpoint_ids,
        V11_GRANT_ENDPOINTS
            .iter()
            .map(|id| (*id).to_string())
            .collect::<Vec<_>>()
    );
    assert_eq!(
        credential.grants.allowed_origins,
        vec![V11_GRANT_ORIGIN.to_string()]
    );
}

#[test]
fn versions_before_v11_reject_nonempty_protocol_routes() {
    for version in [
        V7_PAYLOAD_VERSION,
        V8_PAYLOAD_VERSION,
        V9_PAYLOAD_VERSION,
        V10_PAYLOAD_VERSION,
    ] {
        let mut payload = v11_http_transfer_payload();
        payload.version = version;
        if version < V9_PAYLOAD_VERSION {
            payload.routing_cards = None;
        }
        let error = validate_payload(payload).unwrap_err();
        assert!(
            matches!(
                error,
                TransferError::Invalid(ref message) if message.contains("protocol routes")
            ),
            "version {version}: {error:?}"
        );
    }
}

#[test]
fn v4_through_v10_payloads_without_protocol_routes_still_validate() {
    for version in [4, V5_PAYLOAD_VERSION] {
        validate_payload(sample_legacy_payload(version)).unwrap();
    }
    validate_payload(sample_v6_payload()).unwrap();
    for version in [
        V7_PAYLOAD_VERSION,
        V8_PAYLOAD_VERSION,
        V9_PAYLOAD_VERSION,
        V10_PAYLOAD_VERSION,
    ] {
        let mut payload = v11_http_transfer_payload();
        payload.version = version;
        strip_protocol_routes(&mut payload);
        if version < V9_PAYLOAD_VERSION {
            payload.routing_cards = None;
        }
        let validated = validate_payload(payload).unwrap();
        let custom = validated
            .unified
            .custom_destinations
            .iter()
            .find(|destination| destination.legacy_id == V11_CUSTOM_ACCOUNT_ID)
            .expect("legacy-compatible custom");
        assert_eq!(
            custom.protocol,
            ocg_domain::destination::Protocol::ChatCompletions
        );
        assert_eq!(custom.endpoint_url, V11_DEFAULT_ENDPOINT);
        assert_eq!(custom.models.len(), 2);
        assert!(
            validated
                .unified
                .destination_controls
                .iter()
                .all(|destination| { destination.protocol_routes.is_empty() })
        );
    }
}

#[test]
fn explicit_protocol_close_survives_import_and_pre_v6_radio_shape_is_repaired() {
    let portable = vec![PortableProviderContract {
        provider_id: crate::provider::MINIMAX_PROVIDER_ID.into(),
        catalog_models: vec!["MiniMax-M3".into()],
        catalog_refreshed_at: None,
        catalog_source: "test".into(),
        catalog_source_url: "https://example.test/models".into(),
        evidence: Vec::new(),
        preferences: Vec::new(),
        overrides: vec![
            PortableProtocolOverride {
                model_id: "MiniMax-M3".into(),
                protocol: "messages".into(),
                state: "force_on".into(),
            },
            PortableProtocolOverride {
                model_id: "MiniMax-M3".into(),
                protocol: "chat_completions".into(),
                state: "force_off".into(),
            },
            PortableProtocolOverride {
                model_id: "MiniMax-M3".into(),
                protocol: "responses".into(),
                state: "force_off".into(),
            },
        ],
    }];
    let now = chrono::Utc::now();
    let scope =
        crate::provider_contracts::ContractScope::provider(crate::provider::MINIMAX_PROVIDER_ID);
    let kept = persisted_contracts_from_portable(&portable, now, false).unwrap();
    let kept_off = kept
        .overrides
        .get(&scope)
        .unwrap()
        .iter()
        .filter(|row| row.state == ProtocolOverrideState::ForceOff)
        .count();
    assert_eq!(kept_off, 2, "an explicit close is not guessed away");

    let repaired = persisted_contracts_from_portable(&portable, now, true).unwrap();
    let repaired_off = repaired
        .overrides
        .get(&scope)
        .into_iter()
        .flatten()
        .filter(|row| row.state == ProtocolOverrideState::ForceOff)
        .count();
    assert_eq!(
        repaired_off, 0,
        "pre-V6 radio siblings are restored to Auto"
    );
    assert!(
        repaired
            .overrides
            .get(&scope)
            .unwrap()
            .iter()
            .any(|row| row.state == ProtocolOverrideState::ForceOn)
    );
}

const BUNDLE_PASSWORD: &str = "correct horse battery";
const GOAT_OWNER_ID: &str = "00000000-0000-4000-8000-000000000081";
const GOAT_SIBLING_ID: &str = "00000000-0000-4000-8000-000000000082";
const GOAT_MERGE_ID: &str = "00000000-0000-4000-8000-000000000091";
const GOAT_OWNER_KEY: &str = "goat-owner-plaintext";
const GOAT_SIBLING_KEY: &str = "goat-sibling-plaintext";
const GOAT_HOST_KEY: &str = "goat-host-plaintext";
const GOAT_REPLACED_KEY: &str = "goat-replaced-plaintext";
const GOAT_INCOMING_KEY: &str = "goat-incoming-plaintext";
const ORDINARY_GENERIC: &str = "2027-01-03T03:00:00Z";
const ORDINARY_FIVE_HOURS: &str = "2027-01-02T01:00:00Z";
const ORDINARY_WEEK: &str = "2027-02-01T00:00:00Z";
const LOCAL_FIVE_HOURS: &str = "2027-01-02T08:00:00Z";
const LOCAL_WEEK: &str = "2027-01-15T00:00:00Z";
const LOCAL_MONTH: &str = "2027-04-01T00:00:00Z";

fn at(stamp: &str) -> chrono::DateTime<chrono::Utc> {
    chrono::DateTime::parse_from_rfc3339(stamp)
        .unwrap()
        .with_timezone(&chrono::Utc)
}

fn goat_map(
    five_hours: Option<&str>,
    week: Option<&str>,
    month: Option<&str>,
) -> crate::goat_plan_cooldowns::GoatPlanCooldowns {
    crate::goat_plan_cooldowns::GoatPlanCooldowns {
        five_hours: five_hours.map(at),
        week: week.map(at),
        month: month.map(at),
    }
}

fn goat_model(id: &str, name: &str, key_cipher: String) -> crate::models::Account {
    let now = chrono::Utc::now();
    crate::models::Account {
        id: id.to_string(),
        provider_id: crate::provider::COMMAND_CODE_PROVIDER_ID.to_string(),
        credential_kind: crate::provider::CredentialKind::ApiKey,
        quota_scope: crate::provider::QuotaScope::Key,
        name: name.to_string(),
        username: None,
        password_cipher: None,
        key_cipher,
        enabled: true,
        account_type: crate::models::AccountType::Key,
        setup_step: crate::models::AccountSetupStep::Ready,
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
    }
}

fn share_owner_pool(state: &crate::state::CoreState, owner: &str, sibling: &str) {
    let db = state.db.lock();
    let updated = db
        .conn
        .execute(
            "UPDATE quota_pool_members
             SET pool_id = (SELECT pool_id FROM quota_pool_members WHERE account_id = ?1)
             WHERE account_id = ?2",
            [owner, sibling],
        )
        .unwrap();
    assert_eq!(updated, 1);
}

fn write_goat_map(
    state: &crate::state::CoreState,
    account_id: &str,
    map: &crate::goat_plan_cooldowns::GoatPlanCooldowns,
) {
    use crate::models::UsageWindowKind;

    let db = state.db.lock();
    let (credential_id, binding_id, version, key_cipher): (String, String, i64, String) = db
        .conn
        .query_row(
            "SELECT id, binding_id, credential_version, key_cipher
             FROM credentials WHERE legacy_account_id = ?1",
            [account_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .unwrap();
    for (window, reset) in [
        (UsageWindowKind::FiveHours, map.five_hours),
        (UsageWindowKind::Week, map.week),
        (UsageWindowKind::Month, map.month),
    ] {
        let Some(reset) = reset else {
            continue;
        };
        let stored = crate::goat_plan_cooldowns::record_window_on(
            &db.conn,
            &credential_id,
            account_id,
            &binding_id,
            version as u64,
            &key_cipher,
            window,
            reset,
        )
        .unwrap();
        assert!(stored);
    }
}

fn load_goat_map(
    state: &crate::state::CoreState,
    account_id: &str,
) -> Option<crate::goat_plan_cooldowns::GoatPlanCooldowns> {
    crate::goat_plan_cooldowns::load_for_legacy_on(&state.db.lock().conn, account_id).unwrap()
}

fn stored_account(state: &crate::state::CoreState, account_id: &str) -> crate::models::Account {
    state.db.lock().get_account(account_id).unwrap().unwrap()
}

fn duplicate_payload(payload: &PortablePayload) -> PortablePayload {
    serde_json::from_value(serde_json::to_value(payload).unwrap()).unwrap()
}

fn credential<'a>(payload: &'a PortablePayload, account_id: &str) -> &'a PortableCredential {
    payload
        .credentials
        .iter()
        .find(|credential| credential.legacy_account_id == account_id)
        .unwrap()
}

fn json_instant(value: &serde_json::Value) -> Option<chrono::DateTime<chrono::Utc>> {
    value
        .as_str()
        .and_then(|text| chrono::DateTime::parse_from_rfc3339(text).ok())
        .map(|stamp| stamp.with_timezone(&chrono::Utc))
}

fn encrypt_raw_payload(value: &serde_json::Value, password: &str) -> String {
    let mut salt = [0_u8; SALT_LEN];
    let mut nonce_bytes = [0_u8; NONCE_LEN];
    getrandom::fill(&mut salt).unwrap();
    getrandom::fill(&mut nonce_bytes).unwrap();
    let plaintext = serde_json::to_vec(value).unwrap();
    let key = derive_key(password, &salt).unwrap();
    let cipher = Aes256Gcm::new_from_slice(key.as_ref()).unwrap();
    let ciphertext = cipher
        .encrypt(
            Nonce::from_slice(&nonce_bytes),
            Payload {
                msg: &plaintext,
                aad: AAD,
            },
        )
        .unwrap();
    serde_json::to_string(&EncryptedEnvelope {
        format: ENVELOPE_FORMAT.to_string(),
        version: ENVELOPE_VERSION,
        salt: STANDARD.encode(salt),
        nonce: STANDARD.encode(nonce_bytes),
        ciphertext: STANDARD.encode(ciphertext),
    })
    .unwrap()
}

fn http_payload_at(version: u32) -> PortablePayload {
    let mut payload = v11_http_transfer_payload();
    payload.version = version;
    if version < V11_PAYLOAD_VERSION {
        strip_protocol_routes(&mut payload);
    }
    if version < V9_PAYLOAD_VERSION {
        payload.routing_cards = None;
    }
    payload
}

fn clear_goat_maps(payload: &mut PortablePayload) {
    for credential in &mut payload.credentials {
        credential.goat_plan_cooldowns = None;
    }
}

fn set_plain_key(payload: &mut PortablePayload, account_id: &str, key: &str) {
    credential_mut(payload, account_id).key = key.to_string();
}

fn credential_mut<'a>(
    payload: &'a mut PortablePayload,
    account_id: &str,
) -> &'a mut PortableCredential {
    payload
        .credentials
        .iter_mut()
        .find(|credential| credential.legacy_account_id == account_id)
        .unwrap()
}

#[derive(Debug, PartialEq, Eq)]
struct StoredAccountFacts {
    id: String,
    key_cipher: String,
    goat_plan_json: Option<String>,
    generic_until: Option<String>,
    five_hour_until: Option<String>,
    week_until: Option<String>,
    month_until: Option<String>,
    cooldown_until: Option<String>,
}

#[derive(Debug, PartialEq, Eq)]
struct StoredTransferFacts {
    rows: Vec<StoredAccountFacts>,
    revision: u64,
}

fn stored_transfer_facts(state: &crate::state::CoreState) -> StoredTransferFacts {
    let db = state.db.lock();
    let mut rows = Vec::new();
    for account in db.list_accounts().unwrap() {
        let goat_plan_json: Option<String> = db
            .conn
            .query_row(
                "SELECT goat_plan_cooldowns_json FROM credentials WHERE legacy_account_id = ?1",
                [&account.id],
                |row| row.get(0),
            )
            .unwrap();
        rows.push(StoredAccountFacts {
            id: account.id,
            key_cipher: account.key_cipher,
            goat_plan_json,
            generic_until: account
                .cooldown_generic_until
                .map(|value| value.to_rfc3339()),
            five_hour_until: account.cooldown_5h_until.map(|value| value.to_rfc3339()),
            week_until: account.cooldown_week_until.map(|value| value.to_rfc3339()),
            month_until: account.cooldown_month_until.map(|value| value.to_rfc3339()),
            cooldown_until: account.cooldown_until.map(|value| value.to_rfc3339()),
        });
    }
    rows.sort_by(|left, right| left.id.cmp(&right.id));
    let revision = state.settings_revision();
    StoredTransferFacts { rows, revision }
}

fn assert_rejected_transfer_leaves_rows(
    state: &crate::state::CoreState,
    before: &StoredTransferFacts,
    bundle: &str,
    expected: impl Fn(&TransferError) -> bool,
) {
    let error = decrypt_and_validate(bundle, BUNDLE_PASSWORD).unwrap_err();
    assert!(expected(&error), "{error:?}");
    assert!(local_import(state, BUNDLE_PASSWORD, bundle).is_err());
    assert_eq!(&stored_transfer_facts(state), before);
}

fn remove_transfer_state(state: crate::state::CoreState, dir: std::path::PathBuf) {
    drop(state);
    std::fs::remove_dir_all(dir).unwrap();
}

fn assert_owner_export_keeps_raw_ordinary_deadlines(payload: &PortablePayload) {
    let owner = credential(payload, GOAT_OWNER_ID);
    let sibling = credential(payload, GOAT_SIBLING_ID);
    let local = owner.goat_plan_cooldowns.clone().unwrap();
    let encoded = serde_json::to_value(owner).unwrap();
    let ordinary = &encoded["cooldowns"];
    let encoded_local = &encoded["goatPlanCooldowns"];

    assert_eq!(payload.version, PAYLOAD_VERSION);
    assert_eq!(
        local,
        goat_map(Some(LOCAL_FIVE_HOURS), Some(LOCAL_WEEK), Some(LOCAL_MONTH))
    );
    assert!(local.five_hours.unwrap() > at(ORDINARY_FIVE_HOURS));
    assert!(local.week.unwrap() < at(ORDINARY_WEEK));
    assert!(local.month.unwrap() > at(ORDINARY_WEEK));
    assert_eq!(
        json_instant(&ordinary["fiveHourUntil"]),
        Some(at(ORDINARY_FIVE_HOURS))
    );
    assert_eq!(
        json_instant(&ordinary["weekUntil"]),
        Some(at(ORDINARY_WEEK))
    );
    assert_eq!(
        json_instant(&ordinary["genericUntil"]),
        Some(at(ORDINARY_GENERIC))
    );
    assert!(ordinary["monthUntil"].is_null());
    assert_eq!(
        json_instant(&encoded_local["five_hours"]),
        Some(at(LOCAL_FIVE_HOURS))
    );
    assert_eq!(json_instant(&encoded_local["week"]), Some(at(LOCAL_WEEK)));
    assert_eq!(json_instant(&encoded_local["month"]), Some(at(LOCAL_MONTH)));
    assert_ne!(json_instant(&ordinary["fiveHourUntil"]), local.five_hours);
    assert_ne!(json_instant(&ordinary["weekUntil"]), local.week);

    let sibling_json = serde_json::to_value(sibling).unwrap();
    assert!(sibling.goat_plan_cooldowns.is_none());
    assert!(sibling_json.get("goatPlanCooldowns").is_none());
    assert_eq!(
        json_instant(&sibling_json["cooldowns"]["genericUntil"]),
        Some(at(ORDINARY_GENERIC))
    );
    assert_eq!(
        json_instant(&sibling_json["cooldowns"]["fiveHourUntil"]),
        Some(at(ORDINARY_FIVE_HOURS))
    );
    assert_eq!(
        json_instant(&sibling_json["cooldowns"]["weekUntil"]),
        Some(at(ORDINARY_WEEK))
    );
    assert!(sibling_json["cooldowns"]["monthUntil"].is_null());
    assert!(payload.quota_pools.iter().any(|pool| {
        let members: HashSet<&str> = pool.member_account_ids.iter().map(String::as_str).collect();
        members.contains(GOAT_OWNER_ID) && members.contains(GOAT_SIBLING_ID)
    }));
}

fn assert_stored_ordinary_is_not_the_local_union(
    state: &crate::state::CoreState,
    account_id: &str,
) {
    let account = stored_account(state, account_id);
    assert_eq!(account.cooldown_generic_until, Some(at(ORDINARY_GENERIC)));
    assert_eq!(account.cooldown_5h_until, Some(at(ORDINARY_FIVE_HOURS)));
    assert_eq!(account.cooldown_week_until, Some(at(ORDINARY_WEEK)));
    assert_eq!(account.cooldown_month_until, None);
    assert_eq!(account.cooldown_until, Some(at(ORDINARY_WEEK)));
    assert!(account.cooldown_until.unwrap() < at(LOCAL_MONTH));
}

#[test]
fn v12_export_validate_import_keeps_local_goat_deadlines_apart_from_ordinary_shared_deadlines() {
    let (source_dir, source) = transfer_state("goat-v12-export");
    let owner_cipher = source.encrypt_key(GOAT_OWNER_KEY).unwrap();
    let sibling_cipher = source.encrypt_key(GOAT_SIBLING_KEY).unwrap();
    {
        let db = source.db.lock();
        db.create_account(&goat_model(GOAT_OWNER_ID, "GOAT owner", owner_cipher))
            .unwrap();
        db.create_account(&goat_model(GOAT_SIBLING_ID, "GOAT sibling", sibling_cipher))
            .unwrap();
    }
    share_owner_pool(&source, GOAT_OWNER_ID, GOAT_SIBLING_ID);
    {
        let db = source.db.lock();
        db.set_account_rate_limit(GOAT_OWNER_ID, at(ORDINARY_GENERIC), "generic", None)
            .unwrap();
        db.set_account_rate_limit(
            GOAT_OWNER_ID,
            at(ORDINARY_FIVE_HOURS),
            "five",
            Some(crate::models::UsageWindowKind::FiveHours),
        )
        .unwrap();
        db.set_account_rate_limit(
            GOAT_OWNER_ID,
            at(ORDINARY_WEEK),
            "week",
            Some(crate::models::UsageWindowKind::Week),
        )
        .unwrap();
    }
    let local = goat_map(Some(LOCAL_FIVE_HOURS), Some(LOCAL_WEEK), Some(LOCAL_MONTH));
    write_goat_map(&source, GOAT_OWNER_ID, &local);
    assert_eq!(load_goat_map(&source, GOAT_OWNER_ID), Some(local.clone()));
    assert_eq!(load_goat_map(&source, GOAT_SIBLING_ID), None);
    assert_stored_ordinary_is_not_the_local_union(&source, GOAT_OWNER_ID);
    assert_stored_ordinary_is_not_the_local_union(&source, GOAT_SIBLING_ID);

    let (payload, _, _) = export_payload(&source).unwrap();
    assert_owner_export_keeps_raw_ordinary_deadlines(&payload);
    let bundle = encrypt_payload(&payload, BUNDLE_PASSWORD).unwrap();
    let envelope: serde_json::Value = serde_json::from_str(&bundle).unwrap();
    assert_eq!(envelope["format"], ENVELOPE_FORMAT);
    assert_eq!(envelope["version"], ENVELOPE_VERSION);
    assert!(!bundle.contains(GOAT_OWNER_KEY));
    assert!(!bundle.contains(GOAT_SIBLING_KEY));

    let validated = decrypt_and_validate(&bundle, BUNDLE_PASSWORD).unwrap();
    let validated_owner = validated
        .accounts
        .iter()
        .find(|account| account.id.as_deref() == Some(GOAT_OWNER_ID))
        .unwrap();
    let validated_sibling = validated
        .accounts
        .iter()
        .find(|account| account.id.as_deref() == Some(GOAT_SIBLING_ID))
        .unwrap();
    assert_eq!(validated_owner.goat_plan, Some(local.clone()));
    assert_eq!(
        validated_owner.cooldowns.five_hours,
        Some(at(ORDINARY_FIVE_HOURS))
    );
    assert_eq!(validated_owner.cooldowns.week, Some(at(ORDINARY_WEEK)));
    assert_eq!(
        validated_owner.cooldowns.generic,
        Some(at(ORDINARY_GENERIC))
    );
    assert_eq!(validated_owner.cooldowns.month, None);
    assert_eq!(validated_sibling.goat_plan, None);
    assert_eq!(validated_sibling.cooldowns.week, Some(at(ORDINARY_WEEK)));
    assert_eq!(validated_sibling.key.as_str(), GOAT_SIBLING_KEY);

    let (target_dir, target) = transfer_state("goat-v12-import");
    local_import(&target, BUNDLE_PASSWORD, &bundle).unwrap();
    assert_eq!(load_goat_map(&target, GOAT_OWNER_ID), Some(local.clone()));
    assert_eq!(load_goat_map(&target, GOAT_SIBLING_ID), None);
    assert_stored_ordinary_is_not_the_local_union(&target, GOAT_OWNER_ID);
    assert_stored_ordinary_is_not_the_local_union(&target, GOAT_SIBLING_ID);
    let owner_cipher = stored_account(&target, GOAT_OWNER_ID).key_cipher;
    let sibling_cipher = stored_account(&target, GOAT_SIBLING_ID).key_cipher;
    assert_ne!(owner_cipher, target.encrypt_key(GOAT_OWNER_KEY).unwrap());
    assert_eq!(target.decrypt_key(&owner_cipher).unwrap(), GOAT_OWNER_KEY);
    assert_eq!(
        target.decrypt_key(&sibling_cipher).unwrap(),
        GOAT_SIBLING_KEY
    );

    local_import(&target, BUNDLE_PASSWORD, &bundle).unwrap();
    assert_eq!(
        stored_account(&target, GOAT_OWNER_ID).key_cipher,
        owner_cipher
    );
    assert_eq!(
        stored_account(&target, GOAT_SIBLING_ID).key_cipher,
        sibling_cipher
    );
    assert_eq!(load_goat_map(&target, GOAT_OWNER_ID), Some(local));
    assert_eq!(load_goat_map(&target, GOAT_SIBLING_ID), None);
    assert_stored_ordinary_is_not_the_local_union(&target, GOAT_OWNER_ID);
    assert_stored_ordinary_is_not_the_local_union(&target, GOAT_SIBLING_ID);
    let (exported_again, _, _) = export_payload(&target).unwrap();
    assert_owner_export_keeps_raw_ordinary_deadlines(&exported_again);

    remove_transfer_state(source, source_dir);
    remove_transfer_state(target, target_dir);
}

#[test]
fn same_key_v11_preserves_host_goat_map_and_v12_merges_maxima_or_replaces_on_key_change() {
    let (dir, state) = transfer_state("goat-v11-v12-merge");
    let host_map = goat_map(
        Some("2027-03-05T00:00:00Z"),
        Some("2027-03-12T00:00:00Z"),
        Some("2027-03-20T00:00:00Z"),
    );
    {
        let db = state.db.lock();
        db.create_account(&goat_model(
            GOAT_MERGE_ID,
            "GOAT merge",
            state.encrypt_key(GOAT_HOST_KEY).unwrap(),
        ))
        .unwrap();
    }
    write_goat_map(&state, GOAT_MERGE_ID, &host_map);
    let cipher_before = stored_account(&state, GOAT_MERGE_ID).key_cipher;
    assert_ne!(cipher_before, state.encrypt_key(GOAT_HOST_KEY).unwrap());

    let (payload, _, _) = export_payload(&state).unwrap();
    assert_eq!(
        credential(&payload, GOAT_MERGE_ID).goat_plan_cooldowns,
        Some(host_map.clone())
    );
    let mut smuggled = duplicate_payload(&payload);
    smuggled.version = V11_PAYLOAD_VERSION;
    assert!(matches!(
        validate_payload(smuggled).unwrap_err(),
        TransferError::Invalid(_)
    ));

    let mut same_v11 = duplicate_payload(&payload);
    same_v11.version = V11_PAYLOAD_VERSION;
    clear_goat_maps(&mut same_v11);
    let same_v11_bundle = encrypt_payload(&same_v11, BUNDLE_PASSWORD).unwrap();
    let validated = decrypt_and_validate(&same_v11_bundle, BUNDLE_PASSWORD).unwrap();
    assert!(
        validated
            .accounts
            .iter()
            .all(|account| account.goat_plan.is_none())
    );
    local_import(&state, BUNDLE_PASSWORD, &same_v11_bundle).unwrap();
    assert_eq!(
        stored_account(&state, GOAT_MERGE_ID).key_cipher,
        cipher_before
    );
    assert_eq!(state.decrypt_key(&cipher_before).unwrap(), GOAT_HOST_KEY);
    assert_eq!(load_goat_map(&state, GOAT_MERGE_ID), Some(host_map));

    let mut changed_v11 = duplicate_payload(&payload);
    changed_v11.version = V11_PAYLOAD_VERSION;
    clear_goat_maps(&mut changed_v11);
    set_plain_key(&mut changed_v11, GOAT_MERGE_ID, GOAT_REPLACED_KEY);
    let changed_v11_bundle = encrypt_payload(&changed_v11, BUNDLE_PASSWORD).unwrap();
    local_import(&state, BUNDLE_PASSWORD, &changed_v11_bundle).unwrap();
    assert_eq!(load_goat_map(&state, GOAT_MERGE_ID), None);
    let replaced_cipher = stored_account(&state, GOAT_MERGE_ID).key_cipher;
    assert_ne!(replaced_cipher, cipher_before);
    assert_eq!(
        state.decrypt_key(&replaced_cipher).unwrap(),
        GOAT_REPLACED_KEY
    );

    let reseeded = goat_map(
        Some("2027-05-10T00:00:00Z"),
        Some("2027-05-01T00:00:00Z"),
        Some("2027-05-15T00:00:00Z"),
    );
    write_goat_map(&state, GOAT_MERGE_ID, &reseeded);
    let (refreshed, _, _) = export_payload(&state).unwrap();
    let incoming = goat_map(
        Some("2027-05-01T00:00:00Z"),
        Some("2027-06-01T00:00:00Z"),
        Some("2027-05-20T00:00:00Z"),
    );
    let mut same_v12 = duplicate_payload(&refreshed);
    credential_mut(&mut same_v12, GOAT_MERGE_ID).goat_plan_cooldowns = Some(incoming.clone());
    let same_v12_bundle = encrypt_payload(&same_v12, BUNDLE_PASSWORD).unwrap();
    let validated = decrypt_and_validate(&same_v12_bundle, BUNDLE_PASSWORD).unwrap();
    assert_eq!(
        validated
            .accounts
            .iter()
            .find(|account| account.id.as_deref() == Some(GOAT_MERGE_ID))
            .unwrap()
            .goat_plan,
        Some(incoming.clone())
    );
    local_import(&state, BUNDLE_PASSWORD, &same_v12_bundle).unwrap();
    assert_eq!(
        stored_account(&state, GOAT_MERGE_ID).key_cipher,
        replaced_cipher
    );
    assert_eq!(
        load_goat_map(&state, GOAT_MERGE_ID),
        Some(goat_map(
            Some("2027-05-10T00:00:00Z"),
            Some("2027-06-01T00:00:00Z"),
            Some("2027-05-20T00:00:00Z"),
        ))
    );

    let incoming_only = goat_map(None, Some("2027-07-01T00:00:00Z"), None);
    let mut changed_v12 = duplicate_payload(&refreshed);
    set_plain_key(&mut changed_v12, GOAT_MERGE_ID, GOAT_INCOMING_KEY);
    credential_mut(&mut changed_v12, GOAT_MERGE_ID).goat_plan_cooldowns =
        Some(incoming_only.clone());
    let changed_v12_bundle = encrypt_payload(&changed_v12, BUNDLE_PASSWORD).unwrap();
    local_import(&state, BUNDLE_PASSWORD, &changed_v12_bundle).unwrap();
    assert_eq!(load_goat_map(&state, GOAT_MERGE_ID), Some(incoming_only));
    assert_eq!(
        state
            .decrypt_key(&stored_account(&state, GOAT_MERGE_ID).key_cipher)
            .unwrap(),
        GOAT_INCOMING_KEY
    );
    assert_ne!(
        stored_account(&state, GOAT_MERGE_ID).key_cipher,
        replaced_cipher
    );

    remove_transfer_state(state, dir);
}

#[test]
fn payloads_before_v12_reject_a_local_goat_map_and_older_fixtures_still_validate() {
    let local = goat_map(Some(LOCAL_FIVE_HOURS), Some(LOCAL_WEEK), Some(LOCAL_MONTH));
    for version in [4, V5_PAYLOAD_VERSION] {
        let payload = sample_legacy_payload(version);
        validate_payload(duplicate_payload(&payload)).unwrap();
        let mut value = serde_json::to_value(&payload).unwrap();
        value["accounts"][0]["goatPlanCooldowns"] = serde_json::json!({
            "five_hours": LOCAL_FIVE_HOURS,
            "week": LOCAL_WEEK,
            "month": LOCAL_MONTH
        });
        assert!(serde_json::from_value::<PortablePayload>(value).is_err());
    }
    let v6 = sample_v6_payload();
    validate_payload(duplicate_payload(&v6)).unwrap();
    let mut value = serde_json::to_value(&v6).unwrap();
    value["accounts"][0]["goatPlanCooldowns"] = serde_json::json!({
        "five_hours": LOCAL_FIVE_HOURS
    });
    assert!(serde_json::from_value::<PortablePayload>(value).is_err());

    for version in [
        V7_PAYLOAD_VERSION,
        V8_PAYLOAD_VERSION,
        V9_PAYLOAD_VERSION,
        V10_PAYLOAD_VERSION,
        V11_PAYLOAD_VERSION,
    ] {
        let payload = http_payload_at(version);
        validate_payload(duplicate_payload(&payload)).unwrap();
        let mut smuggled = payload;
        smuggled.credentials[0].goat_plan_cooldowns = Some(local.clone());
        assert!(matches!(
            validate_payload(smuggled).unwrap_err(),
            TransferError::Invalid(_)
        ));
    }
}

#[test]
fn non_goat_and_observer_credentials_reject_a_local_goat_map() {
    use super::portable::{PURPOSE_CPA_OBSERVER, PURPOSE_PLATFORM_OBSERVER};

    let local = goat_map(Some(LOCAL_FIVE_HOURS), Some(LOCAL_WEEK), Some(LOCAL_MONTH));
    let mut opencode = sample_payload();
    opencode.credentials[0].goat_plan_cooldowns = Some(local.clone());
    assert!(matches!(
        validate_payload(opencode).unwrap_err(),
        TransferError::Invalid(_)
    ));

    let (dir, state) = transfer_state("goat-observer-map");
    let parent_id = "00000000-0000-4000-8000-0000000000aa";
    let pat_cipher = state.encrypt_key("123:platform-pat-secret").unwrap();
    state
        .db
        .lock()
        .create_platform_account(
            parent_id,
            crate::platform::PlatformKind::NewApi,
            "Site",
            "https://newapi.example",
            Some(&pat_cipher),
        )
        .unwrap();
    let cpa_cipher = state.encrypt_key("cpa-management-secret").unwrap();
    state
        .db
        .lock()
        .upsert_cpa_integration(&cpa_account(), "http://127.0.0.1:8317", &cpa_cipher)
        .unwrap();
    let (payload, _, _) = export_payload(&state).unwrap();
    validate_payload(duplicate_payload(&payload)).unwrap();
    for purpose in [PURPOSE_PLATFORM_OBSERVER, PURPOSE_CPA_OBSERVER] {
        let mut smuggled = duplicate_payload(&payload);
        let credential = smuggled
            .credentials
            .iter_mut()
            .find(|credential| credential_purpose(credential) == purpose)
            .unwrap();
        credential.goat_plan_cooldowns = Some(local.clone());
        assert!(matches!(
            validate_payload(smuggled).unwrap_err(),
            TransferError::Invalid(_)
        ));
    }
    let before = stored_transfer_facts(&state);
    let mut platform_smuggle = duplicate_payload(&payload);
    platform_smuggle
        .credentials
        .iter_mut()
        .find(|credential| credential_purpose(credential) == PURPOSE_PLATFORM_OBSERVER)
        .unwrap()
        .goat_plan_cooldowns = Some(local);
    let bundle = encrypt_payload(&platform_smuggle, BUNDLE_PASSWORD).unwrap();
    assert_rejected_transfer_leaves_rows(&state, &before, &bundle, |error| {
        matches!(error, TransferError::Invalid(_))
    });
    remove_transfer_state(state, dir);
}

#[test]
fn malformed_goat_map_and_unsupported_transfer_write_no_rows() {
    assert_eq!(PAYLOAD_VERSION, 12);
    let (dir, state) = transfer_state("goat-reject-no-write");
    {
        let db = state.db.lock();
        db.create_account(&goat_model(
            GOAT_MERGE_ID,
            "GOAT reject",
            state.encrypt_key(GOAT_HOST_KEY).unwrap(),
        ))
        .unwrap();
    }
    write_goat_map(
        &state,
        GOAT_MERGE_ID,
        &goat_map(Some(LOCAL_FIVE_HOURS), Some(LOCAL_WEEK), Some(LOCAL_MONTH)),
    );
    {
        let db = state.db.lock();
        db.set_account_rate_limit(
            GOAT_MERGE_ID,
            at(ORDINARY_WEEK),
            "week",
            Some(crate::models::UsageWindowKind::Week),
        )
        .unwrap();
    }
    let before = stored_transfer_facts(&state);
    assert!(before.rows.iter().any(|row| {
        row.id == GOAT_MERGE_ID && row.goat_plan_json.is_some() && row.week_until.is_some()
    }));

    assert_rejected_transfer_leaves_rows(&state, &before, "not-an-envelope", |error| {
        matches!(error, TransferError::InvalidBundle)
    });

    let mut future = sample_payload();
    future.version = 13;
    let future_bundle = encrypt_payload(&future, BUNDLE_PASSWORD).unwrap();
    assert_rejected_transfer_leaves_rows(&state, &before, &future_bundle, |error| {
        matches!(error, TransferError::UnsupportedVersion(13))
    });

    let labeled = sample_legacy_payload(4);
    let mut labeled_json = serde_json::to_value(&labeled).unwrap();
    labeled_json["accounts"][0]["goatPlanCooldowns"] = serde_json::json!({
        "five_hours": LOCAL_FIVE_HOURS,
        "week": LOCAL_WEEK,
        "month": LOCAL_MONTH
    });
    let labeled_bundle = encrypt_raw_payload(&labeled_json, BUNDLE_PASSWORD);
    assert_rejected_transfer_leaves_rows(&state, &before, &labeled_bundle, |error| {
        matches!(error, TransferError::InvalidBundle)
    });

    let (exported, _, _) = export_payload(&state).unwrap();
    let mut old_package = duplicate_payload(&exported);
    old_package.version = V11_PAYLOAD_VERSION;
    let old_bundle = encrypt_payload(&old_package, BUNDLE_PASSWORD).unwrap();
    assert_rejected_transfer_leaves_rows(&state, &before, &old_bundle, |error| {
        matches!(error, TransferError::Invalid(_))
    });

    let mut carrier = sample_payload();
    carrier.credentials[0].goat_plan_cooldowns = Some(goat_map(
        Some(LOCAL_FIVE_HOURS),
        Some(LOCAL_WEEK),
        Some(LOCAL_MONTH),
    ));
    let mut raw = serde_json::to_value(&carrier).unwrap();
    let encoded_map = raw["credentials"][0]["goatPlanCooldowns"]
        .as_object()
        .unwrap();
    assert!(encoded_map.contains_key("five_hours"));
    assert!(encoded_map.contains_key("week"));
    assert!(encoded_map.contains_key("month"));
    let mut unknown_window = raw.clone();
    unknown_window["credentials"][0]["goatPlanCooldowns"]["day"] =
        serde_json::json!("2027-08-01T00:00:00Z");
    let unknown_bundle = encrypt_raw_payload(&unknown_window, BUNDLE_PASSWORD);
    assert_rejected_transfer_leaves_rows(&state, &before, &unknown_bundle, |error| {
        matches!(error, TransferError::InvalidBundle)
    });
    raw["credentials"][0]["goatPlanCooldowns"]["five_hours"] = serde_json::json!("not-a-time");
    let bad_instant_bundle = encrypt_raw_payload(&raw, BUNDLE_PASSWORD);
    assert_rejected_transfer_leaves_rows(&state, &before, &bad_instant_bundle, |error| {
        matches!(error, TransferError::InvalidBundle)
    });

    let mut non_goat = sample_payload();
    non_goat.credentials[0].goat_plan_cooldowns = Some(goat_map(Some(LOCAL_MONTH), None, None));
    let non_goat_bundle = encrypt_payload(&non_goat, BUNDLE_PASSWORD).unwrap();
    assert_rejected_transfer_leaves_rows(&state, &before, &non_goat_bundle, |error| {
        matches!(error, TransferError::Invalid(_))
    });

    remove_transfer_state(state, dir);
}
