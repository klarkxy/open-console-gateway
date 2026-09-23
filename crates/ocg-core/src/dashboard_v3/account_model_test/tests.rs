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
