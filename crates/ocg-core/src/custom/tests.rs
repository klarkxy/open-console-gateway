use super::*;
use crate::provider::ProviderBindingError;
use std::net::IpAddr;

#[test]
fn custom_endpoint_url_rejects_credentials_query_fragment_and_non_http_schemes() {
    use crate::provider::validate_custom_model_id;

    assert!(validate_custom_endpoint_url("http://api.example.com/v1/responses").is_ok());
    assert!(validate_custom_endpoint_url("https://[2001:db8::1]/v1/responses").is_ok());
    assert!(validate_custom_endpoint_url("https://user:pass@api.example.com/messages").is_err());
    assert!(validate_custom_endpoint_url("https://api.example.com/responses?x=1").is_err());
    assert!(validate_custom_endpoint_url("https://api.example.com/responses#frag").is_err());
    assert!(validate_custom_endpoint_url("javascript:alert(1)").is_err());
    assert!(validate_custom_endpoint_url("ftp://api.example.com/responses").is_err());
    assert_eq!(
        validate_custom_model_id("deepseek/deepseek-v4-flash").unwrap(),
        "deepseek/deepseek-v4-flash"
    );
    assert!(validate_custom_model_id("").is_err());
    assert_eq!(
        derive_custom_models_endpoint(
            "https://api.example.com/v1/chat/completions",
            UpstreamProtocolKind::ChatCompletions,
        )
        .unwrap()
        .as_str(),
        "https://api.example.com/v1/models"
    );
    for base in ["https://api.example.com", "https://api.example.com/v1"] {
        assert_eq!(
            derive_custom_models_endpoint(base, UpstreamProtocolKind::ChatCompletions)
                .unwrap()
                .as_str(),
            "https://api.example.com/v1/models"
        );
    }
    assert!(
        derive_custom_models_endpoint(
            "https://api.example.com/v1/custom-chat",
            UpstreamProtocolKind::ChatCompletions,
        )
        .is_err()
    );
}

#[tokio::test]
async fn verification_resolves_root_base_to_the_selected_protocol_path() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("loopback listener");
    let addr = listener.local_addr().unwrap();
    let (request_tx, request_rx) = tokio::sync::oneshot::channel();
    tokio::spawn(async move {
        let Ok((mut stream, _)) = listener.accept().await else {
            return;
        };
        let mut buf = vec![0_u8; 8192];
        let read = stream.read(&mut buf).await.unwrap_or(0);
        let _ = request_tx.send(String::from_utf8_lossy(&buf[..read]).to_string());
        let body = r#"{"choices":[{"message":{"role":"assistant","content":"ok"}}]}"#;
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        let _ = stream.write_all(response.as_bytes()).await;
    });

    let app_config = AppConfig {
        proxy_mode: crate::models::ProxyMode::Direct,
        connect_timeout_secs: 5,
        non_stream_timeout_secs: 5,
        ..AppConfig::default()
    };
    let custom_config = AccountCustomConfig {
        account_id: "acc".into(),
        endpoint_url: format!("http://{addr}"),
        upstream_protocol: UpstreamProtocolKind::ChatCompletions,
        created_at: chrono::Utc::now(),
        updated_at: chrono::Utc::now(),
    };
    let capability = AccountModelCapability {
        account_id: "acc".into(),
        public_model: "local".into(),
        upstream_model: "local-upstream".into(),
        protocol: UpstreamProtocolKind::ChatCompletions,
        verified_at: None,
        source: "manual".into(),
    };
    probe_custom_connection(&app_config, &custom_config, &capability, "sk-test")
        .await
        .expect("root base verification");
    let request = request_rx.await.expect("captured request");
    assert!(
        request.starts_with("POST /v1/chat/completions HTTP/1.1\r\n"),
        "{request}"
    );
    assert!(
        request
            .to_ascii_lowercase()
            .contains("authorization: bearer sk-test"),
        "{request}"
    );
}

#[test]
fn custom_url_host_uses_url_host_not_bracketed_host_str() {
    assert!(validate_custom_endpoint_url("http://[::ffff:127.0.0.1]/v1/responses").is_ok());
    assert!(validate_custom_endpoint_url("http://[::1]/v1/responses").is_ok());
    let mapped_loopback =
        validate_custom_endpoint_url("http://[::ffff:127.0.0.1]/v1/responses").unwrap();
    let parsed = reqwest::Url::parse(&mapped_loopback).unwrap();
    match inspect_custom_url(&parsed).unwrap().host {
        CustomUrlHost::Ip(ip) => {
            assert_eq!(ip, "::ffff:127.0.0.1".parse::<IpAddr>().unwrap());
        }
        CustomUrlHost::Domain(domain) => {
            panic!("mapped loopback must stay an IP host, got {domain}")
        }
    }
    let metadata = reqwest::Url::parse("https://[::ffff:169.254.169.254]/latest").unwrap();
    assert!(
        inspect_custom_url(&metadata).is_err(),
        "mapped metadata IP must be refused before outbound"
    );
}

#[test]
fn custom_endpoint_url_normalizes_decimal_loopback_literals() {
    assert_eq!(
        validate_custom_endpoint_url("http://127.1:8080/v1/responses").unwrap(),
        "http://127.0.0.1:8080/v1/responses"
    );
    assert_eq!(
        validate_custom_endpoint_url("http://127.0.1/v1/responses").unwrap(),
        "http://127.0.0.1/v1/responses"
    );
    let parsed = reqwest::Url::parse("http://127.1/v1").unwrap();
    match inspect_custom_url(&parsed).unwrap().host {
        CustomUrlHost::Ip(ip) => assert_eq!(ip, "127.0.0.1".parse::<IpAddr>().unwrap()),
        CustomUrlHost::Domain(domain) => panic!("127.1 must not stay a domain: {domain}"),
    }
}

fn assert_invalid_custom_url(value: &str) {
    assert!(
        matches!(
            validate_custom_endpoint_url(value),
            Err(ProviderBindingError::InvalidCustomBaseUrl(_))
        ),
        "{value:?} must fail closed as InvalidCustomBaseUrl"
    );
}

#[test]
fn custom_endpoint_url_rejects_empty_overlong_unparsed_and_file_urls() {
    assert_invalid_custom_url("");
    assert_invalid_custom_url("   ");
    assert_invalid_custom_url(&format!("https://api.example.com/{}", "a".repeat(2048)));
    assert_invalid_custom_url("not a url");
    let hostless = reqwest::Url::parse("file:///tmp").unwrap();
    assert!(
        matches!(
            inspect_custom_url(&hostless),
            Err(ProviderBindingError::InvalidCustomBaseUrl(_))
        ),
        "file URLs must fail closed as InvalidCustomBaseUrl"
    );
}

#[test]
fn custom_runtime_identity_is_configurable_http_not_a_base_class() {
    use crate::provider::{
        COMMAND_CODE_PROVIDER_ID, CUSTOM_PROVIDER_ID, OPENCODE_PROVIDER_ID,
        OPENCODE_ZEN_FREE_PROVIDER_ID, ProviderAdapterKind, ProviderRegistry,
    };
    assert_eq!(
        ProviderAdapterKind::from_provider_id(CUSTOM_PROVIDER_ID),
        Some(ProviderAdapterKind::ConfigurableHttp)
    );
    for provider_id in [
        OPENCODE_PROVIDER_ID,
        OPENCODE_ZEN_FREE_PROVIDER_ID,
        COMMAND_CODE_PROVIDER_ID,
    ] {
        assert_ne!(
            ProviderAdapterKind::from_provider_id(provider_id),
            Some(ProviderAdapterKind::ConfigurableHttp)
        );
    }
    let runtime = CustomAccountRuntime {
        account_id: "acc".into(),
        enabled: true,
        verification_status: ConnectionVerificationStatus::Verified,
        setup_ready: true,
        has_key: true,
        config: AccountCustomConfig {
            account_id: "acc".into(),
            endpoint_url: "http://127.0.0.1:9/v1/chat/completions".into(),
            upstream_protocol: UpstreamProtocolKind::ChatCompletions,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        },
        capabilities: Vec::new(),
        protocol_passthrough: false,
    };
    assert!(runtime.eligible());
    let plan = crate::provider::builtin_provider(CUSTOM_PROVIDER_ID).unwrap();
    let verification = ProviderRegistry::get(CUSTOM_PROVIDER_ID)
        .expect("Custom API has a sealed descriptor")
        .verification;
    assert!(verification.never_auto_enable);
    assert!(verification.probe_first_declared_model);
    assert!(!verification.uses_get_models);
    assert_eq!(
        verification.runtime_availability,
        plan.verification_runtime_availability
    );
}

#[test]
fn custom_model_id_matching_is_exact_or_case_folded_without_separator_folding() {
    assert!(custom_model_id_matches("glm-5.2", "GLM-5.2"));
    assert!(custom_model_id_matches("my-local", "my-local"));
    assert!(!custom_model_id_matches("glm-5.2", "glm/5.2"));
    assert!(custom_model_id_matches(
        "deepseek/deepseek-v4-flash",
        "DeepSeek/deepseek-v4-flash"
    ));
    assert!(!custom_model_id_matches(
        "deepseek/deepseek-v4-flash",
        "deepseek-v4-flash"
    ));
}

#[test]
fn verification_bodies_are_non_stream_and_token_bounded() {
    let chat = serde_json::from_slice::<Value>(
        &minimal_verification_body(UpstreamProtocolKind::ChatCompletions, "local-model").unwrap(),
    )
    .unwrap();
    assert_eq!(chat["stream"], false);
    assert_eq!(chat["max_tokens"], 1);
    assert_eq!(chat["model"], "local-model");

    let responses = serde_json::from_slice::<Value>(
        &minimal_verification_body(UpstreamProtocolKind::Responses, "local-model").unwrap(),
    )
    .unwrap();
    assert_eq!(responses["stream"], false);
    assert_eq!(responses["max_output_tokens"], 16);

    let messages = serde_json::from_slice::<Value>(
        &minimal_verification_body(UpstreamProtocolKind::Messages, "local-model").unwrap(),
    )
    .unwrap();
    assert_eq!(messages["stream"], false);
    assert_eq!(messages["max_tokens"], 1);
}

#[test]
fn model_discovery_page_uses_last_id_and_ignores_unsafe_data_ids() {
    let page = parse_model_discovery_page(
            br#"{"data":[{"id":"Model-A"},{"id":"  "},{"id":"model-b\n"}],"has_more":true,"last_id":"Model-A"}"#,
        )
        .unwrap();
    assert_eq!(page.models, vec!["Model-A"]);
    assert_eq!(page.cursor.as_deref(), Some("Model-A"));
    assert!(page.has_more);
}

#[test]
fn model_discovery_cursor_replaces_query_and_rejects_loops() {
    let mut url = derive_custom_models_endpoint(
        "https://api.example.com/v1/responses",
        UpstreamProtocolKind::Responses,
    )
    .unwrap();
    let mut cursors = HashSet::new();
    advance_model_discovery_cursor(&mut url, &mut cursors, "first").unwrap();
    assert_eq!(
        url.as_str(),
        "https://api.example.com/v1/models?after_id=first"
    );
    advance_model_discovery_cursor(&mut url, &mut cursors, "second").unwrap();
    assert_eq!(
        url.as_str(),
        "https://api.example.com/v1/models?after_id=second"
    );
    assert!(advance_model_discovery_cursor(&mut url, &mut cursors, "SECOND").is_err());
}

#[test]
fn malformed_model_discovery_shapes_are_actionable() {
    assert!(parse_model_discovery_page(br#"[]"#).is_err());
    assert!(parse_model_discovery_page(br#"{"data":{}}"#).is_err());
    assert!(parse_model_discovery_page(br#"{"data":[],"has_more":"yes"}"#).is_err());
    assert!(parse_model_discovery_page(br#"{"data":[],"last_id":42}"#).is_err());
}

#[test]
fn model_discovery_headers_are_protocol_specific() {
    let chat = model_discovery_headers(UpstreamProtocolKind::ChatCompletions);
    assert_eq!(
        chat.get(reqwest::header::ACCEPT).unwrap(),
        "application/json"
    );
    assert!(chat.get("anthropic-version").is_none());

    let messages = model_discovery_headers(UpstreamProtocolKind::Messages);
    assert_eq!(messages.get("anthropic-version").unwrap(), "2023-06-01");
    assert!(messages.get(reqwest::header::AUTHORIZATION).is_none());
    assert!(messages.get("x-api-key").is_none());
}

#[test]
fn model_discovery_response_body_limit_is_enforced() {
    assert!(model_discovery_body_size_allowed(MAX_CUSTOM_MODEL_DISCOVERY_BODY_BYTES).is_ok());
    assert!(model_discovery_body_size_allowed(MAX_CUSTOM_MODEL_DISCOVERY_BODY_BYTES + 1).is_err());
}

#[test]
fn model_discovery_timeout_is_shorter_than_the_general_request_timeout() {
    let mut config = AppConfig {
        non_stream_timeout_secs: 900,
        ..AppConfig::default()
    };
    assert_eq!(
        model_discovery_request_timeout(&config),
        Duration::from_secs(CUSTOM_MODEL_DISCOVERY_TIMEOUT_SECS)
    );

    config.non_stream_timeout_secs = 7;
    assert_eq!(
        model_discovery_request_timeout(&config),
        Duration::from_secs(7)
    );
}

#[test]
fn verification_requires_the_requested_protocol_and_rejects_false_success() {
    let cases: [(UpstreamProtocolKind, &[u8]); 3] = [
        (
            UpstreamProtocolKind::ChatCompletions,
            br#"{"choices":[{"message":{"content":"ok"}}]}"#,
        ),
        (
            UpstreamProtocolKind::Responses,
            br#"{"object":"response","output":[],"status":"completed","error":null}"#,
        ),
        (
            UpstreamProtocolKind::Messages,
            br#"{"type":"message","role":"assistant","content":[]}"#,
        ),
    ];
    for (protocol, body) in cases {
        assert!(prove_verified_protocol_response(StatusCode::OK, body, protocol).is_ok());
        for (other, _) in cases {
            if protocol != other {
                assert!(prove_verified_protocol_response(StatusCode::OK, body, other).is_err());
            }
        }
        for invalid in [
            br#"{"id":"ok"}"#.as_slice(),
            br#"{"error":{"message":"denied"}}"#,
            b"[1]",
            b"not-json",
        ] {
            assert!(prove_verified_protocol_response(StatusCode::OK, invalid, protocol).is_err());
        }
        for status in [StatusCode::BAD_REQUEST, StatusCode::FOUND] {
            assert!(prove_verified_protocol_response(status, body, protocol).is_err());
        }
    }
    assert!(
        prove_verified_protocol_response(
            StatusCode::OK,
            br#"{"output":[],"status":"failed"}"#,
            UpstreamProtocolKind::Responses
        )
        .is_err()
    );
}

#[test]
fn verification_contract_identity_covers_revision_key_config_and_order() {
    let config = AccountCustomConfig {
        account_id: "acc".into(),
        endpoint_url: "http://127.0.0.1:9/v1/responses".into(),
        upstream_protocol: UpstreamProtocolKind::Responses,
        created_at: chrono::Utc::now(),
        updated_at: chrono::Utc::now(),
    };
    let caps = vec![
        AccountModelCapability {
            account_id: "acc".into(),
            public_model: "one".into(),
            upstream_model: "upstream-one".into(),
            protocol: UpstreamProtocolKind::Responses,
            verified_at: None,
            source: "manual".into(),
        },
        AccountModelCapability {
            account_id: "acc".into(),
            public_model: "two".into(),
            upstream_model: "upstream-two".into(),
            protocol: UpstreamProtocolKind::Responses,
            verified_at: None,
            source: "manual".into(),
        },
    ];
    let contract =
        CustomVerificationContract::from_parts("acc", "rev-1", "cipher-a", &config, &caps);
    assert_eq!(contract.account_updated_at, "rev-1");
    assert_eq!(contract.key_cipher, "cipher-a");
    assert_eq!(contract.upstream_protocol, UpstreamProtocolKind::Responses);
    assert_eq!(
        contract.capabilities,
        vec![
            (
                "one".into(),
                "upstream-one".into(),
                UpstreamProtocolKind::Responses
            ),
            (
                "two".into(),
                "upstream-two".into(),
                UpstreamProtocolKind::Responses
            )
        ]
    );
    let reordered = CustomVerificationContract::from_parts(
        "acc",
        "rev-1",
        "cipher-a",
        &config,
        &[caps[1].clone(), caps[0].clone()],
    );
    assert_ne!(contract, reordered);
    let rotated_key =
        CustomVerificationContract::from_parts("acc", "rev-1", "cipher-b", &config, &caps);
    assert_ne!(contract, rotated_key);
}

#[tokio::test]
async fn oversized_verification_body_is_rejected_without_certifying() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let pad = "x".repeat(MAX_CUSTOM_VERIFICATION_BODY_BYTES);
    let body = format!(r#"{{"pad":"{pad}"}}"#);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("loopback listener");
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let Ok((mut stream, _)) = listener.accept().await else {
            return;
        };
        let mut buf = vec![0_u8; 8192];
        let _ = stream.read(&mut buf).await;
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        let _ = stream.write_all(response.as_bytes()).await;
    });

    let app_config = AppConfig {
        proxy_mode: crate::models::ProxyMode::Direct,
        connect_timeout_secs: 5,
        non_stream_timeout_secs: 5,
        ..AppConfig::default()
    };
    let custom_config = AccountCustomConfig {
        account_id: "acc".into(),
        endpoint_url: format!("http://127.0.0.1:{}/v1/chat/completions", addr.port()),
        upstream_protocol: UpstreamProtocolKind::ChatCompletions,
        created_at: chrono::Utc::now(),
        updated_at: chrono::Utc::now(),
    };
    let capability = AccountModelCapability {
        account_id: "acc".into(),
        public_model: "local".into(),
        upstream_model: "local".into(),
        protocol: UpstreamProtocolKind::ChatCompletions,
        verified_at: None,
        source: "manual".into(),
    };
    probe_custom_connection(&app_config, &custom_config, &capability, "sk")
        .await
        .expect_err("oversized verification bodies must not prove verified");
}
