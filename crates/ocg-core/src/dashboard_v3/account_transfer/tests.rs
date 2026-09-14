use super::*;

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

fn sample_payload() -> PortablePayload {
    let account_id = "00000000-0000-4000-8000-000000000041";
    let mut account = sample_account("Primary");
    account.id = Some(account_id.to_string());
    let mut payload = PortablePayload {
        platform_accounts: Vec::new(),
        platform_links: Vec::new(),
        version: PAYLOAD_VERSION,
        exported_at: "2026-08-29T00:00:00Z".to_string(),
        accounts: vec![account],
        dynamic_providers: Vec::new(),
        identities: Vec::new(),
        quota_pools: Vec::new(),
        node: Some(sample_node(account_id)),
    };
    attach_default_identity_snapshot(&mut payload);
    payload
}

fn sample_legacy_payload(version: u32) -> PortablePayload {
    let mut payload = sample_payload();
    payload.version = version;
    strip_identity_snapshot(&mut payload);
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
    assert!(imported.platform_accounts.is_empty());
    assert!(imported.platform_links.is_empty());
    assert!(imported.identity_snapshot.is_none());
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

    assert_eq!(PAYLOAD_VERSION, 6);
    let mut payload = sample_payload();
    payload.version = 7;
    let bundle = encrypt_payload(&payload, "correct horse battery").unwrap();
    let error = decrypt_and_validate(&bundle, "correct horse battery").unwrap_err();
    assert!(
        matches!(error, TransferError::UnsupportedVersion(7)),
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
        mapped.body.message.contains("payload version 7"),
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

    let mut payload = sample_payload();
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
        node: Some(sample_node(account_id)),
    };
    attach_default_identity_snapshot(&mut payload);
    let validated = validate_payload(payload).unwrap();
    assert_eq!(validated.dynamic_providers.len(), 1);
    assert_eq!(validated.dynamic_providers[0].name, "Lab");
    assert_eq!(
        validated.accounts[0].credential_kind,
        crate::provider::CredentialKind::ApiKey
    );

    let mut dangling_account = sample_account("Lab");
    dangling_account.id = Some(account_id.to_string());
    dangling_account.provider_id = provider_id.to_string();
    let dangling = PortablePayload {
        platform_accounts: Vec::new(),
        platform_links: Vec::new(),
        version: PAYLOAD_VERSION,
        exported_at: "2026-08-29T00:00:00Z".to_string(),
        accounts: vec![dangling_account],
        dynamic_providers: Vec::new(),
        identities: Vec::new(),
        quota_pools: Vec::new(),
        node: Some(sample_node(account_id)),
    };
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
        node: Some(sample_node(account_id)),
    };
    attach_default_identity_snapshot(&mut payload);
    let json = serde_json::to_value(&payload).unwrap();
    let capability = &json["accounts"][0]["modelCapabilities"][0];
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
    let mut payload = sample_payload();
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
    let mut draft = sample_payload();
    draft.accounts[0].account_type = "managed".to_string();
    draft.accounts[0].setup_step = "payment".to_string();
    draft.accounts[0].enabled = true;
    let draft = validate_payload(draft).unwrap();
    assert_eq!(draft.accounts[0].setup_step, ModelSetupStep::GoogleAccount);
    assert!(!draft.accounts[0].enabled);
    assert!(draft.accounts[0].key.is_empty());

    let mut ready = sample_payload();
    ready.accounts[0].account_type = "managed".to_string();
    let ready = validate_payload(ready).unwrap();
    assert_eq!(ready.accounts[0].setup_step, ModelSetupStep::Ready);
    assert!(ready.accounts[0].enabled);
    assert_eq!(ready.accounts[0].key.as_str(), "sk-ocg-test-secret");
}

#[test]
fn account_count_and_decoded_ciphertext_limits_fail_closed() {
    let mut payload = sample_payload();
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
    assert_eq!(
        validate_payload(payload).unwrap().accounts.len(),
        MAX_ACCOUNTS
    );

    let mut oversized = sample_payload();
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
        assert!(validated.identity_snapshot.is_none());
        assert_eq!(validated.platform_links_authoritative, version >= 5);
    }
}

#[test]
fn v6_without_identity_snapshot_is_rejected() {
    let mut payload = sample_payload();
    strip_identity_snapshot(&mut payload);
    payload.accounts[0].cooldowns = Some(PortableCooldowns::default());
    payload.version = PAYLOAD_VERSION;
    let error = validate_payload(payload).unwrap_err();
    assert!(
        matches!(error, TransferError::Invalid(ref message) if message.contains("identity")),
        "{error:?}"
    );
}

#[test]
fn v5_package_with_identity_semantics_is_not_silently_downgraded() {
    let mut payload = sample_payload();
    payload.version = V5_PAYLOAD_VERSION;
    let error = validate_payload(payload).unwrap_err();
    assert!(
        matches!(error, TransferError::Invalid(ref message) if message.contains("identity semantics")),
        "{error:?}"
    );
}

#[test]
fn v6_dangling_identity_and_verified_relation_are_rejected() {
    let mut dangling = sample_payload();
    dangling.identities[0].id = "00000000-0000-4000-8000-000000000099".to_string();
    let error = validate_payload(dangling).unwrap_err();
    assert!(
        matches!(error, TransferError::Invalid(ref message) if message.contains("identity")),
        "{error:?}"
    );

    let mut verified = sample_payload();
    verified.identities[0].identity_confidence = "verified".to_string();
    let error = validate_payload(verified).unwrap_err();
    assert!(
        matches!(error, TransferError::Invalid(ref message) if message.contains("verified")),
        "{error:?}"
    );

    let mut missing_member = sample_payload();
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
            let mut payload = sample_payload();
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
        "2ccaef5a76f76ee3c2ec9f612f06126ddd2dc076885b90b35edac2429a06d8a3"
    );
}
