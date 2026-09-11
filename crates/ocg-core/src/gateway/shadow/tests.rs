use super::*;
use crate::alias::{self, ResolvedModel, RuntimeCatalogs};
use crate::crypto::{KeyCipher, StaticKeyCipher};
use crate::custom::CustomAccountRuntime;
use crate::dynamic::DynamicProviderRuntime;
use crate::gateway::attempt::{AttemptSpec, CredentialHandle};
use crate::gateway::materialize::materialize_account_routes;
use crate::gateway::protocol::{ApiFormat, ParsedClientRequest, parse_client_request};
use crate::gateway::provider_adapter;
use crate::models::{
    Account, AccountCustomConfig, AccountModelCapability, AccountSetupStep, AccountType, AppConfig,
};
use crate::provider::{
    CUSTOM_PROVIDER_ID, ConnectionVerificationStatus, CredentialKind, OPENCODE_PROVIDER_ID,
    ProviderAdapterKind, QuotaScope, UpstreamProtocolKind,
};
use bytes::Bytes;
use chrono::Utc;
use ocg_domain::dynamic::{DynamicAuthKind, DynamicModelMapping};
use serde_json::json;
use std::collections::HashMap;
use std::sync::Arc;

const NO_IDS: &[String] = &[];
const DYNAMIC_PROVIDER_ID: &str = "11111111-1111-1111-1111-111111111111";
const DYNAMIC_PUBLIC: &str = "lab-shadow-chat";
const DYNAMIC_UPSTREAM: &str = "vendor/lab-shadow-chat";

fn empty_catalogs() -> RuntimeCatalogs<'static> {
    RuntimeCatalogs {
        go: NO_IDS,
        zen_free: NO_IDS,
        custom: NO_IDS,
        command_code: NO_IDS,
        minimax: NO_IDS,
        kimi: NO_IDS,
        cpa: NO_IDS,
        ollama: NO_IDS,
        ollama_pinned: NO_IDS,
        extra: &[],
    }
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

fn custom_account(id: &str) -> Account {
    account(
        id,
        CUSTOM_PROVIDER_ID,
        CredentialKind::ApiKey,
        QuotaScope::Key,
    )
}

fn custom_runtime(account_id: &str, model_id: &str) -> CustomAccountRuntime {
    CustomAccountRuntime {
        account_id: account_id.into(),
        enabled: true,
        verification_status: ConnectionVerificationStatus::Verified,
        setup_ready: true,
        has_key: true,
        config: AccountCustomConfig {
            account_id: account_id.into(),
            endpoint_url: "http://127.0.0.1:9/v1/chat/completions".into(),
            upstream_protocol: UpstreamProtocolKind::ChatCompletions,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        },
        capabilities: vec![AccountModelCapability {
            account_id: account_id.into(),
            public_model: model_id.into(),
            upstream_model: model_id.into(),
            protocol: UpstreamProtocolKind::ChatCompletions,
            verified_at: None,
            source: "manual".into(),
        }],
    }
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

fn static_contracts() -> crate::provider_contracts::EffectiveContractSet {
    contracts_for(&[])
}

fn dynamic_runtime() -> DynamicProviderRuntime {
    DynamicProviderRuntime {
        preset_id: None,
        id: DYNAMIC_PROVIDER_ID.into(),
        name: "Lab".into(),
        endpoint_url: "http://127.0.0.1:9/v1".into(),
        upstream_protocol: UpstreamProtocolKind::ChatCompletions,
        auth_kind: DynamicAuthKind::Bearer,
        mappings: vec![DynamicModelMapping {
            public_model: DYNAMIC_PUBLIC.into(),
            upstream_model: DYNAMIC_UPSTREAM.into(),
            upstream_override: None,
        }],
        created_at: Utc::now(),
        updated_at: Utc::now(),
        origin: crate::provider::ProviderOrigin::Custom,
        offering: "api".into(),
    }
}

fn plan_input<'a>(
    accounts: &'a [Account],
    config: &'a AppConfig,
    parsed: &'a ParsedClientRequest,
    resolved: &'a ResolvedModel,
    client_model: &'a str,
    routing_model: &'a str,
    client_body: &'a Bytes,
    custom_runtimes: &'a HashMap<String, CustomAccountRuntime>,
    goat_runtimes: &'a HashMap<String, crate::goat::GoatAccountRuntime>,
    contracts: &'a crate::provider_contracts::EffectiveContractSet,
    dynamics: &'a [DynamicProviderRuntime],
) -> ShadowPlanInput<'a> {
    ShadowPlanInput {
        accounts,
        config,
        parsed,
        resolved,
        client_model,
        routing_model,
        client_body,
        free_available: true,
        custom_runtimes,
        goat_runtimes,
        cpa_base_url: None,
        contracts,
        dynamics,
        bindings: empty_bindings(),
    }
}

fn empty_bindings() -> &'static crate::gateway::materialize::InferenceBindingIndex {
    static EMPTY: std::sync::OnceLock<crate::gateway::materialize::InferenceBindingIndex> =
        std::sync::OnceLock::new();
    EMPTY.get_or_init(HashMap::new)
}

fn assert_attempts_match(live: &[ShadowAttempt], shadow: &[ShadowAttempt]) {
    let diffs = shadow_diff(live, shadow);
    assert!(
        diffs.is_empty(),
        "live and shadow attempts diverged: {diffs:?}"
    );
}

#[test]
fn shadow_matches_live_materialize_for_dynamic_and_custom_fixtures() {
    let config = AppConfig::default();

    let custom = custom_account("custom-1");
    let runtime = custom_runtime("custom-1", "local-custom");
    let contracts = contracts_for(std::slice::from_ref(&runtime));
    let mut custom_runtimes = HashMap::new();
    custom_runtimes.insert(custom.id.clone(), runtime);
    let custom_ids = vec!["local-custom".to_string()];
    let custom_body = chat_body("local-custom");
    let custom_parsed =
        parse_client_request(ApiFormat::ChatCompletions, custom_body.clone()).unwrap();
    let custom_resolved = alias::resolve_with_runtime_catalogs(
        "local-custom",
        RuntimeCatalogs {
            custom: &custom_ids,
            ..empty_catalogs()
        },
    )
    .unwrap();
    let custom_set = materialize_account_routes(
        std::slice::from_ref(&custom),
        &config,
        &custom_parsed,
        &custom_resolved,
        &custom_parsed.requested_model,
        "local-custom",
        &custom_body,
        true,
        &custom_runtimes,
        &HashMap::new(),
        None,
        &contracts,
        &[],
    )
    .unwrap();
    let goat_runtimes = HashMap::new();
    let custom_input = plan_input(
        std::slice::from_ref(&custom),
        &config,
        &custom_parsed,
        &custom_resolved,
        &custom_parsed.requested_model,
        "local-custom",
        &custom_body,
        &custom_runtimes,
        &goat_runtimes,
        &contracts,
        &[],
    );
    let custom_shadow = plan_shadow_attempts(&custom_input).unwrap();
    let custom_live = live_shadow_attempts(&custom_set.routes, &config, &[]);
    assert_eq!(custom_live.len(), 1);
    assert_eq!(custom_live[0].upstream_model, "local-custom");
    assert_eq!(
        custom_live[0].adapter_kind,
        ProviderAdapterKind::ConfigurableHttp
    );
    assert_eq!(
        custom_live[0].endpoint.as_deref(),
        Some("http://127.0.0.1:9/v1/chat/completions")
    );
    assert_attempts_match(&custom_live, &custom_shadow.attempts);

    let dynamic = dynamic_runtime();
    let dynamics = [dynamic];
    let extra = [dynamics[0].alias_catalog()];
    let dyn_account = account(
        "dyn-1",
        DYNAMIC_PROVIDER_ID,
        CredentialKind::ApiKey,
        QuotaScope::Key,
    );
    let dyn_body = chat_body(DYNAMIC_PUBLIC);
    let dyn_parsed = parse_client_request(ApiFormat::ChatCompletions, dyn_body.clone()).unwrap();
    let dyn_resolved = alias::resolve_with_runtime_catalogs(
        DYNAMIC_PUBLIC,
        RuntimeCatalogs {
            extra: &extra,
            ..empty_catalogs()
        },
    )
    .unwrap();
    let empty_custom = HashMap::new();
    let static_contracts = static_contracts();
    let dyn_set = materialize_account_routes(
        std::slice::from_ref(&dyn_account),
        &config,
        &dyn_parsed,
        &dyn_resolved,
        &dyn_parsed.requested_model,
        DYNAMIC_PUBLIC,
        &dyn_body,
        true,
        &empty_custom,
        &HashMap::new(),
        None,
        &static_contracts,
        &dynamics,
    )
    .unwrap();
    let dyn_input = plan_input(
        std::slice::from_ref(&dyn_account),
        &config,
        &dyn_parsed,
        &dyn_resolved,
        &dyn_parsed.requested_model,
        DYNAMIC_PUBLIC,
        &dyn_body,
        &empty_custom,
        &goat_runtimes,
        &static_contracts,
        &dynamics,
    );
    let dyn_shadow = plan_shadow_attempts(&dyn_input).unwrap();
    let dyn_live = live_shadow_attempts(&dyn_set.routes, &config, &dynamics);
    assert_eq!(dyn_live.len(), 1);
    assert_eq!(dyn_live[0].upstream_model, DYNAMIC_UPSTREAM);
    assert_eq!(
        dyn_live[0].adapter_kind,
        ProviderAdapterKind::ConfigurableHttp
    );
    assert_eq!(
        dyn_live[0].credential_handle,
        CredentialHandle::Account { id: "dyn-1".into() }
    );
    assert_attempts_match(&dyn_live, &dyn_shadow.attempts);
}

#[test]
fn shadow_matches_live_for_builtin_go_alias_when_a_fixture_exists() {
    // Cheap fixture from materialize/tests.rs: glm-5.2 is a published Go alias.
    let config = AppConfig::default();
    let accounts = [go_account("go-1")];
    let body = chat_body("glm-5.2");
    let parsed = parse_client_request(ApiFormat::ChatCompletions, body.clone()).unwrap();
    let resolved = alias::resolve("glm-5.2").unwrap();
    let contracts = static_contracts();
    let empty_custom = HashMap::new();
    let goat_runtimes = HashMap::new();
    let set = materialize_account_routes(
        &accounts,
        &config,
        &parsed,
        &resolved,
        &parsed.requested_model,
        "glm-5.2",
        &body,
        true,
        &empty_custom,
        &goat_runtimes,
        None,
        &contracts,
        &[],
    )
    .unwrap();
    let input = plan_input(
        &accounts,
        &config,
        &parsed,
        &resolved,
        &parsed.requested_model,
        "glm-5.2",
        &body,
        &empty_custom,
        &goat_runtimes,
        &contracts,
        &[],
    );
    let shadow = plan_shadow_attempts(&input).unwrap();
    let live = live_shadow_attempts(&set.routes, &config, &[]);
    assert_eq!(live.len(), 1);
    assert_eq!(live[0].account_id, "go-1");
    assert_eq!(live[0].upstream_model, "glm-5.2");
    assert_eq!(live[0].adapter_kind, ProviderAdapterKind::OpenCodeGo);
    assert_eq!(live[0].protocol, ApiFormat::ChatCompletions);
    assert_eq!(
        live[0].credential_handle,
        CredentialHandle::Account { id: "go-1".into() }
    );
    assert_attempts_match(&live, &shadow.attempts);
}

#[test]
fn enabling_shadow_does_not_increment_outbound_request_count() {
    let _guard = ShadowCompareGuard::enable();
    assert!(shadow_compare_enabled());
    let sends_before = shadow_recorded_outbound_sends();
    let mismatches_before = shadow_mismatch_count();

    let config = AppConfig::default();
    let accounts = [go_account("go-1")];
    let body = chat_body("glm-5.2");
    let parsed = parse_client_request(ApiFormat::ChatCompletions, body.clone()).unwrap();
    let resolved = alias::resolve("glm-5.2").unwrap();
    let contracts = static_contracts();
    let empty_custom = HashMap::new();
    let goat_runtimes = HashMap::new();
    let set = materialize_account_routes(
        &accounts,
        &config,
        &parsed,
        &resolved,
        &parsed.requested_model,
        "glm-5.2",
        &body,
        true,
        &empty_custom,
        &goat_runtimes,
        None,
        &contracts,
        &[],
    )
    .unwrap();
    let input = plan_input(
        &accounts,
        &config,
        &parsed,
        &resolved,
        &parsed.requested_model,
        "glm-5.2",
        &body,
        &empty_custom,
        &goat_runtimes,
        &contracts,
        &[],
    );
    maybe_compare_live_routes(&input, &set);
    let _ = plan_shadow_attempts(&input).unwrap();

    assert_eq!(shadow_recorded_outbound_sends(), sends_before);
    assert_eq!(shadow_recorded_outbound_sends(), 0);
    assert_eq!(shadow_mismatch_count(), mismatches_before);
}

#[test]
fn mismatch_is_reported_and_does_not_alter_the_live_spec() {
    let config = AppConfig::default();
    let account = go_account("go-1");
    let body = chat_body("glm-5.2");
    let parsed = parse_client_request(ApiFormat::ChatCompletions, body.clone()).unwrap();
    let resolved = alias::resolve("glm-5.2").unwrap();
    let contracts = static_contracts();
    let set = materialize_account_routes(
        std::slice::from_ref(&account),
        &config,
        &parsed,
        &resolved,
        &parsed.requested_model,
        "glm-5.2",
        &body,
        true,
        &HashMap::new(),
        &HashMap::new(),
        None,
        &contracts,
        &[],
    )
    .unwrap();
    let live_spec =
        provider_adapter::resolve_route_with_dynamics(&account, &config, &set.routes[0].plan, &[])
            .unwrap();
    let live_spec_before = live_spec.clone();
    let live = shadow_attempt_from_live(&account, &set.routes[0].plan, &live_spec, &[]);
    let live_before = live.clone();
    let mut shadow = live.clone();
    shadow.upstream_model = "not-the-live-model".into();
    shadow.endpoint = Some("https://example.invalid/v1/chat/completions".into());

    let diffs = shadow_diff(std::slice::from_ref(&live), std::slice::from_ref(&shadow));
    assert!(
        diffs.iter().any(|mismatch| matches!(
            mismatch,
            ShadowMismatch::Field {
                field: ShadowField::UpstreamModel,
                ..
            }
        )),
        "expected upstream_model mismatch, got {diffs:?}"
    );
    assert!(
        diffs.iter().any(|mismatch| matches!(
            mismatch,
            ShadowMismatch::Field {
                field: ShadowField::Endpoint,
                ..
            }
        )),
        "expected endpoint mismatch, got {diffs:?}"
    );
    assert_eq!(live, live_before);
    assert_eq!(live_spec, live_spec_before);
    let _unused: &AttemptSpec = &live_spec;
}
