use super::*;
use crate::alias::{self, ResolvedModel, RuntimeCatalogs};
use crate::crypto::{KeyCipher, StaticKeyCipher};
use crate::custom::CustomAccountRuntime;
use crate::gateway::protocol::{ApiFormat, parse_client_request};
use crate::gateway::provider_adapter::install_goat_loopback_route_for_test;
use crate::goat::GoatAccountRuntime;
use crate::models::{
    Account, AccountCustomConfig, AccountModelCapability, AccountSetupStep, AccountType, AppConfig,
};
use crate::provider::{
    COMMAND_CODE_GOAT_DEEPSEEK_V4_FLASH_ALIAS, COMMAND_CODE_GOAT_DEEPSEEK_V4_FLASH_UPSTREAM,
    COMMAND_CODE_PROVIDER_ID, CPA_ACCOUNT_ID, CPA_PROVIDER_ID, CUSTOM_PROVIDER_ID,
    ConnectionVerificationStatus, CredentialKind, OPENCODE_PROVIDER_ID,
    OPENCODE_ZEN_FREE_PROVIDER_ID, ProviderAdapterKind, QuotaScope, UpstreamProtocolKind,
    ZEN_FREE_ACCOUNT_ID, ZEN_FREE_ACCOUNT_NAME,
};
use bytes::Bytes;
use chrono::Utc;
use ocg_domain::credential::{AuthState, ModelScope};
use ocg_domain::destination::Credential as DestinationCredential;
use ocg_domain::destination::{
    AdapterKind, AuthScheme, Cooldowns, Destination, Grants, LegacyDestinationRef, ModelResolution,
    destination_id_for_builtin, destination_id_for_custom_account,
    destination_id_for_platform_account, sealed_capabilities,
};
use serde_json::json;
use std::collections::HashMap;
use std::sync::Arc;

const NO_IDS: &[String] = &[];

fn catalogs<'a>(
    zen_free: &'a [String],
    custom: &'a [String],
    command_code: &'a [String],
) -> RuntimeCatalogs<'a> {
    RuntimeCatalogs {
        go: NO_IDS,
        zen_free,
        custom,
        command_code,
        minimax: NO_IDS,
        kimi: NO_IDS,
        cpa: NO_IDS,
        ollama: NO_IDS,
        ollama_pinned: NO_IDS,
        extra: &[],
    }
}

fn resolve_with_custom(requested: &str, custom_model_ids: &[String]) -> ResolvedModel {
    alias::resolve_with_runtime_catalogs(requested, catalogs(NO_IDS, custom_model_ids, NO_IDS))
        .unwrap()
}

fn resolve_with_catalogs(
    requested: &str,
    zen_free_models: &[String],
    custom_model_ids: &[String],
    goat_model_ids: &[String],
) -> ResolvedModel {
    alias::resolve_with_runtime_catalogs(
        requested,
        catalogs(zen_free_models, custom_model_ids, goat_model_ids),
    )
    .unwrap()
}

fn chat_body(model: &str) -> Bytes {
    Bytes::from(
        serde_json::to_vec(&json!({
            "model": model,
            "messages": [{"role": "user", "content": "hi"}]
        }))
        .unwrap(),
    )
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

fn go_account(id: &str) -> Account {
    account(
        id,
        OPENCODE_PROVIDER_ID,
        CredentialKind::ApiKey,
        QuotaScope::Key,
    )
}

fn zen_account() -> Account {
    let mut item = account(
        ZEN_FREE_ACCOUNT_ID,
        OPENCODE_ZEN_FREE_PROVIDER_ID,
        CredentialKind::None,
        QuotaScope::EgressIp,
    );
    item.name = ZEN_FREE_ACCOUNT_NAME.into();
    item
}

fn cpa_account() -> Account {
    account(
        CPA_ACCOUNT_ID,
        CPA_PROVIDER_ID,
        CredentialKind::ApiKey,
        QuotaScope::Key,
    )
}

fn goat_runtime(id: &str, _models: &[&str]) -> GoatAccountRuntime {
    GoatAccountRuntime {
        account_id: id.into(),
        enabled: true,
        verification_status: ConnectionVerificationStatus::Verified,
        setup_ready: true,
        has_key: true,
    }
}

fn goat_runtimes(
    id: &str,
    models: &[&str],
) -> std::collections::HashMap<String, GoatAccountRuntime> {
    let mut runtimes = std::collections::HashMap::new();
    runtimes.insert(id.to_string(), goat_runtime(id, models));
    runtimes
}

fn goat_account(id: &str) -> Account {
    account(
        id,
        COMMAND_CODE_PROVIDER_ID,
        CredentialKind::ApiKey,
        QuotaScope::Key,
    )
}

fn example_zen_catalog() -> crate::zen_models::ZenFreeModelCatalog {
    crate::zen_models::ZenFreeModelCatalog {
        models: vec!["mimo-v2.5-free".into()],
        refreshed_at: Some(Utc::now()),
        source_url: crate::kernel::zen::ZEN_MODELS_SOURCE_URL.to_string(),
    }
}

fn persist_provider_catalog(
    persisted: &mut crate::provider_contracts::PersistedContracts,
    provider_id: &str,
    models: &[&str],
    source: &str,
    source_url: &str,
) {
    let now = Utc::now();
    let scope = crate::provider_contracts::ContractScope::provider(provider_id);
    persisted.scopes.insert(
        scope.clone(),
        crate::provider_contracts::PersistedScopeRow {
            scope,
            catalog_models: models.iter().map(|model| (*model).to_string()).collect(),
            catalog_refreshed_at: Some(now),
            catalog_source: source.into(),
            catalog_source_url: source_url.into(),
            revision: 1,
            updated_at: now,
        },
    );
}

fn persist_official_docs(
    persisted: &mut crate::provider_contracts::PersistedContracts,
    provider_id: &str,
    pairs: &[(&str, UpstreamProtocolKind)],
) {
    let scope = crate::provider_contracts::ContractScope::provider(provider_id);
    persisted.evidence.insert(
        scope.clone(),
        pairs
            .iter()
            .map(
                |(model_id, protocol)| crate::provider_contracts::PersistedModelProtocol {
                    scope: scope.clone(),
                    model_id: (*model_id).into(),
                    protocol: *protocol,
                    source: crate::provider_contracts::ContractEvidenceSource::Static,
                    verified_at: None,
                    observed_at: None,
                    last_probe_result: None,
                    last_probe_at: None,
                    last_probe_error: None,
                },
            )
            .collect(),
    );
}

fn refreshed_persisted() -> crate::provider_contracts::PersistedContracts {
    let mut persisted = crate::provider_contracts::PersistedContracts::default();
    persist_provider_catalog(
        &mut persisted,
        OPENCODE_PROVIDER_ID,
        &[
            "glm-5.1",
            "glm-5.2",
            "grok-4.5",
            "deepseek-v4-flash",
            "mimo-v2.5",
            "minimax-m3",
            "MiniMax-M3",
            "kimi-k3",
        ],
        crate::provider_contracts::CATALOG_SOURCE_OPENCODE_MODELS,
        crate::provider::OPENCODE_GO_BASE_URL,
    );
    persist_official_docs(
        &mut persisted,
        OPENCODE_PROVIDER_ID,
        &[
            ("glm-5.1", UpstreamProtocolKind::ChatCompletions),
            ("glm-5.2", UpstreamProtocolKind::ChatCompletions),
            ("grok-4.5", UpstreamProtocolKind::Responses),
            ("deepseek-v4-flash", UpstreamProtocolKind::ChatCompletions),
            ("mimo-v2.5", UpstreamProtocolKind::ChatCompletions),
            ("minimax-m3", UpstreamProtocolKind::ChatCompletions),
            ("MiniMax-M3", UpstreamProtocolKind::ChatCompletions),
            ("kimi-k3", UpstreamProtocolKind::ChatCompletions),
            ("kimi-k3", UpstreamProtocolKind::Messages),
        ],
    );
    persisted
}

fn static_contracts() -> crate::provider_contracts::EffectiveContractSet {
    contracts_for(&[])
}

fn resolve_model(model: &str) -> ResolvedModel {
    let zen = example_zen_catalog().models;
    alias::resolve_with_runtime_catalogs(
        model,
        alias::RuntimeCatalogs {
            zen_free: &zen,
            ..alias::RuntimeCatalogs::default()
        },
    )
    .unwrap()
}

fn goat_contracts(models: &[&str]) -> crate::provider_contracts::EffectiveContractSet {
    let now = Utc::now();
    let scope = crate::provider_contracts::ContractScope::provider(COMMAND_CODE_PROVIDER_ID);
    let mut persisted = refreshed_persisted();
    persisted.scopes.insert(
        scope.clone(),
        crate::provider_contracts::PersistedScopeRow {
            scope,
            catalog_models: models.iter().map(|model| (*model).to_string()).collect(),
            catalog_refreshed_at: Some(now),
            catalog_source: crate::provider_contracts::CATALOG_SOURCE_COMMAND_CODE_MODELS.into(),
            catalog_source_url: crate::provider::COMMAND_CODE_GOAT_BASE_URL.into(),
            revision: 1,
            updated_at: now,
        },
    );
    persisted.overrides.insert(
        crate::provider_contracts::ContractScope::provider(COMMAND_CODE_PROVIDER_ID),
        models
            .iter()
            .map(
                |model| crate::provider_contracts::PersistedModelProtocolOverride {
                    scope: crate::provider_contracts::ContractScope::provider(
                        COMMAND_CODE_PROVIDER_ID,
                    ),
                    model_id: (*model).to_string(),
                    protocol: if ocg_domain::protocol::command_code_is_anthropic_model(model) {
                        crate::provider::UpstreamProtocolKind::Messages
                    } else {
                        crate::provider::UpstreamProtocolKind::ChatCompletions
                    },
                    state: crate::provider_contracts::ProtocolOverrideState::ForceOn,
                    updated_at: now,
                },
            )
            .collect(),
    );
    crate::provider_contracts::build_effective_contracts(&example_zen_catalog(), &[], persisted)
}

fn contracts_for(
    runtimes: &[CustomAccountRuntime],
) -> crate::provider_contracts::EffectiveContractSet {
    crate::provider_contracts::build_effective_contracts(
        &example_zen_catalog(),
        runtimes,
        refreshed_persisted(),
    )
}

#[allow(clippy::too_many_arguments)]
fn materialize_account_routes(
    accounts: &[Account],
    config: &AppConfig,
    parsed: &crate::gateway::protocol::ParsedClientRequest,
    resolved: &ResolvedModel,
    client_model: &str,
    routing_model: &str,
    _client_body: &bytes::Bytes,
    free_available: bool,
    custom_runtimes: &HashMap<String, CustomAccountRuntime>,
    goat_runtimes: &HashMap<String, GoatAccountRuntime>,
    cpa_base_url: Option<&str>,
    contracts: &crate::provider_contracts::EffectiveContractSet,
    dynamics: &[crate::dynamic::DynamicProviderRuntime],
) -> Result<MaterializedRouteSet, crate::gateway::protocol::ProtocolError> {
    materialize_account_routes_with_bindings(
        accounts,
        config,
        parsed,
        resolved,
        client_model,
        routing_model,
        free_available,
        custom_runtimes,
        goat_runtimes,
        cpa_base_url,
        contracts,
        dynamics,
        &HashMap::new(),
        None,
    )
}

fn routes_for(
    model: &str,
    accounts: &[Account],
    config: &AppConfig,
    free_available: bool,
) -> MaterializedRouteSet {
    routes_for_with_contracts(model, accounts, config, free_available, &static_contracts())
}

fn routes_for_with_contracts(
    model: &str,
    accounts: &[Account],
    config: &AppConfig,
    free_available: bool,
    contracts: &crate::provider_contracts::EffectiveContractSet,
) -> MaterializedRouteSet {
    let body = chat_body(model);
    let parsed = parse_client_request(ApiFormat::ChatCompletions, body.clone()).unwrap();
    let resolved = resolve_model(model);
    materialize_account_routes(
        accounts,
        config,
        &parsed,
        &resolved,
        &parsed.requested_model,
        model,
        &body,
        free_available,
        &std::collections::HashMap::new(),
        &std::collections::HashMap::new(),
        None,
        contracts,
        &[],
    )
    .unwrap()
}

#[test]
fn go_alias_materializes_opencode_go_candidates() {
    let config = AppConfig::default();
    let set = routes_for(
        "glm-5.2",
        &[go_account("go-1"), zen_account()],
        &config,
        true,
    );
    assert_eq!(set.routes.len(), 1);
    assert_eq!(set.routes[0].routing.account.id, "go-1");
    assert_eq!(set.routes[0].plan.model, "glm-5.2");
    assert_eq!(set.routes[0].plan.client_model, "glm-5.2");
    assert_eq!(set.routes[0].plan.channel, UpstreamChannel::Go);
    assert!(!set.free_only);
    let identity = native_log_identity(&set.routes[0].plan);
    assert_eq!(identity.requested_model, "glm-5.2");
    assert_eq!(identity.resolved_alias.as_deref(), Some("glm-5.2"));
    assert_eq!(identity.upstream_model, "glm-5.2");
}

#[test]
fn cpa_preserves_client_protocol_for_known_and_unknown_models() {
    for model in ["vendor/cpa-new-model", "grok-4.5"] {
        let cpa_models = vec![model.to_string()];
        let resolved = alias::resolve_with_runtime_catalogs(
            model,
            alias::RuntimeCatalogs {
                cpa: &cpa_models,
                ..alias::RuntimeCatalogs::default()
            },
        )
        .unwrap();
        for client in [
            ApiFormat::ChatCompletions,
            ApiFormat::Responses,
            ApiFormat::Messages,
            ApiFormat::Gemini,
        ] {
            let body = Bytes::from(serde_json::to_vec(&match client {
                ApiFormat::Responses => json!({"model":model,"input":"hi","store":false}),
        ApiFormat::Gemini => json!({"contents":[{"role":"user","parts":[{"text":"hi"}]}]}),
        _ => json!({"model":model,"messages":[{"role":"user","content":"hi"}],"max_tokens":16}),
    }).unwrap());
            let parsed = if client == ApiFormat::Gemini {
                parse_gemini(model.into(), false, body.clone()).unwrap()
            } else {
                parse_client_request(client, body.clone()).unwrap()
            };
            let set = materialize_account_routes(
                &[cpa_account()],
                &AppConfig::default(),
                &parsed,
                &resolved,
                model,
                model,
                &body,
                true,
                &std::collections::HashMap::new(),
                &std::collections::HashMap::new(),
                Some(crate::cpa::DEFAULT_CPA_BASE_URL),
                &static_contracts(),
                &[],
            )
            .unwrap();
            assert_eq!(set.routes.len(), 1);
            let expected = if client == ApiFormat::Gemini {
                ApiFormat::ChatCompletions
            } else {
                client
            };
            assert_eq!(set.routes[0].plan.upstream, expected, "{model} {client:?}");
            assert_eq!(
                set.routes[0].plan.upstream_base_override.as_deref(),
                Some(crate::cpa::DEFAULT_CPA_BASE_URL)
            );
        }
    }
}

#[test]
fn mixed_case_go_alias_preserves_requested_casing() {
    let config = AppConfig::default();
    let set = routes_for("MiniMax-M3", &[go_account("go-1")], &config, true);
    assert_eq!(set.routes[0].plan.model, "MiniMax-M3");
    assert_eq!(set.routes[0].plan.client_model, "MiniMax-M3");
    assert_eq!(set.routes[0].plan.upstream, ApiFormat::ChatCompletions);
    let identity = native_log_identity(&set.routes[0].plan);
    assert_eq!(identity.requested_model, "MiniMax-M3");
    assert_eq!(identity.resolved_alias.as_deref(), Some("minimax-m3"));
    assert_eq!(identity.upstream_model, "MiniMax-M3");
}

#[test]
fn zen_free_alias_materializes_anonymous_channel() {
    let config = AppConfig::default();
    let set = routes_for(
        "mimo-v2.5-free",
        &[go_account("go-1"), zen_account()],
        &config,
        true,
    );
    assert!(set.free_only);
    assert_eq!(set.routes.len(), 1);
    assert_eq!(set.routes[0].routing.account.id, ZEN_FREE_ACCOUNT_ID);
    assert_eq!(set.routes[0].plan.channel, UpstreamChannel::Free);
    assert_eq!(set.routes[0].plan.model, "mimo-v2.5-free");
    assert!(set.routes[0].plan.upstream_base_override.is_some());
}

#[test]
fn shared_alias_builds_go_and_free_candidates_in_account_order() {
    let config = AppConfig::default();
    let set = routes_for(
        "mimo-v2.5",
        &[go_account("go-1"), zen_account()],
        &config,
        true,
    );
    assert_eq!(set.routes.len(), 2);
    assert_eq!(set.routes[0].routing.account.id, "go-1");
    assert_eq!(set.routes[0].plan.channel, UpstreamChannel::Go);
    assert_eq!(set.routes[1].routing.account.id, ZEN_FREE_ACCOUNT_ID);
    assert_eq!(set.routes[1].plan.channel, UpstreamChannel::Free);
    assert_eq!(set.routes[1].plan.model, "mimo-v2.5-free");
    assert_eq!(set.routes[1].plan.client_model, "mimo-v2.5");
    assert!(set.routes[1].plan.original_model.is_none());
    let free_identity = native_log_identity(&set.routes[1].plan);
    assert_eq!(free_identity.requested_model, "mimo-v2.5");
    assert_eq!(free_identity.resolved_alias.as_deref(), Some("mimo-v2.5"));
    assert_eq!(free_identity.upstream_model, "mimo-v2.5-free");
}

#[test]
fn pinned_raw_stays_pinned_to_its_provider() {
    let config = AppConfig::default();
    let body = chat_body("vendor.gadget-v1");
    let parsed = parse_client_request(ApiFormat::ChatCompletions, body.clone()).unwrap();
    let resolved = ResolvedModel::PinnedRaw {
        requested: "vendor.gadget-v1".into(),
        mapping: crate::alias::ProviderMapping {
            provider_id: OPENCODE_PROVIDER_ID.to_string(),

            upstream_model: "deepseek-v4-flash".into(),
            routeable: true,
        },
    };
    let set = materialize_account_routes(
        &[go_account("go-1"), zen_account()],
        &config,
        &parsed,
        &resolved,
        "vendor.gadget-v1",
        "vendor.gadget-v1",
        &body,
        true,
        &std::collections::HashMap::new(),
        &std::collections::HashMap::new(),
        None,
        &static_contracts(),
        &[],
    )
    .unwrap();
    assert_eq!(set.routes.len(), 1);
    assert_eq!(set.routes[0].routing.account.id, "go-1");
    assert_eq!(set.routes[0].plan.channel, UpstreamChannel::Go);
    assert_eq!(set.routes[0].plan.model, "deepseek-v4-flash");
    assert_eq!(set.routes[0].plan.client_model, "vendor.gadget-v1");
    assert!(set.routes[0].plan.original_model.is_none());
    let identity = native_log_identity(&set.routes[0].plan);
    assert_eq!(identity.requested_model, "vendor.gadget-v1");
    assert_eq!(
        identity.resolved_alias.as_deref(),
        Some("deepseek-v4-flash")
    );
    assert_eq!(identity.upstream_model, "deepseek-v4-flash");
}

#[test]
fn mapping_plans_follow_registry_order_while_candidates_keep_account_order() {
    let config = AppConfig::default();
    let body = chat_body("widget");
    let parsed = parse_client_request(ApiFormat::ChatCompletions, body.clone()).unwrap();
    let resolved = ResolvedModel::Alias {
        requested: "widget".into(),
        alias: "widget".into(),
        mappings: vec![
            crate::alias::ProviderMapping {
                provider_id: OPENCODE_ZEN_FREE_PROVIDER_ID.to_string(),

                upstream_model: "mimo-v2.5-free".into(),
                routeable: true,
            },
            crate::alias::ProviderMapping {
                provider_id: OPENCODE_PROVIDER_ID.to_string(),

                upstream_model: "glm-5.2".into(),
                routeable: true,
            },
        ],
    };
    let set = materialize_account_routes(
        &[go_account("go-1"), zen_account()],
        &config,
        &parsed,
        &resolved,
        "widget",
        "widget",
        &body,
        true,
        &std::collections::HashMap::new(),
        &std::collections::HashMap::new(),
        None,
        &static_contracts(),
        &[],
    )
    .unwrap();
    assert_eq!(set.routes.len(), 2);
    assert_eq!(set.routes[0].routing.account.id, "go-1");
    assert_eq!(set.routes[0].plan.channel, UpstreamChannel::Go);
    assert_eq!(set.routes[0].plan.model, "glm-5.2");
    assert_eq!(set.routes[1].routing.account.id, ZEN_FREE_ACCOUNT_ID);
    assert_eq!(set.routes[1].plan.channel, UpstreamChannel::Free);
    assert_eq!(set.routes[1].plan.model, "mimo-v2.5-free");
}

#[test]
fn pinned_raw_unverified_goat_is_fail_closed_through_adapter() {
    let config = AppConfig::default();
    let body = chat_body(COMMAND_CODE_GOAT_DEEPSEEK_V4_FLASH_UPSTREAM);
    let parsed = parse_client_request(ApiFormat::ChatCompletions, body.clone()).unwrap();
    let resolved = ResolvedModel::PinnedRaw {
        requested: COMMAND_CODE_GOAT_DEEPSEEK_V4_FLASH_UPSTREAM.into(),
        mapping: crate::alias::ProviderMapping {
            provider_id: COMMAND_CODE_PROVIDER_ID.to_string(),

            upstream_model: COMMAND_CODE_GOAT_DEEPSEEK_V4_FLASH_UPSTREAM.into(),
            routeable: true,
        },
    };
    let set = materialize_account_routes(
        &[goat_account("goat-1"), go_account("go-1")],
        &config,
        &parsed,
        &resolved,
        COMMAND_CODE_GOAT_DEEPSEEK_V4_FLASH_UPSTREAM,
        COMMAND_CODE_GOAT_DEEPSEEK_V4_FLASH_UPSTREAM,
        &body,
        true,
        &std::collections::HashMap::new(),
        &std::collections::HashMap::new(),
        None,
        &goat_contracts(&[COMMAND_CODE_GOAT_DEEPSEEK_V4_FLASH_UPSTREAM]),
        &[],
    )
    .unwrap();
    assert!(set.routes.is_empty());
    assert!(set.incompatibility.as_deref().is_some_and(|message| {
        message.contains("not verified")
            || message.contains("disabled")
            || message.contains("unsupported")
    }));
}

#[test]
fn goat_without_loopback_is_fail_closed() {
    let config = AppConfig::default();
    let set = routes_for(
        "glm-5.2",
        &[goat_account("goat-1"), go_account("go-1")],
        &config,
        true,
    );
    assert_eq!(set.routes.len(), 1);
    assert_eq!(set.routes[0].routing.account.id, "go-1");
}

#[test]
fn goat_alias_does_not_steal_go_requests_even_with_loopback() {
    let config = AppConfig::default();
    let goat = goat_account("goat-loop-alias");
    let _guard =
        install_goat_loopback_route_for_test(goat.id.clone(), "http://127.0.0.1:9").unwrap();
    let set = routes_for(
        COMMAND_CODE_GOAT_DEEPSEEK_V4_FLASH_ALIAS,
        &[goat, go_account("go-1")],
        &config,
        true,
    );
    assert_eq!(set.routes.len(), 1);
    assert_eq!(set.routes[0].routing.account.id, "go-1");
    assert_eq!(
        set.routes[0].plan.model,
        COMMAND_CODE_GOAT_DEEPSEEK_V4_FLASH_ALIAS
    );
    assert_eq!(set.routes[0].plan.upstream, ApiFormat::ChatCompletions);
}

#[test]
fn goat_slash_raw_pins_through_loopback_as_chat() {
    let config = AppConfig::default();
    let goat = goat_account("goat-loop-raw");
    let _guard =
        install_goat_loopback_route_for_test(goat.id.clone(), "http://127.0.0.1:9").unwrap();
    let body = chat_body(COMMAND_CODE_GOAT_DEEPSEEK_V4_FLASH_UPSTREAM);
    let parsed = parse_client_request(ApiFormat::ChatCompletions, body.clone()).unwrap();
    let resolved = resolve_with_catalogs(
        COMMAND_CODE_GOAT_DEEPSEEK_V4_FLASH_UPSTREAM,
        &[],
        &[],
        &[COMMAND_CODE_GOAT_DEEPSEEK_V4_FLASH_UPSTREAM.into()],
    );
    let set = materialize_account_routes(
        &[goat, go_account("go-1")],
        &config,
        &parsed,
        &resolved,
        COMMAND_CODE_GOAT_DEEPSEEK_V4_FLASH_UPSTREAM,
        COMMAND_CODE_GOAT_DEEPSEEK_V4_FLASH_UPSTREAM,
        &body,
        true,
        &std::collections::HashMap::new(),
        &goat_runtimes(
            "goat-loop-raw",
            &[COMMAND_CODE_GOAT_DEEPSEEK_V4_FLASH_UPSTREAM],
        ),
        None,
        &goat_contracts(&[COMMAND_CODE_GOAT_DEEPSEEK_V4_FLASH_UPSTREAM]),
        &[],
    )
    .unwrap();
    assert_eq!(set.routes.len(), 1);
    assert_eq!(set.routes[0].routing.account.id, "goat-loop-raw");
    assert_eq!(
        set.routes[0].plan.model,
        COMMAND_CODE_GOAT_DEEPSEEK_V4_FLASH_UPSTREAM
    );
    assert_eq!(set.routes[0].plan.upstream, ApiFormat::ChatCompletions);
    assert_eq!(
        set.routes[0].plan.client_model,
        COMMAND_CODE_GOAT_DEEPSEEK_V4_FLASH_UPSTREAM
    );
}

#[test]
fn goat_anthropic_alias_uses_messages_and_converts_client_responses() {
    let config = AppConfig::default();
    let goat = goat_account("goat-claude");
    let runtimes = goat_runtimes("goat-claude", &["claude-sonnet-4-6"]);
    let body = Bytes::from(
        serde_json::to_vec(&json!({
            "model": "claude-sonnet-4-6",
            "input": [{"role": "user", "content": "hi"}],
            "store": false
        }))
        .unwrap(),
    );
    let parsed = parse_client_request(ApiFormat::Responses, body.clone()).unwrap();
    let resolved =
        resolve_with_catalogs("claude-sonnet-4-6", &[], &[], &["claude-sonnet-4-6".into()]);
    let set = materialize_account_routes(
        &[goat],
        &config,
        &parsed,
        &resolved,
        "claude-sonnet-4-6",
        "claude-sonnet-4-6",
        &body,
        true,
        &std::collections::HashMap::new(),
        &runtimes,
        None,
        &goat_contracts(&["claude-sonnet-4-6"]),
        &[],
    )
    .unwrap();
    assert_eq!(set.routes.len(), 1);
    assert_eq!(set.routes[0].plan.client, ApiFormat::Responses);
    assert_eq!(set.routes[0].plan.upstream, ApiFormat::Messages);
    assert_eq!(set.routes[0].plan.model, "claude-sonnet-4-6");
}

#[test]
fn goat_slash_raw_without_loopback_is_fail_closed() {
    let config = AppConfig::default();
    let set = routes_for_with_contracts(
        COMMAND_CODE_GOAT_DEEPSEEK_V4_FLASH_UPSTREAM,
        &[goat_account("goat-1"), go_account("go-1")],
        &config,
        true,
        &goat_contracts(&[COMMAND_CODE_GOAT_DEEPSEEK_V4_FLASH_UPSTREAM]),
    );
    assert!(set.routes.is_empty());
    assert!(set.incompatibility.as_deref().is_some_and(|message| {
        message.contains("not verified")
            || message.contains("disabled")
            || message.contains("unsupported")
    }));
}

#[test]
fn r01_ambiguous_raw_model_id_fails_closed_without_outbound() {
    let error = protocol_error_from_resolve(crate::alias::ResolveError::Ambiguous {
        requested: "shared-raw".into(),
        mappings: vec![
            crate::alias::ProviderMapping {
                provider_id: OPENCODE_PROVIDER_ID.to_string(),

                upstream_model: "shared-raw".into(),
                routeable: true,
            },
            crate::alias::ProviderMapping {
                provider_id: OPENCODE_ZEN_FREE_PROVIDER_ID.to_string(),

                upstream_model: "shared-raw".into(),
                routeable: true,
            },
        ],
    });
    assert_eq!(error.status, axum::http::StatusCode::BAD_REQUEST);
    assert_eq!(error.code, Some(crate::alias::AMBIGUOUS_MODEL_ID));
    assert!(error.message.contains("shared-raw"), "{}", error.message);
}

#[test]
fn parse_helpers_are_reexported_for_adapters() {
    let parsed = parse_client(ApiFormat::ChatCompletions, chat_body("glm-5.2")).unwrap();
    assert_eq!(parsed.requested_model, "glm-5.2");
    let gemini = parse_gemini(
        "glm-5.2".into(),
        false,
        Bytes::from(
            serde_json::to_vec(&json!({"contents":[{"role":"user","parts":[{"text":"hi"}]}]}))
                .unwrap(),
        ),
    )
    .unwrap();
    assert_eq!(gemini.client, ApiFormat::Gemini);
}

#[test]
fn materialize_keeps_client_name_and_mapped_upstream_alias() {
    let body = Bytes::from(
        serde_json::to_vec(&json!({
            "model": "client-opus",
            "max_tokens": 1,
            "messages": [{"role": "user", "content": "hi"}]
        }))
        .unwrap(),
    );
    let parsed = parse_client_request(ApiFormat::Messages, body).unwrap();
    let plan = materialize_parsed_request(
        &parsed,
        &MaterializeSpec {
            client_model: parsed.requested_model.clone(),
            upstream_model: "glm-5.2".into(),
            resolved_alias: Some("glm-5.2".into()),
            channel: UpstreamChannel::Go,
            upstream_base_override: None,
            original_model: None,
            forced_upstream: None,
            custom_route: None,
        },
    )
    .unwrap();
    let identity = native_log_identity(&plan);
    assert_eq!(identity.requested_model, "client-opus");
    assert_eq!(identity.resolved_alias.as_deref(), Some("glm-5.2"));
    assert_eq!(identity.upstream_model, "glm-5.2");
}

fn custom_account(id: &str) -> Account {
    account(
        id,
        CUSTOM_PROVIDER_ID,
        CredentialKind::ApiKey,
        QuotaScope::Key,
    )
}

fn custom_runtime(
    account_id: &str,
    model_id: &str,
    protocol: UpstreamProtocolKind,
) -> CustomAccountRuntime {
    CustomAccountRuntime {
        account_id: account_id.into(),
        enabled: true,
        verification_status: ConnectionVerificationStatus::Verified,
        setup_ready: true,
        has_key: true,
        auth_kind: match protocol {
            UpstreamProtocolKind::Messages => ocg_domain::dynamic::DynamicAuthKind::XApiKey,
            UpstreamProtocolKind::ChatCompletions | UpstreamProtocolKind::Responses => {
                ocg_domain::dynamic::DynamicAuthKind::Bearer
            }
        },
        config: AccountCustomConfig {
            account_id: account_id.into(),
            endpoint_url: match protocol {
                UpstreamProtocolKind::ChatCompletions => "http://127.0.0.1:9/v1/chat/completions",
                UpstreamProtocolKind::Responses => "http://127.0.0.1:9/v1/responses",
                UpstreamProtocolKind::Messages => "http://127.0.0.1:9/v1/messages",
            }
            .into(),
            upstream_protocol: protocol,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        },
        capabilities: vec![AccountModelCapability {
            account_id: account_id.into(),
            public_model: model_id.into(),
            upstream_model: model_id.into(),
            protocol,
            verified_at: None,
            source: "manual".into(),
        }],
        route_overrides: Vec::new(),
        protocol_passthrough: false,
    }
}

#[test]
fn materialize_dispatches_builtin_and_custom_through_adapter_kinds() {
    assert_eq!(
        mapping_adapter_kind(&crate::alias::ProviderMapping {
            provider_id: OPENCODE_PROVIDER_ID.to_string(),

            upstream_model: "glm-5.2".into(),
            routeable: true
        }),
        Some(ProviderAdapterKind::OpenCodeGo)
    );
    assert_eq!(
        mapping_adapter_kind(&crate::alias::ProviderMapping {
            provider_id: OPENCODE_ZEN_FREE_PROVIDER_ID.to_string(),

            upstream_model: "mimo-v2.5-free".into(),
            routeable: true
        }),
        Some(ProviderAdapterKind::ZenFree)
    );
    assert_eq!(
        mapping_adapter_kind(&crate::alias::ProviderMapping {
            provider_id: CUSTOM_PROVIDER_ID.to_string(),

            upstream_model: "local".into(),
            routeable: true
        }),
        Some(ProviderAdapterKind::ConfigurableHttp)
    );
    assert!(!mapping_is_configurable_http(
        &crate::alias::ProviderMapping {
            provider_id: COMMAND_CODE_PROVIDER_ID.to_string(),

            upstream_model: COMMAND_CODE_GOAT_DEEPSEEK_V4_FLASH_UPSTREAM.into(),
            routeable: false
        }
    ));
}

#[test]
fn custom_candidate_diagnostic_passthrough_keeps_client_protocol() {
    let resolved = resolve_with_custom("local-custom", &["local-custom".into()]);
    assert_eq!(
        diagnostic_forced_upstream(&resolved, ApiFormat::Responses),
        Some(ApiFormat::Responses)
    );
    assert_eq!(
        diagnostic_forced_upstream(&resolved, ApiFormat::Messages),
        Some(ApiFormat::Messages)
    );
    let mixed = resolve_with_custom("hy3", &["hy3".into()]);
    assert_eq!(
        diagnostic_forced_upstream(&mixed, ApiFormat::Responses),
        Some(ApiFormat::Responses)
    );
    let builtin = alias::resolve("hy3").unwrap();
    assert_eq!(
        diagnostic_forced_upstream(&builtin, ApiFormat::Responses),
        None
    );
    let goat = resolve_with_catalogs("claude-sonnet-4-6", &[], &[], &["claude-sonnet-4-6".into()]);
    assert_eq!(
        diagnostic_forced_upstream(&goat, ApiFormat::Responses),
        Some(ApiFormat::Responses)
    );
    assert_eq!(
        diagnostic_forced_upstream(&goat, ApiFormat::Messages),
        Some(ApiFormat::Messages)
    );
    let zen = resolve_with_catalogs(
        "brand-new-promo",
        &["brand-new-promo-free".into()],
        &[],
        &[],
    );
    assert_eq!(
        diagnostic_forced_upstream(&zen, ApiFormat::ChatCompletions),
        Some(ApiFormat::ChatCompletions)
    );
    assert_eq!(
        diagnostic_forced_upstream(&zen, ApiFormat::Messages),
        Some(ApiFormat::ChatCompletions)
    );
}

#[test]
fn custom_native_responses_structured_format_does_not_guess_chat() {
    let body = Bytes::from(
        serde_json::to_vec(&json!({
            "model": "local-custom",
            "input": "hi",
            "store": false,
            "text": {
                "format": {
                    "type": "json_schema",
                    "name": "answer",
                    "schema": {"type": "object"}
                }
            }
        }))
        .unwrap(),
    );
    let parsed = parse_client_request(ApiFormat::Responses, body.clone()).unwrap();
    let resolved = resolve_with_custom("local-custom", &["local-custom".into()]);
    let account = custom_account("custom-1");
    let runtime = custom_runtime("custom-1", "local-custom", UpstreamProtocolKind::Responses);
    let mut runtimes = std::collections::HashMap::new();
    let contracts = contracts_for(std::slice::from_ref(&runtime));
    runtimes.insert(account.id.clone(), runtime);
    let set = materialize_account_routes(
        &[account],
        &AppConfig::default(),
        &parsed,
        &resolved,
        &parsed.requested_model,
        "local-custom",
        &body,
        false,
        &runtimes,
        &std::collections::HashMap::new(),
        None,
        &contracts,
        &[],
    )
    .expect("native Responses structured output must not be rejected via Chat conversion");
    assert_eq!(set.routes.len(), 1);
    assert_eq!(set.routes[0].plan.upstream, ApiFormat::Responses);
    assert_eq!(set.routes[0].plan.client, ApiFormat::Responses);
    let upstream: serde_json::Value = serde_json::from_slice(&set.routes[0].plan.body).unwrap();
    assert_eq!(upstream["text"]["format"]["type"], "json_schema");
}

#[test]
fn custom_native_messages_structured_format_does_not_guess_chat() {
    let body = Bytes::from(
        serde_json::to_vec(&json!({
            "model": "local-custom",
            "max_tokens": 16,
            "messages": [{"role": "user", "content": "hi"}],
            "output_config": {
                "format": {"type": "json_schema", "schema": {"type": "object"}}
            }
        }))
        .unwrap(),
    );
    let parsed = parse_client_request(ApiFormat::Messages, body.clone()).unwrap();
    let resolved = resolve_with_custom("local-custom", &["local-custom".into()]);
    let account = custom_account("custom-1");
    let runtime = custom_runtime("custom-1", "local-custom", UpstreamProtocolKind::Messages);
    let mut runtimes = std::collections::HashMap::new();
    let contracts = contracts_for(std::slice::from_ref(&runtime));
    runtimes.insert(account.id.clone(), runtime);
    let set = materialize_account_routes(
        &[account],
        &AppConfig::default(),
        &parsed,
        &resolved,
        &parsed.requested_model,
        "local-custom",
        &body,
        false,
        &runtimes,
        &std::collections::HashMap::new(),
        None,
        &contracts,
        &[],
    )
    .expect("native Messages structured output must not be rejected via Chat conversion");
    assert_eq!(set.routes.len(), 1);
    assert_eq!(set.routes[0].plan.upstream, ApiFormat::Messages);
    let upstream: serde_json::Value = serde_json::from_slice(&set.routes[0].plan.body).unwrap();
    assert_eq!(upstream["output_config"]["format"]["type"], "json_schema");
}

#[test]
fn custom_model_override_owns_protocol_endpoint_and_auth_independently() {
    let body = chat_body("local-custom");
    let parsed = parse_client_request(ApiFormat::ChatCompletions, body.clone()).unwrap();
    let resolved = resolve_with_custom("local-custom", &["local-custom".into()]);
    let account = custom_account("custom-override");
    let mut runtime = custom_runtime(
        "custom-override",
        "local-custom",
        UpstreamProtocolKind::ChatCompletions,
    );
    runtime.auth_kind = ocg_domain::dynamic::DynamicAuthKind::XApiKey;
    runtime.capabilities[0].protocol = UpstreamProtocolKind::Messages;
    runtime.route_overrides.push((
        "local-custom".into(),
        ocg_domain::dynamic::DynamicModelUpstreamOverride {
            protocol: UpstreamProtocolKind::Messages,
            endpoint_url: "http://127.0.0.1:9/alternate/messages".into(),
        },
    ));
    let contracts = contracts_for(std::slice::from_ref(&runtime));
    let mut runtimes = std::collections::HashMap::new();
    runtimes.insert(account.id.clone(), runtime);
    let set = materialize_account_routes(
        &[account],
        &AppConfig::default(),
        &parsed,
        &resolved,
        &parsed.requested_model,
        "local-custom",
        &body,
        false,
        &runtimes,
        &std::collections::HashMap::new(),
        None,
        &contracts,
        &[],
    )
    .unwrap();
    let plan = &set.routes[0].plan;
    assert_eq!(plan.upstream, ApiFormat::Messages);
    assert_eq!(
        plan.custom_route,
        Some(CustomRouteSpec {
            endpoint_url: "http://127.0.0.1:9/alternate/messages".into(),
            auth_kind: ocg_domain::dynamic::DynamicAuthKind::XApiKey,
        })
    );
}

#[test]
fn custom_single_protocol_converts_other_client_wire_formats() {
    fn route_upstream(client: ApiFormat, body: Bytes) -> ApiFormat {
        let parsed = if client == ApiFormat::Gemini {
            parse_gemini("local-custom".into(), false, body.clone()).unwrap()
        } else {
            parse_client_request(client, body.clone()).unwrap()
        };
        let resolved = resolve_with_custom("local-custom", &["local-custom".into()]);
        let account = custom_account("custom-single");
        let runtime = custom_runtime(
            "custom-single",
            "local-custom",
            UpstreamProtocolKind::Messages,
        );
        let contracts = contracts_for(std::slice::from_ref(&runtime));
        let mut runtimes = std::collections::HashMap::new();
        runtimes.insert(account.id.clone(), runtime);
        let set = materialize_account_routes(
            &[account],
            &AppConfig::default(),
            &parsed,
            &resolved,
            &parsed.requested_model,
            "local-custom",
            &body,
            false,
            &runtimes,
            &std::collections::HashMap::new(),
            None,
            &contracts,
            &[],
        )
        .expect("single-protocol account must convert supported client formats");
        assert_eq!(set.routes.len(), 1);
        set.routes[0].plan.upstream
    }

    let chat = route_upstream(ApiFormat::ChatCompletions, chat_body("local-custom"));
    assert_eq!(chat, ApiFormat::Messages);
    let responses = route_upstream(
        ApiFormat::Responses,
        Bytes::from(
            serde_json::to_vec(&json!({
                "model": "local-custom",
                "input": "hi",
                "store": false,
                "max_output_tokens": 4
            }))
            .unwrap(),
        ),
    );
    assert_eq!(responses, ApiFormat::Messages);
    let messages = route_upstream(
        ApiFormat::Messages,
        Bytes::from(
            serde_json::to_vec(&json!({
                "model": "local-custom",
                "max_tokens": 4,
                "messages": [{"role": "user", "content": "hi"}]
            }))
            .unwrap(),
        ),
    );
    assert_eq!(messages, ApiFormat::Messages);
    let gemini = route_upstream(
        ApiFormat::Gemini,
        Bytes::from(
            serde_json::to_vec(&json!({
                "contents": [{"role": "user", "parts": [{"text": "hi"}]}],
                "generationConfig": {"maxOutputTokens": 4}
            }))
            .unwrap(),
        ),
    );
    assert_eq!(gemini, ApiFormat::Messages);
}

#[test]
fn platform_passthrough_keeps_matching_client_protocols() {
    fn route_upstream(client: ApiFormat, body: Bytes) -> crate::gateway::protocol::RequestPlan {
        let parsed = if client == ApiFormat::Gemini {
            parse_gemini("local-custom".into(), false, body.clone()).unwrap()
        } else {
            parse_client_request(client, body.clone()).unwrap()
        };
        let resolved = resolve_with_custom("local-custom", &["local-custom".into()]);
        let account = custom_account("platform-key");
        let mut runtime = custom_runtime(
            "platform-key",
            "local-custom",
            UpstreamProtocolKind::ChatCompletions,
        );
        runtime.config.endpoint_url = "http://127.0.0.1:9".into();
        runtime.protocol_passthrough = true;
        let contracts = contracts_for(std::slice::from_ref(&runtime));
        let mut runtimes = std::collections::HashMap::new();
        runtimes.insert(account.id.clone(), runtime);
        let set = materialize_account_routes(
            &[account],
            &AppConfig::default(),
            &parsed,
            &resolved,
            &parsed.requested_model,
            "local-custom",
            &body,
            false,
            &runtimes,
            &std::collections::HashMap::new(),
            None,
            &contracts,
            &[],
        )
        .expect("platform-linked Keys pass matching client protocols through");
        assert_eq!(set.routes.len(), 1);
        set.routes[0].plan.clone()
    }

    let chat = route_upstream(ApiFormat::ChatCompletions, chat_body("local-custom"));
    assert_eq!(chat.upstream, ApiFormat::ChatCompletions);
    assert_eq!(
        chat.custom_route
            .as_ref()
            .map(|route| route.endpoint_url.as_str()),
        Some("http://127.0.0.1:9")
    );
    let messages = route_upstream(
        ApiFormat::Messages,
        Bytes::from(
            serde_json::to_vec(&json!({
                "model": "local-custom",
                "max_tokens": 4,
                "messages": [{"role": "user", "content": "hi"}]
            }))
            .unwrap(),
        ),
    );
    assert_eq!(messages.upstream, ApiFormat::Messages);
    let responses = route_upstream(
        ApiFormat::Responses,
        Bytes::from(
            serde_json::to_vec(&json!({
                "model": "local-custom",
                "input": "hi",
                "store": false,
                "max_output_tokens": 4
            }))
            .unwrap(),
        ),
    );
    assert_eq!(responses.upstream, ApiFormat::Responses);
    let gemini = route_upstream(
        ApiFormat::Gemini,
        Bytes::from(
            serde_json::to_vec(&json!({
                "contents": [{"role": "user", "parts": [{"text": "hi"}]}],
                "generationConfig": {"maxOutputTokens": 4}
            }))
            .unwrap(),
        ),
    );
    assert_eq!(gemini.upstream, ApiFormat::ChatCompletions);
}

#[test]
fn custom_without_scope_contract_does_not_produce_a_candidate() {
    let body = chat_body("local-custom");
    let parsed = parse_client_request(ApiFormat::ChatCompletions, body.clone()).unwrap();
    let resolved = resolve_with_custom("local-custom", &["local-custom".into()]);
    let account = custom_account("custom-missing-scope");
    let runtime = custom_runtime(
        "custom-missing-scope",
        "local-custom",
        UpstreamProtocolKind::ChatCompletions,
    );
    let mut runtimes = std::collections::HashMap::new();
    runtimes.insert(account.id.clone(), runtime);
    let set = materialize_account_routes(
        &[account],
        &AppConfig::default(),
        &parsed,
        &resolved,
        &parsed.requested_model,
        "local-custom",
        &body,
        false,
        &runtimes,
        &std::collections::HashMap::new(),
        None,
        &static_contracts(),
        &[],
    )
    .expect(
        "missing custom contract must fail closed without a protocol error for mixed resolution",
    );
    assert!(
        set.routes.is_empty(),
        "no production Custom candidate without ContractScope::CustomEndpoint"
    );
    assert!(set.incompatibility.as_deref().is_some_and(|message| {
        message.contains("no effective contract") || message.contains("custom_endpoint")
    }));
}

#[test]
fn model_preference_survives_legacy_probe_evidence() {
    let body = chat_body("grok-4.5");
    let parsed = parse_client_request(ApiFormat::ChatCompletions, body.clone()).unwrap();
    let resolved = alias::resolve("grok-4.5").unwrap();
    let account = go_account("go-probe");
    let before = materialize_account_routes(
        std::slice::from_ref(&account),
        &AppConfig::default(),
        &parsed,
        &resolved,
        &parsed.requested_model,
        "grok-4.5",
        &body,
        false,
        &std::collections::HashMap::new(),
        &std::collections::HashMap::new(),
        None,
        &static_contracts(),
        &[],
    )
    .unwrap();
    assert_eq!(before.routes.len(), 1);
    assert_eq!(before.routes[0].plan.upstream, ApiFormat::Responses);

    let now = Utc::now();
    let mut persisted = refreshed_persisted();
    let scope = crate::provider_contracts::ContractScope::provider(OPENCODE_PROVIDER_ID);
    persisted.evidence.entry(scope.clone()).or_default().push(
        crate::provider_contracts::PersistedModelProtocol {
            scope,
            model_id: "grok-4.5".into(),
            protocol: UpstreamProtocolKind::ChatCompletions,
            source: crate::provider_contracts::ContractEvidenceSource::ProbeConfirmed,
            verified_at: Some(now),
            observed_at: Some(now),
            last_probe_result: Some(crate::provider_contracts::ProbeResultKind::Success),
            last_probe_at: Some(now),
            last_probe_error: None,
        },
    );
    let contracts = crate::provider_contracts::build_effective_contracts(
        &example_zen_catalog(),
        &[],
        persisted,
    );
    let after = materialize_account_routes(
        &[account],
        &AppConfig::default(),
        &parsed,
        &resolved,
        &parsed.requested_model,
        "grok-4.5",
        &body,
        false,
        &std::collections::HashMap::new(),
        &std::collections::HashMap::new(),
        None,
        &contracts,
        &[],
    )
    .unwrap();
    assert_eq!(after.routes.len(), 1);
    assert_eq!(after.routes[0].plan.upstream, ApiFormat::ChatCompletions);
    assert_eq!(
        contracts
            .providers
            .get(OPENCODE_PROVIDER_ID)
            .unwrap()
            .model("grok-4.5")
            .unwrap()
            .preferred_protocol,
        UpstreamProtocolKind::Responses
    );
    assert_eq!(after.routes[0].routing.account.id, "go-probe");
}

fn routes_for_with_bindings(
    model: &str,
    accounts: &[Account],
    bindings: &InferenceBindingIndex,
) -> MaterializedRouteSet {
    let body = chat_body(model);
    let parsed = parse_client_request(ApiFormat::ChatCompletions, body.clone()).unwrap();
    let resolved = resolve_model(model);
    materialize_account_routes_with_bindings(
        accounts,
        &AppConfig::default(),
        &parsed,
        &resolved,
        &parsed.requested_model,
        model,
        true,
        &HashMap::new(),
        &HashMap::new(),
        None,
        &static_contracts(),
        &[],
        bindings,
        None,
    )
    .unwrap()
}

#[test]
fn d01_model_scope_keeps_x_off_key_b_without_disabling_other_models() {
    let accounts = [go_account("key-a"), go_account("key-b")];
    let mut bindings = InferenceBindingIndex::new();
    bindings.insert(
        "key-a".into(),
        InferenceBindingGate {
            enabled: true,
            model_scope: ModelScope::All,
        },
    );
    bindings.insert(
        "key-b".into(),
        InferenceBindingGate {
            enabled: true,
            model_scope: ModelScope::Only {
                models: vec!["glm-5.1".into()],
            },
        },
    );

    let request_x = routes_for_with_bindings("glm-5.2", &accounts, &bindings);
    let x_ids: Vec<_> = request_x
        .routes
        .iter()
        .map(|route| route.routing.account.id.as_str())
        .collect();
    assert_eq!(x_ids, vec!["key-a"]);
    assert!(
        request_x
            .rejected
            .iter()
            .any(|reason| reason.contains("key-b") && reason.contains("model scope")),
        "{:?}",
        request_x.rejected
    );
    assert!(
        request_x.rejections.iter().any(|rejection| rejection.code
            == RouteRejectionCode::ModelScopeDenied
            && rejection.account_id.as_deref() == Some("key-b")),
        "{:?}",
        request_x.rejections
    );

    let request_other = routes_for_with_bindings("glm-5.1", &accounts, &bindings);
    let other_ids: Vec<_> = request_other
        .routes
        .iter()
        .map(|route| route.routing.account.id.as_str())
        .collect();
    assert_eq!(other_ids, vec!["key-a", "key-b"]);
}

#[test]
fn d01_disabled_binding_is_skipped_and_default_all_preserves_routes() {
    let accounts = [go_account("key-a"), go_account("key-b")];
    let mut bindings = InferenceBindingIndex::new();
    bindings.insert(
        "key-b".into(),
        InferenceBindingGate {
            enabled: false,
            model_scope: ModelScope::All,
        },
    );
    let set = routes_for_with_bindings("glm-5.2", &accounts, &bindings);
    let ids: Vec<_> = set
        .routes
        .iter()
        .map(|route| route.routing.account.id.as_str())
        .collect();
    assert_eq!(ids, vec!["key-a"]);
    assert!(
        set.rejected
            .iter()
            .any(|reason| reason.contains("key-b") && reason.contains("disabled")),
        "{:?}",
        set.rejected
    );
    assert!(
        set.rejections.iter().any(|rejection| rejection.code
            == RouteRejectionCode::BindingDisabled
            && rejection.account_id.as_deref() == Some("key-b")),
        "{:?}",
        set.rejections
    );

    let unrestricted = routes_for("glm-5.2", &accounts, &AppConfig::default(), true);
    let unrestricted_ids: Vec<_> = unrestricted
        .routes
        .iter()
        .map(|route| route.routing.account.id.as_str())
        .collect();
    assert_eq!(unrestricted_ids, vec!["key-a", "key-b"]);
}

fn routes_for_client(
    client: ApiFormat,
    model: &str,
    accounts: &[Account],
    contracts: &crate::provider_contracts::EffectiveContractSet,
    custom_runtimes: &HashMap<String, CustomAccountRuntime>,
    resolved: &ResolvedModel,
) -> MaterializedRouteSet {
    let body = Bytes::from(
        serde_json::to_vec(&match client {
            ApiFormat::Responses => json!({"model": model, "input": "hi", "store": false}),
            ApiFormat::Messages => json!({
                "model": model,
                "max_tokens": 16,
                "messages": [{"role": "user", "content": "hi"}]
            }),
            _ => json!({
                "model": model,
                "messages": [{"role": "user", "content": "hi"}]
            }),
        })
        .unwrap(),
    );
    let parsed = parse_client_request(client, body.clone()).unwrap();
    materialize_account_routes(
        accounts,
        &AppConfig::default(),
        &parsed,
        resolved,
        &parsed.requested_model,
        model,
        &body,
        true,
        custom_runtimes,
        &HashMap::new(),
        None,
        contracts,
        &[],
    )
    .unwrap()
}

#[test]
fn p03_changing_convert_default_keeps_other_native_capability() {
    let accounts = [go_account("go-1")];
    let resolved = alias::resolve("kimi-k3").unwrap();
    let before = routes_for_client(
        ApiFormat::ChatCompletions,
        "kimi-k3",
        &accounts,
        &static_contracts(),
        &HashMap::new(),
        &resolved,
    );
    assert_eq!(before.routes[0].plan.upstream, ApiFormat::ChatCompletions);
    let before_messages = routes_for_client(
        ApiFormat::Messages,
        "kimi-k3",
        &accounts,
        &static_contracts(),
        &HashMap::new(),
        &resolved,
    );
    assert_eq!(before_messages.routes[0].plan.upstream, ApiFormat::Messages);

    let scope = crate::provider_contracts::ContractScope::provider(OPENCODE_PROVIDER_ID);
    let mut persisted = refreshed_persisted();
    persisted.preferences.insert(
        scope,
        vec![("kimi-k3".into(), UpstreamProtocolKind::Messages)],
    );
    let contracts = crate::provider_contracts::build_effective_contracts(
        &example_zen_catalog(),
        &[],
        persisted,
    );
    assert_eq!(
        contracts
            .providers
            .get(OPENCODE_PROVIDER_ID)
            .unwrap()
            .model("kimi-k3")
            .unwrap()
            .preferred_protocol,
        UpstreamProtocolKind::Messages
    );

    let after_chat = routes_for_client(
        ApiFormat::ChatCompletions,
        "kimi-k3",
        &accounts,
        &contracts,
        &HashMap::new(),
        &resolved,
    );
    let after_messages = routes_for_client(
        ApiFormat::Messages,
        "kimi-k3",
        &accounts,
        &contracts,
        &HashMap::new(),
        &resolved,
    );
    assert_eq!(
        after_chat.routes[0].plan.upstream,
        ApiFormat::ChatCompletions,
        "changing the convert default must not drop native Chat"
    );
    assert_eq!(
        after_messages.routes[0].plan.upstream,
        ApiFormat::Messages,
        "changing the convert default must not drop native Messages"
    );
}

#[test]
fn p04_does_not_skip_higher_priority_convertible_subscription_for_later_native() {
    let go = go_account("sub-1");
    let custom = custom_account("api-1");
    let runtime = custom_runtime("api-1", "grok-4.5", UpstreamProtocolKind::ChatCompletions);
    let contracts = contracts_for(std::slice::from_ref(&runtime));
    let mut runtimes = HashMap::new();
    runtimes.insert(custom.id.clone(), runtime);
    let resolved = ResolvedModel::Alias {
        requested: "grok-4.5".into(),
        alias: "grok-4.5".into(),
        mappings: vec![
            crate::alias::ProviderMapping {
                provider_id: OPENCODE_PROVIDER_ID.to_string(),
                upstream_model: "grok-4.5".into(),
                routeable: true,
            },
            crate::alias::ProviderMapping {
                provider_id: CUSTOM_PROVIDER_ID.to_string(),
                upstream_model: "grok-4.5".into(),
                routeable: true,
            },
        ],
    };
    let set = routes_for_client(
        ApiFormat::ChatCompletions,
        "grok-4.5",
        &[go, custom],
        &contracts,
        &runtimes,
        &resolved,
    );
    let ids: Vec<_> = set
        .routes
        .iter()
        .map(|route| route.routing.account.id.as_str())
        .collect();
    assert_eq!(ids, vec!["sub-1", "api-1"]);
    assert_eq!(set.routes[0].plan.upstream, ApiFormat::Responses);
    assert_eq!(set.routes[1].plan.upstream, ApiFormat::ChatCompletions);
}

#[test]
fn r06_live_reread_skips_disabled_or_revoked_stale_snapshot() {
    use crate::routing_runtime::account_is_available_for;

    let snapshot = [go_account("a"), go_account("b")];
    let first = routes_for("glm-5.2", &snapshot, &AppConfig::default(), true);
    assert_eq!(first.routes.len(), 2);

    let mut disabled = snapshot[1].clone();
    disabled.enabled = false;
    assert!(account_is_available_for(
        &snapshot[1],
        UpstreamChannel::Go,
        &[]
    ));
    assert!(!account_is_available_for(
        &disabled,
        UpstreamChannel::Go,
        &["a"]
    ));

    let mut revoked = snapshot[1].clone();
    revoked.auth_error = Some("credential revoked".into());
    assert!(!account_is_available_for(
        &revoked,
        UpstreamChannel::Go,
        &[]
    ));

    let live = [snapshot[0].clone(), disabled];
    let rematerialized = routes_for("glm-5.2", &live, &AppConfig::default(), true);
    let usable: Vec<_> = rematerialized
        .routes
        .iter()
        .filter(|route| {
            account_is_available_for(&route.routing.account, route.routing.channel, &["a"])
        })
        .collect();
    assert!(
        usable.is_empty(),
        "a live disable/revoke must skip the stale snapshot candidate: {:?}",
        rematerialized
            .routes
            .iter()
            .map(|route| (
                route.routing.account.id.as_str(),
                route.routing.account.enabled
            ))
            .collect::<Vec<_>>()
    );
}

#[test]
fn r08_cpa_routing_does_not_assume_local_account_or_unbounded_retry() {
    let resolved = alias::resolve_with_runtime_catalogs(
        "vendor/cpa-new-model",
        alias::RuntimeCatalogs {
            cpa: &["vendor/cpa-new-model".to_string()],
            ..alias::RuntimeCatalogs::default()
        },
    )
    .unwrap();
    let body = chat_body("vendor/cpa-new-model");
    let parsed = parse_client_request(ApiFormat::ChatCompletions, body.clone()).unwrap();
    let missing = materialize_account_routes(
        &[cpa_account()],
        &AppConfig::default(),
        &parsed,
        &resolved,
        "vendor/cpa-new-model",
        "vendor/cpa-new-model",
        &body,
        true,
        &HashMap::new(),
        &HashMap::new(),
        None,
        &static_contracts(),
        &[],
    );
    assert!(
        missing.is_err(),
        "CPA without a configured base must fail closed"
    );

    let cpa = account(
        "cpa-adapter-1",
        CPA_PROVIDER_ID,
        CredentialKind::ApiKey,
        QuotaScope::Key,
    );
    let set = materialize_account_routes(
        &[cpa],
        &AppConfig::default(),
        &parsed,
        &resolved,
        "vendor/cpa-new-model",
        "vendor/cpa-new-model",
        &body,
        true,
        &HashMap::new(),
        &HashMap::new(),
        Some(crate::cpa::DEFAULT_CPA_BASE_URL),
        &static_contracts(),
        &[],
    )
    .unwrap();
    assert_eq!(
        set.routes.len(),
        1,
        "CPA routes by adapter, not the reserved account id: {:?}",
        set.rejected
    );
    assert_eq!(
        set.routes[0].routing.adapter,
        crate::provider::ProviderAdapterKind::Cpa
    );

    let go = account(
        "local-oauth-1",
        OPENCODE_PROVIDER_ID,
        CredentialKind::ApiKey,
        QuotaScope::Key,
    );
    let set = materialize_account_routes(
        &[go],
        &AppConfig::default(),
        &parsed,
        &resolved,
        "vendor/cpa-new-model",
        "vendor/cpa-new-model",
        &body,
        true,
        &HashMap::new(),
        &HashMap::new(),
        Some(crate::cpa::DEFAULT_CPA_BASE_URL),
        &static_contracts(),
        &[],
    )
    .unwrap();
    assert!(
        set.routes.is_empty(),
        "a non-CPA adapter must not become a CPA candidate: {:?}",
        set.routes
            .iter()
            .map(|route| route.routing.account.id.as_str())
            .collect::<Vec<_>>()
    );
}

fn mapping(provider_id: &str, model: &str) -> crate::alias::ProviderMapping {
    crate::alias::ProviderMapping {
        provider_id: provider_id.to_string(),
        upstream_model: model.into(),
        routeable: true,
    }
}

fn test_destination(adapter: AdapterKind, legacy: LegacyDestinationRef) -> Destination {
    let id = match &legacy {
        LegacyDestinationRef::Builtin(id) | LegacyDestinationRef::Dynamic(id) => {
            destination_id_for_builtin(id)
        }
        LegacyDestinationRef::CustomAccount(id) => destination_id_for_custom_account(id),
        LegacyDestinationRef::PlatformParent(id) => destination_id_for_platform_account(id),
    };
    let model_resolution = match &legacy {
        LegacyDestinationRef::Builtin(_) => ModelResolution::AdapterDefined,
        LegacyDestinationRef::Dynamic(_) => ModelResolution::PublicAndUpstream,
        LegacyDestinationRef::CustomAccount(_) | LegacyDestinationRef::PlatformParent(_) => {
            ModelResolution::PublicOnly
        }
    };
    Destination {
        id,
        legacy,
        adapter,
        name: "dest".into(),
        brand_family: None,
        base_url: None,
        protocols: Vec::new(),
        auth_scheme: AuthScheme::Bearer,
        model_resolution,
        catalog: Vec::new(),
        capabilities: sealed_capabilities(adapter),
        plan: None,
        max_credentials: None,
        observer_credential_id: None,
        enabled: true,
    }
}

fn test_credential(account_id: &str, destination_id: &str) -> DestinationCredential {
    DestinationCredential {
        id: format!("cred-{account_id}"),
        legacy_account_id: account_id.into(),
        destination_id: destination_id.into(),
        name: account_id.into(),
        notes: None,
        has_secret: true,
        enabled: true,
        routing_rank: 0,
        scope: ModelScope::All,
        grants: Grants {
            allowed_endpoint_ids: Vec::new(),
            allowed_origins: Vec::new(),
        },
        auth_state: AuthState::Unknown,
        last_error: None,
        cooldowns: Cooldowns {
            generic_until: None,
            five_hour_until: None,
            week_until: None,
            month_until: None,
            free_until: None,
        },
        quota_pool_id: None,
        onboarding_task: None,
        purchase_date: None,
    }
}

#[test]
fn destination_match_uses_adapter_and_legacy_not_custom_predicate() {
    let custom = test_destination(
        AdapterKind::Http,
        LegacyDestinationRef::CustomAccount("api-1".into()),
    );
    let platform = test_destination(
        AdapterKind::Http,
        LegacyDestinationRef::PlatformParent("plat-1".into()),
    );
    let go = test_destination(
        AdapterKind::OpencodeGo,
        LegacyDestinationRef::Builtin(OPENCODE_PROVIDER_ID.into()),
    );
    let custom_mapping = mapping(CUSTOM_PROVIDER_ID, "local-custom");
    let go_mapping = mapping(OPENCODE_PROVIDER_ID, "glm-5.2");
    let dynamic_mapping = mapping("aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa", "dyn-model");

    assert!(destination_matches_mapping(&custom, &custom_mapping));
    assert!(destination_matches_mapping(&platform, &custom_mapping));
    assert!(!destination_matches_mapping(&custom, &go_mapping));
    assert!(!destination_matches_mapping(&platform, &go_mapping));
    assert!(!destination_matches_mapping(&custom, &dynamic_mapping));
    assert!(destination_matches_mapping(&go, &go_mapping));
    assert!(!destination_matches_mapping(&go, &custom_mapping));
    assert!(mapping_is_custom_http_catalog(&custom_mapping));
    assert!(!mapping_is_custom_http_catalog(&dynamic_mapping));
    assert!(!mapping_is_custom_http_catalog(&go_mapping));
}

#[test]
fn projection_supplies_adapter_and_matches_custom_without_reserved_account_id() {
    let account = custom_account("owned-http");
    let destination = test_destination(
        AdapterKind::Http,
        LegacyDestinationRef::CustomAccount(account.id.clone()),
    );
    let projection = crate::destination_projection::DestinationProjection {
        destinations: vec![destination.clone()],
        credentials: vec![test_credential(&account.id, &destination.id)],
    };
    let runtime = custom_runtime(
        &account.id,
        "local-custom",
        UpstreamProtocolKind::ChatCompletions,
    );
    let mut runtimes = HashMap::new();
    let contracts = contracts_for(std::slice::from_ref(&runtime));
    runtimes.insert(account.id.clone(), runtime);
    let body = chat_body("local-custom");
    let parsed = parse_client_request(ApiFormat::ChatCompletions, body.clone()).unwrap();
    let resolved = resolve_with_custom("local-custom", &["local-custom".into()]);
    let set = materialize_account_routes_with_bindings(
        &[account],
        &AppConfig::default(),
        &parsed,
        &resolved,
        "local-custom",
        "local-custom",
        true,
        &runtimes,
        &HashMap::new(),
        None,
        &contracts,
        &[],
        &HashMap::new(),
        Some(&projection),
    )
    .unwrap();
    assert_eq!(set.routes.len(), 1, "{:?}", set.rejected);
    assert_eq!(
        set.routes[0].routing.adapter,
        ProviderAdapterKind::ConfigurableHttp
    );
}

#[test]
fn leftover_row_adapter_comes_from_mapping_catalog_not_account_id() {
    let cpa = account(
        "cpa-leftover",
        CPA_PROVIDER_ID,
        CredentialKind::ApiKey,
        QuotaScope::Key,
    );
    let resolved = alias::resolve_with_runtime_catalogs(
        "vendor/cpa-new-model",
        alias::RuntimeCatalogs {
            cpa: &["vendor/cpa-new-model".to_string()],
            ..alias::RuntimeCatalogs::default()
        },
    )
    .unwrap();
    let body = chat_body("vendor/cpa-new-model");
    let parsed = parse_client_request(ApiFormat::ChatCompletions, body.clone()).unwrap();
    let set = materialize_account_routes(
        &[cpa],
        &AppConfig::default(),
        &parsed,
        &resolved,
        "vendor/cpa-new-model",
        "vendor/cpa-new-model",
        &body,
        true,
        &HashMap::new(),
        &HashMap::new(),
        Some(crate::cpa::DEFAULT_CPA_BASE_URL),
        &static_contracts(),
        &[],
    )
    .unwrap();
    assert_eq!(set.routes.len(), 1, "{:?}", set.rejected);
    assert_eq!(set.routes[0].routing.adapter, ProviderAdapterKind::Cpa);
    assert_ne!(set.routes[0].routing.account.id, CPA_ACCOUNT_ID);
}

#[test]
fn diagnostic_plan_does_not_veto_a_refreshed_go_model_missing_a_static_profile() {
    for name in [
        "muse-spark-1.3-contributor",
        "omen-alpha",
        "future-go-model",
    ] {
        let go = vec![name.to_string()];
        let resolved = alias::resolve_with_runtime_catalogs(
            name,
            RuntimeCatalogs {
                go: &go,
                ..Default::default()
            },
        )
        .unwrap();
        for client in [
            ApiFormat::ChatCompletions,
            ApiFormat::Responses,
            ApiFormat::Messages,
        ] {
            assert_eq!(diagnostic_forced_upstream(&resolved, client), Some(client));
        }
        assert_eq!(
            diagnostic_forced_upstream(&resolved, ApiFormat::Gemini),
            Some(ApiFormat::ChatCompletions)
        );
        assert!(alias::resolve_with_runtime_catalogs(name, RuntimeCatalogs::default()).is_err());
    }
    let go = vec!["grok-4.6".to_string()];
    let known = alias::resolve_with_runtime_catalogs(
        "grok-4.6",
        RuntimeCatalogs {
            go: &go,
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(
        diagnostic_forced_upstream(&known, ApiFormat::Responses),
        None
    );
}

fn rejection_codes(set: &MaterializedRouteSet) -> Vec<RouteRejectionCode> {
    set.rejections
        .iter()
        .map(|rejection| rejection.code)
        .collect()
}

#[test]
fn typed_rejections_cover_current_materialize_branches() {
    let config = AppConfig::default();
    let body = chat_body("glm-5.2");
    let parsed = parse_client_request(ApiFormat::ChatCompletions, body.clone()).unwrap();

    let mut disabled_cred =
        test_credential("go-off", &destination_id_for_builtin(OPENCODE_PROVIDER_ID));
    disabled_cred.enabled = false;
    let go_dest = test_destination(
        AdapterKind::OpencodeGo,
        LegacyDestinationRef::Builtin(OPENCODE_PROVIDER_ID.into()),
    );
    let projection = crate::destination_projection::DestinationProjection {
        destinations: vec![go_dest.clone()],
        credentials: vec![disabled_cred],
    };
    let credential_set = materialize_account_routes_with_bindings(
        &[go_account("go-off")],
        &config,
        &parsed,
        &resolve_model("glm-5.2"),
        "glm-5.2",
        "glm-5.2",
        true,
        &HashMap::new(),
        &HashMap::new(),
        None,
        &static_contracts(),
        &[],
        &HashMap::new(),
        Some(&projection),
    )
    .unwrap();
    assert!(rejection_codes(&credential_set).contains(&RouteRejectionCode::CredentialDisabled));

    let custom = custom_account("custom-missing");
    let custom_set = materialize_account_routes(
        &[custom],
        &config,
        &parsed,
        &resolve_with_custom("local-custom", &["local-custom".into()]),
        "local-custom",
        "local-custom",
        &body,
        true,
        &HashMap::new(),
        &HashMap::new(),
        None,
        &static_contracts(),
        &[],
    )
    .unwrap();
    assert!(
        rejection_codes(&custom_set).contains(&RouteRejectionCode::CandidateMaterializationFailed),
        "{:?}",
        custom_set.rejections
    );

    let goat = goat_account("goat-1");
    let goat_unverified = materialize_account_routes(
        &[goat.clone()],
        &config,
        &parsed,
        &ResolvedModel::PinnedRaw {
            requested: COMMAND_CODE_GOAT_DEEPSEEK_V4_FLASH_UPSTREAM.into(),
            mapping: crate::alias::ProviderMapping {
                provider_id: COMMAND_CODE_PROVIDER_ID.to_string(),
                upstream_model: COMMAND_CODE_GOAT_DEEPSEEK_V4_FLASH_UPSTREAM.into(),
                routeable: true,
            },
        },
        COMMAND_CODE_GOAT_DEEPSEEK_V4_FLASH_UPSTREAM,
        COMMAND_CODE_GOAT_DEEPSEEK_V4_FLASH_UPSTREAM,
        &body,
        true,
        &HashMap::new(),
        &HashMap::new(),
        None,
        &goat_contracts(&[COMMAND_CODE_GOAT_DEEPSEEK_V4_FLASH_UPSTREAM]),
        &[],
    )
    .unwrap();
    assert!(
        rejection_codes(&goat_unverified).contains(&RouteRejectionCode::GoatUnverified),
        "{:?}",
        goat_unverified.rejections
    );

    let mut ineligible = goat_runtime("goat-1", &[COMMAND_CODE_GOAT_DEEPSEEK_V4_FLASH_UPSTREAM]);
    ineligible.enabled = false;
    let mut ineligible_runtimes = HashMap::new();
    ineligible_runtimes.insert(goat.id.clone(), ineligible);
    let goat_ineligible = materialize_account_routes(
        &[goat.clone()],
        &config,
        &parsed,
        &ResolvedModel::PinnedRaw {
            requested: COMMAND_CODE_GOAT_DEEPSEEK_V4_FLASH_UPSTREAM.into(),
            mapping: crate::alias::ProviderMapping {
                provider_id: COMMAND_CODE_PROVIDER_ID.to_string(),
                upstream_model: COMMAND_CODE_GOAT_DEEPSEEK_V4_FLASH_UPSTREAM.into(),
                routeable: true,
            },
        },
        COMMAND_CODE_GOAT_DEEPSEEK_V4_FLASH_UPSTREAM,
        COMMAND_CODE_GOAT_DEEPSEEK_V4_FLASH_UPSTREAM,
        &body,
        true,
        &HashMap::new(),
        &ineligible_runtimes,
        None,
        &goat_contracts(&[COMMAND_CODE_GOAT_DEEPSEEK_V4_FLASH_UPSTREAM]),
        &[],
    )
    .unwrap();
    assert!(
        rejection_codes(&goat_ineligible).contains(&RouteRejectionCode::GoatNotEligible),
        "{:?}",
        goat_ineligible.rejections
    );

    let mut mismatched_zen = zen_account();
    mismatched_zen.credential_kind = CredentialKind::ApiKey;
    mismatched_zen.quota_scope = QuotaScope::Key;
    let zen_unsupported = materialize_account_routes(
        &[mismatched_zen],
        &config,
        &parsed,
        &resolve_model("mimo-v2.5-free"),
        "mimo-v2.5-free",
        "mimo-v2.5-free",
        &body,
        true,
        &HashMap::new(),
        &HashMap::new(),
        None,
        &static_contracts(),
        &[],
    )
    .unwrap();
    assert!(
        rejection_codes(&zen_unsupported).contains(&RouteRejectionCode::ProductionRouteUnsupported),
        "{:?}",
        zen_unsupported.rejections
    );

    let mixed = ResolvedModel::Alias {
        requested: "glm-5.2".into(),
        alias: "glm-5.2".into(),
        mappings: vec![
            crate::alias::ProviderMapping {
                provider_id: CPA_PROVIDER_ID.to_string(),
                upstream_model: "vendor/cpa-new-model".into(),
                routeable: true,
            },
            crate::alias::ProviderMapping {
                provider_id: OPENCODE_PROVIDER_ID.to_string(),
                upstream_model: "glm-5.2".into(),
                routeable: true,
            },
        ],
    };
    let mixed_set = materialize_account_routes(
        &[go_account("go-1")],
        &config,
        &parsed,
        &mixed,
        "glm-5.2",
        "glm-5.2",
        &body,
        true,
        &HashMap::new(),
        &HashMap::new(),
        None,
        &static_contracts(),
        &[],
    )
    .unwrap();
    assert!(
        rejection_codes(&mixed_set).contains(&RouteRejectionCode::MappingProtocolIncompatible),
        "{:?}",
        mixed_set.rejections
    );
    assert_eq!(
        RouteRejectionCode::CredentialDisabled.as_str(),
        "credential_disabled"
    );
    assert_eq!(
        RouteRejectionCode::MappingProtocolIncompatible.as_str(),
        "mapping_protocol_incompatible"
    );
}
