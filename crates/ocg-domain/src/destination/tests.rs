use super::*;
use crate::account::{AccountSetupStep, AccountType};
use crate::catalog::UpstreamProtocolKind;
use crate::credential::{
    AuthState, ModelScope, OnboardingTaskKind, OnboardingTaskState,
    credential_id_for_legacy_account, observer_credential_id_for_platform_account,
};
use crate::dynamic::{DynamicAuthKind, DynamicModelMapping, DynamicProviderDefinition};
use crate::ids::{
    COMMAND_CODE_PROVIDER_ID, CPA_ACCOUNT_ID, CPA_ACCOUNT_NAME, CPA_PROVIDER_ID,
    CUSTOM_PROVIDER_ID, KIMI_PROVIDER_ID, MINIMAX_PROVIDER_ID, OLLAMA_PROVIDER_ID,
    OPENCODE_PROVIDER_ID, OPENCODE_ZEN_FREE_PROVIDER_ID, ZEN_FREE_ACCOUNT_ID,
    ZEN_FREE_ACCOUNT_NAME,
};
use crate::provider::ProviderAdapterKind;
use chrono::{DateTime, TimeZone, Utc};
use uuid::Uuid;

fn utc(year: i32, month: u32, day: u32, hour: u32) -> DateTime<Utc> {
    Utc.with_ymd_and_hms(year, month, day, hour, 0, 0).unwrap()
}

fn map_builtin(provider_id: &str) -> Destination {
    destination_from_legacy(&LegacyDestinationFacts::Builtin {
        provider_id: provider_id.to_string(),
    })
    .unwrap_or_else(|error| panic!("builtin {provider_id} should map: {error}"))
}

fn credential_facts(id: &str, provider_id: &str) -> LegacyCredentialFacts {
    LegacyCredentialFacts {
        id: id.to_string(),
        provider_id: provider_id.to_string(),
        name: "card".to_string(),
        notes: Some("note".to_string()),
        has_key: true,
        enabled: true,
        order_index: 3,
        setup_step: AccountSetupStep::Ready,
        account_type: AccountType::Key,
        auth_error: None,
        last_error: None,
        purchase_date: Some("2026-04-01".to_string()),
        cooldown_generic_until: None,
        cooldown_5h_until: None,
        cooldown_week_until: None,
        cooldown_month_until: None,
        cooldown_free_until: None,
        verified: false,
        platform_link: None,
        identity: None,
    }
}

fn dynamic_definition(auth_kind: DynamicAuthKind) -> DynamicProviderDefinition {
    DynamicProviderDefinition {
        preset_id: None,
        id: "lab-provider".to_string(),
        name: "Lab".to_string(),
        endpoint_url: "https://lab.example/v1".to_string(),
        upstream_protocol: Protocol::ChatCompletions,
        auth_kind,
        mappings: vec![DynamicModelMapping {
            public_model: "lab-opus".to_string(),
            upstream_model: "opus-upstream".to_string(),
            upstream_override: None,
        }],
    }
}

#[test]
fn adapter_kind_maps_from_provider_adapter_kind_and_sealed_ids() {
    assert_eq!(
        AdapterKind::from(ProviderAdapterKind::OpenCodeGo),
        AdapterKind::OpencodeGo
    );
    assert_eq!(
        AdapterKind::from(ProviderAdapterKind::ZenFree),
        AdapterKind::Zen
    );
    assert_eq!(
        AdapterKind::from(ProviderAdapterKind::CommandCodeGoat),
        AdapterKind::Goat
    );
    assert_eq!(
        AdapterKind::from(ProviderAdapterKind::MiniMaxCn),
        AdapterKind::Minimax
    );
    assert_eq!(
        AdapterKind::from(ProviderAdapterKind::KimiCn),
        AdapterKind::Kimi
    );
    assert_eq!(
        AdapterKind::from(ProviderAdapterKind::OllamaCloud),
        AdapterKind::Ollama
    );
    assert_eq!(
        AdapterKind::from(ProviderAdapterKind::ConfigurableHttp),
        AdapterKind::Http
    );
    assert_eq!(
        AdapterKind::from(ProviderAdapterKind::Cpa),
        AdapterKind::Cpa
    );
    for kind in AdapterKind::ALL {
        assert_eq!(AdapterKind::from(ProviderAdapterKind::from(kind)), kind);
    }

    assert_eq!(
        adapter_kind_for_builtin(OPENCODE_PROVIDER_ID),
        Some(AdapterKind::OpencodeGo)
    );
    assert_eq!(
        adapter_kind_for_builtin(OPENCODE_ZEN_FREE_PROVIDER_ID),
        Some(AdapterKind::Zen)
    );
    assert_eq!(
        adapter_kind_for_builtin(COMMAND_CODE_PROVIDER_ID),
        Some(AdapterKind::Goat)
    );
    assert_eq!(
        adapter_kind_for_builtin(MINIMAX_PROVIDER_ID),
        Some(AdapterKind::Minimax)
    );
    assert_eq!(
        adapter_kind_for_builtin(KIMI_PROVIDER_ID),
        Some(AdapterKind::Kimi)
    );
    assert_eq!(
        adapter_kind_for_builtin(OLLAMA_PROVIDER_ID),
        Some(AdapterKind::Ollama)
    );
    assert_eq!(
        adapter_kind_for_builtin(CPA_PROVIDER_ID),
        Some(AdapterKind::Cpa)
    );
    assert_eq!(
        adapter_kind_for_builtin(CUSTOM_PROVIDER_ID),
        Some(AdapterKind::Http)
    );
    assert!(adapter_kind_for_builtin("not-a-provider").is_none());
}

#[test]
fn wire_names_are_snake_case() {
    assert_eq!(
        serde_json::to_value(AdapterKind::OpencodeGo).unwrap(),
        serde_json::json!("opencode_go")
    );
    assert_eq!(
        serde_json::to_value(AuthScheme::XApiKey).unwrap(),
        serde_json::json!("x_api_key")
    );
    assert_eq!(
        serde_json::to_value(RedirectPolicy::FollowKeyless).unwrap(),
        serde_json::json!("follow_keyless")
    );
    assert_eq!(
        serde_json::to_value(UsageSource::OfficialApi).unwrap(),
        serde_json::json!("official_api")
    );
    assert_eq!(
        serde_json::to_value(PlanWindowKind::FiveHours).unwrap(),
        serde_json::json!("five_hours")
    );
    assert_eq!(
        serde_json::to_value(PricingSource::VerifiedSnapshot).unwrap(),
        serde_json::json!("verified_snapshot")
    );
    assert_eq!(
        serde_json::to_value(PlatformKind::Sub2Api).unwrap(),
        serde_json::json!("sub2_api")
    );
    assert_eq!(
        serde_json::to_value(Protocol::ChatCompletions).unwrap(),
        serde_json::json!("chat_completions")
    );
}

#[test]
fn every_adapter_kind_has_sealed_capabilities() {
    for kind in AdapterKind::ALL {
        let capabilities = sealed_capabilities(kind);
        // Observer is a per-destination flag (platform parents), never sealed.
        assert!(!capabilities.observer);
        // Every ready credential can be tested except an external integration.
        assert_eq!(capabilities.testable, kind != AdapterKind::Cpa);
        assert_eq!(capabilities.external_integration, kind == AdapterKind::Cpa);
        assert_eq!(capabilities.managed_signup, kind == AdapterKind::OpencodeGo);
        assert_eq!(
            capabilities.billing_tier_required,
            kind == AdapterKind::Ollama
        );
        assert_eq!(capabilities.discoverable_models, kind == AdapterKind::Http);
        // Only the keyless adapter may follow redirects.
        assert_eq!(
            capabilities.redirect_policy,
            if kind == AdapterKind::Zen {
                RedirectPolicy::FollowKeyless
            } else {
                RedirectPolicy::NoFollow
            }
        );
        assert_eq!(
            capabilities.identity_headers,
            matches!(kind, AdapterKind::OpencodeGo | AdapterKind::Zen)
        );
        if kind == AdapterKind::Http {
            assert_eq!(
                capabilities.official_balance_probe,
                vec![
                    "api.deepseek.com".to_string(),
                    "api.moonshot.cn".to_string(),
                    "api.moonshot.ai".to_string(),
                    "api.stepfun.com".to_string(),
                ]
            );
        } else {
            assert!(capabilities.official_balance_probe.is_empty());
        }
    }
}

#[test]
fn sealed_builtin_destinations_map_all_seven_ids() {
    let go = map_builtin(OPENCODE_PROVIDER_ID);
    assert_eq!(go.id, destination_id_for_builtin(OPENCODE_PROVIDER_ID));
    assert_eq!(
        go.legacy,
        LegacyDestinationRef::Builtin(OPENCODE_PROVIDER_ID.to_string())
    );
    assert_eq!(go.adapter, AdapterKind::OpencodeGo);
    assert_eq!(go.name, "OpenCode Go");
    assert_eq!(go.brand_family.as_deref(), Some("OpenCode"));
    assert_eq!(go.base_url.as_deref(), Some(OPENCODE_GO_BASE_URL));
    assert_eq!(
        go.protocols,
        vec![
            Protocol::ChatCompletions,
            Protocol::Responses,
            Protocol::Messages,
        ]
    );
    assert_eq!(go.auth_scheme, AuthScheme::Bearer);
    assert!(go.catalog.is_empty());
    assert_eq!(
        go.capabilities,
        sealed_capabilities(AdapterKind::OpencodeGo)
    );
    let go_plan = go.plan.expect("OpenCode Go has a plan");
    assert_eq!(go_plan.usage_source, UsageSource::OfficialApi);
    assert_eq!(
        go_plan.windows,
        vec![
            PlanWindow {
                kind: PlanWindowKind::FiveHours,
            },
            PlanWindow {
                kind: PlanWindowKind::Week,
            },
            PlanWindow {
                kind: PlanWindowKind::Month,
            },
        ]
    );
    assert_eq!(go_plan.expiry_cadence, Some(ExpiryCadence::Monthly));
    assert_eq!(go_plan.pricing_source, PricingSource::Official);
    assert!(!go_plan.manual_calibration);
    assert_eq!(go.max_credentials, None);
    assert!(go.observer_credential_id.is_none());
    assert!(go.enabled);

    let zen = map_builtin(OPENCODE_ZEN_FREE_PROVIDER_ID);
    assert_eq!(zen.adapter, AdapterKind::Zen);
    assert_eq!(zen.auth_scheme, AuthScheme::None);
    assert_eq!(zen.max_credentials, Some(1));
    assert_eq!(zen.base_url.as_deref(), Some(OPENCODE_ZEN_BASE_URL));
    let zen_plan = zen.plan.expect("Zen has a free-window plan");
    assert_eq!(zen_plan.usage_source, UsageSource::None);
    assert_eq!(
        zen_plan.windows,
        vec![PlanWindow {
            kind: PlanWindowKind::Free,
        }]
    );
    assert_eq!(zen_plan.pricing_source, PricingSource::Unpriced);
    assert_eq!(
        zen.capabilities.redirect_policy,
        RedirectPolicy::FollowKeyless
    );
    assert!(zen.capabilities.identity_headers);

    let goat = map_builtin(COMMAND_CODE_PROVIDER_ID);
    assert_eq!(goat.adapter, AdapterKind::Goat);
    assert_eq!(goat.base_url.as_deref(), Some(COMMAND_CODE_GOAT_BASE_URL));
    assert_eq!(
        goat.protocols,
        vec![Protocol::ChatCompletions, Protocol::Messages]
    );
    let goat_plan = goat.plan.expect("GOAT has a plan");
    assert_eq!(goat_plan.usage_source, UsageSource::LocalProjection);
    assert!(goat_plan.manual_calibration);
    assert_eq!(goat_plan.pricing_source, PricingSource::VerifiedSnapshot);
    assert_eq!(goat.max_credentials, None);

    let minimax = map_builtin(MINIMAX_PROVIDER_ID);
    assert_eq!(minimax.adapter, AdapterKind::Minimax);
    assert_eq!(minimax.base_url.as_deref(), Some(MINIMAX_CN_BASE_URL));
    let minimax_plan = minimax.plan.expect("MiniMax has official usage");
    assert_eq!(minimax_plan.usage_source, UsageSource::OfficialApi);
    assert_eq!(minimax_plan.pricing_source, PricingSource::Unpriced);
    assert!(!minimax_plan.manual_calibration);
    // CN token plans show a rolling 5h window plus a weekly window, and a
    // monthly purchase cadence on the card.
    let cn_windows = vec![
        PlanWindow {
            kind: PlanWindowKind::FiveHours,
        },
        PlanWindow {
            kind: PlanWindowKind::Week,
        },
    ];
    assert_eq!(minimax_plan.windows, cn_windows);
    assert_eq!(minimax_plan.expiry_cadence, Some(ExpiryCadence::Monthly));

    let kimi = map_builtin(KIMI_PROVIDER_ID);
    assert_eq!(kimi.adapter, AdapterKind::Kimi);
    assert_eq!(kimi.base_url.as_deref(), Some(KIMI_CN_BASE_URL));
    let kimi_plan = kimi.plan.expect("Kimi has official usage");
    assert_eq!(kimi_plan.usage_source, UsageSource::OfficialApi);
    assert_eq!(kimi_plan.pricing_source, PricingSource::Unpriced);
    assert_eq!(kimi_plan.windows, cn_windows);
    assert_eq!(kimi_plan.expiry_cadence, Some(ExpiryCadence::Monthly));
    assert_eq!(goat_plan.expiry_cadence, Some(ExpiryCadence::Monthly));

    let ollama = map_builtin(OLLAMA_PROVIDER_ID);
    assert_eq!(ollama.adapter, AdapterKind::Ollama);
    assert_eq!(ollama.base_url.as_deref(), Some(OLLAMA_CLOUD_BASE_URL));
    assert_eq!(ollama.protocols, vec![Protocol::ChatCompletions]);
    assert!(ollama.capabilities.billing_tier_required);
    let ollama_plan = ollama.plan.expect("Ollama has a local plan");
    assert_eq!(ollama_plan.usage_source, UsageSource::LocalProjection);
    assert_eq!(
        ollama_plan.windows,
        vec![PlanWindow {
            kind: PlanWindowKind::Month,
        }]
    );
    assert!(ollama_plan.manual_calibration);
    assert_eq!(ollama_plan.pricing_source, PricingSource::Official);

    let cpa = map_builtin(CPA_PROVIDER_ID);
    let cpa_variant = destination_from_legacy(&LegacyDestinationFacts::Cpa).unwrap();
    assert_eq!(cpa, cpa_variant);
    assert_eq!(cpa.adapter, AdapterKind::Cpa);
    assert_eq!(cpa.name, "CPA Subscription Pool");
    assert!(cpa.base_url.is_none());
    assert!(cpa.capabilities.external_integration);
    assert!(!cpa.capabilities.testable);
    assert_eq!(cpa.max_credentials, Some(1));
    assert!(cpa.plan.is_none());
}

#[test]
fn unknown_provider_id_yields_unknown_provider() {
    let error = destination_from_legacy(&LegacyDestinationFacts::Builtin {
        provider_id: "not-a-provider".to_string(),
    })
    .expect_err("unknown sealed id must refuse");
    assert_eq!(
        error,
        MappingError::UnknownProvider {
            provider_id: "not-a-provider".to_string(),
        }
    );
    assert_eq!(
        destination_from_legacy(&LegacyDestinationFacts::Builtin {
            provider_id: CUSTOM_PROVIDER_ID.to_string(),
        }),
        Err(MappingError::CustomRequiresAccount)
    );
    assert!(sealed_plan("not-a-provider").is_none());
    assert!(sealed_plan(CUSTOM_PROVIDER_ID).is_none());
}

#[test]
fn blank_endpoints_refuse_with_named_errors() {
    let mut definition = dynamic_definition(DynamicAuthKind::Bearer);
    definition.endpoint_url = "   ".to_string();
    assert_eq!(
        destination_from_legacy(&LegacyDestinationFacts::Dynamic { definition }),
        Err(MappingError::DynamicMissingEndpoint {
            provider_id: "lab-provider".to_string(),
        })
    );
    assert_eq!(
        destination_from_legacy(&LegacyDestinationFacts::PlatformParent {
            id: "plat-blank".to_string(),
            kind: PlatformKind::NewApi,
            name: "Blank".to_string(),
            base_url: String::new(),
            has_user_credential: false,
        }),
        Err(MappingError::PlatformMissingBaseUrl {
            id: "plat-blank".to_string(),
        })
    );
}

#[test]
fn maps_custom_root_url_account() {
    let destination = destination_from_legacy(&LegacyDestinationFacts::CustomAccount {
        account_id: "custom-root".to_string(),
        name: "DeepSeek".to_string(),
        endpoint_url: "https://api.deepseek.com".to_string(),
        protocol: Protocol::ChatCompletions,
        model_capabilities: vec![("deepseek-chat".to_string(), "deepseek-chat".to_string())],
    })
    .unwrap();
    assert_eq!(
        destination.id,
        destination_id_for_custom_account("custom-root")
    );
    assert_eq!(destination.adapter, AdapterKind::Http);
    assert_eq!(destination.name, "DeepSeek");
    assert_eq!(
        destination.base_url.as_deref(),
        Some("https://api.deepseek.com")
    );
    assert_eq!(destination.protocols, vec![Protocol::ChatCompletions]);
    assert_eq!(destination.auth_scheme, AuthScheme::Bearer);
    assert_eq!(destination.max_credentials, None);
    assert!(destination.plan.is_none());
    assert!(destination.capabilities.discoverable_models);
    assert_eq!(
        destination.catalog,
        vec![CatalogModel {
            public_model: "deepseek-chat".to_string(),
            upstream_model: "deepseek-chat".to_string(),
            protocols: vec![Protocol::ChatCompletions],
            preferred: Some(Protocol::ChatCompletions),
            enabled: true,
            upstream_override: None,
        }]
    );

    let destinations = [destination.clone()];
    let credential = credential_from_legacy(
        &credential_facts("custom-root", CUSTOM_PROVIDER_ID),
        &destinations,
    )
    .unwrap();
    assert_eq!(credential.destination_id, destination.id);
    assert!(credential.has_secret);
    assert_eq!(credential.routing_rank, 3);
}

#[test]
fn maps_custom_complete_path_account() {
    let destination = destination_from_legacy(&LegacyDestinationFacts::CustomAccount {
        account_id: "custom-path".to_string(),
        name: "Anthropic path".to_string(),
        endpoint_url: "https://api.example.com/v1/messages".to_string(),
        protocol: Protocol::Messages,
        model_capabilities: vec![("claude".to_string(), "claude-sonnet".to_string())],
    })
    .unwrap();
    assert_eq!(
        destination.base_url.as_deref(),
        Some("https://api.example.com/v1/messages")
    );
    assert_eq!(destination.protocols, vec![Protocol::Messages]);
    assert_eq!(destination.auth_scheme, AuthScheme::XApiKey);
    assert_eq!(destination.max_credentials, None);
    assert_eq!(destination.catalog[0].preferred, Some(Protocol::Messages));

    assert_eq!(
        destination_from_legacy(&LegacyDestinationFacts::CustomAccount {
            account_id: "empty-url".to_string(),
            name: "broken".to_string(),
            endpoint_url: "   ".to_string(),
            protocol: Protocol::ChatCompletions,
            model_capabilities: vec![],
        }),
        Err(MappingError::CustomAccountMissingEndpoint {
            account_id: "empty-url".to_string(),
        })
    );
}

#[test]
fn maps_dynamic_provider_with_and_without_key() {
    let keyed = destination_from_legacy(&LegacyDestinationFacts::Dynamic {
        definition: dynamic_definition(DynamicAuthKind::Bearer),
    })
    .unwrap();
    assert_eq!(keyed.id, destination_id_for_dynamic("lab-provider"));
    assert_eq!(keyed.adapter, AdapterKind::Http);
    assert_eq!(keyed.max_credentials, None);
    assert_eq!(keyed.auth_scheme, AuthScheme::Bearer);
    assert!(keyed.plan.is_none());
    assert_eq!(keyed.catalog.len(), 1);
    assert_eq!(keyed.catalog[0].public_model, "lab-opus");
    assert_eq!(keyed.catalog[0].upstream_model, "opus-upstream");

    let mut keyed_facts = credential_facts("dyn-key", "lab-provider");
    keyed_facts.has_key = true;
    let keyed_credential =
        credential_from_legacy(&keyed_facts, std::slice::from_ref(&keyed)).unwrap();
    assert_eq!(keyed_credential.destination_id, keyed.id);
    assert!(keyed_credential.has_secret);

    let keyless_def = {
        let mut definition = dynamic_definition(DynamicAuthKind::None);
        definition.id = "lab-anon".to_string();
        definition
    };
    let keyless = destination_from_legacy(&LegacyDestinationFacts::Dynamic {
        definition: keyless_def,
    })
    .unwrap();
    assert_eq!(keyless.auth_scheme, AuthScheme::None);
    assert_eq!(keyless.max_credentials, None);

    let mut keyless_facts = credential_facts("dyn-anon", "lab-anon");
    keyless_facts.has_key = false;
    let keyless_credential =
        credential_from_legacy(&keyless_facts, std::slice::from_ref(&keyless)).unwrap();
    assert!(!keyless_credential.has_secret);
    assert_eq!(keyless_credential.destination_id, keyless.id);
}

#[test]
fn maps_platform_parent_linked_and_unlinked_keys() {
    let parent = destination_from_legacy(&LegacyDestinationFacts::PlatformParent {
        id: "plat-1".to_string(),
        kind: PlatformKind::NewApi,
        name: "Site".to_string(),
        base_url: "https://newapi.example".to_string(),
        has_user_credential: true,
    })
    .unwrap();
    assert_eq!(parent.id, destination_id_for_platform_account("plat-1"));
    assert_eq!(parent.adapter, AdapterKind::Http);
    assert!(parent.capabilities.observer);
    assert_eq!(parent.max_credentials, None);
    assert_eq!(
        parent.observer_credential_id.as_deref(),
        Some(observer_credential_id_for_platform_account("plat-1").as_str())
    );
    assert_eq!(parent.protocols, Protocol::ALL.to_vec());
    assert_eq!(parent.auth_scheme, AuthScheme::Bearer);
    assert_eq!(parent.brand_family.as_deref(), Some("New API"));

    let unlinked_dest = destination_from_legacy(&LegacyDestinationFacts::CustomAccount {
        account_id: "unlinked-key".to_string(),
        name: "Loose custom".to_string(),
        endpoint_url: "https://loose.example/v1".to_string(),
        protocol: Protocol::Responses,
        model_capabilities: vec![],
    })
    .unwrap();

    let destinations = [parent.clone(), unlinked_dest.clone()];

    let mut linked_facts = credential_facts("linked-key", CUSTOM_PROVIDER_ID);
    linked_facts.platform_link = Some(LegacyPlatformLink {
        parent_id: "plat-1".to_string(),
    });
    let linked = credential_from_legacy(&linked_facts, &destinations).unwrap();
    assert_eq!(linked.destination_id, parent.id);
    assert_eq!(linked.legacy_account_id, linked_facts.id);
    assert_ne!(linked.id, linked.legacy_account_id);
    assert_eq!(
        parent.legacy,
        LegacyDestinationRef::PlatformParent("plat-1".to_string())
    );
    assert_ne!(
        linked.destination_id,
        destination_id_for_custom_account("linked-key")
    );
    assert!(linked.has_secret);

    let unlinked = credential_from_legacy(
        &credential_facts("unlinked-key", CUSTOM_PROVIDER_ID),
        &destinations,
    )
    .unwrap();
    assert_eq!(unlinked.destination_id, unlinked_dest.id);
    assert_eq!(
        unlinked.destination_id,
        destination_id_for_custom_account("unlinked-key")
    );
}

#[test]
fn linked_key_targets_platform_destination_never_per_account() {
    let parent = destination_from_legacy(&LegacyDestinationFacts::PlatformParent {
        id: "plat-2".to_string(),
        kind: PlatformKind::Sub2Api,
        name: "Relay".to_string(),
        base_url: "https://sub2.example".to_string(),
        has_user_credential: false,
    })
    .unwrap();
    let own = destination_from_legacy(&LegacyDestinationFacts::CustomAccount {
        account_id: "same-key".to_string(),
        name: "Should not win".to_string(),
        endpoint_url: "https://sub2.example".to_string(),
        protocol: Protocol::ChatCompletions,
        model_capabilities: vec![],
    })
    .unwrap();
    let mut facts = credential_facts("same-key", CUSTOM_PROVIDER_ID);
    facts.platform_link = Some(LegacyPlatformLink {
        parent_id: "plat-2".to_string(),
    });
    let credential = credential_from_legacy(&facts, &[parent.clone(), own.clone()]).unwrap();
    assert_eq!(credential.destination_id, parent.id);
    assert_ne!(credential.destination_id, own.id);
    assert_ne!(
        credential.destination_id,
        destination_id_for_custom_account("same-key")
    );
}

#[test]
fn maps_managed_draft_at_non_ready_step() {
    let go = map_builtin(OPENCODE_PROVIDER_ID);
    let mut facts = credential_facts("managed-1", OPENCODE_PROVIDER_ID);
    facts.account_type = AccountType::Managed;
    facts.setup_step = AccountSetupStep::Payment;
    facts.has_key = false;
    facts.verified = false;
    let credential = credential_from_legacy(&facts, std::slice::from_ref(&go)).unwrap();
    let task = credential
        .onboarding_task
        .expect("non-ready managed draft carries a task");
    assert_eq!(task.kind, OnboardingTaskKind::ManagedRegistration);
    assert_eq!(task.state, OnboardingTaskState::InProgress);
    assert_eq!(task.step, AccountSetupStep::Payment.as_str());
    assert!(!credential.has_secret);
    assert_eq!(credential.destination_id, go.id);

    facts.setup_step = AccountSetupStep::Ready;
    let ready = credential_from_legacy(&facts, std::slice::from_ref(&go)).unwrap();
    assert!(ready.onboarding_task.is_none());
}

#[test]
fn maps_zen_reserved_account() {
    let zen = map_builtin(OPENCODE_ZEN_FREE_PROVIDER_ID);
    let mut facts = credential_facts(ZEN_FREE_ACCOUNT_ID, OPENCODE_ZEN_FREE_PROVIDER_ID);
    facts.name = ZEN_FREE_ACCOUNT_NAME.to_string();
    facts.has_key = true;
    facts.purchase_date = None;
    let credential = credential_from_legacy(&facts, std::slice::from_ref(&zen)).unwrap();
    assert_eq!(
        credential.id,
        credential_id_for_legacy_account(ZEN_FREE_ACCOUNT_ID).as_str()
    );
    assert_eq!(credential.destination_id, zen.id);
    assert!(!credential.has_secret);
    assert!(credential.onboarding_task.is_none());
    assert_eq!(credential.auth_state, AuthState::Unknown);
}

#[test]
fn maps_cpa_reserved_account() {
    let cpa = destination_from_legacy(&LegacyDestinationFacts::Cpa).unwrap();
    let mut facts = credential_facts(CPA_ACCOUNT_ID, CPA_PROVIDER_ID);
    facts.name = CPA_ACCOUNT_NAME.to_string();
    facts.has_key = true;
    facts.verified = true;
    let credential = credential_from_legacy(&facts, std::slice::from_ref(&cpa)).unwrap();
    assert_eq!(credential.destination_id, cpa.id);
    assert!(!credential.has_secret);
    assert_eq!(credential.auth_state, AuthState::Valid);
    assert_eq!(cpa.max_credentials, Some(1));
}

#[test]
fn credential_copies_identity_grants_cooldowns_and_rank() {
    let go = map_builtin(OPENCODE_PROVIDER_ID);
    let until = utc(2026, 9, 17, 8);
    let mut facts = credential_facts("go-1", OPENCODE_PROVIDER_ID);
    facts.order_index = 11;
    facts.auth_error = Some("401".to_string());
    facts.last_error = Some("stale".to_string());
    facts.cooldown_generic_until = Some(until);
    facts.cooldown_5h_until = Some(until);
    facts.cooldown_week_until = Some(until);
    facts.cooldown_month_until = Some(until);
    facts.cooldown_free_until = Some(until);
    facts.identity = Some(LegacyIdentityFacts {
        quota_pool_id: Some("pool-1".to_string()),
        model_scope: ModelScope::Only {
            models: vec!["mimo-v2.5".to_string()],
        },
        allowed_endpoint_ids: vec!["ep-chat".to_string()],
        allowed_origins: vec!["https://opencode.ai".to_string()],
        binding_enabled: true,
    });
    let credential = credential_from_legacy(&facts, std::slice::from_ref(&go)).unwrap();
    assert_eq!(credential.routing_rank, 11);
    assert_eq!(credential.auth_state, AuthState::Invalid);
    assert_eq!(credential.last_error.as_deref(), Some("stale"));
    assert_eq!(credential.quota_pool_id.as_deref(), Some("pool-1"));
    assert_eq!(
        credential.scope,
        ModelScope::Only {
            models: vec!["mimo-v2.5".to_string()],
        }
    );
    assert_eq!(
        credential.grants.allowed_endpoint_ids,
        vec!["ep-chat".to_string()]
    );
    assert_eq!(credential.cooldowns.five_hour_until, Some(until));
    assert_eq!(credential.purchase_date.as_deref(), Some("2026-04-01"));
}

#[test]
fn missing_known_destination_refuses_the_credential() {
    let facts = credential_facts("go-missing", OPENCODE_PROVIDER_ID);
    assert_eq!(
        credential_from_legacy(&facts, &[]),
        Err(MappingError::MissingDestination {
            destination_id: destination_id_for_builtin(OPENCODE_PROVIDER_ID),
        })
    );
    assert_eq!(
        credential_from_legacy(&credential_facts("dyn-x", "unknown-dynamic"), &[]),
        Err(MappingError::UnknownProvider {
            provider_id: "unknown-dynamic".to_string(),
        })
    );
}

#[test]
fn destination_ids_are_deterministic_and_kind_scoped() {
    assert_eq!(
        destination_id_for_custom_account("acct"),
        destination_id_for_custom_account("acct")
    );
    assert_ne!(
        destination_id_for_custom_account("acct"),
        destination_id_for_builtin(CUSTOM_PROVIDER_ID)
    );
    assert_ne!(
        destination_id_for_platform_account("acct"),
        destination_id_for_custom_account("acct")
    );
    assert!(Uuid::parse_str(&destination_id_for_custom_account("acct")).is_ok());
}

#[test]
fn protocol_alias_is_the_catalog_wire_enum() {
    assert_eq!(Protocol::Responses, UpstreamProtocolKind::Responses);
    assert_eq!(Protocol::ALL.len(), 3);
}
