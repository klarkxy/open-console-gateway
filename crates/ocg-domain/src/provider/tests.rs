use super::*;
use crate::catalog::{
    CredentialKind, OPENCODE_GO_USAGE_URL, QuotaScope, UpstreamAuthScheme, UpstreamProtocolKind,
};
use crate::ids::{
    COMMAND_CODE_PROVIDER_ID, CPA_ACCOUNT_ID, CPA_PROVIDER_ID, CUSTOM_PROVIDER_ID,
    KIMI_PROVIDER_ID, MINIMAX_PROVIDER_ID, OLLAMA_PROVIDER_ID, OPENCODE_PROVIDER_ID,
    OPENCODE_ZEN_FREE_PROVIDER_ID, ZEN_FREE_ACCOUNT_ID,
};

#[test]
fn catalog_parse_errors_map_to_provider_binding_errors() {
    assert!(matches!(
        ProviderBindingError::from(CredentialKind::try_from("cookie").unwrap_err()),
        ProviderBindingError::UnknownCredentialKind(value) if value == "cookie"
    ));
    assert!(matches!(
        ProviderBindingError::from(QuotaScope::try_from("account").unwrap_err()),
        ProviderBindingError::UnknownQuotaScope(value) if value == "account"
    ));
    assert!(matches!(
        ProviderBindingError::from(UpstreamProtocolKind::try_from("gemini").unwrap_err()),
        ProviderBindingError::UnknownUpstreamProtocol(value) if value == "gemini"
    ));
    assert!(matches!(
        ProviderBindingError::from(UpstreamAuthScheme::try_from("basic").unwrap_err()),
        ProviderBindingError::UnknownAuthScheme(value) if value == "basic"
    ));
}

#[test]
fn builtin_providers_derive_credential_and_quota_scope() {
    let goat = builtin_provider(COMMAND_CODE_PROVIDER_ID).unwrap();
    assert_eq!(goat.credential_kind, CredentialKind::ApiKey);
    assert_eq!(goat.quota_scope, QuotaScope::Key);

    let free = builtin_provider(OPENCODE_ZEN_FREE_PROVIDER_ID).unwrap();
    assert_eq!(free.credential_kind, CredentialKind::None);
    assert_eq!(free.quota_scope, QuotaScope::EgressIp);
    assert_eq!(free.singleton_account_id, Some(ZEN_FREE_ACCOUNT_ID));

    let cpa = builtin_provider(CPA_PROVIDER_ID).unwrap();
    assert_eq!(cpa.credential_kind, CredentialKind::ApiKey);
    assert_eq!(cpa.quota_scope, QuotaScope::Key);
    assert_eq!(cpa.singleton_account_id, Some(CPA_ACCOUNT_ID));
}

#[test]
fn goat_included_model_set_is_exact_unique_and_mode_gated() {
    let mut unique = std::collections::HashSet::new();
    for model in COMMAND_CODE_GOAT_INCLUDED_MODEL_IDS {
        assert!(
            unique.insert(model.to_ascii_lowercase()),
            "duplicate GOAT model {model}"
        );
    }
    assert!(command_code_goat_includes_model(
        "deepseek/deepseek-v4-flash"
    ));
    assert!(command_code_goat_includes_model("XAI/GROK-4.6"));
    assert!(!command_code_goat_includes_model(
        "anthropic/claude-opus-4.1"
    ));
}

#[test]
fn singleton_and_provider_validation_is_fail_closed() {
    assert!(
        validate_account_binding(
            "account-1",
            OPENCODE_PROVIDER_ID,
            CredentialKind::ApiKey,
            QuotaScope::Key,
        )
        .is_ok()
    );
    assert!(
        validate_account_binding(
            "account-1",
            OPENCODE_ZEN_FREE_PROVIDER_ID,
            CredentialKind::None,
            QuotaScope::EgressIp,
        )
        .is_err()
    );
    assert!(
        validate_account_binding(
            CPA_ACCOUNT_ID,
            CPA_PROVIDER_ID,
            CredentialKind::ApiKey,
            QuotaScope::Key,
        )
        .is_ok()
    );
    assert!(
        validate_account_binding(
            "account-1",
            CPA_PROVIDER_ID,
            CredentialKind::ApiKey,
            QuotaScope::Key,
        )
        .is_err()
    );
    assert!(
        validate_account_binding(
            ZEN_FREE_ACCOUNT_ID,
            OPENCODE_PROVIDER_ID,
            CredentialKind::ApiKey,
            QuotaScope::Key,
        )
        .is_err()
    );
    assert!(
        validate_account_binding(
            "account-1",
            "unknown-provider",
            CredentialKind::ApiKey,
            QuotaScope::Key,
        )
        .is_err()
    );
}

#[test]
fn catalog_hardcodes_providers_and_keeps_unverified_providers_unroutable() {
    let goat = builtin_provider(COMMAND_CODE_PROVIDER_ID).unwrap();
    assert!(goat.routable);
    assert_eq!(goat.verification_policy, VerificationPolicy::NotRequired);
    assert_eq!(goat.verification_runtime_availability, "not_applicable");
    assert_eq!(goat.creation_availability, CreationAvailability::Available);
    assert_eq!(goat.pricing_availability, "available");
    assert_eq!(goat.usage_availability, "available");
    assert!(goat.manual_usage_calibration);
    assert_eq!(goat.auth_schemes, &BEARER_AUTH);
    assert_eq!(goat.upstream_protocols, &GOAT_PROTOCOLS);
    assert!(
        !goat
            .upstream_protocols
            .contains(&UpstreamProtocolKind::Responses)
    );
    assert_eq!(goat.model_source, COMMAND_CODE_GOAT_MODEL_SOURCE);
    assert!(is_command_code_goat(COMMAND_CODE_PROVIDER_ID));
    assert!(!is_command_code_goat(OPENCODE_PROVIDER_ID));

    let custom = builtin_provider(CUSTOM_PROVIDER_ID).unwrap();
    assert!(custom.routable);
    assert_eq!(custom.verification_runtime_availability, "available");
    assert_eq!(custom.verification_policy, VerificationPolicy::Required);
    assert_eq!(custom.pricing_availability, "unpriced");
    assert_eq!(custom.usage_availability, "unavailable");
    assert!(plan_requires_custom_config(custom));
    assert!(is_custom_api(CUSTOM_PROVIDER_ID));
    assert!(!is_custom_api(OPENCODE_PROVIDER_ID));

    let cpa = builtin_provider(CPA_PROVIDER_ID).unwrap();
    assert!(cpa.routable);
    assert_eq!(
        cpa.product_surface,
        ProviderProductSurface::ExternalIntegration
    );
    assert!(cpa.product_surface.is_external_integration());
    assert_eq!(cpa.creation_availability, CreationAvailability::Unavailable);
    assert_eq!(cpa.singleton_account_id, Some(CPA_ACCOUNT_ID));
    assert_eq!(cpa.auth_schemes, &BEARER_AUTH);
    assert_eq!(cpa.upstream_protocols, &CUSTOM_PROTOCOLS);
    assert!(cpa.form_fields.is_empty());
    assert!(is_cpa_external_integration(CPA_PROVIDER_ID));
    assert!(!is_cpa_external_integration(OPENCODE_PROVIDER_ID));

    let go = builtin_provider(OPENCODE_PROVIDER_ID).unwrap();
    assert!(go.routable);
    assert_eq!(
        default_verification_status(go),
        ConnectionVerificationStatus::NotRequired
    );

    for provider_id in [
        OPENCODE_PROVIDER_ID,
        COMMAND_CODE_PROVIDER_ID,
        MINIMAX_PROVIDER_ID,
        KIMI_PROVIDER_ID,
        OLLAMA_PROVIDER_ID,
    ] {
        let plan = builtin_provider(provider_id).unwrap();
        assert!(
            plan.form_fields
                .iter()
                .any(|field| field.id == "purchase_date"),
            "{provider_id} must collect its subscription purchase date"
        );
    }
    assert!(
        !custom
            .form_fields
            .iter()
            .any(|field| field.id == "purchase_date")
    );
}

#[test]
fn ollama_cloud_exposes_priced_local_month_credits_and_billing_form() {
    let plan = builtin_provider(OLLAMA_PROVIDER_ID).unwrap();
    assert_eq!(plan.pricing_availability, "available");
    assert_eq!(plan.usage_availability, "local_state");
    assert!(plan.manual_usage_calibration);
    assert_eq!(plan.quota_unit, "usd_credits");
    assert!(
        plan.form_fields
            .iter()
            .any(|field| field.id == "ollama_billing_tier" && field.required)
    );
    assert!(
        plan.form_fields
            .iter()
            .any(|field| field.id == "purchase_date" && field.required)
    );
    let capabilities = ProviderRegistry::get(OLLAMA_PROVIDER_ID).unwrap();
    assert!(!capabilities.card_actions.usage_refresh);
    assert!(capabilities.card_actions.manual_usage_calibration);
    assert!(capabilities.usage.manual_calibration);
    assert_eq!(OllamaBillingTier::Pro.monthly_credit_limit(), 60.0);
    assert_eq!(OllamaBillingTier::Max.monthly_credit_limit(), 300.0);
    assert_eq!(OllamaBillingTier::Team.monthly_credit_limit(), 1000.0);
    assert!(OllamaBillingTier::Pro.requires_purchase_date());
    assert_eq!(
        OllamaBillingTier::parse("pro").unwrap(),
        OllamaBillingTier::Pro
    );
    assert!(OllamaBillingTier::parse("free").is_err());
    assert!(OllamaBillingTier::parse("unconfigured").is_err());
    assert!(OllamaBillingTier::parse("starter").is_err());
}

#[test]
fn catalog_enablement_gate_is_fail_closed_for_unroutable_plans() {
    for plan in BUILTIN_PROVIDERS {
        let provider_id = plan.provider_id;
        assert_eq!(plan_allows_enablement(plan), plan.routable, "{provider_id}");
        assert_eq!(
            provider_allows_enablement(provider_id),
            plan.routable,
            "{provider_id}"
        );
        assert!(
            ensure_enabled_provider_is_routable(provider_id, false).is_ok(),
            "disabled drafts must stay writable: {provider_id}"
        );
        let enabled = ensure_enabled_provider_is_routable(provider_id, true);
        if plan.routable {
            enabled.expect("routable providers may enable");
            ensure_provider_can_enable(provider_id).unwrap();
        } else {
            let error = enabled.expect_err("unroutable providers must reject enabled=true");
            assert!(
                matches!(
                    error,
                    ProviderBindingError::EnablementNotRoutable {
                        provider_id: rejected_provider,
                        display_name,
                    } if rejected_provider == provider_id
                        && display_name == plan.display_name
                ),
                "{error:?}"
            );
        }
    }
    assert!(!provider_allows_enablement("unknown-provider"));
    assert!(matches!(
        ensure_provider_can_enable("unknown-provider"),
        Err(ProviderBindingError::UnknownProvider { .. })
    ));
}

#[test]
fn custom_model_ids_stay_stable() {
    assert_eq!(
        validate_custom_model_id("deepseek/deepseek-v4-flash").unwrap(),
        "deepseek/deepseek-v4-flash"
    );
    assert_eq!(validate_custom_model_id("  glm-5.2  ").unwrap(), "glm-5.2");
    assert!(matches!(
        validate_custom_model_id(""),
        Err(ProviderBindingError::InvalidModelId(message)) if message == "model id is required"
    ));
    assert!(matches!(
        validate_custom_model_id("   "),
        Err(ProviderBindingError::InvalidModelId(message)) if message == "model id is required"
    ));
    assert!(matches!(
        validate_custom_model_id(&"a".repeat(201)),
        Err(ProviderBindingError::InvalidModelId(message)) if message == "model id is too long"
    ));
    assert_eq!(
        validate_custom_model_id(&"a".repeat(200)).unwrap().len(),
        200
    );
    assert!(matches!(
        validate_custom_model_id("bad\0id"),
        Err(ProviderBindingError::InvalidModelId(message))
            if message == "model id must not contain control characters"
    ));
    assert!(matches!(
        validate_custom_model_id("bad\nid"),
        Err(ProviderBindingError::InvalidModelId(message))
            if message == "model id must not contain control characters"
    ));
}

#[test]
fn provider_registry_is_exhaustive_for_plans_and_adapter_kinds() {
    let mut seen = std::collections::HashSet::new();
    assert_eq!(ProviderRegistry::iter().count(), BUILTIN_PROVIDERS.len());
    for plan in BUILTIN_PROVIDERS {
        let kind = ProviderAdapterKind::from_provider_id(plan.provider_id)
            .expect("every catalog plan has an adapter kind");
        seen.insert(kind);
        let descriptor = ProviderRegistry::get(plan.provider_id)
            .expect("every catalog plan has a composed descriptor");
        assert_eq!(descriptor.kind, kind);
        assert_eq!(descriptor.provider_id, plan.provider_id);
        assert_eq!(descriptor.inference.catalog_routable, plan.routable);
        assert_eq!(descriptor.inference.credential_kind, plan.credential_kind);
        assert_eq!(descriptor.inference.quota_scope, plan.quota_scope);
        assert_eq!(descriptor.verification.policy, plan.verification_policy);
        assert_eq!(
            descriptor.verification.runtime_availability,
            plan.verification_runtime_availability
        );
        assert_eq!(descriptor.pricing.availability, plan.pricing_availability);
        assert_eq!(
            descriptor.usage.catalog_availability,
            plan.usage_availability
        );
        assert_eq!(
            descriptor.usage.manual_calibration,
            plan.manual_usage_calibration
        );
        assert_eq!(descriptor.model_catalog.catalog_source, plan.model_source);
        assert_eq!(
            descriptor.card_actions.managed_registration,
            plan.managed_registration
        );
        assert_eq!(
            descriptor.card_actions.persisted_enable_allowed,
            plan.routable
        );
        assert!(!descriptor.protocol_probe.request_path_may_trial);
        assert!(!descriptor.protocol_probe.fallback_priority.is_empty());
        assert_eq!(
            descriptor.protocol_probe.explicit_probe,
            descriptor.card_actions.protocol_probe
        );
        assert_eq!(
            descriptor.card_actions.catalog_refresh,
            matches!(
                kind,
                ProviderAdapterKind::OpenCodeGo
                    | ProviderAdapterKind::ZenFree
                    | ProviderAdapterKind::CommandCodeGoat
                    | ProviderAdapterKind::MiniMaxCn
                    | ProviderAdapterKind::KimiCn
                    | ProviderAdapterKind::OllamaCloud
                    | ProviderAdapterKind::Cpa
            )
        );
        assert_eq!(
            descriptor.card_actions.protocol_probe,
            matches!(
                kind,
                ProviderAdapterKind::OpenCodeGo
                    | ProviderAdapterKind::ZenFree
                    | ProviderAdapterKind::CommandCodeGoat
                    | ProviderAdapterKind::MiniMaxCn
                    | ProviderAdapterKind::KimiCn
            )
        );
        assert_eq!(
            descriptor.verification.uses_get_models,
            kind == ProviderAdapterKind::Cpa
        );
        assert_eq!(
            descriptor.usage.egress_ip_shared_cooldown_window,
            kind == ProviderAdapterKind::ZenFree
        );
        match kind {
            ProviderAdapterKind::OpenCodeGo
            | ProviderAdapterKind::ZenFree
            | ProviderAdapterKind::CommandCodeGoat
            | ProviderAdapterKind::MiniMaxCn
            | ProviderAdapterKind::KimiCn
            | ProviderAdapterKind::OllamaCloud
            | ProviderAdapterKind::ConfigurableHttp
            | ProviderAdapterKind::Cpa => {
                assert!(descriptor.inference.production_inference);
                assert!(descriptor.inference.catalog_routable);
            }
        }
    }
    for kind in ProviderAdapterKind::ALL {
        assert!(
            seen.contains(&kind),
            "{kind:?} must be wired to at least one catalog provider"
        );
    }
    assert_eq!(seen.len(), ProviderAdapterKind::ALL.len());
    assert!(ProviderAdapterKind::from_provider_id("unknown").is_none());
    assert!(ProviderRegistry::get("unknown").is_none());
    assert_eq!(ProviderAdapterKind::ALL.len(), 8);
}

#[test]
fn adapter_descriptors_preserve_current_capability_decisions() {
    let go = ProviderRegistry::get(OPENCODE_PROVIDER_ID).unwrap();
    assert_eq!(go.kind, ProviderAdapterKind::OpenCodeGo);
    assert_eq!(
        go.inference.auth,
        InferenceAuthDescriptor::OpenCodeProtocolDefault
    );
    assert!(go.inference.follow_redirects);
    assert_eq!(go.inference.origin, InferenceOriginKind::OfficialFixed);
    assert!(OPENCODE_GO_USAGE_URL.starts_with(OPENCODE_GO_BASE_URL));
    assert!(go.usage.automatic_sync);
    assert!(go.usage.authoritative_for_quota);
    assert_eq!(go.usage.endpoint, Some(OPENCODE_GO_USAGE_URL));
    assert_eq!(go.usage.contract, UsageContractKind::Authoritative);
    assert!(go.usage.publishes_capability);
    assert!(!go.usage.egress_ip_shared_cooldown_window);
    assert_eq!(
        go.protocol_probe.matrix,
        ProtocolMatrixKind::OpenCodeModelProtocols
    );
    assert!(go.protocol_probe.explicit_probe);
    assert_eq!(
        go.protocol_probe.structural_ceiling,
        StructuralProbeCeiling::OpenCodeConstructable
    );
    assert_eq!(
        go.card_actions.connection_verify,
        CardVerifyAction::Optional
    );
    assert!(go.card_actions.usage_refresh);
    assert!(go.card_actions.protocol_probe);
    assert!(go.card_actions.catalog_refresh);

    let zen = ProviderRegistry::get(OPENCODE_ZEN_FREE_PROVIDER_ID).unwrap();
    assert_eq!(zen.kind, ProviderAdapterKind::ZenFree);
    assert_eq!(zen.inference.auth, InferenceAuthDescriptor::None);
    assert_eq!(zen.inference.credential_kind, CredentialKind::None);
    assert_eq!(zen.inference.quota_scope, QuotaScope::EgressIp);
    assert_eq!(zen.inference.channel, Some(InferenceChannelKind::Free));
    assert!(zen.inference.follow_redirects);
    assert_eq!(zen.inference.origin, InferenceOriginKind::OfficialFixed);
    assert!(zen.model_catalog.admin_explicit_refresh);
    assert!(zen.protocol_probe.explicit_probe);
    assert_eq!(
        zen.protocol_probe.structural_ceiling,
        StructuralProbeCeiling::ZenFreeConstructable
    );
    assert!(!zen.usage.experimental);
    assert!(zen.usage.egress_ip_shared_cooldown_window);
    assert!(zen.card_actions.fetch_zen_models);
    assert!(zen.card_actions.protocol_probe);
    assert!(zen.card_actions.catalog_refresh);
    assert_eq!(
        zen.card_actions.connection_verify,
        CardVerifyAction::NotApplicable
    );

    let goat = ProviderRegistry::get(COMMAND_CODE_PROVIDER_ID).unwrap();
    assert_eq!(goat.kind, ProviderAdapterKind::CommandCodeGoat);
    assert!(!goat.inference.loopback_test_seam_only);
    assert!(goat.inference.production_inference);
    assert!(goat.inference.catalog_routable);
    assert!(!goat.inference.follow_redirects);
    assert_eq!(goat.inference.auth, InferenceAuthDescriptor::Bearer);
    assert!(!goat.usage.experimental);
    assert!(goat.usage.publishes_capability);
    assert_eq!(goat.usage.contract, UsageContractKind::Authoritative);
    assert_eq!(goat.usage.catalog_availability, "available");
    assert_eq!(goat.usage.endpoint, Some(COMMAND_CODE_GOAT_USAGE_URL));
    assert!(!goat.usage.automatic_sync);
    assert!(!goat.usage.authoritative_for_quota);
    assert!(goat.usage.manual_calibration);
    assert!(!goat.usage.egress_ip_shared_cooldown_window);
    assert_eq!(
        goat.protocol_probe.matrix,
        ProtocolMatrixKind::CommandCodeNative
    );
    assert!(goat.protocol_probe.explicit_probe);
    assert_eq!(
        goat.protocol_probe.structural_ceiling,
        StructuralProbeCeiling::CommandCodeConstructable
    );
    assert!(goat.card_actions.protocol_probe);
    assert!(goat.card_actions.catalog_refresh);
    assert!(goat.card_actions.usage_refresh);
    assert!(goat.card_actions.manual_usage_calibration);
    assert_eq!(
        goat.card_actions.connection_verify,
        CardVerifyAction::NotApplicable
    );
    assert!(!goat.verification.uses_get_models);
    assert!(!goat.verification.never_auto_enable);

    for fixed_provider in [
        ProviderRegistry::get(MINIMAX_PROVIDER_ID).unwrap(),
        ProviderRegistry::get(KIMI_PROVIDER_ID).unwrap(),
    ] {
        assert!(fixed_provider.protocol_probe.explicit_probe);
        assert_eq!(
            fixed_provider.protocol_probe.structural_ceiling,
            StructuralProbeCeiling::Fixed(&CHAT_MESSAGES_PROTOCOLS)
        );
        assert_eq!(
            fixed_provider.protocol_probe.matrix,
            ProtocolMatrixKind::FixedProviderProtocols
        );
        assert_eq!(
            fixed_provider.protocol_probe.fallback_priority,
            &CHAT_MESSAGES_PROTOCOLS
        );
        assert!(fixed_provider.card_actions.protocol_probe);
    }

    let ollama = ProviderRegistry::get(OLLAMA_PROVIDER_ID).unwrap();
    assert_eq!(ollama.kind, ProviderAdapterKind::OllamaCloud);
    assert_eq!(ollama.inference.auth, InferenceAuthDescriptor::Bearer);
    assert!(!ollama.inference.follow_redirects);
    assert_eq!(ollama.inference.origin, InferenceOriginKind::OfficialFixed);
    assert!(ollama.inference.catalog_routable);
    assert!(!ollama.protocol_probe.explicit_probe);
    assert_eq!(
        ollama.protocol_probe.structural_ceiling,
        StructuralProbeCeiling::Unavailable
    );
    assert_eq!(ollama.protocol_probe.fallback_priority, &CHAT_PROTOCOLS);
    assert_eq!(ollama.usage.contract, UsageContractKind::LocalState);
    assert!(ollama.usage.publishes_capability);
    assert!(ollama.usage.manual_calibration);
    assert!(!ollama.card_actions.usage_refresh);
    assert!(ollama.card_actions.manual_usage_calibration);
    assert!(ollama.card_actions.catalog_refresh);
    assert!(ollama.card_actions.persisted_enable_allowed);
    assert!(!ollama.card_actions.protocol_probe);
    assert_eq!(
        ollama.model_catalog.kind,
        ModelCatalogKind::ProviderPersistedSnapshot
    );

    let custom = ProviderRegistry::get(CUSTOM_PROVIDER_ID).unwrap();
    assert_eq!(custom.kind, ProviderAdapterKind::ConfigurableHttp);
    assert_eq!(
        custom.inference.auth,
        InferenceAuthDescriptor::ProtocolDerivedBearerOrXApiKey
    );
    assert!(!custom.inference.follow_redirects);
    assert_eq!(
        custom.inference.origin,
        InferenceOriginKind::AccountConfigured
    );
    assert!(custom.model_catalog.overlays_declared_ids);
    assert!(custom.verification.never_auto_enable);
    assert!(custom.verification.probe_first_declared_model);
    assert!(!custom.usage.publishes_capability);
    assert_eq!(
        custom.card_actions.connection_verify,
        CardVerifyAction::Optional
    );
    assert!(!custom.card_actions.protocol_and_auth_immutable_after_create);
    assert!(!custom.card_actions.enable_requires_verification);
    assert!(custom.card_actions.discover_models);
    assert!(!custom.card_actions.protocol_probe);
    assert!(!custom.card_actions.catalog_refresh);
    assert_eq!(
        custom.protocol_probe.structural_ceiling,
        StructuralProbeCeiling::Unavailable
    );
    assert!(!custom.protocol_probe.explicit_probe);
    assert!(!custom.usage.egress_ip_shared_cooldown_window);

    assert_ne!(go.kind, ProviderAdapterKind::ConfigurableHttp);
    assert_ne!(zen.kind, ProviderAdapterKind::ConfigurableHttp);
    assert_ne!(goat.kind, ProviderAdapterKind::ConfigurableHttp);
    assert_eq!(
        ProviderAdapterKind::from_provider_id(CUSTOM_PROVIDER_ID),
        Some(ProviderAdapterKind::ConfigurableHttp)
    );

    let cpa = ProviderRegistry::get(CPA_PROVIDER_ID).unwrap();
    assert_eq!(cpa.kind, ProviderAdapterKind::Cpa);
    assert_eq!(
        cpa.product_surface,
        ProviderProductSurface::ExternalIntegration
    );
    assert_eq!(cpa.inference.auth, InferenceAuthDescriptor::Bearer);
    assert_eq!(
        cpa.inference.origin,
        InferenceOriginKind::LocalExternalIntegration
    );
    assert_eq!(
        cpa.model_catalog.kind,
        ModelCatalogKind::ProviderPersistedSnapshot
    );
    assert_eq!(
        cpa.protocol_probe.matrix,
        ProtocolMatrixKind::FixedStandardProtocols
    );
    assert!(!cpa.protocol_probe.explicit_probe);
    assert_eq!(
        cpa.protocol_probe.structural_ceiling,
        StructuralProbeCeiling::Unavailable
    );
    assert!(cpa.verification.never_auto_enable);
    assert!(cpa.verification.uses_get_models);
    assert_eq!(cpa.usage.contract, UsageContractKind::Unavailable);
    assert!(!cpa.usage.publishes_capability);
    assert!(cpa.card_actions.catalog_refresh);
    assert!(!cpa.card_actions.protocol_probe);
}

#[test]
fn contract_scopes_are_unique_and_limited_to_ordinary_providers() {
    let mut scopes = std::collections::HashSet::new();
    for plan in BUILTIN_PROVIDERS {
        let descriptor =
            ProviderRegistry::get(plan.provider_id).expect("every built-in plan has a descriptor");
        assert_eq!(descriptor.contract_scope_id, plan.contract_scope_id);
        match plan.product_surface {
            ProviderProductSurface::Provider if is_custom_api(plan.provider_id) => {
                assert_eq!(plan.contract_scope_id, None)
            }
            ProviderProductSurface::Provider => {
                let scope = plan.contract_scope_id.expect("ordinary Provider scope");
                assert!(!scope.is_empty());
                assert!(scopes.insert(scope), "duplicate contract scope `{scope}`");
                assert_eq!(scope, plan.provider_id);
            }
            ProviderProductSurface::ExternalIntegration => {
                assert_eq!(plan.contract_scope_id, None)
            }
        }
    }
}

#[test]
fn defaults_and_verification_status_tokens_round_trip() {
    assert_eq!(default_provider_id(), OPENCODE_PROVIDER_ID);
    assert_eq!(default_credential_kind(), CredentialKind::ApiKey);
    assert_eq!(default_quota_scope(), QuotaScope::Key);
    assert_eq!(CreationAvailability::Available.as_str(), "available");
    assert_eq!(CreationAvailability::Unavailable.as_str(), "unavailable");
    assert_eq!(VerificationPolicy::NotRequired.as_str(), "not_required");
    assert_eq!(VerificationPolicy::Required.as_str(), "required");
    assert_eq!(
        ConnectionVerificationStatus::NotRequired.as_str(),
        "not_required"
    );
    assert!(ConnectionVerificationStatus::NotRequired.allows_enablement());
    assert!(ConnectionVerificationStatus::Verified.allows_enablement());
    assert!(!ConnectionVerificationStatus::Pending.allows_enablement());
    assert!(!ConnectionVerificationStatus::Failed.allows_enablement());
    assert_eq!(
        ConnectionVerificationStatus::try_from("verified").unwrap(),
        ConnectionVerificationStatus::Verified
    );
    assert!(matches!(
        ConnectionVerificationStatus::try_from("unknown"),
        Err(ProviderBindingError::UnknownVerificationStatus(value)) if value == "unknown"
    ));
}

#[test]
fn command_code_models_catalog_parses_openai_list_and_rejects_empty() {
    let parsed = parse_command_code_models_catalog(
            br#"{"object":"list","data":[{"id":"deepseek/deepseek-v4-flash"},{"id":"claude-sonnet-4-6"},{"id":"deepseek/deepseek-v4-flash"}]}"#,
        )
        .unwrap();
    assert_eq!(
        parsed,
        vec![
            "deepseek/deepseek-v4-flash".to_string(),
            "claude-sonnet-4-6".to_string()
        ]
    );
    assert!(parse_command_code_models_catalog(br#"["id"]"#).is_err());
    assert!(parse_command_code_models_catalog(br#"{"data":[]}"#).is_err());
    assert!(
        parse_command_code_models_catalog(br#"{"models":[{"model":"gpt-5.4"}]}"#)
            .is_ok_and(|models| models == ["gpt-5.4"])
    );
}

#[test]
fn zen_free_key_validation_skips_empty_secret() {
    let zen = builtin_provider(OPENCODE_ZEN_FREE_PROVIDER_ID).unwrap();
    assert_eq!(zen.credential_kind, CredentialKind::None);
    assert!(validate_plan_key(zen, "").is_ok());
    assert!(validate_plan_key(zen, "   ").is_ok());
    let go = builtin_provider(OPENCODE_PROVIDER_ID).unwrap();
    assert!(matches!(
        validate_plan_key(go, "   "),
        Err(ProviderBindingError::KeyRequired)
    ));
}

#[test]
fn provider_origin_round_trips_strings_and_rejects_unknowns() {
    for (origin, text) in [
        (ProviderOrigin::Builtin, "builtin"),
        (ProviderOrigin::Preset, "preset"),
        (ProviderOrigin::Custom, "custom"),
    ] {
        assert_eq!(origin.as_str(), text);
        assert_eq!(origin.to_string(), text);
        assert_eq!(ProviderOrigin::try_from(text).unwrap(), origin);
    }
    assert!(matches!(
        ProviderOrigin::try_from("unknown"),
        Err(ProviderBindingError::UnknownUpstreamProtocol(value)) if value == "unknown"
    ));
    assert_eq!(ProviderOrigin::ALL.len(), 3);
}

#[test]
fn preset_offering_resolves_plan_presets_and_falls_back_to_api() {
    for plan_id in [
        "zhipu-coding",
        "zai-coding",
        "tencent-token",
        "tencent-enterprise-pro",
        "bailian-coding",
        "qwencloud-coding",
        "qwencloud-token",
        "volcengine-agent",
        "volcengine-coding",
        "byteplus-coding",
        "qianfan-coding",
        "qianfan-token-team",
        "stepfun-plan",
        "stepfun-plan-intl",
        "xiaomi-mimo-token",
        "streamlake-coding",
        "compshare-coding",
        "atlascloud",
    ] {
        assert_eq!(preset_offering(plan_id), "plan", "{plan_id}");
    }
    for api_id in [
        "openai",
        "anthropic",
        "gemini",
        "xai",
        "azure-openai",
        "bedrock",
        "deepseek",
        "moonshot",
        "zhipu",
        "zai",
        "minimax-api-cn",
        "minimax-api-intl",
        "longcat",
        "tencent-hunyuan",
        "tencent-token-unknown",
        "bailian",
        "qwencloud",
        "volcengine",
        "qianfan",
        "stepfun-api",
        "stepfun-api-intl",
        "xiaomi-mimo",
        "ant-ling",
        "streamlake",
        "openrouter",
        "siliconflow-cn",
        "siliconflow-intl",
        "nvidia",
        "modelscope",
        "ppio",
        "qiniu",
        "novita",
        "compshare",
    ] {
        assert_eq!(preset_offering(api_id), "api", "{api_id}");
    }
    assert_eq!(preset_offering(""), "api");
    assert_eq!(preset_offering("  "), "api");
    assert_eq!(preset_offering("definitely-not-a-real-preset"), "api");
}

#[test]
fn builtin_offering_maps_paid_families_to_plan_and_the_rest_to_api() {
    for plan_id in [
        OPENCODE_PROVIDER_ID,
        COMMAND_CODE_PROVIDER_ID,
        MINIMAX_PROVIDER_ID,
        KIMI_PROVIDER_ID,
        OLLAMA_PROVIDER_ID,
    ] {
        assert_eq!(builtin_offering(plan_id), "plan", "{plan_id}");
    }
    for api_id in [
        OPENCODE_ZEN_FREE_PROVIDER_ID,
        CUSTOM_PROVIDER_ID,
        CPA_PROVIDER_ID,
    ] {
        assert_eq!(builtin_offering(api_id), "api", "{api_id}");
    }
    assert_eq!(builtin_offering("not-a-builtin"), "api");
    assert_eq!(builtin_offering(""), "api");
}
