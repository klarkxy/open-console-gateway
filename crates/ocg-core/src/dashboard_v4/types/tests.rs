use super::*;
use crate::dashboard_v3::{ControlRevision, MutationExpectation, ProviderDefinitionAuthKind};
use serde_json::json;

#[test]
fn wire_fields_are_camel_case() {
    let list = TemplateList {
        templates: vec![ProviderTemplate {
            id: "custom-http".into(),
            version: 1,
            display_name: "Custom HTTP".into(),
            family_id: None,
            offering_tags: vec![OfferingKind::Api],
            adapter_kind: "configurable_http".into(),
            source: TemplateSource::Builtin,
            credential_kind: AccountCredentialKind::ApiKey,
            auth_schemes: vec![AccountAuthScheme::Bearer],
            upstream_protocols: vec![AccountUpstreamProtocol::ChatCompletions],
            editable_fields: vec!["name".into()],
            default_endpoints: vec![EndpointSpec {
                operation: EndpointOperation::ChatCreate,
                wire_protocol: AccountUpstreamProtocol::ChatCompletions,
                url: None,
                locked: false,
            }],
            pricing_multiplier_editable: false,
        }],
    };
    let value = serde_json::to_value(&list).unwrap();
    assert_eq!(value["templates"][0]["displayName"], "Custom HTTP");
    assert_eq!(value["templates"][0]["pricingMultiplierEditable"], false);
    assert_eq!(value["templates"][0]["familyId"], Value::Null);
    assert_eq!(value["templates"][0]["offeringTags"], json!(["api"]));
    assert_eq!(value["templates"][0]["adapterKind"], "configurable_http");
    assert_eq!(value["templates"][0]["credentialKind"], "api_key");
    assert_eq!(value["templates"][0]["editableFields"], json!(["name"]));
    assert_eq!(
        value["templates"][0]["defaultEndpoints"][0]["wireProtocol"],
        "chat_completions"
    );
    assert_eq!(
        value["templates"][0]["defaultEndpoints"][0]["url"],
        Value::Null
    );
}

#[test]
fn connection_summary_emits_null_optional_fields() {
    let summary = ConnectionSummary {
        id: "id".into(),
        name: "Lab".into(),
        origin: ConnectionOrigin::Custom,
        template_ref: None,
        adapter_kind: "configurable_http".into(),
        lifecycle: ConnectionLifecycle::Configured,
        authorization: AuthorizationState::Unknown,
        eligibility: Eligibility {
            state: EligibilityState::Eligible,
            reason: EligibilityReason::None,
        },
        credential_count: 1,
        enabled_credential_count: 1,
        target_count: 0,
        endpoints: Vec::new(),
        targets: Vec::new(),
        legacy: LegacyIdentity {
            kind: LegacyConnectionKind::DynamicProvider,
            id: "legacy".into(),
        },
        display_family: None,
        offering: OfferingKind::Api,
    };
    let value = serde_json::to_value(&summary).unwrap();
    assert_eq!(value["templateRef"], Value::Null);
    assert_eq!(value["displayFamily"], Value::Null);
    assert_eq!(value["authorization"], "unknown");
    assert_eq!(value["eligibility"]["reason"], "none");
    assert_eq!(value["legacy"]["kind"], "dynamic_provider");
}

#[test]
fn schema_catalog_names_v4_types_and_shared_error() {
    let schema = contract_schema();
    assert_eq!(schema["title"], "DashboardApiV4");
    let defs = schema["$defs"].as_object().expect("catalog $defs");
    for name in CATALOG_TYPE_NAMES {
        assert!(defs.contains_key(*name), "missing $defs/{name}");
    }
    let required_error = defs["V3Error"]["required"]
        .as_array()
        .expect("V3Error.required");
    for field in ["code", "message", "currentRevision", "processGeneration"] {
        assert!(
            required_error.iter().any(|value| value == field),
            "{field} must stay required so responses emit T|null"
        );
    }
    assert!(defs.contains_key("RoutingExplanation"));
}

#[test]
fn routing_explanation_emits_camel_case_and_null_optionals() {
    let explanation = RoutingExplanation {
        requested_model: "glm-5.2".into(),
        client_protocol: RoutingClientProtocol::ChatCompletions,
        resolved: RoutingResolvedModel {
            kind: RoutingResolvedKind::PinnedRaw,
            alias: None,
            mappings: vec![RoutingResolvedMapping {
                provider_id: "opencode".into(),
                upstream_model: "glm-5.2".into(),
                routeable: true,
            }],
        },
        revision: ControlRevision {
            revision: 1,
            process_generation: 2,
            pricing_revision: "p".into(),
        },
        observed_at: "2024-01-02T03:04:05Z".into(),
        routing_mode: RoutingMode::StrictPriority,
        conversation_sticky: true,
        conversation_binding: RoutingConversationBinding::NotEvaluated,
        eligible: Vec::new(),
        exclusions: vec![RoutingExclusion {
            code: RoutingExclusionCode::AccountDisabled,
            detail: "account `a` is disabled".into(),
            account_id: Some("a".into()),
            provider_id: Some("opencode".into()),
            upstream_model: None,
        }],
        expected_base_policy_first_pick: None,
        runtime_only_uncertainty: vec![RuntimeOnlyUncertainty::UpstreamResultUnknown],
    };
    let value = serde_json::to_value(&explanation).unwrap();
    assert_eq!(value["requestedModel"], "glm-5.2");
    assert_eq!(value["clientProtocol"], "chat_completions");
    assert_eq!(value["resolved"]["kind"], "pinned_raw");
    assert_eq!(value["resolved"]["alias"], Value::Null);
    assert_eq!(value["routingMode"], "strict-priority");
    assert_eq!(value["conversationBinding"], "not_evaluated");
    assert_eq!(value["expectedBasePolicyFirstPick"], Value::Null);
    assert_eq!(value["exclusions"][0]["accountId"], "a");
    assert_eq!(value["exclusions"][0]["upstreamModel"], Value::Null);
    assert_eq!(value["exclusions"][0]["code"], "account_disabled");
    assert_eq!(
        value["runtimeOnlyUncertainty"],
        json!(["upstream_result_unknown"])
    );
}

#[test]
fn quota_recovery_dto_is_camel_case_and_nullable_on_credentials() {
    let recovery = QuotaRecoveryDto {
        status: QuotaRecoveryStatus::Waiting,
        reason: QuotaRecoveryReason::QuotaExhausted,
        window: QuotaRecoveryWindow::FiveHours,
        observed_at: "2026-09-20T00:00:00Z".into(),
        resets_at: None,
        next_retry_at: "2026-09-20T00:15:00Z".into(),
        failure_count: 1,
    };
    let value = serde_json::to_value(&recovery).unwrap();
    assert_eq!(value["status"], "waiting");
    assert_eq!(value["reason"], "quota_exhausted");
    assert_eq!(value["window"], "five_hours");
    assert_eq!(value["observedAt"], "2026-09-20T00:00:00Z");
    assert_eq!(value["resetsAt"], Value::Null);
    assert_eq!(value["nextRetryAt"], "2026-09-20T00:15:00Z");
    assert_eq!(value["failureCount"], 1);
    let schema = contract_schema();
    let defs = schema["$defs"].as_object().unwrap();
    for name in [
        "QuotaRecoveryDto",
        "QuotaRecoveryStatus",
        "QuotaRecoveryReason",
        "QuotaRecoveryWindow",
        "QuotaRetryResult",
    ] {
        assert!(defs.contains_key(name), "missing $defs/{name}");
    }
    let required = defs["DestinationCredentialDto"]["required"]
        .as_array()
        .expect("DestinationCredentialDto.required");
    assert!(
        !required.iter().any(|value| value == "quotaRecovery"),
        "quotaRecovery must be optional so older listings may omit it"
    );
}

#[test]
fn quota_recovery_field_may_be_omitted_null_or_object() {
    let mut body = json!({
        "id": "cred-1",
        "legacyAccountId": "acct-1",
        "destinationId": "dest-1",
        "name": "Key",
        "notes": null,
        "hasSecret": true,
        "enabled": true,
        "routingRank": 0,
        "scope": { "kind": "all" },
        "grants": { "allowedEndpointIds": [], "allowedOrigins": [] },
        "authState": "unknown",
        "lastError": null,
        "cooldowns": {
            "genericUntil": null,
            "fiveHourUntil": null,
            "weekUntil": null,
            "monthUntil": null,
            "freeUntil": null
        },
        "quotaPoolId": null,
        "onboardingTask": null,
        "purchaseDate": null
    });
    let omitted: DestinationCredentialDto = serde_json::from_value(body.clone()).unwrap();
    assert!(omitted.quota_recovery.is_none());
    let omitted_json = serde_json::to_value(&omitted).unwrap();
    assert!(omitted_json.get("quotaRecovery").is_none());

    body["quotaRecovery"] = Value::Null;
    let null_field: DestinationCredentialDto = serde_json::from_value(body.clone()).unwrap();
    assert!(null_field.quota_recovery.is_none());
    assert!(
        serde_json::to_value(&null_field)
            .unwrap()
            .get("quotaRecovery")
            .is_none()
    );

    body["quotaRecovery"] = json!({
        "status": "ready",
        "reason": "insufficient_balance",
        "window": "unknown",
        "observedAt": "2026-09-20T00:00:00Z",
        "resetsAt": null,
        "nextRetryAt": "2026-09-20T00:15:00Z",
        "failureCount": 2
    });
    let present: DestinationCredentialDto = serde_json::from_value(body).unwrap();
    let recovery = present.quota_recovery.as_ref().expect("object present");
    assert_eq!(recovery.status, QuotaRecoveryStatus::Ready);
    assert_eq!(recovery.reason, QuotaRecoveryReason::InsufficientBalance);
    assert_eq!(recovery.failure_count, 2);
    let present_json = serde_json::to_value(&present).unwrap();
    assert_eq!(present_json["quotaRecovery"]["status"], "ready");
    assert_eq!(present_json["quotaRecovery"]["resetsAt"], Value::Null);
}

#[test]
fn onboarding_commit_request_is_camel_case_and_includes_secret_in_canonical_json() {
    let request = OnboardingCommitRequest {
        expectation: MutationExpectation {
            expected_revision: 3,
            process_generation: 9,
        },
        operation_id: "11111111-1111-1111-1111-111111111111".into(),
        connection: OnboardingConnection::New(OnboardingConnectionNew {
            template_id: "custom-http".into(),
            name: "Lab".into(),
            endpoint_url: "https://lab.example/v1/chat/completions".into(),
            upstream_protocol: AccountUpstreamProtocol::ChatCompletions,
            auth_kind: ProviderDefinitionAuthKind::Bearer,
        }),
        authorization: Some(OnboardingAuthorization::ApiKey(
            OnboardingAuthorizationApiKey {
                secret_input: "sk-canonical".into(),
                account_label: Some("Primary".into()),
                notes: None,
            },
        )),
        targets: vec![OnboardingTarget {
            public_model: "lab-opus".into(),
            upstream_model: "vendor/opus".into(),
            upstream_override: None,
        }],
        mode: None,
        authorize_current_endpoint: false,
    };
    let value = serde_json::to_value(&request).unwrap();
    assert_eq!(value["expectedRevision"], 3);
    assert_eq!(value["processGeneration"], 9);
    assert_eq!(value["operationId"], "11111111-1111-1111-1111-111111111111");
    assert_eq!(value["connection"]["kind"], "new");
    assert_eq!(value["connection"]["templateId"], "custom-http");
    assert_eq!(
        value["connection"]["endpointUrl"],
        "https://lab.example/v1/chat/completions"
    );
    assert_eq!(value["authorization"]["kind"], "api_key");
    assert_eq!(value["authorization"]["secretInput"], "sk-canonical");
    assert_eq!(value["authorization"]["accountLabel"], "Primary");
    assert_eq!(value["authorization"]["notes"], Value::Null);
    assert_eq!(value["targets"][0]["publicModel"], "lab-opus");
    assert_eq!(value["targets"][0]["upstreamOverride"], Value::Null);
    let result = OnboardingCommitResult {
        revision: ControlRevision {
            revision: 4,
            process_generation: 9,
            pricing_revision: "p".into(),
        },
        connection_id: "conn".into(),
        credential_id: None,
        target_ids: vec![],
        replayed: false,
        account_id: None,
    };
    let result_value = serde_json::to_value(&result).unwrap();
    assert_eq!(result_value["connectionId"], "conn");
    assert_eq!(result_value["credentialId"], Value::Null);
    assert_eq!(result_value["targetIds"], json!([]));
    assert_eq!(result_value["replayed"], false);
    assert_eq!(result_value["accountId"], Value::Null);
}

#[test]
fn identity_list_emits_camel_case_and_null_unknowns() {
    let list = IdentityList {
        revision: ControlRevision {
            revision: 1,
            process_generation: 2,
            pricing_revision: "p".into(),
        },
        identities: vec![IdentitySummary {
            identity: UpstreamAccountDto {
                id: "id".into(),
                label: "Lab".into(),
                authority_ref: None,
                identity_confidence: ocg_domain::credential::IdentityConfidence::Opaque,
                enabled: true,
                notes: None,
            },
            credentials: vec![CredentialSummary {
                credential: CredentialDto {
                    id: "cred".into(),
                    purpose: ocg_domain::credential::CredentialPurpose::Inference,
                    material_kind: ocg_domain::credential::MaterialKind::ApiKey,
                    secret_ref: "account:a".into(),
                    has_material: true,
                    version: 1,
                    enabled: true,
                    auth_state: ocg_domain::credential::AuthState::Unknown,
                    auth_state_version: 1,
                    expires_at: None,
                },
                subject: ocg_domain::credential::RuntimeSubjectKind::AccountCredential,
                bindings: Vec::new(),
                quota_windows: Vec::new(),
                quota_pool_id: None,
                onboarding_task: None,
                subscription: None,
                last_error: None,
                legacy: IdentityLegacy {
                    kind: IdentityLegacyKind::Account,
                    id: "a".into(),
                },
            }],
            declared_relations: Vec::new(),
            legacy: IdentityLegacy {
                kind: IdentityLegacyKind::Account,
                id: "a".into(),
            },
        }],
    };
    let value = serde_json::to_value(&list).unwrap();
    assert_eq!(
        value["identities"][0]["identity"]["authorityRef"],
        Value::Null
    );
    assert_eq!(
        value["identities"][0]["credentials"][0]["subscription"],
        Value::Null
    );
    assert_eq!(
        value["identities"][0]["credentials"][0]["quotaPoolId"],
        Value::Null
    );
    assert_eq!(
        value["identities"][0]["credentials"][0]["credential"]["expiresAt"],
        Value::Null
    );
    assert_eq!(
        value["identities"][0]["credentials"][0]["credential"]["secretRef"],
        "account:a"
    );
    assert_eq!(value["identities"][0]["legacy"]["kind"], "account");
}

#[test]
fn credential_rotate_request_is_camel_case_and_result_is_secret_free() {
    let request = CredentialRotateRequest {
        expectation: MutationExpectation {
            expected_revision: 3,
            process_generation: 9,
        },
        secret_input: "sk-rotate".into(),
    };
    let value = serde_json::to_value(&request).unwrap();
    assert_eq!(value["expectedRevision"], 3);
    assert_eq!(value["processGeneration"], 9);
    assert_eq!(value["secretInput"], "sk-rotate");
    assert!(value.get("operationId").is_none());
    let result = CredentialRotateResult {
        revision: ControlRevision {
            revision: 4,
            process_generation: 9,
            pricing_revision: "p".into(),
        },
        credential_id: "cred".into(),
        version: 2,
        auth_state_version: 3,
        replayed: false,
    };
    let result_value = serde_json::to_value(&result).unwrap();
    assert_eq!(result_value["credentialId"], "cred");
    assert_eq!(result_value["version"], 2);
    assert_eq!(result_value["authStateVersion"], 3);
    assert_eq!(result_value["replayed"], false);
    assert!(result_value.get("secretInput").is_none());
}

#[test]
fn binding_patch_and_identity_credential_create_are_camel_case() {
    let request = BindingPatchRequest {
        expectation: MutationExpectation {
            expected_revision: 3,
            process_generation: 9,
        },
        model_scope: Some(ocg_domain::credential::ModelScope::Only {
            models: vec!["glm-5.2".into()],
        }),
        enabled: Some(false),
        allowed_endpoint_ids: None,
        allowed_origins: None,
    };
    let value = serde_json::to_value(&request).unwrap();
    assert_eq!(value["expectedRevision"], 3);
    assert_eq!(value["modelScope"]["kind"], "only");
    assert_eq!(value["enabled"], false);
    assert!(value.get("operationId").is_none());

    let create = IdentityCredentialCreateRequest {
        expectation: MutationExpectation {
            expected_revision: 3,
            process_generation: 9,
        },
        connection_id: "conn".into(),
        secret_input: "sk-second".into(),
        operation_id: None,
        quota_sharing: QuotaSharing::Independent,
        account_label: None,
    };
    let create_value = serde_json::to_value(&create).unwrap();
    assert_eq!(create_value["connectionId"], "conn");
    assert_eq!(create_value["secretInput"], "sk-second");
    assert!(create_value.get("operationId").is_none());
    assert!(create_value.get("quotaSharing").is_none());
    assert!(create_value.get("accountLabel").is_none());
    let omitted: IdentityCredentialCreateRequest = serde_json::from_value(json!({
        "expectedRevision": 3,
        "processGeneration": 9,
        "connectionId": "conn",
        "secretInput": "sk-second"
    }))
    .unwrap();
    assert_eq!(omitted.quota_sharing, QuotaSharing::Independent);
    let shared: IdentityCredentialCreateRequest = serde_json::from_value(json!({
        "expectedRevision": 3,
        "processGeneration": 9,
        "connectionId": "conn",
        "secretInput": "sk-second",
        "quotaSharing": { "kind": "shared", "credentialId": "cred-1" },
        "accountLabel": "Key B",
        "operationId": "00000000-0000-4000-8000-000000000001"
    }))
    .unwrap();
    assert_eq!(
        shared.quota_sharing,
        QuotaSharing::Shared {
            credential_id: "cred-1".into()
        }
    );
    assert_eq!(shared.account_label.as_deref(), Some("Key B"));

    let result = IdentityCredentialCreateResult {
        revision: ControlRevision {
            revision: 4,
            process_generation: 9,
            pricing_revision: "p".into(),
        },
        identity_id: "id".into(),
        credential_id: "cred".into(),
        binding_id: "bind".into(),
        account_id: "acct".into(),
        connection_id: "conn".into(),
        version: 1,
        auth_state_version: 1,
        replayed: false,
    };
    let result_value = serde_json::to_value(&result).unwrap();
    assert_eq!(result_value["identityId"], "id");
    assert_eq!(result_value["credentialId"], "cred");
    assert_eq!(result_value["bindingId"], "bind");
    assert_eq!(result_value["accountId"], "acct");
    assert!(result_value.get("secretInput").is_none());
}

#[test]
fn cpa_catalog_is_camel_case() {
    let catalog = CpaCatalog {
        revision: ControlRevision {
            revision: 3,
            process_generation: 1,
            pricing_revision: "p".into(),
        },
        models: vec![CpaCatalogEntry {
            id: "gpt-5".into(),
            owned_by: Some("openai".into()),
            enabled: true,
        }],
        source_url: Some("http://127.0.0.1:8317".into()),
        refreshed_at: Some("2026-09-12T00:00:00Z".into()),
    };
    let value = serde_json::to_value(&catalog).unwrap();
    assert_eq!(value["models"][0]["ownedBy"], "openai");
    assert_eq!(value["models"][0]["enabled"], true);
    assert_eq!(value["sourceUrl"], "http://127.0.0.1:8317");
    let update = CpaCatalogUpdate {
        expectation: MutationExpectation {
            expected_revision: 3,
            process_generation: 1,
        },
        enabled_ids: vec!["gpt-5".into()],
    };
    let update_value = serde_json::to_value(&update).unwrap();
    assert_eq!(update_value["enabledIds"], json!(["gpt-5"]));
    assert_eq!(update_value["expectedRevision"], 3);
}

#[test]
fn catalog_models_remove_request_is_camel_case() {
    let request = CatalogModelsRemoveRequest {
        expectation: MutationExpectation {
            expected_revision: 4,
            process_generation: 2,
        },
        model_ids: vec!["drop-me".into()],
    };
    let value = serde_json::to_value(&request).unwrap();
    assert_eq!(value["modelIds"], json!(["drop-me"]));
    assert_eq!(value["expectedRevision"], 4);
    assert_eq!(value["processGeneration"], 2);
    let result = CatalogModelsRemoveResult {
        revision: ControlRevision {
            revision: 5,
            process_generation: 2,
            pricing_revision: "p".into(),
        },
        removed_ids: vec!["drop-me".into()],
        catalog_models: vec!["keep-me".into()],
    };
    let result_value = serde_json::to_value(&result).unwrap();
    assert_eq!(result_value["removedIds"], json!(["drop-me"]));
    assert_eq!(result_value["catalogModels"], json!(["keep-me"]));
}

#[test]
fn alias_publication_is_camel_case() {
    let publication = AliasPublication {
        revision: ControlRevision {
            revision: 6,
            process_generation: 2,
            pricing_revision: "p".into(),
        },
        unpublished: vec!["deepseek-v4-flashnh".into()],
    };
    let value = serde_json::to_value(&publication).unwrap();
    assert_eq!(value["unpublished"], json!(["deepseek-v4-flashnh"]));
    assert_eq!(value["revision"]["revision"], 6);
    let update = AliasPublicationUpdate {
        expectation: MutationExpectation {
            expected_revision: 6,
            process_generation: 2,
        },
        public_model: "DeepSeek-V4-FlashNH".into(),
        published: false,
    };
    let update_value = serde_json::to_value(&update).unwrap();
    assert_eq!(update_value["publicModel"], "DeepSeek-V4-FlashNH");
    assert_eq!(update_value["published"], false);
    assert_eq!(update_value["expectedRevision"], 6);
}

#[test]
fn dsh_application_contract_is_camel_case_and_secret_free() {
    let application = DshApplication {
        status: DshApplicationStatus::Ready,
        detected: true,
        installed: false,
        install_supported: true,
        activation_required: false,
        version: Some("0.1.5-rc.2".into()),
        detail: Some("ready".into()),
        target_paths: vec!["DSH web profile".into()],
        fingerprint: Some("abc".into()),
        revision: ControlRevision {
            revision: 7,
            process_generation: 2,
            pricing_revision: "p".into(),
        },
    };
    let value = serde_json::to_value(&application).unwrap();
    assert_eq!(value["installSupported"], true);
    assert_eq!(value["activationRequired"], false);
    assert_eq!(value["targetPaths"], json!(["DSH web profile"]));
    assert_eq!(value["status"], "ready");
    assert!(value.get("key").is_none());

    let request: DshApplicationInstallRequest = serde_json::from_value(json!({
        "expectedRevision": 7,
        "processGeneration": 2,
        "keyId": "primary",
        "expectedFingerprint": "abc"
    }))
    .unwrap();
    assert_eq!(request.key_id, "primary");
    assert_eq!(request.expected_fingerprint, "abc");
    assert_eq!(request.expectation.expected_revision, 7);
}
