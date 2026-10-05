use super::*;
use crate::models::AppConfig;
use crate::provider::{COMMAND_CODE_PROVIDER_ID, OPENCODE_PROVIDER_ID};
use std::net::SocketAddr;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

fn runtime(enabled: bool, verification_status: ConnectionVerificationStatus) -> GoatAccountRuntime {
    GoatAccountRuntime {
        account_id: "goat-1".into(),
        enabled,
        verification_status,
        setup_ready: true,
        has_key: true,
    }
}

#[test]
fn account_eligibility_does_not_reinterpret_the_provider_model_preset() {
    let pending = runtime(true, ConnectionVerificationStatus::Pending);
    assert!(pending.eligible());
    assert!(pending.serves("any-model-in-the-provider-contract"));
    assert_eq!(
        pending.serves("any-model-in-the-provider-contract"),
        pending.eligible()
    );
    let disabled = runtime(false, ConnectionVerificationStatus::Verified);
    assert!(!disabled.eligible());
    assert!(!disabled.serves("any-model-in-the-provider-contract"));
    let mut missing_key = runtime(true, ConnectionVerificationStatus::Verified);
    missing_key.has_key = false;
    assert!(!missing_key.eligible());
    assert_eq!(missing_key.serves("catalog-model"), missing_key.eligible());
}

#[test]
fn opencode_go_models_url_keeps_the_official_v1_segment() {
    assert_eq!(
        opencode_go_models_url_for_base("https://opencode.ai/zen/go"),
        "https://opencode.ai/zen/go/v1/models"
    );
    assert_eq!(
        opencode_go_models_url_for_base("http://127.0.0.1:9/provider/v1/"),
        "http://127.0.0.1:9/provider/v1/models"
    );
}

#[test]
fn goat_catalog_discovery_keeps_supported_endpoints_metadata() {
    let discovery = parse_provider_catalog_discovery(
        br#"{
            "object":"list",
            "data":[
                {"id":"xiaomi/mimo-v2.6-flash","supported_endpoints":["/chat/completions","/responses"]},
                {"id":"claude-sonnet-4-6","supported_endpoints":["/messages"]}
            ]
        }"#,
        "Command Code",
    )
    .unwrap();
    assert_eq!(
        discovery.models,
        vec![
            "xiaomi/mimo-v2.6-flash".to_string(),
            "claude-sonnet-4-6".to_string()
        ]
    );
    assert_eq!(
        discovery
            .protocol_baseline
            .protocols_for("command-code", "xiaomi/mimo-v2.6-flash"),
        Some(vec![
            crate::provider::UpstreamProtocolKind::ChatCompletions,
            crate::provider::UpstreamProtocolKind::Responses
        ])
    );
}

#[test]
fn go_catalog_discovery_does_not_invent_protocol_lists() {
    let discovery = parse_provider_catalog_discovery(
        br#"{"object":"list","data":[{"id":"mimo-v2.6-flash"}]}"#,
        "OpenCode Go",
    )
    .unwrap();
    assert_eq!(discovery.models, vec!["mimo-v2.6-flash".to_string()]);
    assert_eq!(
        discovery.protocol_baseline,
        OfficialProtocolBaseline::Unavailable
    );
}

#[test]
fn official_catalog_builders_match_the_sealed_models_urls() {
    assert_eq!(
        super::official_opencode_go_models_url(),
        "https://opencode.ai/zen/go/v1/models"
    );
    assert_eq!(
        goat_models_url_for_base(crate::provider::COMMAND_CODE_GOAT_BASE_URL),
        official_goat_models_url()
    );
    assert_eq!(
        official_goat_models_url(),
        "https://api.commandcode.ai/provider/v1/models"
    );
}

#[test]
fn nonofficial_catalog_url_does_not_call_rewrite() {
    let mut called = false;
    let goat_built = "http://127.0.0.1:9/provider/v1/models".to_string();
    let selected =
        super::select_catalog_outbound(goat_built.clone(), &official_goat_models_url(), |_| {
            called = true;
            Err("malformed".to_string())
        })
        .unwrap();
    assert!(!called);
    assert_eq!(selected, goat_built);

    let go_built = "http://127.0.0.1:9/zen/go/v1/models".to_string();
    let selected = super::select_catalog_outbound(
        go_built.clone(),
        &super::official_opencode_go_models_url(),
        |_| {
            called = true;
            Err("malformed".to_string())
        },
    )
    .unwrap();
    assert!(!called);
    assert_eq!(selected, go_built);

    let zen = "https://opencode.ai/zen/v1/models".to_string();
    let selected = super::select_catalog_outbound(
        zen.clone(),
        &super::official_opencode_go_models_url(),
        |_| {
            called = true;
            Err("malformed".to_string())
        },
    )
    .unwrap();
    assert!(!called);
    assert_eq!(selected, zen);
}

fn catalog_body(id: &str) -> String {
    format!(r#"{{"object":"list","data":[{{"id":"{id}"}}]}}"#)
}

async fn serve_catalog(body: String) -> (SocketAddr, tokio::task::JoinHandle<String>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let task = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut buf = Vec::new();
        let mut chunk = [0_u8; 1024];
        loop {
            let read = stream.read(&mut chunk).await.unwrap_or(0);
            if read == 0 {
                break;
            }
            buf.extend_from_slice(&chunk[..read]);
            if buf.windows(4).any(|window| window == b"\r\n\r\n") {
                break;
            }
        }
        let text = String::from_utf8_lossy(&buf);
        let path = text
            .lines()
            .next()
            .unwrap_or_default()
            .split_whitespace()
            .nth(1)
            .unwrap_or_default()
            .to_string();
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        stream.write_all(response.as_bytes()).await.unwrap();
        path
    });
    (addr, task)
}

#[tokio::test]
async fn opencode_go_catalog_fetch_gets_the_documented_models_path() {
    let (addr, task) = serve_catalog(catalog_body("ocg-go-chat")).await;
    let base = format!("http://127.0.0.1:{}/zen/go", addr.port());
    let discovery = refresh_opencode_go_catalog_discovery(&AppConfig::default(), &base)
        .await
        .unwrap();
    assert_eq!(discovery.models, vec!["ocg-go-chat".to_string()]);
    assert_eq!(task.await.unwrap(), "/zen/go/v1/models");
    assert!(base.starts_with("http://127.0.0.1:"));
}

#[tokio::test]
async fn command_code_catalog_fetch_gets_the_documented_models_path() {
    let (addr, task) = serve_catalog(catalog_body("ocg-goat-chat")).await;
    let base = format!("http://127.0.0.1:{}/provider/v1", addr.port());
    let discovery = refresh_command_code_catalog_discovery(&AppConfig::default(), &base)
        .await
        .unwrap();
    assert_eq!(discovery.models, vec!["ocg-goat-chat".to_string()]);
    assert_eq!(task.await.unwrap(), "/provider/v1/models");
    assert!(base.starts_with("http://127.0.0.1:"));
}

#[cfg(feature = "ollama-cloud-loopback-test")]
#[tokio::test]
async fn feature_on_catalog_rewrite_preserves_official_path_and_query() {
    assert_rewritten_catalog(
        OPENCODE_PROVIDER_ID,
        &super::official_opencode_go_models_url(),
        "OpenCode Go",
        "ocg-go-chat",
    )
    .await;
    assert_rewritten_catalog(
        COMMAND_CODE_PROVIDER_ID,
        &official_goat_models_url(),
        "Command Code",
        "ocg-goat-chat",
    )
    .await;
}

#[cfg(feature = "ollama-cloud-loopback-test")]
async fn assert_rewritten_catalog(provider_id: &str, official: &str, label: &str, model_id: &str) {
    for suffix in ["", "?beta=1"] {
        let (addr, task) = serve_catalog(catalog_body(model_id)).await;
        let origin = format!("http://127.0.0.1:{}", addr.port());
        let mapping = format!(r#"{{"{provider_id}":"{origin}"}}"#);
        let canonical = format!("{official}{suffix}");
        let rewritten = crate::cpa_test_endpoints::rewrite_url_with_mapping(
            provider_id,
            &canonical,
            Some(&mapping),
        )
        .unwrap()
        .expect("official catalog URL rewrites");
        let parsed = reqwest::Url::parse(&rewritten).unwrap();
        let canonical_url = reqwest::Url::parse(&canonical).unwrap();
        assert_eq!(parsed.scheme(), "http");
        assert_eq!(parsed.host_str(), Some("127.0.0.1"));
        assert_eq!(parsed.port(), Some(addr.port()));
        assert_eq!(parsed.path(), canonical_url.path());
        assert_eq!(parsed.query(), canonical_url.query());
        if suffix.is_empty() {
            let selected = super::select_catalog_outbound(official.to_string(), official, |_| {
                Ok(Some(rewritten.clone()))
            })
            .unwrap();
            assert_eq!(selected, rewritten);
        }
        let discovery =
            super::probe_public_provider_catalog_at_url(&AppConfig::default(), &rewritten, label)
                .await
                .unwrap();
        assert_eq!(discovery.models, vec![model_id.to_string()]);
        let expected_path = match canonical_url.query() {
            Some(query) => format!("{}?{query}", canonical_url.path()),
            None => canonical_url.path().to_string(),
        };
        assert_eq!(task.await.unwrap(), expected_path);
    }
}

#[cfg(feature = "ollama-cloud-loopback-test")]
#[test]
fn malformed_catalog_mapping_rejects_before_outbound() {
    for (provider_id, official) in [
        (
            OPENCODE_PROVIDER_ID,
            super::official_opencode_go_models_url(),
        ),
        (COMMAND_CODE_PROVIDER_ID, official_goat_models_url()),
    ] {
        let parsed =
            crate::cpa_test_endpoints::rewrite_url_with_mapping(provider_id, &official, Some("{"));
        assert!(parsed.is_err());
        let failure =
            super::select_catalog_outbound(official.clone(), &official, |_| parsed.clone())
                .unwrap_err();
        assert!(!failure.message.is_empty());
        let kept =
            super::select_catalog_outbound(official.clone(), &official, |_| Ok(None)).unwrap();
        assert_eq!(kept, official);
    }
}

#[cfg(not(feature = "ollama-cloud-loopback-test"))]
#[test]
fn feature_off_catalog_ignores_endpoint_env_mapping() {
    let populated =
        r#"{"opencode":"http://127.0.0.1:18080","command-code":"http://127.0.0.1:18080"}"#;
    let cases = [
        (
            OPENCODE_PROVIDER_ID,
            super::official_opencode_go_models_url(),
            "https://opencode.ai/",
        ),
        (
            COMMAND_CODE_PROVIDER_ID,
            official_goat_models_url(),
            "https://api.commandcode.ai/",
        ),
    ];
    for (provider_id, official, host_prefix) in cases {
        assert!(official.starts_with(host_prefix));
        assert_eq!(
            crate::cpa_test_endpoints::rewrite_url(provider_id, &official),
            Ok(None)
        );
        assert_eq!(
            crate::cpa_test_endpoints::rewrite_url_with_mapping(
                provider_id,
                &official,
                Some(populated)
            ),
            Ok(None)
        );
        assert_eq!(
            crate::cpa_test_endpoints::rewrite_url_with_mapping(provider_id, &official, Some("{")),
            Ok(None)
        );
        let selected = super::select_catalog_outbound(official.clone(), &official, |url| {
            crate::cpa_test_endpoints::rewrite_url_with_mapping(provider_id, url, Some(populated))
        })
        .unwrap();
        assert_eq!(selected, official);
    }
}
