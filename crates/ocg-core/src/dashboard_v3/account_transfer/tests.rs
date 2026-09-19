use super::*;
use ocg_domain::credential::ModelScope;
use std::collections::{HashMap, HashSet};

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
    if payload.version < PAYLOAD_VERSION {
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

    assert_eq!(PAYLOAD_VERSION, 7);
    let mut payload = sample_payload();
    payload.version = 8;
    let bundle = encrypt_payload(&payload, "correct horse battery").unwrap();
    let error = decrypt_and_validate(&bundle, "correct horse battery").unwrap_err();
    assert!(
        matches!(error, TransferError::UnsupportedVersion(8)),
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
        mapped.body.message.contains("payload version 8"),
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
        accounts: Vec::new(),
        account_order: vec![crate::provider::ZEN_FREE_ACCOUNT_ID.to_string()],
        config_json: serde_json::to_string(&config).unwrap(),
        sub_keys: Vec::new(),
        zen_free_enabled: false,
        zen_catalog: crate::kernel::zen::ZenFreeModelCatalog::default(),
        provider_contracts: crate::provider_contracts::PersistedContracts::default(),
        dynamic_providers: Vec::new(),
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
