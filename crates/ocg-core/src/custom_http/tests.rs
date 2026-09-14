use super::*;
use crate::models::AppConfig;
use reqwest::StatusCode;
use reqwest::header::AUTHORIZATION;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

#[test]
fn official_presets_resolve_to_their_exact_inference_endpoints() {
    // Exercise the shipped data through the same resolver used by discovery,
    // verification and forwarding, including nested v3/v4/compatibility paths.
    let presets: Vec<serde_json::Value> =
        serde_json::from_str(include_str!("../../../../resources/provider-presets.json")).unwrap();
    assert!(!presets.is_empty());
    for preset in presets {
        let id = preset["id"].as_str().unwrap();
        let endpoint = preset["endpointUrl"].as_str().unwrap();
        let protocol =
            UpstreamProtocolKind::try_from(preset["protocol"].as_str().unwrap()).unwrap();
        let auth =
            ocg_domain::dynamic::DynamicAuthKind::try_from(preset["authKind"].as_str().unwrap())
                .unwrap();
        assert!(auth.requires_key(), "{id}");
        if endpoint.is_empty() {
            assert!(
                resolve_custom_endpoints(endpoint, protocol).is_err(),
                "{id}"
            );
            continue;
        }
        let resolved = resolve_custom_endpoints(endpoint, protocol).unwrap();
        assert_eq!(resolved.inference.as_str(), endpoint, "{id}");
        assert_eq!(resolved.inference.scheme(), "https", "{id}");
        assert!(resolved.inference.query().is_none(), "{id}");
        let headers =
            isolated_inference_headers(auth.upstream_auth().unwrap(), "preset-test-key").unwrap();
        match auth {
            ocg_domain::dynamic::DynamicAuthKind::Bearer => {
                assert_eq!(headers[AUTHORIZATION], "Bearer preset-test-key", "{id}");
                assert!(!headers.contains_key("x-api-key"), "{id}");
            }
            ocg_domain::dynamic::DynamicAuthKind::XApiKey => {
                assert_eq!(headers["x-api-key"], "preset-test-key", "{id}");
                assert!(!headers.contains_key(AUTHORIZATION), "{id}");
            }
            ocg_domain::dynamic::DynamicAuthKind::None => unreachable!(),
        }
        if preset["modelDiscovery"].as_bool() != Some(false) {
            let models = resolved.models.expect(id);
            assert_eq!(models.origin(), resolved.inference.origin(), "{id}");
            assert!(models.path().ends_with("/models"), "{id}");
        }
    }
}

#[test]
fn preset_offerings_mirror_matches_shipped_presets() {
    // ocg-domain freezes a point-in-time mirror of the JSON `offering` field
    // for the v42 seed and create paths. A plan preset missing from the mirror
    // would silently persist as "api", so pin every shipped id here.
    let presets: Vec<serde_json::Value> =
        serde_json::from_str(include_str!("../../../../resources/provider-presets.json")).unwrap();
    assert!(!presets.is_empty());
    for preset in &presets {
        let id = preset["id"].as_str().unwrap();
        let expected = preset["offering"].as_str().unwrap_or("api");
        assert_eq!(ocg_domain::provider::preset_offering(id), expected, "{id}");
    }
}

fn test_config(mode: ProxyMode, proxy_url: &str) -> AppConfig {
    AppConfig {
        proxy_mode: mode,
        proxy_url: proxy_url.to_string(),
        connect_timeout_secs: 5,
        ..AppConfig::default()
    }
}

async fn send_get(
    client: &CustomHttpClient,
    url: reqwest::Url,
) -> Result<reqwest::Response, CustomHttpError> {
    client
        .send_isolated(
            reqwest::Method::GET,
            url,
            UpstreamAuthScheme::Bearer,
            "test-key",
            HeaderMap::new(),
            None,
            None,
        )
        .await
}

async fn serve_http(
    status: u16,
    reason: &str,
    headers: &[(&str, String)],
    body: &str,
    hits: Arc<AtomicUsize>,
) -> std::net::SocketAddr {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("loopback listener");
    let addr = listener.local_addr().unwrap();
    let reason = reason.to_string();
    let body = body.to_string();
    let headers = headers
        .iter()
        .map(|(name, value)| (name.to_string(), value.clone()))
        .collect::<Vec<_>>();
    tokio::spawn(async move {
        loop {
            let Ok((mut stream, _)) = listener.accept().await else {
                break;
            };
            hits.fetch_add(1, Ordering::SeqCst);
            let mut buf = vec![0_u8; 4096];
            let _ = stream.read(&mut buf).await;
            let mut response = format!("HTTP/1.1 {status} {reason}\r\n");
            for (name, value) in &headers {
                response.push_str(&format!("{name}: {value}\r\n"));
            }
            response.push_str(&format!(
                "Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            ));
            let _ = stream.write_all(response.as_bytes()).await;
        }
    });
    addr
}

async fn serve_counting_proxy(hits: Arc<AtomicUsize>) -> std::net::SocketAddr {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("proxy listener");
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        loop {
            let Ok((mut stream, _)) = listener.accept().await else {
                break;
            };
            hits.fetch_add(1, Ordering::SeqCst);
            let mut buf = vec![0_u8; 4096];
            let _ = stream.read(&mut buf).await;
            let body = "proxy";
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = stream.write_all(response.as_bytes()).await;
        }
    });
    addr
}

#[test]
fn custom_http_builds_for_direct_manual_and_auto() {
    assert!(build_custom_http_client(&test_config(ProxyMode::Direct, "")).is_ok());
    assert!(
        build_custom_http_client(&test_config(ProxyMode::Manual, "http://127.0.0.1:8080")).is_ok()
    );
    let auto = build_custom_http_client(&test_config(ProxyMode::Auto, "")).unwrap();
    assert_eq!(auto.proxy_mode(), ProxyMode::Auto);
}

#[test]
fn http_inference_transport_is_policy_neutral_and_owns_join_auth_and_timeout() {
    let spec_none = HttpInferenceTransportSpec::no_redirects();
    let spec_follow = HttpInferenceTransportSpec::follow_redirects();
    assert_eq!(spec_none.redirect, InferenceRedirectPolicy::None);
    assert_eq!(spec_follow.redirect, InferenceRedirectPolicy::Follow);
    let direct =
        HttpInferenceTransport::build(&test_config(ProxyMode::Direct, ""), spec_none).unwrap();
    assert_eq!(direct.proxy_mode(), ProxyMode::Direct);
    assert_eq!(direct.redirect_policy(), InferenceRedirectPolicy::None);
    assert_eq!(direct.spec(), spec_none);
    let auto =
        HttpInferenceTransport::build(&test_config(ProxyMode::Auto, ""), spec_follow).unwrap();
    assert_eq!(auto.proxy_mode(), ProxyMode::Auto);
    assert_eq!(auto.redirect_policy(), InferenceRedirectPolicy::Follow);
    assert!(
        HttpInferenceTransport::build(
            &test_config(ProxyMode::Manual, "http://127.0.0.1:8080"),
            spec_none,
        )
        .is_ok()
    );

    let joined = HttpInferenceTransport::join_endpoint(
        crate::provider::COMMAND_CODE_GOAT_BASE_URL,
        crate::provider::COMMAND_CODE_GOAT_CHAT_COMPLETIONS_PATH,
    )
    .unwrap();
    assert_eq!(
        joined.as_str(),
        "https://api.commandcode.ai/provider/v1/chat/completions"
    );
    let with_userinfo = HttpInferenceTransport::join_endpoint(
        "https://user:pass@api.example.com/v1",
        "chat/completions",
    );
    assert!(
        with_userinfo.is_ok(),
        "neutral join must not apply Custom URL trust validation"
    );
    let bearer =
        HttpInferenceTransport::isolated_headers(UpstreamAuthScheme::Bearer, "sk-test").unwrap();
    assert_eq!(bearer.get(AUTHORIZATION).unwrap(), "Bearer sk-test");
    assert_eq!(
        HttpInferenceTransport::connect_timeout(&test_config(ProxyMode::Direct, "")),
        Duration::from_secs(5)
    );
    assert_eq!(
        HttpInferenceTransport::connect_timeout(&test_config(ProxyMode::Direct, "")),
        inference_connect_timeout(&test_config(ProxyMode::Direct, ""))
    );
}

#[test]
fn inference_endpoint_join_rejects_parent_path_escape() {
    let goat = join_inference_endpoint(
        crate::provider::COMMAND_CODE_GOAT_BASE_URL,
        crate::provider::COMMAND_CODE_GOAT_CHAT_COMPLETIONS_PATH,
    )
    .unwrap();
    assert_eq!(
        goat.as_str(),
        "https://api.commandcode.ai/provider/v1/chat/completions"
    );
    let messages = join_inference_endpoint(
        "http://127.0.0.1:9/provider/v1",
        crate::provider::COMMAND_CODE_GOAT_MESSAGES_PATH,
    )
    .unwrap();
    assert_eq!(messages.as_str(), "http://127.0.0.1:9/provider/v1/messages");
    assert!(join_inference_endpoint("https://api.commandcode.ai/provider/v1", "../admin").is_err());
    let bearer = isolated_inference_headers(UpstreamAuthScheme::Bearer, "sk-test").unwrap();
    assert_eq!(bearer.get(AUTHORIZATION).unwrap(), "Bearer sk-test");
}

#[test]
fn custom_endpoint_resolution_supports_common_bases_and_legacy_endpoints() {
    let root = resolve_custom_endpoints(
        "https://newapi.klarkxy.xyz",
        UpstreamProtocolKind::ChatCompletions,
    )
    .unwrap();
    assert_eq!(
        root.inference.as_str(),
        "https://newapi.klarkxy.xyz/v1/chat/completions"
    );
    assert_eq!(
        root.models.unwrap().as_str(),
        "https://newapi.klarkxy.xyz/v1/models"
    );

    for base in [
        "https://api.example.com/v1",
        "https://api.example.com/v1/",
        "https://api.example.com/openai/v1",
    ] {
        let resolved = resolve_custom_endpoints(base, UpstreamProtocolKind::Responses).unwrap();
        assert_eq!(
            resolved.inference.as_str(),
            format!("{}/responses", base.trim_end_matches('/'))
        );
        assert_eq!(
            resolved.models.unwrap().as_str(),
            format!("{}/models", base.trim_end_matches('/'))
        );
    }

    for (endpoint, protocol, models) in [
        (
            "https://api.example.com/v1/chat/completions",
            UpstreamProtocolKind::ChatCompletions,
            "https://api.example.com/v1/models",
        ),
        (
            "https://api.example.com/openai/v1/responses",
            UpstreamProtocolKind::Responses,
            "https://api.example.com/openai/v1/models",
        ),
        (
            "https://api.example.com/v1/messages",
            UpstreamProtocolKind::Messages,
            "https://api.example.com/v1/models",
        ),
    ] {
        let resolved = resolve_custom_endpoints(endpoint, protocol).unwrap();
        assert_eq!(resolved.inference.as_str(), endpoint);
        assert_eq!(resolved.models.unwrap().as_str(), models);
    }

    let opaque = resolve_custom_endpoints(
        "https://api.example.com/custom/infer",
        UpstreamProtocolKind::ChatCompletions,
    )
    .unwrap();
    assert_eq!(
        opaque.inference.as_str(),
        "https://api.example.com/custom/infer"
    );
    assert!(opaque.models.is_none());

    let mismatch = resolve_custom_endpoints(
        "https://api.example.com/v1/messages",
        UpstreamProtocolKind::Responses,
    )
    .unwrap();
    assert_eq!(
        mismatch.inference.as_str(),
        "https://api.example.com/v1/messages"
    );
    assert!(mismatch.models.is_none());
}

#[test]
fn custom_connect_timeout_clamps_without_changing_neutral_transport_timeout() {
    let mut config = test_config(ProxyMode::Direct, "");
    for secs in [1_u64, 4] {
        config.connect_timeout_secs = secs;
        assert_eq!(
            custom_connect_timeout(&config),
            Duration::from_secs(5),
            "Custom lower bound for {secs}"
        );
        assert_eq!(
            HttpInferenceTransport::connect_timeout(&config),
            Duration::from_secs(secs),
            "neutral transport preserves {secs}"
        );
    }
    for secs in [5_u64, 30, 60] {
        config.connect_timeout_secs = secs;
        assert_eq!(
            custom_connect_timeout(&config),
            Duration::from_secs(secs),
            "Custom in-range {secs}"
        );
        assert_eq!(
            HttpInferenceTransport::connect_timeout(&config),
            Duration::from_secs(secs),
            "neutral transport preserves {secs}"
        );
    }
    for secs in [61_u64, 300] {
        config.connect_timeout_secs = secs;
        assert_eq!(
            custom_connect_timeout(&config),
            Duration::from_secs(60),
            "Custom upper bound for {secs}"
        );
        assert_eq!(
            HttpInferenceTransport::connect_timeout(&config),
            Duration::from_secs(secs),
            "neutral transport preserves {secs}"
        );
    }
}

#[test]
fn isolated_headers_do_not_copy_client_or_dashboard_credentials() {
    let bearer = isolated_custom_headers(UpstreamAuthScheme::Bearer, "sk-custom").unwrap();
    assert_eq!(bearer.get(AUTHORIZATION).unwrap(), "Bearer sk-custom");
    assert!(!header_map_contains_forbidden_client_credentials(
        &bearer,
        UpstreamAuthScheme::Bearer
    ));
    assert!(bearer.get("cookie").is_none());
    assert!(bearer.get("x-api-key").is_none());
    assert!(bearer.get("x-goog-api-key").is_none());
    assert_eq!(bearer.len(), 1);

    let x_api = isolated_custom_headers(UpstreamAuthScheme::XApiKey, "sk-custom").unwrap();
    assert_eq!(x_api.get("x-api-key").unwrap(), "sk-custom");
    assert!(x_api.get(AUTHORIZATION).is_none());
    assert!(!header_map_contains_forbidden_client_credentials(
        &x_api,
        UpstreamAuthScheme::XApiKey
    ));
    assert_eq!(x_api.len(), 1);
    assert!(forbidden_forwarded_header_names().contains(&"cookie"));
    assert!(forbidden_forwarded_header_names().contains(&"authorization"));
}

#[tokio::test]
async fn http_inference_transport_send_and_bounded_read_do_not_discover_models() {
    let hits = Arc::new(AtomicUsize::new(0));
    let addr = serve_http(200, "OK", &[], r#"{"ok":true}"#, hits.clone()).await;
    let transport = HttpInferenceTransport::build(
        &test_config(ProxyMode::Direct, ""),
        HttpInferenceTransportSpec::no_redirects(),
    )
    .unwrap();
    let url = reqwest::Url::parse(&format!("http://127.0.0.1:{}/v1/ping", addr.port())).unwrap();
    let response = transport
        .send(InferenceHttpRequest {
            method: reqwest::Method::POST,
            url,
            auth: Some((UpstreamAuthScheme::Bearer, "transport-key")),
            extra_headers: HeaderMap::new(),
            body: Some(br#"{"ping":true}"#.to_vec()),
            request_timeout: Some(Duration::from_secs(5)),
        })
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = HttpInferenceTransport::read_body_limited(response, 64)
        .await
        .unwrap();
    assert_eq!(body, br#"{"ok":true}"#);
    assert_eq!(hits.load(Ordering::SeqCst), 1);

    let oversized = Arc::new(AtomicUsize::new(0));
    let oversized_addr = serve_http(
        200,
        "OK",
        &[("Content-Type", "application/json".to_string())],
        &"x".repeat(32),
        oversized.clone(),
    )
    .await;
    let oversized_url = reqwest::Url::parse(&format!(
        "http://127.0.0.1:{}/v1/ping",
        oversized_addr.port()
    ))
    .unwrap();
    let oversized_response = transport
        .send(InferenceHttpRequest {
            method: reqwest::Method::GET,
            url: oversized_url,
            auth: None,
            extra_headers: HeaderMap::new(),
            body: None,
            request_timeout: Some(Duration::from_secs(5)),
        })
        .await
        .unwrap();
    let error = HttpInferenceTransport::read_body_limited(oversized_response, 8)
        .await
        .expect_err("bounded reader must reject an oversized body");
    assert!(
        matches!(error, InferenceHttpError::Oversize { limit: 8 }),
        "{error:?}"
    );
}

#[tokio::test]
async fn http_inference_transport_redirect_policy_is_owned_by_the_spec() {
    for status in [301_u16, 302, 307, 308] {
        let second_hits = Arc::new(AtomicUsize::new(0));
        let second = serve_http(200, "OK", &[], "second", second_hits.clone()).await;
        let first_hits = Arc::new(AtomicUsize::new(0));
        let location = format!("http://127.0.0.1:{}/next", second.port());
        let first = serve_http(
            status,
            "Redirect",
            &[("Location", location)],
            "",
            first_hits.clone(),
        )
        .await;
        let start =
            reqwest::Url::parse(&format!("http://127.0.0.1:{}/start", first.port())).unwrap();

        let none = HttpInferenceTransport::build(
            &test_config(ProxyMode::Direct, ""),
            HttpInferenceTransportSpec::no_redirects(),
        )
        .unwrap();
        let blocked = none
            .send(InferenceHttpRequest {
                method: reqwest::Method::GET,
                url: start.clone(),
                auth: None,
                extra_headers: HeaderMap::new(),
                body: None,
                request_timeout: None,
            })
            .await
            .unwrap();
        assert_eq!(blocked.status().as_u16(), status, "status {status}");
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert_eq!(second_hits.load(Ordering::SeqCst), 0, "none {status}");

        let follow = HttpInferenceTransport::build(
            &test_config(ProxyMode::Direct, ""),
            HttpInferenceTransportSpec::follow_redirects(),
        )
        .unwrap();
        let followed = follow
            .send(InferenceHttpRequest {
                method: reqwest::Method::GET,
                url: start,
                auth: None,
                extra_headers: HeaderMap::new(),
                body: None,
                request_timeout: None,
            })
            .await
            .unwrap();
        assert_eq!(followed.status(), StatusCode::OK, "follow {status}");
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert_eq!(
            second_hits.load(Ordering::SeqCst),
            1,
            "follow {status} must open the Location target"
        );
    }
}

#[tokio::test]
async fn s02_secret_bearing_follow_spec_refuses_before_any_hop() {
    let second_hits = Arc::new(AtomicUsize::new(0));
    let second = serve_http(200, "OK", &[], "second", second_hits.clone()).await;
    let first_hits = Arc::new(AtomicUsize::new(0));
    let location = format!("http://127.0.0.1:{}/next", second.port());
    let first = serve_http(
        302,
        "Redirect",
        &[("Location", location)],
        "",
        first_hits.clone(),
    )
    .await;
    let follow = HttpInferenceTransport::build(
        &test_config(ProxyMode::Direct, ""),
        HttpInferenceTransportSpec::follow_redirects(),
    )
    .unwrap();
    let url = reqwest::Url::parse(&format!("http://127.0.0.1:{}/start", first.port())).unwrap();
    let error = follow
        .send(InferenceHttpRequest {
            method: reqwest::Method::GET,
            url,
            auth: Some((UpstreamAuthScheme::Bearer, "sk-secret")),
            extra_headers: HeaderMap::new(),
            body: None,
            request_timeout: None,
        })
        .await
        .expect_err("secret-bearing follow must fail closed");
    let _ = error;
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert_eq!(first_hits.load(Ordering::SeqCst), 0);
    assert_eq!(second_hits.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn s03_metadata_targets_never_cause_a_secret_bearing_connect() {
    let hits = Arc::new(AtomicUsize::new(0));
    let bait = serve_http(200, "OK", &[], "metadata", hits.clone()).await;
    let client = build_custom_http_client(&test_config(ProxyMode::Direct, "")).unwrap();
    for value in [
        "https://169.254.169.254/latest",
        "http://metadata.google.internal/messages",
        "http://0xa9fea9fe/latest",
    ] {
        let url = reqwest::Url::parse(value).unwrap();
        let error = client
            .send_isolated(
                reqwest::Method::GET,
                url,
                UpstreamAuthScheme::Bearer,
                "sk-secret",
                HeaderMap::new(),
                None,
                None,
            )
            .await
            .expect_err(value);
        assert!(
            matches!(error, CustomHttpError::InvalidUrl(_)),
            "{value}: {error}"
        );
    }
    let allowed = reqwest::Url::parse(&format!("http://127.0.0.1:{}/v1", bait.port())).unwrap();
    let ok = send_get(&client, allowed).await.unwrap();
    assert_eq!(ok.status(), StatusCode::OK);
    assert_eq!(hits.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn redirects_are_not_followed_for_301_302_307_308() {
    for status in [301_u16, 302, 307, 308] {
        let second_hits = Arc::new(AtomicUsize::new(0));
        let second = serve_http(200, "OK", &[], "second", second_hits.clone()).await;
        let first_hits = Arc::new(AtomicUsize::new(0));
        let location = format!("http://127.0.0.1:{}/next", second.port());
        let first = serve_http(
            status,
            "Redirect",
            &[("Location", location)],
            "",
            first_hits.clone(),
        )
        .await;
        let client = build_custom_http_client(&test_config(ProxyMode::Direct, "")).unwrap();
        let url = reqwest::Url::parse(&format!("http://127.0.0.1:{}/start", first.port())).unwrap();
        let response = send_get(&client, url).await.unwrap();
        assert_eq!(response.status().as_u16(), status, "status {status}");
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert_eq!(first_hits.load(Ordering::SeqCst), 1, "first hop {status}");
        assert_eq!(
            second_hits.load(Ordering::SeqCst),
            0,
            "redirect {status} must not open a second connection"
        );
    }
}

#[tokio::test]
async fn direct_does_not_use_manual_proxy_and_manual_does_not_bypass_it() {
    let upstream_hits = Arc::new(AtomicUsize::new(0));
    let upstream = serve_http(200, "OK", &[], "direct", upstream_hits.clone()).await;
    let proxy_hits = Arc::new(AtomicUsize::new(0));
    let proxy = serve_counting_proxy(proxy_hits.clone()).await;

    let target = reqwest::Url::parse(&format!("http://127.0.0.1:{}/v1", upstream.port())).unwrap();
    let direct = build_custom_http_client(&test_config(ProxyMode::Direct, "")).unwrap();
    let response = send_get(&direct, target.clone()).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(upstream_hits.load(Ordering::SeqCst), 1);
    assert_eq!(proxy_hits.load(Ordering::SeqCst), 0);

    let manual = build_custom_http_client(&test_config(
        ProxyMode::Manual,
        &format!("http://127.0.0.1:{}", proxy.port()),
    ))
    .unwrap();
    let proxied = send_get(&manual, target).await.unwrap();
    assert_eq!(proxied.status(), StatusCode::OK);
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert_eq!(proxy_hits.load(Ordering::SeqCst), 1);
    assert_eq!(upstream_hits.load(Ordering::SeqCst), 1);
}

async fn serve_delayed_json(delay: Duration, body: &str) -> std::net::SocketAddr {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("loopback listener");
    let addr = listener.local_addr().unwrap();
    let body = body.to_string();
    tokio::spawn(async move {
        let Ok((mut stream, _)) = listener.accept().await else {
            return;
        };
        let mut buf = vec![0_u8; 4096];
        let _ = stream.read(&mut buf).await;
        tokio::time::sleep(delay).await;
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        let _ = stream.write_all(response.as_bytes()).await;
    });
    addr
}

#[tokio::test]
async fn connect_timeout_does_not_bound_post_connect_non_stream_reads() {
    let addr = serve_delayed_json(Duration::from_millis(1500), r#"{"ok":true}"#).await;
    let mut config = test_config(ProxyMode::Direct, "");
    config.connect_timeout_secs = 1;
    let client = build_custom_http_client(&config).unwrap();
    let url = reqwest::Url::parse(&format!("http://127.0.0.1:{}/v1", addr.port())).unwrap();
    let response = client
        .send_isolated(
            reqwest::Method::GET,
            url,
            UpstreamAuthScheme::Bearer,
            "test-key",
            HeaderMap::new(),
            None,
            Some(Duration::from_secs(5)),
        )
        .await
        .expect("post-connect delay must use the per-request timeout, not connect_timeout");
    assert_eq!(response.status(), StatusCode::OK);
}

#[tokio::test]
async fn non_stream_request_timeout_is_enforced_per_request() {
    let addr = serve_delayed_json(Duration::from_secs(3), r#"{"ok":true}"#).await;
    let mut config = test_config(ProxyMode::Direct, "");
    config.connect_timeout_secs = 5;
    let client = build_custom_http_client(&config).unwrap();
    let url = reqwest::Url::parse(&format!("http://127.0.0.1:{}/v1", addr.port())).unwrap();
    let error = client
        .send_isolated(
            reqwest::Method::GET,
            url,
            UpstreamAuthScheme::Bearer,
            "test-key",
            HeaderMap::new(),
            None,
            Some(Duration::from_secs(1)),
        )
        .await
        .expect_err("non-stream Custom requests must honor the per-request timeout");
    assert!(
        error.to_string().to_ascii_lowercase().contains("timed")
            || error.to_string().to_ascii_lowercase().contains("timeout"),
        "{error}"
    );
}
