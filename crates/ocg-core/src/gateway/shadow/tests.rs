use super::*;
use crate::alias::{self, ResolvedModel, RuntimeCatalogs};
use crate::crypto::{KeyCipher, StaticKeyCipher};
use crate::custom::CustomAccountRuntime;
use crate::dynamic::DynamicProviderRuntime;
use crate::gateway::attempt::CredentialHandle;
use crate::gateway::materialize::{
    InferenceBindingGate, MaterializedRouteSet, materialize_account_routes_with_bindings,
};
use crate::gateway::protocol::{ApiFormat, ParsedClientRequest, parse_client_request};
use crate::gateway::provider_adapter;
use crate::models::{
    Account, AccountCustomConfig, AccountModelCapability, AccountSetupStep, AccountType, AppConfig,
    ProxyMode,
};
use crate::provider::{
    CUSTOM_PROVIDER_ID, ConnectionVerificationStatus, CredentialKind, OPENCODE_PROVIDER_ID,
    ProviderAdapterKind, QuotaScope, UpstreamProtocolKind,
};
use bytes::Bytes;
use chrono::Utc;
use ocg_domain::credential::ModelScope;
use ocg_domain::dynamic::{DynamicAuthKind, DynamicModelMapping};
use serde_json::json;
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

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
        protocol_passthrough: false,
    }
}

fn refreshed_go_persisted() -> crate::provider_contracts::PersistedContracts {
    let now = Utc::now();
    let scope = crate::provider_contracts::ContractScope::provider(OPENCODE_PROVIDER_ID);
    let mut persisted = crate::provider_contracts::PersistedContracts::default();
    persisted.scopes.insert(
        scope.clone(),
        crate::provider_contracts::PersistedScopeRow {
            scope: scope.clone(),
            catalog_models: vec!["glm-5.2".into()],
            catalog_refreshed_at: Some(now),
            catalog_source: crate::provider_contracts::CATALOG_SOURCE_OPENCODE_MODELS.into(),
            catalog_source_url: crate::provider::OPENCODE_GO_BASE_URL.into(),
            revision: 1,
            updated_at: now,
        },
    );
    persisted.evidence.insert(
        scope.clone(),
        vec![crate::provider_contracts::PersistedModelProtocol {
            scope,
            model_id: "glm-5.2".into(),
            protocol: UpstreamProtocolKind::ChatCompletions,
            source: crate::provider_contracts::ContractEvidenceSource::Static,
            verified_at: None,
            observed_at: None,
            last_probe_result: None,
            last_probe_at: None,
            last_probe_error: None,
        }],
    );
    persisted
}

fn contracts_for(
    runtimes: &[CustomAccountRuntime],
) -> crate::provider_contracts::EffectiveContractSet {
    crate::provider_contracts::build_effective_contracts(
        &crate::zen_models::ZenFreeModelCatalog::default(),
        runtimes,
        refreshed_go_persisted(),
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

/// Shadow plan snapshot needs the full routing scene as distinct borrows.
#[allow(clippy::too_many_arguments)]
fn plan_input<'a>(
    accounts: &'a [Account],
    config: &'a AppConfig,
    parsed: &'a ParsedClientRequest,
    resolved: &'a ResolvedModel,
    client_model: &'a str,
    routing_model: &'a str,
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
        free_available: true,
        custom_runtimes,
        goat_runtimes,
        cpa_base_url: None,
        contracts,
        dynamics,
        bindings: empty_bindings(),
    }
}

#[allow(clippy::too_many_arguments)]
fn materialize_account_routes(
    accounts: &[Account],
    config: &AppConfig,
    parsed: &ParsedClientRequest,
    resolved: &ResolvedModel,
    client_model: &str,
    routing_model: &str,
    _client_body: &Bytes,
    free_available: bool,
    custom_runtimes: &HashMap<String, CustomAccountRuntime>,
    goat_runtimes: &HashMap<String, crate::goat::GoatAccountRuntime>,
    cpa_base_url: Option<&str>,
    contracts: &crate::provider_contracts::EffectiveContractSet,
    dynamics: &[DynamicProviderRuntime],
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
    )
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
    let base_contracts = static_contracts();
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
        &base_contracts,
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
        &empty_custom,
        &goat_runtimes,
        &base_contracts,
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

    // Cheap fixture from materialize/tests.rs: glm-5.2 is a published Go alias.
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
        &empty_custom,
        &goat_runtimes,
        &contracts,
        &[],
    );
    let shadow = plan_shadow_attempts(&input).unwrap();
    let live = live_shadow_attempts(&set.routes, &config, &[]);
    assert_eq!(live.len(), 1, "go-alias");
    assert_eq!(live[0].account_id, "go-1", "go-alias");
    assert_eq!(live[0].upstream_model, "glm-5.2", "go-alias");
    assert_eq!(
        live[0].adapter_kind,
        ProviderAdapterKind::OpenCodeGo,
        "go-alias"
    );
    assert_eq!(live[0].protocol, ApiFormat::ChatCompletions, "go-alias");
    assert_eq!(
        live[0].credential_handle,
        CredentialHandle::Account { id: "go-1".into() },
        "go-alias"
    );
    assert_attempts_match(&live, &shadow.attempts);
}

#[test]
fn shadow_compare_env_accepts_only_trimmed_one() {
    assert!(!shadow_compare_env_value_enables(None));
    assert!(!shadow_compare_env_value_enables(Some("")));
    assert!(!shadow_compare_env_value_enables(Some("0")));
    assert!(!shadow_compare_env_value_enables(Some("true")));
    assert!(!shadow_compare_env_value_enables(Some("on")));
    assert!(!shadow_compare_env_value_enables(Some("yes")));
    assert!(!shadow_compare_env_value_enables(Some("01")));
    assert!(shadow_compare_env_value_enables(Some("1")));
    assert!(shadow_compare_env_value_enables(Some(" 1 ")));
    assert_eq!(SHADOW_COMPARE_ENV, "OCG_SHADOW_COMPARE");
    assert_eq!(SHADOW_COMPARE_ENABLE_VALUE, "1");
}

#[test]
fn shadow_diff_reports_reject_mismatch() {
    let live = ShadowPlan {
        attempts: Vec::new(),
        rejects: vec!["live-reject".into()],
    };
    let shadow = ShadowPlan {
        attempts: Vec::new(),
        rejects: vec!["shadow-reject".into()],
    };
    let diffs = shadow_plan_diff(&live, &shadow);
    assert!(
        diffs.iter().any(|mismatch| matches!(
            mismatch,
            ShadowMismatch::Rejects {
                live,
                shadow
            } if live == &["live-reject".to_string()] && shadow == &["shadow-reject".to_string()]
        )),
        "expected reject mismatch, got {diffs:?}"
    );
}

#[test]
fn shadow_matches_live_rejects_when_inference_binding_is_disabled() {
    let config = AppConfig::default();
    let accounts = [go_account("go-1")];
    let body = chat_body("glm-5.2");
    let parsed = parse_client_request(ApiFormat::ChatCompletions, body.clone()).unwrap();
    let resolved = alias::resolve("glm-5.2").unwrap();
    let contracts = static_contracts();
    let empty_custom = HashMap::new();
    let goat_runtimes = HashMap::new();
    let mut bindings = HashMap::new();
    bindings.insert(
        "go-1".to_string(),
        InferenceBindingGate {
            enabled: false,
            model_scope: ModelScope::All,
        },
    );
    let set = materialize_account_routes_with_bindings(
        &accounts,
        &config,
        &parsed,
        &resolved,
        &parsed.requested_model,
        "glm-5.2",
        true,
        &empty_custom,
        &goat_runtimes,
        None,
        &contracts,
        &[],
        &bindings,
    )
    .unwrap();
    let input = ShadowPlanInput {
        accounts: &accounts,
        config: &config,
        parsed: &parsed,
        resolved: &resolved,
        client_model: &parsed.requested_model,
        routing_model: "glm-5.2",
        free_available: true,
        custom_runtimes: &empty_custom,
        goat_runtimes: &goat_runtimes,
        cpa_base_url: None,
        contracts: &contracts,
        dynamics: &[],
        bindings: &bindings,
    };
    let shadow = plan_shadow_attempts(&input).unwrap();
    let live = shadow_plan_from_materialized(&set, &config, &[]);
    assert!(live.attempts.is_empty(), "{live:?}");
    assert!(
        live.rejects
            .iter()
            .any(|reject| reject.contains("inference binding is disabled")),
        "{live:?}"
    );
    let diffs = shadow_plan_diff(&live, &shadow);
    assert!(
        diffs.is_empty(),
        "live and shadow rejects diverged: {diffs:?}"
    );
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
    let live = shadow_attempt_from_live(&account, &set.routes[0].plan, &live_spec, &[]);
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
}

#[tokio::test]
async fn r07_gateway_path_sends_once_whether_compare_is_off_or_on() {
    reset_shadow_compare_hook_entries();
    assert!(
        !shadow_compare_enabled(),
        "compare must stay default-off without env or test override"
    );

    let hits = Arc::new(AtomicUsize::new(0));
    let app = axum::Router::new()
        .fallback(axum::routing::any(r07_count_ok))
        .with_state(hits.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let (stop_upstream, stop_rx) = tokio::sync::oneshot::channel();
    tokio::spawn(async move {
        let _ = axum::serve(listener, app)
            .with_graceful_shutdown(async {
                let _ = stop_rx.await;
            })
            .await;
    });
    let base_url = format!("http://{addr}");

    let dir = std::env::temp_dir().join(format!("ocg-shadow-r07-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let cipher: Arc<dyn KeyCipher + Send + Sync> = Arc::new(StaticKeyCipher::new("test"));
    let db = crate::db::Database::open(dir.clone()).unwrap();
    let state = Arc::new(crate::state::CoreStateInner::new(db, dir.clone(), cipher).unwrap());
    let mut config = state.config();
    config.gateway_key = "gw-test".into();
    config.upstream_base_url = base_url;
    config.proxy_mode = ProxyMode::Direct;
    state.set_config(config).unwrap();
    state.db.lock().create_account(&go_account("go-1")).unwrap();
    let now = Utc::now();
    state
        .db
        .lock()
        .set_contract_catalog(
            &crate::provider_contracts::ContractScope::provider(OPENCODE_PROVIDER_ID),
            &["glm-5.2".to_string()],
            Some(now),
            crate::provider_contracts::CATALOG_SOURCE_OPENCODE_MODELS,
            crate::provider::OPENCODE_GO_BASE_URL,
            now,
        )
        .unwrap();
    state
        .db
        .lock()
        .apply_official_protocol_baseline(
            &crate::provider_contracts::ContractScope::provider(OPENCODE_PROVIDER_ID),
            &["glm-5.2".to_string()],
            &crate::official_protocols::OfficialProtocolBaseline::mapped([(
                "glm-5.2",
                UpstreamProtocolKind::ChatCompletions,
            )]),
            now,
        )
        .unwrap();
    state.reload_provider_contracts().unwrap();

    let port = {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.local_addr().unwrap().port()
    };
    let handle = crate::gateway::start_gateway(state.clone(), port)
        .await
        .unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    let client = reqwest::Client::builder().no_proxy().build().unwrap();

    let off = client
        .post(format!("http://127.0.0.1:{port}/v1/chat/completions"))
        .header(reqwest::header::AUTHORIZATION, "Bearer gw-test")
        .json(&json!({
            "model": "glm-5.2",
            "messages": [{"role": "user", "content": "hi"}]
        }))
        .send()
        .await
        .unwrap();
    assert!(
        off.status().is_success(),
        "compare-off request failed: {} {}",
        off.status(),
        off.text().await.unwrap_or_default()
    );
    assert_eq!(hits.load(Ordering::SeqCst), 1);
    assert_eq!(shadow_compare_hook_entries(), 0);

    let _guard = ShadowCompareGuard::enable();
    assert!(shadow_compare_enabled());
    let on = client
        .post(format!("http://127.0.0.1:{port}/v1/chat/completions"))
        .header(reqwest::header::AUTHORIZATION, "Bearer gw-test")
        .json(&json!({
            "model": "glm-5.2",
            "messages": [{"role": "user", "content": "hi"}]
        }))
        .send()
        .await
        .unwrap();
    assert!(
        on.status().is_success(),
        "compare-on request failed: {} {}",
        on.status(),
        on.text().await.unwrap_or_default()
    );
    assert_eq!(
        hits.load(Ordering::SeqCst),
        2,
        "each client request must send exactly once with compare on"
    );
    assert_eq!(
        shadow_compare_hook_entries(),
        1,
        "enabled compare must run on the live gateway request path"
    );

    crate::gateway::stop_gateway(handle);
    let _ = stop_upstream.send(());
    drop(state);
    let _ = std::fs::remove_dir_all(dir);
}

async fn r07_count_ok(
    axum::extract::State(hits): axum::extract::State<Arc<AtomicUsize>>,
) -> impl axum::response::IntoResponse {
    hits.fetch_add(1, Ordering::SeqCst);
    (
        axum::http::StatusCode::OK,
        [("content-type", "application/json")],
        r#"{"id":"ok","object":"chat.completion","model":"glm-5.2","choices":[{"index":0,"message":{"role":"assistant","content":"ok"},"finish_reason":"stop"}],"usage":{"prompt_tokens":1,"completion_tokens":1}}"#,
    )
}
