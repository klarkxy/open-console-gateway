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
use chrono::Utc;
use ocg_domain::credential::ModelScope;
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

fn static_contracts() -> crate::provider_contracts::EffectiveContractSet {
    contracts_for(&[])
}

fn goat_contracts(models: &[&str]) -> crate::provider_contracts::EffectiveContractSet {
    let now = Utc::now();
    let scope = crate::provider_contracts::ContractScope::provider(COMMAND_CODE_PROVIDER_ID);
    let mut persisted = crate::provider_contracts::PersistedContracts::default();
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
    crate::provider_contracts::build_effective_contracts(
        &crate::zen_models::ZenFreeModelCatalog::default(),
        &[],
        persisted,
    )
}

fn contracts_for(
    runtimes: &[CustomAccountRuntime],
) -> crate::provider_contracts::EffectiveContractSet {
    crate::provider_contracts::build_effective_contracts(
        &crate::zen_models::ZenFreeModelCatalog::default(),
        runtimes,
        crate::provider_contracts::PersistedContracts::default(),
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
    let resolved = alias::resolve(model).unwrap();
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
    assert!(!set.routes[1].plan.allow_go_fallback);
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
    assert!(!set.routes[0].plan.allow_go_fallback);
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
fn claude_desktop_identity_keeps_client_name_and_mapped_alias() {
    let body = Bytes::from(
        serde_json::to_vec(&json!({
            "model": crate::models::CLAUDE_DESKTOP_OPUS_ALIAS,
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
            allow_go_fallback: false,
            forced_upstream: None,
            custom_route: None,
        },
    )
    .unwrap();
    let identity = native_log_identity(&plan);
    assert_eq!(
        identity.requested_model,
        crate::models::CLAUDE_DESKTOP_OPUS_ALIAS
    );
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
    let mut persisted = crate::provider_contracts::PersistedContracts::default();
    let scope = crate::provider_contracts::ContractScope::provider(OPENCODE_PROVIDER_ID);
    persisted.evidence.insert(
        scope.clone(),
        vec![crate::provider_contracts::PersistedModelProtocol {
            scope,
            model_id: "grok-4.5".into(),
            protocol: UpstreamProtocolKind::ChatCompletions,
            source: crate::provider_contracts::ContractEvidenceSource::ProbeConfirmed,
            verified_at: Some(now),
            observed_at: Some(now),
            last_probe_result: Some(crate::provider_contracts::ProbeResultKind::Success),
            last_probe_at: Some(now),
            last_probe_error: None,
        }],
    );
    let contracts = crate::provider_contracts::build_effective_contracts(
        &crate::zen_models::ZenFreeModelCatalog::default(),
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
    let resolved = alias::resolve(model).unwrap();
    materialize_account_routes_with_bindings(
        accounts,
        &AppConfig::default(),
        &parsed,
        &resolved,
        &parsed.requested_model,
        model,
        &body,
        true,
        &HashMap::new(),
        &HashMap::new(),
        None,
        &static_contracts(),
        &[],
        bindings,
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
    let mut persisted = crate::provider_contracts::PersistedContracts::default();
    persisted.preferences.insert(
        scope,
        vec![("kimi-k3".into(), UpstreamProtocolKind::Messages)],
    );
    let contracts = crate::provider_contracts::build_effective_contracts(
        &crate::zen_models::ZenFreeModelCatalog::default(),
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

    let local = account(
        "local-oauth-1",
        CPA_PROVIDER_ID,
        CredentialKind::ApiKey,
        QuotaScope::Key,
    );
    let set = materialize_account_routes(
        &[local],
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
        "CPA must not invent a local OAuth/account candidate: {:?}",
        set.rejected
    );
    assert!(
        set.rejected
            .iter()
            .any(|reason| reason.contains("reserved") || reason.contains("CPA")),
        "{:?}",
        set.rejected
    );
}
