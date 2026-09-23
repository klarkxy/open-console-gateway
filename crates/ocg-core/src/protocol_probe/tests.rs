use super::*;
use crate::custom_http::{
    DestinationResolveLog, HttpInferenceTransportSpec, InferenceHttpRequest,
    guarded_destination_resolver, recording_resolver,
};
use crate::models::ProxyMode;
use reqwest::dns::{Addrs, Name, Resolve, Resolving};
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use tokio::io::AsyncReadExt;

struct OneHost(Vec<SocketAddr>);

impl Resolve for OneHost {
    fn resolve(&self, name: Name) -> Resolving {
        let host = name.as_str().trim_end_matches('.').to_ascii_lowercase();
        let addrs = self.0.clone();
        Box::pin(async move {
            assert_eq!(host, "imds.test");
            Ok(Box::new(addrs.into_iter()) as Addrs)
        })
    }
}

#[test]
fn unique_protocols_preserve_caller_order_and_reject_duplicates() {
    let unique = [
        UpstreamProtocolKind::ChatCompletions,
        UpstreamProtocolKind::Responses,
    ];
    require_unique_probe_protocols(&unique).expect("unique caller order is preserved");
    require_unique_probe_protocols(&[
        UpstreamProtocolKind::ChatCompletions,
        UpstreamProtocolKind::Responses,
        UpstreamProtocolKind::ChatCompletions,
    ])
    .expect_err("duplicates must fail locally");
}

#[test]
fn null_error_field_is_not_a_probe_failure() {
    let success = serde_json::json!({ "id": "response-1", "error": null });
    assert!(non_null_probe_error(&success).is_none());

    let failure = serde_json::json!({ "error": { "message": "model unavailable" } });
    assert_eq!(
        non_null_probe_error(&failure)
            .and_then(|error| error.get("message"))
            .and_then(serde_json::Value::as_str),
        Some("model unavailable")
    );
}

#[tokio::test]
async fn isolated_trusted_admin_probe_transport_rejects_metadata_resolution() {
    let hits = Arc::new(AtomicUsize::new(0));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("loopback listener");
    let mock = listener.local_addr().unwrap();
    let accept_hits = hits.clone();
    tokio::spawn(async move {
        if let Ok((mut stream, _)) = listener.accept().await {
            accept_hits.fetch_add(1, Ordering::SeqCst);
            let mut buf = vec![0_u8; 1024];
            let _ = stream.read(&mut buf).await;
        }
    });

    let config = AppConfig {
        proxy_mode: ProxyMode::Direct,
        connect_timeout_secs: 5,
        ..AppConfig::default()
    };
    let inner: Arc<dyn Resolve> = Arc::new(OneHost(vec![SocketAddr::new(
        IpAddr::V4(Ipv4Addr::new(169, 254, 169, 254)),
        0,
    )]));
    let log = Arc::new(DestinationResolveLog::default());
    let resolver = recording_resolver(guarded_destination_resolver(inner), Arc::clone(&log));
    let transport = HttpInferenceTransport::build_isolated_trusted_admin_with_dns_resolver(
        &config,
        HttpInferenceTransportSpec::no_redirects(),
        std::time::Duration::from_secs(5),
        resolver,
    )
    .unwrap();
    let url = reqwest::Url::parse(&format!("http://imds.test:{}/v1", mock.port())).unwrap();
    let error = transport
        .send(InferenceHttpRequest {
            method: reqwest::Method::POST,
            url,
            auth: Some((UpstreamAuthScheme::Bearer, "sk-test-probe")),
            extra_headers: reqwest::header::HeaderMap::new(),
            body: Some(b"{}".to_vec()),
            request_timeout: Some(std::time::Duration::from_secs(2)),
        })
        .await
        .expect_err("probe IsolatedTrustedAdmin must use the destination DNS guard");
    log.assert_guard_rejected("imds.test");
    let text = format!("{error:?} {error}");
    assert!(
        !text.contains("sk-test-probe"),
        "probe error must not echo the synthetic key: {text}"
    );
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    assert_eq!(hits.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn stored_key_probe_does_not_send_to_ungranted_destination() {
    use crate::crypto::{KeyCipher, StaticKeyCipher};
    use crate::db::Database;
    use crate::models::{
        Account, AccountCustomConfigInput, AccountModelCapabilityInput, AccountSetupStep,
        AccountType, AppConfig, ProxyMode,
    };
    use crate::provider::{CUSTOM_PROVIDER_ID, ProviderAdapterKind, UpstreamProtocolKind};
    use crate::state::CoreStateInner;
    use chrono::Utc;
    use ocg_domain::connection::{
        EndpointOperation, LegacyConnectionKind, connection_id_for_legacy,
    };
    use ocg_domain::credential::{RouteSpec, assigned_endpoints_for_routes, normalize_origin};

    let secret = "sk-test-probe-ungranted";
    let (addr, hits, stop_tx) = {
        let hits = Arc::new(AtomicUsize::new(0));
        let app = axum::Router::new().fallback(axum::routing::any(
            |axum::extract::State(hits): axum::extract::State<Arc<AtomicUsize>>| async move {
                hits.fetch_add(1, Ordering::SeqCst);
                (
                    axum::http::StatusCode::OK,
                    [("content-type", "application/json")],
                    r#"{"id":"ok","object":"chat.completion","choices":[{"index":0,"message":{"role":"assistant","content":"ok"},"finish_reason":"stop"}]}"#,
                )
            },
        )).with_state(hits.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let (stop_tx, stop_rx) = tokio::sync::oneshot::channel();
        tokio::spawn(async move {
            let _ = axum::serve(listener, app)
                .with_graceful_shutdown(async {
                    let _ = stop_rx.await;
                })
                .await;
        });
        (addr, hits, stop_tx)
    };
    let endpoint = format!("http://{addr}/v1/chat/completions");
    let dir = std::env::temp_dir().join(format!("ocg-probe-ungranted-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let cipher: Arc<dyn KeyCipher + Send + Sync> = Arc::new(StaticKeyCipher::new("probe-grant"));
    let db = Database::open(dir.clone()).unwrap();
    let state = Arc::new(CoreStateInner::new(db, dir.clone(), cipher).unwrap());
    let now = Utc::now();
    let account = Account {
        id: "probe-custom".into(),
        provider_id: CUSTOM_PROVIDER_ID.into(),
        credential_kind: crate::provider::default_credential_kind(),
        quota_scope: crate::provider::default_quota_scope(),
        name: "probe".into(),
        username: None,
        password_cipher: None,
        key_cipher: state.encrypt_key(secret).unwrap(),
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
    state
        .db
        .lock()
        .create_account_with_contract(
            &account,
            Some(&AccountCustomConfigInput {
                endpoint_url: endpoint.clone(),
                upstream_protocol: UpstreamProtocolKind::ChatCompletions,
            }),
            &[AccountModelCapabilityInput {
                public_model: "local-custom".into(),
                upstream_model: "local-custom".into(),
                protocol: UpstreamProtocolKind::ChatCompletions,
                source: None,
            }],
        )
        .unwrap();
    {
        let db = state.db.lock();
        let binding = db
            .list_inference_bindings()
            .unwrap()
            .into_iter()
            .find(|row| row.account_id == account.id)
            .unwrap();
        db.update_credential_binding(&binding.binding_id, None, None, Some(&[]), Some(&[]))
            .unwrap();
    }
    let config = AppConfig {
        proxy_mode: ProxyMode::Direct,
        ..AppConfig::default()
    };
    let error = super::execute_account_model_test(super::AccountModelTestInput {
        state: &state,
        config: &config,
        account: &account,
        adapter: ProviderAdapterKind::ConfigurableHttp,
        public_model: "local-custom",
        model_id: "local-custom",
        protocol: UpstreamProtocolKind::ChatCompletions,
        custom_route: Some(crate::gateway::protocol::CustomRouteSpec {
            endpoint_url: endpoint.clone(),
            auth_kind: ocg_domain::dynamic::DynamicAuthKind::Bearer,
        }),
        dynamics: &[],
    })
    .await
    .expect_err("ungranted stored-key probe must fail closed");
    let message = error.1;
    assert!(
        message.contains("not authorized") || message.contains("refusing"),
        "{message}"
    );
    assert!(!message.contains(secret), "{message}");
    assert_eq!(hits.load(Ordering::SeqCst), 0);

    let assigned = assigned_endpoints_for_routes(
        &connection_id_for_legacy(LegacyConnectionKind::CustomAccount, &account.id),
        &[RouteSpec {
            operation: EndpointOperation::ChatCreate,
            url: Some(endpoint.clone()),
        }],
    );
    let ids: Vec<String> = assigned
        .iter()
        .map(|endpoint| endpoint.id.clone())
        .collect();
    let origins: Vec<String> = assigned
        .iter()
        .filter_map(|endpoint| endpoint.url.as_deref().and_then(normalize_origin))
        .collect();
    {
        let db = state.db.lock();
        let binding = db
            .list_inference_bindings()
            .unwrap()
            .into_iter()
            .find(|row| row.account_id == account.id)
            .unwrap();
        db.update_credential_binding(
            &binding.binding_id,
            None,
            None,
            Some(ids.as_slice()),
            Some(origins.as_slice()),
        )
        .unwrap();
    }
    super::execute_account_model_test(super::AccountModelTestInput {
        state: &state,
        config: &config,
        account: &account,
        adapter: ProviderAdapterKind::ConfigurableHttp,
        public_model: "local-custom",
        model_id: "local-custom",
        protocol: UpstreamProtocolKind::ChatCompletions,
        custom_route: Some(crate::gateway::protocol::CustomRouteSpec {
            endpoint_url: endpoint.clone(),
            auth_kind: ocg_domain::dynamic::DynamicAuthKind::Bearer,
        }),
        dynamics: &[],
    })
    .await
    .expect("granted stored-key probe may send");
    assert_eq!(hits.load(Ordering::SeqCst), 1);

    let _ = stop_tx.send(());
    drop(state);
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn stored_key_probe_honors_cleared_sealed_endpoint_grant() {
    use crate::crypto::{KeyCipher, StaticKeyCipher};
    use crate::db::Database;
    use crate::models::{Account, AccountSetupStep, AccountType, AppConfig, ProxyMode};
    use crate::provider::{OPENCODE_PROVIDER_ID, ProviderAdapterKind, UpstreamProtocolKind};
    use crate::state::CoreStateInner;
    use chrono::Utc;
    use ocg_domain::connection::{
        EndpointOperation, LegacyConnectionKind, connection_id_for_legacy, endpoint_id_for,
    };

    let secret = "sk-test-probe-sealed";
    let hits = Arc::new(AtomicUsize::new(0));
    let app = axum::Router::new()
        .fallback(axum::routing::any(
            |axum::extract::State(hits): axum::extract::State<Arc<AtomicUsize>>| async move {
                hits.fetch_add(1, Ordering::SeqCst);
                (
                    axum::http::StatusCode::OK,
                    [("content-type", "application/json")],
                    r#"{"id":"ok","object":"chat.completion","choices":[{"index":0,"message":{"role":"assistant","content":"ok"},"finish_reason":"stop"}]}"#,
                )
            },
        ))
        .with_state(hits.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let (stop_tx, stop_rx) = tokio::sync::oneshot::channel();
    tokio::spawn(async move {
        let _ = axum::serve(listener, app)
            .with_graceful_shutdown(async {
                let _ = stop_rx.await;
            })
            .await;
    });
    let dir = std::env::temp_dir().join(format!("ocg-probe-sealed-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let cipher: Arc<dyn KeyCipher + Send + Sync> = Arc::new(StaticKeyCipher::new("probe-sealed"));
    let db = Database::open(dir.clone()).unwrap();
    let state = Arc::new(CoreStateInner::new(db, dir.clone(), cipher).unwrap());
    let now = Utc::now();
    let account = Account {
        id: "probe-go".into(),
        provider_id: OPENCODE_PROVIDER_ID.into(),
        credential_kind: crate::provider::default_credential_kind(),
        quota_scope: crate::provider::default_quota_scope(),
        name: "go".into(),
        username: None,
        password_cipher: None,
        key_cipher: state.encrypt_key(secret).unwrap(),
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
    state.db.lock().create_account(&account).unwrap();
    {
        let db = state.db.lock();
        let binding = db
            .list_inference_bindings()
            .unwrap()
            .into_iter()
            .find(|row| row.account_id == account.id)
            .unwrap();
        db.update_credential_binding(&binding.binding_id, None, None, Some(&[]), Some(&[]))
            .unwrap();
    }
    let config = AppConfig {
        proxy_mode: ProxyMode::Direct,
        upstream_base_url: format!("http://{addr}"),
        ..AppConfig::default()
    };
    let error = super::execute_account_model_test(super::AccountModelTestInput {
        state: &state,
        config: &config,
        account: &account,
        adapter: ProviderAdapterKind::OpenCodeGo,
        public_model: "deepseek-v4-flash",
        model_id: "deepseek-v4-flash",
        protocol: UpstreamProtocolKind::ChatCompletions,
        custom_route: None,
        dynamics: &[],
    })
    .await
    .expect_err("cleared sealed endpoint grant must fail closed");
    assert!(
        error.1.contains("not authorized") || error.1.contains("refusing"),
        "{}",
        error.1
    );
    assert!(!error.1.contains(secret), "{}", error.1);
    assert_eq!(hits.load(Ordering::SeqCst), 0);

    let chat_id = endpoint_id_for(
        &connection_id_for_legacy(LegacyConnectionKind::BuiltinProvider, OPENCODE_PROVIDER_ID),
        EndpointOperation::ChatCreate,
    )
    .to_string();
    {
        let db = state.db.lock();
        let binding = db
            .list_inference_bindings()
            .unwrap()
            .into_iter()
            .find(|row| row.account_id == account.id)
            .unwrap();
        db.update_credential_binding(
            &binding.binding_id,
            None,
            None,
            Some(std::slice::from_ref(&chat_id)),
            Some(&[]),
        )
        .unwrap();
    }
    super::execute_account_model_test(super::AccountModelTestInput {
        state: &state,
        config: &config,
        account: &account,
        adapter: ProviderAdapterKind::OpenCodeGo,
        public_model: "deepseek-v4-flash",
        model_id: "deepseek-v4-flash",
        protocol: UpstreamProtocolKind::ChatCompletions,
        custom_route: None,
        dynamics: &[],
    })
    .await
    .expect("restored official protocol grant may send");
    assert_eq!(hits.load(Ordering::SeqCst), 1);

    let _ = stop_tx.send(());
    drop(state);
    let _ = std::fs::remove_dir_all(dir);
}
