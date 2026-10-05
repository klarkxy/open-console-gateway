use super::*;
use crate::crypto::{KeyCipher, StaticKeyCipher};
use crate::db::Database;
use crate::models::{
    Account, AccountCustomConfigInput, AccountModelCapabilityInput, AccountSetupStep, AccountType,
};
use crate::provider::{CUSTOM_PROVIDER_ID, OPENCODE_PROVIDER_ID, UpstreamProtocolKind};
use crate::state::CoreStateInner;
use chrono::Utc;
use ocg_domain::destination::{
    AuthScheme, HttpProtocolRoute, Protocol, destination_id_for_custom_account,
};
use ocg_domain::dynamic::{DynamicAuthKind, DynamicModelMapping, DynamicModelUpstreamOverride};
use std::sync::Arc;

fn state(tag: &str) -> (std::path::PathBuf, crate::state::CoreState) {
    let dir = std::env::temp_dir().join(format!(
        "ocg-account-model-test-{tag}-{}",
        uuid::Uuid::new_v4()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let db = Database::open(dir.clone()).unwrap();
    let cipher: Arc<dyn KeyCipher + Send + Sync> = Arc::new(StaticKeyCipher::new(tag));
    (
        dir.clone(),
        Arc::new(CoreStateInner::new(db, dir, cipher).unwrap()),
    )
}

fn custom_account(state: &crate::state::CoreState, id: &str, enabled: bool) -> Account {
    let now = Utc::now();
    Account {
        id: id.into(),
        provider_id: CUSTOM_PROVIDER_ID.into(),
        credential_kind: crate::provider::default_credential_kind(),
        quota_scope: crate::provider::default_quota_scope(),
        name: id.into(),
        username: None,
        password_cipher: None,
        key_cipher: state.encrypt_key("sk-account-test").unwrap(),
        enabled,
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
        created_at: now,
        updated_at: now,
    }
}

#[test]
fn prepared_debug_redacts_account_config_and_message() {
    let (_dir, state) = state("debug-redaction");
    let account = custom_account(&state, "debug-account", true);
    let mut config = crate::models::AppConfig::default();
    config.gateway_key = "sentinel-gateway-key".into();
    let prepared = super::PreparedAccountModelTest {
        account,
        config,
        adapter: crate::provider::ProviderAdapterKind::ConfigurableHttp,
        public_model: "public-sentinel".into(),
        upstream_model: "upstream-sentinel".into(),
        protocol: UpstreamProtocolKind::ChatCompletions,
        custom_route: None,
        message: Some("sentinel-user-message".into()),
        max_tokens: Some(7),
    };
    let text = format!("{prepared:?}");
    assert!(text.contains("PreparedAccountModelTest"), "{text}");
    assert!(text.contains("[redacted]"), "{text}");
    assert!(text.contains("public-sentinel"), "{text}");
    assert!(!text.contains("sentinel-gateway-key"), "{text}");
    assert!(!text.contains("sk-account-test"), "{text}");
    assert!(!text.contains("sentinel-user-message"), "{text}");
}

fn persist_custom(
    state: &crate::state::CoreState,
    account: &Account,
    endpoint: &str,
    capabilities: &[AccountModelCapabilityInput],
) {
    state
        .db
        .lock()
        .create_account_with_contract(
            account,
            Some(&AccountCustomConfigInput {
                endpoint_url: endpoint.into(),
                upstream_protocol: UpstreamProtocolKind::ChatCompletions,
            }),
            capabilities,
        )
        .unwrap();
}

fn install_chat_and_messages_routes(state: &crate::state::CoreState, destination_id: &str) {
    let chat_url: String = state
        .db
        .lock()
        .conn
        .query_row(
            "SELECT base_url FROM destinations WHERE id = ?1",
            [destination_id],
            |row| row.get(0),
        )
        .unwrap();
    let origin = chat_url
        .strip_suffix("/v1/chat/completions")
        .expect("fixture uses a chat completions URL")
        .to_string();
    let routes = vec![
        HttpProtocolRoute {
            protocol: Protocol::ChatCompletions,
            endpoint_url: chat_url,
            auth_scheme: AuthScheme::Bearer,
        },
        HttpProtocolRoute {
            protocol: Protocol::Messages,
            endpoint_url: format!("{origin}/anthropic/v1/messages"),
            auth_scheme: AuthScheme::XApiKey,
        },
    ];
    let protocols: Vec<_> = routes.iter().map(|route| route.protocol).collect();
    state
        .db
        .lock()
        .conn
        .execute(
            "UPDATE destinations SET protocol_routes_json = ?2, protocols_json = ?3 WHERE id = ?1",
            rusqlite::params![
                destination_id,
                serde_json::to_string(&routes).unwrap(),
                serde_json::to_string(&protocols).unwrap(),
            ],
        )
        .unwrap();
}

fn persist_dynamic(
    state: &crate::state::CoreState,
    account: &Account,
    endpoint: &str,
    mappings: Vec<DynamicModelMapping>,
) {
    let now = Utc::now();
    state
        .db
        .lock()
        .create_dynamic_provider(
            &crate::dynamic::DynamicProviderRuntime {
                preset_id: None,
                id: account.provider_id.clone(),
                name: "Lab".into(),
                endpoint_url: endpoint.into(),
                upstream_protocol: UpstreamProtocolKind::ChatCompletions,
                auth_kind: DynamicAuthKind::Bearer,
                mappings,
                created_at: now,
                updated_at: now,
                origin: crate::provider::ProviderOrigin::Custom,
                offering: "api".into(),
            },
            account,
        )
        .unwrap();
}

fn prefer_messages(state: &crate::state::CoreState, destination_id: &str, public_model: &str) {
    let mut catalog = crate::db::destination_store::load_destination_catalog(
        &state.db.lock().conn,
        destination_id,
    )
    .unwrap();
    for model in &mut catalog {
        if crate::custom::custom_model_id_matches(&model.public_model, public_model) {
            model.protocols = vec![Protocol::ChatCompletions, Protocol::Messages];
            model.preferred = Some(Protocol::Messages);
        }
    }
    crate::db::destination_store::replace_destination_catalog(
        &state.db.lock().conn,
        destination_id,
        &catalog,
    )
    .unwrap();
}

#[test]
fn explicit_protocol_routes_use_preferred_destination_route() {
    let (dir, state) = state("protocol-routes");
    let account = custom_account(&state, "custom-routes", true);
    persist_custom(
        &state,
        &account,
        "http://127.0.0.1:9/v1/chat/completions",
        &[AccountModelCapabilityInput {
            public_model: "lab-opus".into(),
            upstream_model: "vendor/opus".into(),
            protocol: UpstreamProtocolKind::ChatCompletions,
            source: None,
        }],
    );
    let destination_id = destination_id_for_custom_account(&account.id);
    install_chat_and_messages_routes(&state, &destination_id);
    prefer_messages(&state, &destination_id, "lab-opus");

    let prepared = prepare_account_model_test(
        &state,
        &account.id,
        AccountModelTestRequest {
            message: None,
            max_tokens: None,
            model_id: "lab-opus".into(),
        },
    )
    .expect("Accounts test should use the already selected destination route");
    assert_eq!(prepared.public_model, "lab-opus");
    assert_eq!(prepared.upstream_model, "vendor/opus");
    assert_eq!(prepared.protocol, UpstreamProtocolKind::Messages);
    let route = prepared.custom_route.expect("HTTP test carries a route");
    assert_eq!(
        route.endpoint_url,
        "http://127.0.0.1:9/anthropic/v1/messages"
    );
    assert_eq!(
        route.auth_kind,
        ocg_domain::dynamic::DynamicAuthKind::XApiKey
    );
    drop(state);
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn shared_upstream_prepare_keeps_selected_public_model() {
    let (dir, state) = state("shared-public");
    let account = custom_account(&state, "custom-shared", false);
    persist_custom(
        &state,
        &account,
        "http://127.0.0.1:9/v1/chat/completions",
        &[
            AccountModelCapabilityInput {
                public_model: "public-a".into(),
                upstream_model: "upstream-x".into(),
                protocol: UpstreamProtocolKind::ChatCompletions,
                source: None,
            },
            AccountModelCapabilityInput {
                public_model: "public-b".into(),
                upstream_model: "upstream-x".into(),
                protocol: UpstreamProtocolKind::ChatCompletions,
                source: None,
            },
        ],
    );
    let prepared = prepare_account_model_test(
        &state,
        &account.id,
        AccountModelTestRequest {
            message: None,
            max_tokens: None,
            model_id: "public-b".into(),
        },
    )
    .expect("disabled card may still prepare a selected public mapping");
    assert_eq!(prepared.public_model, "public-b");
    assert_eq!(prepared.upstream_model, "upstream-x");
    assert_eq!(prepared.protocol, UpstreamProtocolKind::ChatCompletions);
    drop(state);
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn unknown_public_mapping_is_rejected() {
    let (dir, state) = state("unknown-public");
    let account = custom_account(&state, "custom-missing", true);
    persist_custom(
        &state,
        &account,
        "http://127.0.0.1:9/v1/chat/completions",
        &[AccountModelCapabilityInput {
            public_model: "public-a".into(),
            upstream_model: "upstream-x".into(),
            protocol: UpstreamProtocolKind::ChatCompletions,
            source: None,
        }],
    );
    match prepare_account_model_test(
        &state,
        &account.id,
        AccountModelTestRequest {
            message: None,
            max_tokens: None,
            model_id: "public-missing".into(),
        },
    ) {
        Err(error) => {
            assert!(format!("{error:?}").contains("not declared"), "{error:?}");
        }
        Ok(_) => panic!("unknown public mapping must fail locally"),
    }
    drop(state);
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn dynamic_provider_prepare_uses_destination_route() {
    let (dir, state) = state("dynamic-dest");
    let mut account = custom_account(&state, "dyn-account", true);
    account.provider_id = uuid::Uuid::new_v4().to_string();
    persist_dynamic(
        &state,
        &account,
        "http://127.0.0.1:9/v1",
        vec![
            DynamicModelMapping {
                public_model: "public-a".into(),
                upstream_model: "upstream-x".into(),
                upstream_override: None,
            },
            DynamicModelMapping {
                public_model: "public-b".into(),
                upstream_model: "upstream-x".into(),
                upstream_override: Some(DynamicModelUpstreamOverride {
                    protocol: UpstreamProtocolKind::ChatCompletions,
                    endpoint_url: "http://127.0.0.1:9/other/v1".into(),
                }),
            },
        ],
    );

    let prepared_a = prepare_account_model_test(
        &state,
        &account.id,
        AccountModelTestRequest {
            message: None,
            max_tokens: None,
            model_id: "public-a".into(),
        },
    )
    .expect("dynamic public-a uses the destination default route");
    assert_eq!(prepared_a.public_model, "public-a");
    assert_eq!(prepared_a.upstream_model, "upstream-x");
    assert_eq!(prepared_a.protocol, UpstreamProtocolKind::ChatCompletions);
    let route_a = prepared_a.custom_route.expect("HTTP test carries a route");
    assert_eq!(route_a.endpoint_url, "http://127.0.0.1:9/v1");
    assert_eq!(route_a.auth_kind, DynamicAuthKind::Bearer);

    let prepared_b = prepare_account_model_test(
        &state,
        &account.id,
        AccountModelTestRequest {
            message: None,
            max_tokens: None,
            model_id: "public-b".into(),
        },
    )
    .expect("dynamic public-b uses the selected destination override");
    assert_eq!(prepared_b.public_model, "public-b");
    assert_eq!(prepared_b.upstream_model, "upstream-x");
    let route_b = prepared_b.custom_route.expect("HTTP test carries a route");
    assert_eq!(route_b.endpoint_url, "http://127.0.0.1:9/other/v1");
    drop(state);
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn sealed_account_prepare_keeps_production_route_without_http_override() {
    let (dir, state) = state("sealed-go");
    let now = Utc::now();
    let account = Account {
        id: "go-account".into(),
        provider_id: OPENCODE_PROVIDER_ID.into(),
        credential_kind: crate::provider::default_credential_kind(),
        quota_scope: crate::provider::default_quota_scope(),
        name: "go".into(),
        username: None,
        password_cipher: None,
        key_cipher: state.encrypt_key("sk-go").unwrap(),
        enabled: false,
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
        created_at: now,
        updated_at: now,
    };
    state.db.lock().create_account(&account).unwrap();
    match prepare_account_model_test(
        &state,
        &account.id,
        AccountModelTestRequest {
            message: None,
            max_tokens: None,
            model_id: "deepseek-v4-flash".into(),
        },
    ) {
        Ok(prepared) => {
            assert_eq!(prepared.public_model, "deepseek-v4-flash");
            assert_eq!(prepared.upstream_model, "deepseek-v4-flash");
            assert!(
                prepared.custom_route.is_none(),
                "sealed AccountTest keeps the production route family"
            );
        }
        Err(error) => {
            let text = format!("{error:?}");
            assert!(
                text.contains("not routable") || text.contains("unknown provider"),
                "disabled sealed cards may only fail admission, not HTTP re-resolution: {error:?}"
            );
        }
    }
    drop(state);
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn supplied_message_and_max_tokens_do_not_select_another_route() {
    let rejected: Result<AccountModelTestRequest, _> = serde_json::from_value(serde_json::json!({
        "modelId": "public-b",
        "message": "custom prompt",
        "maxTokens": 8,
        "endpointUrl": "http://127.0.0.1:9/override",
        "clientKey": "sk-secret",
        "pin": "hop-pin"
    }));
    assert!(
        rejected.is_err(),
        "model test rejects endpoint, key, and pin"
    );

    let absent: AccountModelTestRequest =
        serde_json::from_value(serde_json::json!({"modelId": "public-b"})).unwrap();
    assert!(absent.message.is_none());
    assert!(absent.max_tokens.is_none());
    let supplied: AccountModelTestRequest = serde_json::from_value(serde_json::json!({
        "modelId": "public-b",
        "message": "custom prompt",
        "maxTokens": 8
    }))
    .unwrap();
    assert_eq!(supplied.message.as_deref(), Some("custom prompt"));
    assert_eq!(supplied.max_tokens, Some(8));

    let (dir, state) = state("optional-body");
    let zero = prepare_account_model_test(
        &state,
        "missing-account",
        AccountModelTestRequest {
            model_id: "public-b".into(),
            message: Some("custom prompt".into()),
            max_tokens: Some(0),
        },
    );
    let zero = zero.expect_err("zero maxTokens is rejected");
    let zero_text = format!("{zero:?}");
    assert!(
        zero_text.contains("maxTokens must be positive"),
        "{zero_text}"
    );
    assert!(!zero_text.contains("not found"), "{zero_text}");

    let account = custom_account(&state, "custom-optional", true);
    persist_custom(
        &state,
        &account,
        "http://127.0.0.1:9/v1/chat/completions",
        &[AccountModelCapabilityInput {
            public_model: "public-b".into(),
            upstream_model: "upstream-x".into(),
            protocol: UpstreamProtocolKind::ChatCompletions,
            source: None,
        }],
    );
    let plain = prepare_account_model_test(&state, &account.id, absent)
        .expect("absent message keeps the selected route");
    let custom = prepare_account_model_test(&state, &account.id, supplied)
        .expect("supplied message keeps the same selected route");
    assert_eq!(plain.public_model, "public-b");
    assert_eq!(plain.public_model, custom.public_model);
    assert_eq!(plain.upstream_model, custom.upstream_model);
    assert_eq!(plain.protocol, UpstreamProtocolKind::ChatCompletions);
    assert_eq!(plain.protocol, custom.protocol);
    let plain_url = plain
        .custom_route
        .as_ref()
        .map(|route| route.endpoint_url.as_str());
    let custom_url = custom
        .custom_route
        .as_ref()
        .map(|route| route.endpoint_url.as_str());
    assert_eq!(plain_url, custom_url);
    assert!(
        custom_url.is_some_and(|url| !url.contains("custom prompt")),
        "{custom_url:?}"
    );
    assert!(plain.message.is_none());
    assert!(plain.max_tokens.is_none());
    assert_eq!(custom.message.as_deref(), Some("custom prompt"));
    assert_eq!(custom.max_tokens, Some(8));
    drop(state);
    std::fs::remove_dir_all(dir).unwrap();
}

fn request(model_id: &str) -> AccountModelTestRequest {
    AccountModelTestRequest {
        model_id: model_id.into(),
        message: None,
        max_tokens: None,
    }
}

fn catalog_json(state: &crate::state::CoreState) -> String {
    state
        .db
        .lock()
        .conn
        .query_row(
            "SELECT models_json FROM provider_model_catalogs WHERE provider_id = 'cpa'",
            [],
            |row| row.get(0),
        )
        .unwrap()
}

fn bind_owned_native(state: &crate::state::CoreState) -> crate::db::native_binding::BoundNative {
    let db = state.db.lock();
    let bound =
        crate::db::native_binding::bind_native_account(&db.conn, "codex", "fixture.json").unwrap();
    crate::db::native_binding::insert_native_models_if_new(
        &db.conn,
        &bound.destination_id,
        &[
            crate::db::native_binding::NativeModelInsert {
                public_model: "alias-chat".into(),
                upstream_model: "vendor/Alias-Chat".into(),
                protocols: vec![
                    "chat_completions".into(),
                    "responses".into(),
                    "messages".into(),
                ],
            },
            crate::db::native_binding::NativeModelInsert {
                public_model: "Alias-Responses".into(),
                upstream_model: "vendor/alias-responses".into(),
                protocols: vec!["responses".into(), "messages".into()],
            },
            crate::db::native_binding::NativeModelInsert {
                public_model: "alias-messages".into(),
                upstream_model: "vendor/alias-messages".into(),
                protocols: vec!["messages".into(), "chat_completions".into()],
            },
            crate::db::native_binding::NativeModelInsert {
                public_model: "alias-empty".into(),
                upstream_model: "vendor/alias-empty".into(),
                protocols: vec!["chat_completions".into()],
            },
        ],
    )
    .unwrap();
    db.conn
        .execute(
            "UPDATE destination_models
             SET protocols_json = '[]', preferred = NULL
             WHERE destination_id = ?1 AND public_model = 'alias-empty'",
            [&bound.destination_id],
        )
        .unwrap();
    bound
}

fn assert_prepared(
    prepared: &PreparedAccountModelTest,
    public_model: &str,
    upstream_model: &str,
    protocol: UpstreamProtocolKind,
) {
    assert_eq!(prepared.public_model, public_model);
    assert_eq!(prepared.upstream_model, upstream_model);
    assert_eq!(prepared.protocol, protocol);
    assert!(prepared.custom_route.is_none());
    assert_eq!(prepared.adapter, crate::provider::ProviderAdapterKind::Cpa);
}

#[test]
fn owned_native_model_test_prepares_advertised_protocols_and_exact_aliases() {
    let (dir, state) = state("owned-native-prepare");
    state
        .db
        .lock()
        .replace_cpa_model_catalog(
            &[crate::db::CpaCatalogModel {
                id: "global-only".into(),
                owned_by: Some("pool".into()),
                enabled: true,
            }],
            "https://cpa.invalid/models",
            Utc::now(),
        )
        .unwrap();
    let stored = catalog_json(&state);
    let bound = bind_owned_native(&state);
    let account = state
        .db
        .lock()
        .get_account(&bound.account_id)
        .unwrap()
        .unwrap();
    assert_eq!(account.provider_id, crate::provider::CPA_PROVIDER_ID);
    assert_ne!(account.id, crate::provider::CPA_ACCOUNT_ID);

    let chat = prepare_account_model_test(&state, &bound.account_id, request("alias-chat"))
        .expect("advertised chat alias prepares on the owned destination");
    assert_prepared(
        &chat,
        "alias-chat",
        "vendor/Alias-Chat",
        UpstreamProtocolKind::ChatCompletions,
    );
    let responses = prepare_account_model_test(
        &state,
        &bound.account_id,
        AccountModelTestRequest {
            model_id: "Alias-Responses".into(),
            message: Some("fixture prompt".into()),
            max_tokens: Some(8),
        },
    )
    .expect("advertised responses alias prepares on the owned destination");
    assert_prepared(
        &responses,
        "Alias-Responses",
        "vendor/alias-responses",
        UpstreamProtocolKind::Responses,
    );
    assert_eq!(responses.message.as_deref(), Some("fixture prompt"));
    assert_eq!(responses.max_tokens, Some(8));
    let messages = prepare_account_model_test(&state, &bound.account_id, request("alias-messages"))
        .expect("advertised messages alias prepares on the owned destination");
    assert_prepared(
        &messages,
        "alias-messages",
        "vendor/alias-messages",
        UpstreamProtocolKind::Messages,
    );

    for model_id in ["alias-responses", "vendor/alias-responses", "global-only"] {
        let error = prepare_account_model_test(&state, &bound.account_id, request(model_id))
            .expect_err("only the exact stored public alias is declared");
        let text = format!("{error:?}");
        assert!(text.contains("not declared"), "{model_id}: {text}");
        assert!(!text.contains("validated protocol pin"), "{text}");
    }
    let empty = prepare_account_model_test(&state, &bound.account_id, request("alias-empty"))
        .expect_err("a destination model with no supported protocol stays local");
    let empty_text = format!("{empty:?}");
    assert!(
        empty_text.contains("not routable for this provider"),
        "{empty_text}"
    );
    assert!(
        !empty_text.contains("validated protocol pin"),
        "{empty_text}"
    );
    assert_eq!(catalog_json(&state), stored);
    drop(state);
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn secret_bearing_owned_native_binding_is_refused_before_io() {
    let (dir, state) = state("owned-native-secret");
    let bound = bind_owned_native(&state);
    state
        .db
        .lock()
        .conn
        .execute(
            "UPDATE credentials
             SET key_cipher = 'not-a-provider-key', has_secret = 1, credential_kind = 'api_key'
             WHERE legacy_account_id = ?1",
            [&bound.account_id],
        )
        .unwrap();
    let error = prepare_account_model_test(&state, &bound.account_id, request("alias-chat"))
        .expect_err("a secret-bearing row is not the keyless owned binding");
    let text = format!("{error:?}");
    assert!(
        text.contains("owned native destination credential is not the selected binding"),
        "{text}"
    );
    assert!(!text.contains(RETIRED_CPA_MODEL_TEST), "{text}");
    assert!(!text.contains("validated protocol pin"), "{text}");
    assert!(!text.contains("ChatCompletions"), "{text}");
    drop(state);
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn historical_cpa_model_test_fails_before_io() {
    use crate::models::{Account, AccountSetupStep, AccountType};
    use crate::provider::{CPA_ACCOUNT_ID, CPA_ACCOUNT_NAME, CPA_PROVIDER_ID, CredentialKind};
    use axum::response::IntoResponse;

    let (dir, state) = state("historical-cpa-model");
    let now = Utc::now();
    let account = Account {
        id: CPA_ACCOUNT_ID.into(),
        provider_id: CPA_PROVIDER_ID.into(),
        credential_kind: CredentialKind::ApiKey,
        quota_scope: crate::provider::QuotaScope::Key,
        name: CPA_ACCOUNT_NAME.into(),
        username: None,
        password_cipher: None,
        key_cipher: state.encrypt_key("fixture-inference").unwrap(),
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
        created_at: now,
        updated_at: now,
    };
    let management = state.encrypt_key("fixture-management").unwrap();
    state
        .db
        .lock()
        .upsert_cpa_integration(&account, "http://127.0.0.1:9", &management)
        .unwrap();
    state
        .db
        .lock()
        .replace_cpa_model_catalog(
            &[crate::db::CpaCatalogModel {
                id: "historical-model".into(),
                owned_by: Some("pool".into()),
                enabled: true,
            }],
            "https://cpa.invalid/models",
            now,
        )
        .unwrap();
    let stored = catalog_json(&state);
    let destination_rows: Vec<(String, String, i64)> = {
        let db = state.db.lock();
        let mut statement = db
            .conn
            .prepare(
                "SELECT destination_id, public_model, enabled
                 FROM destination_models ORDER BY rowid",
            )
            .unwrap();
        let rows = statement
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, i64>(2)?,
                ))
            })
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        rows
    };
    assert!(
        destination_rows
            .iter()
            .any(|(_, model, enabled)| model == "historical-model" && *enabled == 1)
    );
    assert!(
        state
            .db
            .lock()
            .get_account(CPA_ACCOUNT_ID)
            .unwrap()
            .unwrap()
            .enabled
    );

    let error = prepare_account_model_test(&state, CPA_ACCOUNT_ID, request("historical-model"))
        .expect_err("the historical singleton fails before a hop");
    let text = format!("{error:?}");
    assert!(text.contains(RETIRED_CPA_MODEL_TEST), "{text}");
    assert!(!text.contains("validated protocol pin"), "{text}");
    assert!(!text.contains("ChatCompletions"), "{text}");
    assert_eq!(
        error.into_response().status(),
        axum::http::StatusCode::PRECONDITION_FAILED
    );
    assert_eq!(catalog_json(&state), stored);
    let destination_rows_after: Vec<(String, String, i64)> = {
        let db = state.db.lock();
        let mut statement = db
            .conn
            .prepare(
                "SELECT destination_id, public_model, enabled
                 FROM destination_models ORDER BY rowid",
            )
            .unwrap();
        statement
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, i64>(2)?,
                ))
            })
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap()
    };
    assert_eq!(destination_rows_after, destination_rows);
    drop(state);
    std::fs::remove_dir_all(dir).unwrap();
}
