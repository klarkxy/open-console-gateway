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

fn persist_goat_model(
    state: &crate::state::CoreState,
    account: &Account,
    model: &str,
    protocol: UpstreamProtocolKind,
    alias: &str,
    official_evidence: bool,
) {
    use crate::provider::COMMAND_CODE_PROVIDER_ID;
    use ocg_domain::connection::{
        EndpointOperation, LegacyConnectionKind, connection_id_for_legacy, endpoint_id_for,
    };
    let scope = ContractScope::provider(COMMAND_CODE_PROVIDER_ID);
    let now = Utc::now();
    let db = state.db.lock();
    db.create_account(account).unwrap();
    db.set_contract_catalog(
        &scope,
        &[model.into()],
        Some(now),
        "official_models",
        "https://api.commandcode.ai/provider/v1/models",
        now,
    )
    .unwrap();
    if official_evidence {
        let endpoint = match protocol {
            UpstreamProtocolKind::ChatCompletions => "/chat/completions",
            UpstreamProtocolKind::Responses => "/responses",
            UpstreamProtocolKind::Messages => "/messages",
        };
        let payload = serde_json::to_vec(
            &serde_json::json!({"data":[{"id":model,"supported_endpoints":[endpoint]}]}),
        )
        .unwrap();
        let baseline =
            crate::official_protocols::parse_catalog_supported_endpoints_baseline(&payload);
        db.apply_official_protocol_baseline(&scope, &[model.into()], &baseline, now)
            .unwrap();
    }
    let destination_id =
        ocg_domain::destination::destination_id_for_builtin(COMMAND_CODE_PROVIDER_ID);
    let mut catalog =
        crate::db::destination_store::load_destination_catalog(&db.conn, &destination_id).unwrap();
    catalog
        .iter_mut()
        .find(|row| row.upstream_model == model)
        .unwrap()
        .public_model = alias.into();
    crate::db::destination_store::replace_destination_catalog(&db.conn, &destination_id, &catalog)
        .unwrap();
    let binding = db
        .list_inference_bindings()
        .unwrap()
        .into_iter()
        .find(|row| row.account_id == account.id)
        .unwrap();
    let endpoint_id = endpoint_id_for(
        &connection_id_for_legacy(
            LegacyConnectionKind::BuiltinProvider,
            COMMAND_CODE_PROVIDER_ID,
        ),
        EndpointOperation::from(protocol),
    )
    .to_string();
    db.update_credential_binding(
        &binding.binding_id,
        None,
        None,
        Some(&[endpoint_id]),
        Some(&["https://api.commandcode.ai".into()]),
    )
    .unwrap();
    drop(db);
    state.reload_provider_contracts().unwrap();
}

async fn execute_prepared(
    state: &crate::state::CoreState,
    prepared: &PreparedAccountModelTest,
) -> Result<u16, (Option<u16>, String)> {
    let config = crate::models::AppConfig {
        proxy_mode: crate::models::ProxyMode::Direct,
        ..prepared.config.clone()
    };
    crate::protocol_probe::execute_account_model_test(
        crate::protocol_probe::AccountModelTestInput {
            state,
            config: &config,
            account: &prepared.account,
            adapter: prepared.adapter,
            requested_model: &prepared.requested_model,
            public_model: &prepared.public_model,
            model_id: &prepared.upstream_model,
            protocol: prepared.protocol,
            custom_route: prepared.custom_route.clone(),
        },
    )
    .await
}

#[tokio::test]
async fn saved_official_new_goat_models_use_exact_protocol_and_upstream_with_alias() {
    use std::sync::{
        Mutex,
        atomic::{AtomicUsize, Ordering},
    };
    type RequestCapture = (
        Arc<AtomicUsize>,
        Arc<Mutex<Vec<(String, serde_json::Value)>>>,
    );
    let hits = Arc::new(AtomicUsize::new(0));
    let requests = Arc::new(Mutex::new(Vec::<(String, serde_json::Value)>::new()));
    let captured = (hits.clone(), requests.clone());
    let app = axum::Router::new().fallback(axum::routing::post(
        |axum::extract::State((hits, requests)): axum::extract::State<RequestCapture>, uri: axum::http::Uri, body: axum::body::Bytes| async move {
            hits.fetch_add(1, Ordering::SeqCst);
            requests.lock().unwrap().push((uri.path().into(), serde_json::from_slice(&body).unwrap()));
            let response = if uri.path().ends_with("/responses") {
                serde_json::json!({"id":"r-test","object":"response","status":"completed","output":[{"type":"message","role":"assistant","content":[{"type":"output_text","text":"ok"}]}]})
            } else {
                serde_json::json!({"id":"c-test","object":"chat.completion","choices":[{"index":0,"message":{"role":"assistant","content":"ok"},"finish_reason":"stop"}]})
            };
            axum::Json(response)
        }
    )).with_state(captured);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    for (protocol, path, model, public, requested_alias) in [
        (
            UpstreamProtocolKind::ChatCompletions,
            "/provider/v1/chat/completions",
            "vendor/new-goat-chat-2026",
            "my-goat",
            "my-goat",
        ),
        (
            UpstreamProtocolKind::Responses,
            "/provider/v1/responses",
            "vendor/new-goat-responses-2026",
            "my-goat",
            "my-goat",
        ),
        (
            UpstreamProtocolKind::Responses,
            "/provider/v1/responses",
            "vendor/generated-goat-responses-2026",
            "vendor/generated-goat-responses-2026",
            "generated-goat-responses-2026",
        ),
    ] {
        let (dir, state) = state("goat-official");
        let mut account = custom_account(&state, &uuid::Uuid::new_v4().to_string(), false);
        account.provider_id = crate::provider::COMMAND_CODE_PROVIDER_ID.into();
        persist_goat_model(&state, &account, model, protocol, public, true);
        let _route = crate::gateway::provider_adapter::install_goat_loopback_route_for_test(
            &account.id,
            &origin,
        )
        .unwrap();
        for requested in [model, requested_alias] {
            let prepared = prepare_account_model_test(
                &state,
                &account.id,
                AccountModelTestRequest {
                    model_id: requested.into(),
                },
            )
            .unwrap();
            assert_eq!(prepared.public_model, public);
            assert_eq!(prepared.requested_model, requested);
            assert_eq!(prepared.upstream_model, model);
            assert_eq!(prepared.protocol, protocol);
            assert_eq!(execute_prepared(&state, &prepared).await.unwrap(), 200);
            let request = requests.lock().unwrap().last().unwrap().clone();
            assert_eq!(request.0, path);
            assert_eq!(request.1["model"], model);
        }
        if public == model {
            let scope = ocg_domain::credential::ModelScope::Only {
                models: vec![requested_alias.into()],
            };
            {
                let db = state.db.lock();
                let binding = db
                    .list_inference_bindings()
                    .unwrap()
                    .into_iter()
                    .find(|row| row.account_id == account.id)
                    .unwrap();
                db.update_credential_binding(&binding.binding_id, Some(&scope), None, None, None)
                    .unwrap();
            }
            let prepared = prepare_account_model_test(
                &state,
                &account.id,
                AccountModelTestRequest {
                    model_id: requested_alias.into(),
                },
            )
            .unwrap();
            assert_eq!(execute_prepared(&state, &prepared).await.unwrap(), 200);
            let sends = hits.load(Ordering::SeqCst);
            let destination_id =
                ocg_domain::destination::destination_id_for_builtin(&account.provider_id);
            let original = crate::db::destination_store::load_destination_catalog(
                &state.db.lock().conn,
                &destination_id,
            )
            .unwrap();
            for change in ["alias", "upstream"] {
                let mut catalog = original.clone();
                if change == "alias" {
                    catalog[0].public_model = "changed-alias".into();
                } else {
                    catalog[0].upstream_model = "changed/upstream".into();
                }
                crate::db::destination_store::replace_destination_catalog(
                    &state.db.lock().conn,
                    &destination_id,
                    &catalog,
                )
                .unwrap();
                assert!(
                    execute_prepared(&state, &prepared).await.is_err(),
                    "scoped generated alias must reject changed {change}"
                );
                assert_eq!(hits.load(Ordering::SeqCst), sends);
            }
        }
        drop(state);
        std::fs::remove_dir_all(dir).unwrap();
    }
    assert_eq!(hits.load(Ordering::SeqCst), 7);
    server.abort();
}

#[tokio::test]
async fn stale_goat_model_tests_and_missing_evidence_send_nothing() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let hits = Arc::new(AtomicUsize::new(0));
    let app = axum::Router::new()
        .fallback(axum::routing::any(
            |axum::extract::State(hits): axum::extract::State<Arc<AtomicUsize>>| async move {
                hits.fetch_add(1, Ordering::SeqCst);
                axum::Json(serde_json::json!({}))
            },
        ))
        .with_state(hits.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    for change in [
        "missing_evidence",
        "revoked_evidence",
        "removed",
        "disabled_model",
        "disabled_protocol",
        "changed_alias",
        "changed_upstream",
        "revoked_endpoint",
        "revoked_origin",
        "disabled_binding",
        "rotated_key",
    ] {
        let (dir, state) = state(change);
        let mut account = custom_account(&state, &uuid::Uuid::new_v4().to_string(), false);
        account.provider_id = crate::provider::COMMAND_CODE_PROVIDER_ID.into();
        let model = "vendor/arbitrary-goat-id";
        persist_goat_model(
            &state,
            &account,
            model,
            UpstreamProtocolKind::Responses,
            "my-goat",
            change != "missing_evidence",
        );
        let _route = crate::gateway::provider_adapter::install_goat_loopback_route_for_test(
            &account.id,
            &origin,
        )
        .unwrap();
        let prepared = if change == "missing_evidence" {
            assert!(
                prepare_account_model_test(
                    &state,
                    &account.id,
                    AccountModelTestRequest {
                        model_id: model.into()
                    }
                )
                .is_err()
            );
            PreparedAccountModelTest {
                account: account.clone(),
                config: state.config(),
                adapter: ProviderAdapterKind::CommandCodeGoat,
                requested_model: model.into(),
                public_model: "my-goat".into(),
                upstream_model: model.into(),
                protocol: UpstreamProtocolKind::Responses,
                custom_route: None,
            }
        } else {
            prepare_account_model_test(
                &state,
                &account.id,
                AccountModelTestRequest {
                    model_id: model.into(),
                },
            )
            .unwrap()
        };
        {
            let db = state.db.lock();
            let destination_id =
                ocg_domain::destination::destination_id_for_builtin(&account.provider_id);
            let binding = db
                .list_inference_bindings()
                .unwrap()
                .into_iter()
                .find(|row| row.account_id == account.id)
                .unwrap();
            let mut catalog =
                crate::db::destination_store::load_destination_catalog(&db.conn, &destination_id)
                    .unwrap();
            match change {
                "revoked_evidence" => {
                    db.conn.execute("DELETE FROM provider_contract_model_protocols WHERE scope_id = ?1 AND model_id = ?2", rusqlite::params![account.provider_id, model]).unwrap();
                }
                "removed" => catalog.clear(),
                "disabled_model" => catalog[0].enabled = false,
                "disabled_protocol" => catalog[0].protocols.clear(),
                "changed_alias" => catalog[0].public_model = "different-alias".into(),
                "changed_upstream" => catalog[0].upstream_model = "different/upstream".into(),
                "revoked_endpoint" => {
                    db.update_credential_binding(
                        &binding.binding_id,
                        None,
                        None,
                        Some(&[]),
                        Some(&[]),
                    )
                    .unwrap();
                }
                "disabled_binding" => {
                    db.update_credential_binding(
                        &binding.binding_id,
                        None,
                        Some(false),
                        None,
                        None,
                    )
                    .unwrap();
                }
                "rotated_key" => {
                    let cipher = state.encrypt_key("rotated-test-key").unwrap();
                    db.update_account(
                        &account.id,
                        &crate::models::AccountUpdate::default(),
                        Some(&cipher),
                        None,
                    )
                    .unwrap();
                }
                "revoked_origin" => {
                    db.update_credential_binding(
                        &binding.binding_id,
                        None,
                        None,
                        Some(&binding.allowed_endpoint_ids),
                        Some(&["https://unrelated.example".into()]),
                    )
                    .unwrap();
                }
                _ => {}
            }
            crate::db::destination_store::replace_destination_catalog(
                &db.conn,
                &destination_id,
                &catalog,
            )
            .unwrap();
        }
        assert!(
            execute_prepared(&state, &prepared).await.is_err(),
            "{change}"
        );
        assert_eq!(
            hits.load(Ordering::SeqCst),
            0,
            "{change} sent an upstream request"
        );
        drop(state);
        std::fs::remove_dir_all(dir).unwrap();
    }
    server.abort();
}
