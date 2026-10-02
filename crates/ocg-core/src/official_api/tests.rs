use super::*;
use serde_json::json;

pub(crate) fn runtime(kind: OfficialApiKind) -> DynamicProviderRuntime {
    DynamicProviderRuntime {
        preset_id: Some(kind.id().into()),
        id: "11111111-1111-1111-1111-111111111159".into(),
        name: "Official fixture".into(),
        endpoint_url: match kind {
            OfficialApiKind::Deepseek => "https://api.deepseek.com/chat/completions",
            OfficialApiKind::Zhipu => "https://open.bigmodel.cn/api/paas/v4/chat/completions",
        }
        .into(),
        upstream_protocol: UpstreamProtocolKind::ChatCompletions,
        auth_kind: DynamicAuthKind::Bearer,
        mappings: vec![],
        created_at: at(),
        updated_at: at(),
        origin: ocg_domain::provider::ProviderOrigin::Preset,
        offering: "api".into(),
    }
}
fn at() -> DateTime<Utc> {
    DateTime::parse_from_rfc3339("2026-09-17T01:00:00Z")
        .unwrap()
        .with_timezone(&Utc)
}
#[test]
fn official_api_financial_capability_requires_provenance_and_fixed_destination() {
    for kind in [OfficialApiKind::Deepseek, OfficialApiKind::Zhipu] {
        let r = runtime(kind);
        assert_eq!(kind_for_runtime(&r), Some(kind));
        let mut edited = r.clone();
        edited.endpoint_url = "https://api.deepseek.com.attacker.test/chat/completions".into();
        assert_eq!(kind_for_runtime(&edited), None);
        edited = r.clone();
        edited.preset_id = None;
        assert_eq!(kind_for_runtime(&edited), None);
        edited = r.clone();
        edited.auth_kind = DynamicAuthKind::None;
        assert_eq!(kind_for_runtime(&edited), None);
        edited = r.clone();
        edited.offering = "plan".into();
        assert_eq!(kind_for_runtime(&edited), None);
        edited = r.clone();
        edited.endpoint_url.push_str("?secret=x");
        assert_eq!(kind_for_runtime(&edited), None);
    }
    assert!(!route_is_official(
        OfficialApiKind::Zhipu,
        "https://open.bigmodel.cn/api/coding/paas/v4/chat/completions",
        UpstreamProtocolKind::ChatCompletions
    ));
    assert!(!route_is_official(
        OfficialApiKind::Deepseek,
        "http://api.deepseek.com/chat/completions",
        UpstreamProtocolKind::ChatCompletions
    ));
}
#[test]
fn official_api_balances_keep_total_and_components_separate_and_missing_unknown() {
    let body = json!({"is_available":true,"balance_infos":[{"currency":"CNY","total_balance":"12.5","granted_balance":"2.5","topped_up_balance":"10"},{"currency":"USD","total_balance":"-0.1","granted_balance":"0","topped_up_balance":"-0.1"}]});
    let rows = balance::parse(&serde_json::to_vec(&body).unwrap(), at()).unwrap();
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].total, 12.5);
    assert_eq!(rows[1].total, -0.1);
    for invalid in [
        json!({}),
        json!({"is_available":false,"balance_infos":[]}),
        json!({"is_available":true,"balance_infos":[{"currency":"CNY","total_balance":"NaN","granted_balance":"0","topped_up_balance":"1"}]}),
        json!({"is_available":true,"balance_infos":[body["balance_infos"][0].clone(),body["balance_infos"][0].clone()]}),
    ] {
        assert!(balance::parse(&serde_json::to_vec(&invalid).unwrap(), at()).is_err());
    }
}

#[tokio::test]
async fn official_api_http_never_follows_redirects_and_bounds_headerless_bodies() {
    use axum::{Router, body::Body, response::IntoResponse, routing::get};
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };
    let leaks = Arc::new(AtomicUsize::new(0));
    let leaked = leaks.clone();
    let router = Router::new()
        .route(
            "/redirect",
            get(|| async {
                (
                    axum::http::StatusCode::FOUND,
                    [("Location", "/leak")],
                    "redirect",
                )
            }),
        )
        .route(
            "/leak",
            get(move || {
                let leaks = leaked.clone();
                async move {
                    leaks.fetch_add(1, Ordering::SeqCst);
                    "leak"
                }
            }),
        )
        .route(
            "/oversize",
            get(|| async { Body::from(vec![b'x'; 128 * 1024]) }),
        )
        .route(
            "/chunked",
            get(|| async {
                Body::from_stream(futures_util::stream::iter((0..3).map(|_| {
                    Ok::<_, std::convert::Infallible>(bytes::Bytes::from(vec![b'x'; 32 * 1024]))
                })))
            }),
        )
        .route(
            "/rejected",
            get(|| async {
                (axum::http::StatusCode::UNAUTHORIZED, "sk-sensitive-fixture").into_response()
            }),
        );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    let config = crate::models::AppConfig {
        proxy_mode: crate::models::ProxyMode::Direct,
        ..Default::default()
    };
    for path in ["redirect", "oversize", "chunked", "rejected"] {
        let _guard = install_official_api_endpoint_for_test(
            591,
            BALANCE_URL,
            &format!("http://{addr}/{path}"),
        )
        .unwrap();
        let error = balance::fetch(&config, "sk-sensitive-fixture", 591, at)
            .await
            .unwrap_err()
            .to_string();
        assert!(!error.contains("sk-sensitive-fixture"));
        assert_eq!(leaks.load(Ordering::SeqCst), 0);
    }
    server.abort();
}
