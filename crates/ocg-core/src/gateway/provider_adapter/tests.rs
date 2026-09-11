use super::*;
use crate::crypto::{KeyCipher, StaticKeyCipher};
use crate::gateway::protocol::{CustomRouteSpec, opencode_supports_upstream};
use crate::models::{Account, AccountSetupStep, AccountType, AppConfig};
use crate::provider::{
    COMMAND_CODE_GOAT_BASE_URL, COMMAND_CODE_GOAT_DEEPSEEK_V4_FLASH_UPSTREAM,
    COMMAND_CODE_PROVIDER_ID, CUSTOM_PROVIDER_ID, KIMI_CN_BASE_URL, KIMI_CN_CHAT_COMPLETIONS_PATH,
    KIMI_CN_MESSAGES_PATH, KIMI_PROVIDER_ID, MINIMAX_CN_ANTHROPIC_BASE_URL, MINIMAX_CN_BASE_URL,
    MINIMAX_CN_CHAT_COMPLETIONS_PATH, MINIMAX_CN_MESSAGES_PATH, MINIMAX_PROVIDER_ID,
    OLLAMA_CLOUD_BASE_URL, OLLAMA_CLOUD_CHAT_COMPLETIONS_PATH, OLLAMA_PROVIDER_ID,
    OPENCODE_PROVIDER_ID, OPENCODE_ZEN_FREE_PROVIDER_ID, ZEN_FREE_ACCOUNT_NAME,
};
use bytes::Bytes;
use chrono::Utc;
use serde_json::json;
use std::sync::Arc;

fn resolve_route(
    account: &Account,
    config: &AppConfig,
    plan: &RequestPlan,
) -> Result<AttemptSpec, String> {
    resolve_route_with_dynamics(account, config, plan, &[])
}

fn account(
    id: &str,
    provider_id: &str,

    credential_kind: CredentialKind,
    quota_scope: QuotaScope,
) -> Account {
    let cipher: Arc<dyn KeyCipher + Send + Sync> = Arc::new(StaticKeyCipher::new("test"));
    Account {
        id: id.into(),
        provider_id: provider_id.into(),

        credential_kind,
        quota_scope,
        name: id.into(),
        username: None,
        password_cipher: None,
        key_cipher: cipher.encrypt("key").unwrap(),
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
        created_at: Utc::now(),
        updated_at: Utc::now(),
    }
}

fn chat_plan(
    model: &str,
    channel: UpstreamChannel,
    upstream: ApiFormat,
    custom_route: Option<CustomRouteSpec>,
) -> RequestPlan {
    RequestPlan {
        client: ApiFormat::ChatCompletions,
        upstream,
        model: model.into(),
        client_model: model.into(),
        stream: false,
        body: Bytes::from(
            serde_json::to_vec(&json!({
                "model": model,
                "messages": [{"role": "user", "content": "hi"}]
            }))
            .unwrap(),
        ),
        channel,
        upstream_base_override: None,
        original_model: None,
        allow_go_fallback: false,
        resolved_alias: Some(model.into()),
        custom_route,
        service_tier: None,
        custom_tools: Vec::new(),
        namespace_tools: Vec::new(),
        response_parallel_tool_calls: true,
        response_tool_choice: json!("auto"),
        response_tools: Vec::new(),
    }
}

#[test]
fn official_transport_is_fixed_bearer_chat_without_redirects_or_zdr() {
    let spec = command_code_goat_transport_spec();
    assert_eq!(spec.base_url, "https://api.commandcode.ai/provider/v1");
    assert_eq!(spec.host, "api.commandcode.ai");
    assert_eq!(spec.chat_completions_path, "/chat/completions");
    assert_eq!(spec.messages_path, "/messages");
    assert_eq!(spec.models_path, "/models");
    assert_eq!(spec.auth_scheme, UpstreamAuthScheme::Bearer);
    assert!(!spec.follow_redirects);
    assert_eq!(spec.zdr_header_name, None);
    assert!(spec.public_catalog_refresh);
    assert_eq!(
        command_code_goat_official_url(ApiFormat::ChatCompletions).unwrap(),
        "https://api.commandcode.ai/provider/v1/chat/completions"
    );
    assert_eq!(
        command_code_goat_official_url(ApiFormat::Messages).unwrap(),
        "https://api.commandcode.ai/provider/v1/messages"
    );
    assert!(command_code_goat_official_url(ApiFormat::Responses).is_err());
    let loopback = command_code_goat_loopback_base("http://127.0.0.1:9");
    assert_eq!(loopback, "http://127.0.0.1:9/provider/v1");
    assert_eq!(
        command_code_goat_join_url(&loopback, ApiFormat::ChatCompletions).unwrap(),
        "http://127.0.0.1:9/provider/v1/chat/completions"
    );
    assert!(crate::provider::is_command_code_goat(
        COMMAND_CODE_PROVIDER_ID
    ));
    assert_eq!(
        crate::provider::COMMAND_CODE_GOAT_DEEPSEEK_V4_FLASH_ALIAS,
        "deepseek-v4-flash"
    );
    assert_eq!(
        crate::provider::COMMAND_CODE_GOAT_DEEPSEEK_V4_FLASH_UPSTREAM,
        "deepseek/deepseek-v4-flash"
    );
}

#[test]
fn adapter_kind_dispatch_preserves_route_auth_and_model_decisions() {
    let config = AppConfig::default();
    let go = account(
        "go-1",
        OPENCODE_PROVIDER_ID,
        CredentialKind::ApiKey,
        QuotaScope::Key,
    );
    let go_route = resolve_route(
        &go,
        &config,
        &chat_plan(
            "glm-5.2",
            UpstreamChannel::Go,
            ApiFormat::ChatCompletions,
            None,
        ),
    )
    .unwrap();
    assert_eq!(go_route.auth, UpstreamAuth::OpenCodeProtocolDefault);
    assert!(go_route.follow_redirects);
    assert_eq!(go_route.path, "/v1/chat/completions");
    assert_eq!(
        go_route.credential,
        CredentialHandle::Account { id: "go-1".into() }
    );
    assert_eq!(
        go_route.proxy_routing,
        ProxyRoutingModel::RequestEntrySnapshot
    );
    assert!(go_route.restricted_upstream_url());
    assert!(!go_route.isolates_client_headers());
    assert_eq!(go_route.wire_auth(), UpstreamAuth::Bearer);
    assert!(
        resolve_route(
            &go,
            &config,
            &chat_plan(
                "glm-5.2",
                UpstreamChannel::Free,
                ApiFormat::ChatCompletions,
                None
            ),
        )
        .unwrap_err()
        .contains("does not serve the Zen free channel")
    );

    let mut zen = account(
        ZEN_FREE_ACCOUNT_ID,
        OPENCODE_ZEN_FREE_PROVIDER_ID,
        CredentialKind::None,
        QuotaScope::EgressIp,
    );
    zen.name = ZEN_FREE_ACCOUNT_NAME.into();
    let zen_route = resolve_route(
        &zen,
        &config,
        &chat_plan(
            "mimo-v2.5-free",
            UpstreamChannel::Free,
            ApiFormat::ChatCompletions,
            None,
        ),
    )
    .unwrap();
    assert_eq!(zen_route.auth, UpstreamAuth::None);
    assert!(zen_route.follow_redirects);
    assert_eq!(zen_route.credential, CredentialHandle::None);
    assert_eq!(
        zen_route.proxy_routing,
        ProxyRoutingModel::RequestEntrySnapshot
    );
    assert!(zen_route.credential_account_id().is_none());

    let goat = account(
        "goat-1",
        COMMAND_CODE_PROVIDER_ID,
        CredentialKind::ApiKey,
        QuotaScope::Key,
    );
    let official = resolve_route(
        &goat,
        &config,
        &chat_plan(
            COMMAND_CODE_GOAT_DEEPSEEK_V4_FLASH_UPSTREAM,
            UpstreamChannel::Go,
            ApiFormat::ChatCompletions,
            None,
        ),
    )
    .unwrap();
    assert_eq!(official.base_url, COMMAND_CODE_GOAT_BASE_URL);
    assert_eq!(official.path, "/chat/completions");
    assert_eq!(official.auth, UpstreamAuth::Bearer);
    assert!(!official.follow_redirects);
    assert_eq!(
        official.proxy_routing,
        ProxyRoutingModel::ProcessWideNoRedirect
    );
    let claude = resolve_route(
        &goat,
        &config,
        &chat_plan(
            "claude-sonnet-4-6",
            UpstreamChannel::Go,
            ApiFormat::Messages,
            None,
        ),
    )
    .unwrap();
    assert_eq!(claude.path, "/messages");
    let _guard =
        install_goat_loopback_route_for_test(goat.id.clone(), "http://127.0.0.1:9").unwrap();
    let goat_route = resolve_route(
        &goat,
        &config,
        &chat_plan(
            COMMAND_CODE_GOAT_DEEPSEEK_V4_FLASH_UPSTREAM,
            UpstreamChannel::Go,
            ApiFormat::ChatCompletions,
            None,
        ),
    )
    .unwrap();
    assert_eq!(goat_route.auth, UpstreamAuth::Bearer);
    assert!(!goat_route.follow_redirects);
    assert_eq!(goat_route.base_url, "http://127.0.0.1:9/provider/v1");
    assert_eq!(goat_route.path, "/chat/completions");
    assert_eq!(
        goat_route.proxy_routing,
        ProxyRoutingModel::ProcessWideNoRedirect
    );
    assert!(goat_route.restricted_upstream_url());

    let custom = account(
        "custom-1",
        CUSTOM_PROVIDER_ID,
        CredentialKind::ApiKey,
        QuotaScope::Key,
    );
    let custom_missing = resolve_route(
        &custom,
        &config,
        &chat_plan(
            "local-model",
            UpstreamChannel::Go,
            ApiFormat::ChatCompletions,
            None,
        ),
    )
    .unwrap_err();
    assert!(custom_missing.contains("missing a persisted endpoint URL"));
    let custom_route = resolve_route(
        &custom,
        &config,
        &chat_plan(
            "local-model",
            UpstreamChannel::Go,
            ApiFormat::Messages,
            Some(CustomRouteSpec {
                endpoint_url: "http://127.0.0.1:9/v1/messages".into(),
            }),
        ),
    )
    .unwrap();
    assert_eq!(custom_route.auth, UpstreamAuth::XApiKey);
    assert!(!custom_route.follow_redirects);
    assert_eq!(custom_route.base_url, "http://127.0.0.1:9");
    assert_eq!(custom_route.path, "/v1/messages");
    assert_eq!(
        custom_route.proxy_routing,
        ProxyRoutingModel::IsolatedTrustedAdmin
    );
    assert!(custom_route.isolates_client_headers());
    assert!(!custom_route.restricted_upstream_url());
    assert_eq!(
        custom_route.credential,
        CredentialHandle::Account {
            id: "custom-1".into()
        }
    );

    for endpoint_url in ["http://127.0.0.1:9", "http://127.0.0.1:9/v1"] {
        let resolved = resolve_route(
            &custom,
            &config,
            &chat_plan(
                "local-model",
                UpstreamChannel::Go,
                ApiFormat::ChatCompletions,
                Some(CustomRouteSpec {
                    endpoint_url: endpoint_url.into(),
                }),
            ),
        )
        .unwrap();
        assert_eq!(resolved.base_url, "http://127.0.0.1:9");
        assert_eq!(resolved.path, "/v1/chat/completions");
    }

    let unknown = account(
        "unknown-1",
        "unknown",
        CredentialKind::ApiKey,
        QuotaScope::Key,
    );
    assert_eq!(
        resolve_route(
            &unknown,
            &config,
            &chat_plan(
                "glm-5.2",
                UpstreamChannel::Go,
                ApiFormat::ChatCompletions,
                None
            ),
        )
        .unwrap_err(),
        "unsupported provider offering `unknown/unknown`"
    );
}

#[test]
fn resolve_uses_the_same_sealed_descriptors() {
    let go = ProviderRegistry::get(OPENCODE_PROVIDER_ID).unwrap();
    let zen = ProviderRegistry::get(OPENCODE_ZEN_FREE_PROVIDER_ID).unwrap();
    let goat = ProviderRegistry::get(COMMAND_CODE_PROVIDER_ID).unwrap();
    let custom = ProviderRegistry::get(CUSTOM_PROVIDER_ID).unwrap();
    assert!(go.inference.production_inference);
    assert_eq!(
        zen.inference.channel,
        Some(crate::provider::InferenceChannelKind::Free)
    );
    assert!(goat.inference.production_inference);
    assert!(!goat.inference.loopback_test_seam_only);
    assert_eq!(
        custom.inference.auth,
        InferenceAuthDescriptor::ProtocolDerivedBearerOrXApiKey
    );
}

#[test]
fn adapter_kind_match_is_exhaustive_and_consistent_with_descriptors() {
    for kind in ProviderAdapterKind::ALL {
        match kind {
            ProviderAdapterKind::OpenCodeGo
            | ProviderAdapterKind::ZenFree
            | ProviderAdapterKind::CommandCodeGoat
            | ProviderAdapterKind::MiniMaxCn
            | ProviderAdapterKind::KimiCn
            | ProviderAdapterKind::OllamaCloud
            | ProviderAdapterKind::Cpa
            | ProviderAdapterKind::ConfigurableHttp => {}
        }
        let descriptor = ProviderRegistry::iter()
            .find(|entry| entry.kind == kind)
            .expect("each adapter kind has a registry descriptor");
        match kind {
            ProviderAdapterKind::OpenCodeGo => {
                assert_eq!(
                    descriptor.inference.auth,
                    InferenceAuthDescriptor::OpenCodeProtocolDefault
                );
                assert!(descriptor.inference.follow_redirects);
            }
            ProviderAdapterKind::ZenFree => {
                assert_eq!(descriptor.inference.auth, InferenceAuthDescriptor::None);
                assert!(descriptor.inference.follow_redirects);
            }
            ProviderAdapterKind::CommandCodeGoat => {
                assert_eq!(descriptor.inference.auth, InferenceAuthDescriptor::Bearer);
                assert!(!descriptor.inference.follow_redirects);
                assert!(descriptor.inference.production_inference);
                assert!(!descriptor.inference.loopback_test_seam_only);
            }
            ProviderAdapterKind::MiniMaxCn
            | ProviderAdapterKind::KimiCn
            | ProviderAdapterKind::OllamaCloud => {
                assert_eq!(descriptor.inference.auth, InferenceAuthDescriptor::Bearer);
                assert!(!descriptor.inference.follow_redirects);
                assert!(descriptor.inference.catalog_routable);
            }
            ProviderAdapterKind::Cpa => {
                assert_eq!(descriptor.inference.auth, InferenceAuthDescriptor::Bearer);
                assert!(!descriptor.inference.follow_redirects);
            }
            ProviderAdapterKind::ConfigurableHttp => {
                assert_eq!(
                    descriptor.inference.auth,
                    InferenceAuthDescriptor::ProtocolDerivedBearerOrXApiKey
                );
                assert!(!descriptor.inference.follow_redirects);
            }
        }
    }
}

#[test]
fn probe_route_allows_ceiling_without_static_support_production_requires_contract() {
    let config = AppConfig::default();
    let go = account(
        "go-1",
        OPENCODE_PROVIDER_ID,
        CredentialKind::ApiKey,
        QuotaScope::Key,
    );
    let chat_grok = chat_plan(
        "grok-4.5",
        UpstreamChannel::Go,
        ApiFormat::ChatCompletions,
        None,
    );
    let probe = resolve_probe_route(&go, &config, &chat_grok).unwrap();
    assert_eq!(probe.path, "/v1/chat/completions");
    assert_eq!(probe.upstream, ApiFormat::ChatCompletions);
    let fetched_model_probe = resolve_probe_route(
        &go,
        &config,
        &chat_plan(
            "future-go-model",
            UpstreamChannel::Go,
            ApiFormat::Messages,
            None,
        ),
    )
    .expect("the Dashboard catalog gate admits fetched models before route construction");
    assert_eq!(fetched_model_probe.path, "/v1/messages");

    let static_contracts = crate::provider_contracts::build_effective_contracts(
        &crate::zen_models::ZenFreeModelCatalog::default(),
        &[],
        crate::provider_contracts::PersistedContracts::default(),
    );
    assert!(
        supports_production_plan(&go, &config, &chat_grok, &static_contracts, &[]).is_err(),
        "static grok-4.5 Chat must stay unverified until a probe succeeds"
    );
    assert!(
        supports_production_plan(
            &go,
            &config,
            &chat_plan("grok-4.5", UpstreamChannel::Go, ApiFormat::Responses, None),
            &static_contracts,
            &[],
        )
        .is_ok()
    );

    let now = Utc::now();
    let mut persisted = crate::provider_contracts::PersistedContracts::default();
    let scope = crate::provider_contracts::ContractScope::provider(OPENCODE_PROVIDER_ID);
    persisted.evidence.insert(
        scope.clone(),
        vec![crate::provider_contracts::PersistedModelProtocol {
            scope,
            model_id: "grok-4.5".into(),
            protocol: crate::provider::UpstreamProtocolKind::ChatCompletions,
            source: crate::provider_contracts::ContractEvidenceSource::ProbeConfirmed,
            verified_at: Some(now),
            observed_at: Some(now),
            last_probe_result: Some(crate::provider_contracts::ProbeResultKind::Success),
            last_probe_at: Some(now),
            last_probe_error: None,
        }],
    );
    let probed = crate::provider_contracts::build_effective_contracts(
        &crate::zen_models::ZenFreeModelCatalog::default(),
        &[],
        persisted,
    );
    assert!(supports_production_plan(&go, &config, &chat_grok, &probed, &[]).is_ok());

    let goat = account(
        "goat-1",
        COMMAND_CODE_PROVIDER_ID,
        CredentialKind::ApiKey,
        QuotaScope::Key,
    );
    let goat_probe = resolve_probe_route(
        &goat,
        &config,
        &chat_plan(
            COMMAND_CODE_GOAT_DEEPSEEK_V4_FLASH_UPSTREAM,
            UpstreamChannel::Go,
            ApiFormat::ChatCompletions,
            None,
        ),
    )
    .expect("GOAT provider probes reuse the fixed official route");
    assert_eq!(goat_probe.base_url, COMMAND_CODE_GOAT_BASE_URL);
    assert_eq!(goat_probe.path, COMMAND_CODE_GOAT_CHAT_COMPLETIONS_PATH);
}

#[test]
fn fixed_provider_plans_expose_documented_chat_and_messages_routes() {
    let config = AppConfig::default();
    for (provider_id, chat_base, chat_path, messages_base, messages_path, model_id) in [
        (
            MINIMAX_PROVIDER_ID,
            MINIMAX_CN_BASE_URL,
            MINIMAX_CN_CHAT_COMPLETIONS_PATH,
            MINIMAX_CN_ANTHROPIC_BASE_URL,
            MINIMAX_CN_MESSAGES_PATH,
            "MiniMax-M3",
        ),
        (
            KIMI_PROVIDER_ID,
            KIMI_CN_BASE_URL,
            KIMI_CN_CHAT_COMPLETIONS_PATH,
            KIMI_CN_BASE_URL,
            KIMI_CN_MESSAGES_PATH,
            "kimi-for-coding",
        ),
    ] {
        let account = account(
            &format!("{provider_id}-test"),
            provider_id,
            CredentialKind::ApiKey,
            QuotaScope::Key,
        );
        for (protocol, base_url, path) in [
            (ApiFormat::ChatCompletions, chat_base, chat_path),
            (ApiFormat::Messages, messages_base, messages_path),
        ] {
            let plan = chat_plan(model_id, UpstreamChannel::Go, protocol, None);
            let account_route =
                resolve_account_test_route_with_dynamics(&account, &config, &plan, &[])
                    .expect("account-level tests use the documented production route");
            let provider_route = resolve_probe_route(&account, &config, &plan)
                .expect("provider probes reuse the documented production route");
            for route in [account_route, provider_route] {
                assert_eq!(route.base_url, base_url);
                assert_eq!(route.path, path);
                assert_eq!(route.upstream, protocol);
                assert_eq!(route.auth, UpstreamAuth::Bearer);
                assert!(!route.follow_redirects);
            }
        }
        assert!(
            resolve_probe_route(
                &account,
                &config,
                &chat_plan(model_id, UpstreamChannel::Go, ApiFormat::Responses, None,),
            )
            .unwrap_err()
            .contains("no official upstream path")
        );
    }
}

#[test]
fn minimax_kimi_ollama_do_not_inherit_opencode_responses_for_shared_model_names() {
    let config = AppConfig::default();
    let shared = "grok-4.5";
    assert!(opencode_supports_upstream(shared, ApiFormat::Responses));
    assert!(!opencode_supports_upstream(
        shared,
        ApiFormat::ChatCompletions
    ));

    let minimax = account(
        "minimax-shared",
        MINIMAX_PROVIDER_ID,
        CredentialKind::ApiKey,
        QuotaScope::Key,
    );
    let kimi = account(
        "kimi-shared",
        KIMI_PROVIDER_ID,
        CredentialKind::ApiKey,
        QuotaScope::Key,
    );
    let ollama = account(
        "ollama-shared",
        OLLAMA_PROVIDER_ID,
        CredentialKind::ApiKey,
        QuotaScope::Key,
    );

    for account in [&minimax, &kimi, &ollama] {
        let err = resolve_account_test_route_with_dynamics(
            account,
            &config,
            &chat_plan(shared, UpstreamChannel::Go, ApiFormat::Responses, None),
            &[],
        )
        .unwrap_err();
        assert!(
            err.contains("no official upstream path") || err.contains("no verified support"),
            "{} Responses must not inherit OpenCode support: {err}",
            account.provider_id
        );
        let probe_err = resolve_probe_route(
            account,
            &config,
            &chat_plan(shared, UpstreamChannel::Go, ApiFormat::Responses, None),
        )
        .unwrap_err();
        assert!(
            probe_err.contains("no official upstream path")
                || probe_err.contains("no verified support"),
            "{} probe Responses must not inherit OpenCode support: {probe_err}",
            account.provider_id
        );
    }

    let minimax_chat = resolve_account_test_route_with_dynamics(
        &minimax,
        &config,
        &chat_plan(
            "MiniMax-M3",
            UpstreamChannel::Go,
            ApiFormat::ChatCompletions,
            None,
        ),
        &[],
    )
    .unwrap();
    assert_eq!(minimax_chat.base_url, MINIMAX_CN_BASE_URL);
    assert_eq!(minimax_chat.path, MINIMAX_CN_CHAT_COMPLETIONS_PATH);
    let minimax_messages = resolve_account_test_route_with_dynamics(
        &minimax,
        &config,
        &chat_plan("MiniMax-M3", UpstreamChannel::Go, ApiFormat::Messages, None),
        &[],
    )
    .unwrap();
    assert_eq!(minimax_messages.base_url, MINIMAX_CN_ANTHROPIC_BASE_URL);
    assert_eq!(minimax_messages.path, MINIMAX_CN_MESSAGES_PATH);

    let kimi_chat = resolve_account_test_route_with_dynamics(
        &kimi,
        &config,
        &chat_plan(
            "kimi-for-coding",
            UpstreamChannel::Go,
            ApiFormat::ChatCompletions,
            None,
        ),
        &[],
    )
    .unwrap();
    assert_eq!(kimi_chat.base_url, KIMI_CN_BASE_URL);
    assert_eq!(kimi_chat.path, KIMI_CN_CHAT_COMPLETIONS_PATH);
    let kimi_messages = resolve_account_test_route_with_dynamics(
        &kimi,
        &config,
        &chat_plan(
            "kimi-for-coding",
            UpstreamChannel::Go,
            ApiFormat::Messages,
            None,
        ),
        &[],
    )
    .unwrap();
    assert_eq!(kimi_messages.path, KIMI_CN_MESSAGES_PATH);

    let ollama_chat = resolve_account_test_route_with_dynamics(
        &ollama,
        &config,
        &chat_plan(
            "deepseek-v4-flash",
            UpstreamChannel::Go,
            ApiFormat::ChatCompletions,
            None,
        ),
        &[],
    )
    .unwrap();
    assert_eq!(ollama_chat.base_url, OLLAMA_CLOUD_BASE_URL);
    assert_eq!(ollama_chat.path, OLLAMA_CLOUD_CHAT_COMPLETIONS_PATH);
}

fn dynamic_runtime(
    endpoint_url: &str,
    override_url: Option<&str>,
    auth_kind: ocg_domain::dynamic::DynamicAuthKind,
) -> crate::dynamic::DynamicProviderRuntime {
    crate::dynamic::DynamicProviderRuntime {
        preset_id: None,
        id: "11111111-1111-1111-1111-111111111111".into(),
        name: "Lab".into(),
        endpoint_url: endpoint_url.into(),
        upstream_protocol: crate::provider::UpstreamProtocolKind::ChatCompletions,
        auth_kind,
        mappings: vec![ocg_domain::dynamic::DynamicModelMapping {
            public_model: "lab".into(),
            upstream_model: "vendor/lab".into(),
            upstream_override: override_url.map(|url| {
                ocg_domain::dynamic::DynamicModelUpstreamOverride {
                    protocol: crate::provider::UpstreamProtocolKind::ChatCompletions,
                    endpoint_url: url.into(),
                }
            }),
        }],
        created_at: Utc::now(),
        updated_at: Utc::now(),
        origin: crate::provider::ProviderOrigin::Custom,
        offering: "api".into(),
    }
}

#[test]
fn s01_dynamic_cross_origin_override_refuses_to_inherit_the_key() {
    let config = AppConfig::default();
    let runtime = dynamic_runtime(
        "https://lab.example/v1",
        Some("https://evil.example/v1"),
        ocg_domain::dynamic::DynamicAuthKind::Bearer,
    );
    let account = account(
        "dyn-1",
        &runtime.id,
        CredentialKind::ApiKey,
        QuotaScope::Key,
    );
    let error = resolve_route_with_dynamics(
        &account,
        &config,
        &chat_plan(
            "vendor/lab",
            UpstreamChannel::Go,
            ApiFormat::ChatCompletions,
            None,
        ),
        std::slice::from_ref(&runtime),
    )
    .unwrap_err();
    assert!(
        error.contains("not authorized") || error.contains("refusing to send credentials"),
        "{error}"
    );

    let same_origin = dynamic_runtime(
        "https://lab.example/v1",
        Some("https://lab.example/other/v1"),
        ocg_domain::dynamic::DynamicAuthKind::Bearer,
    );
    let allowed = resolve_route_with_dynamics(
        &account,
        &config,
        &chat_plan(
            "vendor/lab",
            UpstreamChannel::Go,
            ApiFormat::ChatCompletions,
            None,
        ),
        std::slice::from_ref(&same_origin),
    )
    .unwrap();
    assert_eq!(allowed.base_url, "https://lab.example");
    assert_eq!(allowed.path, "/other/v1/chat/completions");
}

#[test]
fn s01_keyless_dynamic_override_may_use_another_origin() {
    let config = AppConfig::default();
    let runtime = dynamic_runtime(
        "https://lab.example/v1",
        Some("https://evil.example/v1"),
        ocg_domain::dynamic::DynamicAuthKind::None,
    );
    let mut account = account(
        "dyn-anon",
        &runtime.id,
        CredentialKind::None,
        QuotaScope::Key,
    );
    account.key_cipher.clear();
    let route = resolve_route_with_dynamics(
        &account,
        &config,
        &chat_plan(
            "vendor/lab",
            UpstreamChannel::Go,
            ApiFormat::ChatCompletions,
            None,
        ),
        std::slice::from_ref(&runtime),
    )
    .unwrap();
    assert_eq!(route.base_url, "https://evil.example");
    assert_eq!(route.auth, UpstreamAuth::None);
}
