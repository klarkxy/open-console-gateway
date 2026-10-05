use super::{
    ProtocolProbeContext, ProtocolProbeRunError, ProtocolRequestError, ValidatedGeneration,
    ValidatedHop, ValidatedSendError, VerificationOverrides, classify_provider_response,
    non_null_probe_error, post_validated_hop, protocol_shaped_success,
    reject_provider_wide_custom_probe, require_unique_probe_protocols, run_protocol_probes,
    select_single_probe_account, send_validated_generation,
};
use crate::crypto::{KeyCipher, StaticKeyCipher};
use crate::db::Database;
use crate::models::{Account, AccountSetupStep, AccountType, AppConfig};
use crate::provider::UpstreamProtocolKind;
use crate::provider_contracts::ContractScope;
use crate::state::{CoreState, CoreStateInner};
use chrono::Utc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

fn open_state(tag: &str) -> (TempDir, CoreState) {
    let path =
        std::env::temp_dir().join(format!("ocg-validated-hop-{tag}-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&path).unwrap();
    let db = Database::open(path.clone()).unwrap();
    let cipher: Arc<dyn KeyCipher + Send + Sync> = Arc::new(StaticKeyCipher::new(tag));
    let state = Arc::new(CoreStateInner::new(db, path.clone(), cipher).unwrap());
    (TempDir(path), state)
}

struct TempDir(std::path::PathBuf);

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn account(id: &str) -> Account {
    let now = Utc::now();
    Account {
        id: id.into(),
        provider_id: "opencode".into(),
        credential_kind: crate::provider::default_credential_kind(),
        quota_scope: crate::provider::default_quota_scope(),
        name: id.into(),
        username: None,
        password_cipher: None,
        key_cipher: "cipher".into(),
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
    }
}

fn hop(base: &str, protocol: &str) -> ValidatedHop {
    ValidatedHop {
        base_url: base.into(),
        hop_secret: HOP_SECRET.into(),
        child_generation: 17,
        applied_revision: 23,
        auth_id: "auth-pin-1".into(),
        credential_id: "credential-not-a-header".into(),
        credential_version: 4,
        binding_id: "binding-not-a-header".into(),
        material_revision: "material-rev-not-a-header".into(),
        public_model: "public-alias".into(),
        protocol: protocol.into(),
    }
}

const HOP_SECRET: &str = "hop-secret-Z9f3aK";

struct Loopback {
    origin: String,
    hits: Arc<AtomicUsize>,
    requests: Arc<Mutex<Vec<String>>>,
}

enum Reply {
    Status {
        code: u16,
        body: Vec<u8>,
        extra_headers: String,
    },
    Hang,
}

async fn serve(reply: Reply) -> Loopback {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("loopback listener");
    let port = listener.local_addr().unwrap().port();
    let hits = Arc::new(AtomicUsize::new(0));
    let requests = Arc::new(Mutex::new(Vec::new()));
    let hits_task = hits.clone();
    let requests_task = requests.clone();
    tokio::spawn(async move {
        loop {
            let Ok((mut socket, _)) = listener.accept().await else {
                return;
            };
            hits_task.fetch_add(1, Ordering::SeqCst);
            let request = tokio::time::timeout(Duration::from_secs(2), read_request(&mut socket))
                .await
                .unwrap_or_else(|_| "timed out reading request".to_string());
            requests_task.lock().expect("requests").push(request);
            match &reply {
                Reply::Hang => {}
                Reply::Status {
                    code,
                    body,
                    extra_headers,
                } => {
                    let reason = match *code {
                        200 => "OK",
                        302 => "Found",
                        500 => "Internal Server Error",
                        _ => "Status",
                    };
                    let header = format!(
                        "HTTP/1.1 {code} {reason}\r\n{extra_headers}Content-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                        body.len()
                    );
                    let _ = socket.write_all(header.as_bytes()).await;
                    let _ = socket.write_all(body).await;
                }
            }
        }
    });
    Loopback {
        origin: format!("http://127.0.0.1:{port}"),
        hits,
        requests,
    }
}

async fn read_request(socket: &mut tokio::net::TcpStream) -> String {
    let mut buf = Vec::new();
    let mut tmp = [0_u8; 1024];
    loop {
        let read = socket.read(&mut tmp).await.unwrap_or(0);
        if read == 0 {
            break;
        }
        buf.extend_from_slice(&tmp[..read]);
        let Some(header_end) = buf.windows(4).position(|window| window == b"\r\n\r\n") else {
            if buf.len() > 65_536 {
                break;
            }
            continue;
        };
        let headers = String::from_utf8_lossy(&buf[..header_end]).into_owned();
        let Some(content_length) = content_length_of(&headers) else {
            break;
        };
        let body_end = header_end + 4 + content_length;
        while buf.len() < body_end {
            let read = socket.read(&mut tmp).await.unwrap_or(0);
            if read == 0 {
                break;
            }
            buf.extend_from_slice(&tmp[..read]);
        }
        let end = body_end.min(buf.len());
        return String::from_utf8_lossy(&buf[..end]).into_owned();
    }
    String::from_utf8_lossy(&buf).into_owned()
}

fn content_length_of(headers: &str) -> Option<usize> {
    headers.lines().find_map(|line| {
        let (name, value) = line.split_once(':')?;
        if name.eq_ignore_ascii_case("content-length") {
            value.trim().parse().ok()
        } else {
            None
        }
    })
}

fn header<'a>(request: &'a str, name: &str) -> &'a str {
    request
        .lines()
        .find_map(|line| {
            let (key, value) = line.split_once(':')?;
            key.eq_ignore_ascii_case(name).then_some(value.trim())
        })
        .unwrap_or_else(|| panic!("missing {name}"))
}

fn not_sent(error: ValidatedSendError) -> String {
    match error {
        ValidatedSendError::NotSent(message) => message,
    }
}

async fn post_minimal_hop(
    state: &CoreState,
    hop: &ValidatedHop,
    timeout: Duration,
) -> Result<super::ValidatedHttpResult, ValidatedSendError> {
    post_validated_hop(state, hop, timeout, VerificationOverrides::default()).await
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

#[test]
fn custom_provider_wide_probe_stays_rejected() {
    assert_eq!(
        reject_provider_wide_custom_probe(crate::provider::CUSTOM_PROVIDER_ID),
        Err("protocol probes for Custom API are account-owned")
    );
    assert!(reject_provider_wide_custom_probe("opencode").is_ok());
}

#[test]
fn one_eligible_account_is_selected_and_an_unknown_id_is_refused() {
    let accounts = [account("first"), account("second")];
    assert_eq!(
        select_single_probe_account(&accounts, None).unwrap().id,
        "first"
    );
    assert_eq!(
        select_single_probe_account(&accounts, Some("  "))
            .unwrap()
            .id,
        "first"
    );
    assert_eq!(
        select_single_probe_account(&accounts, Some("second"))
            .unwrap()
            .id,
        "second"
    );
    assert_eq!(
        select_single_probe_account(&accounts, Some("missing")).unwrap_err(),
        "selected account is not eligible for this provider protocol probe"
    );
    assert_eq!(
        select_single_probe_account(&[], None).unwrap_err(),
        "no eligible provider accounts are available for protocol probes"
    );
    assert_eq!(
        select_single_probe_account(&[], Some("missing")).unwrap_err(),
        "selected account is not eligible for this provider protocol probe"
    );
}

#[test]
fn shaped_success_accepts_only_the_requested_protocol_body() {
    let shaped = br#"{"choices":[{"message":{"role":"assistant","content":"pong"}}]}"#;
    protocol_shaped_success(200, shaped, UpstreamProtocolKind::ChatCompletions).unwrap();
    let malformed =
        protocol_shaped_success(200, br#"{"id":"x"}"#, UpstreamProtocolKind::ChatCompletions)
            .unwrap_err();
    assert!(malformed.contains("does not match"));
}

#[test]
fn provider_status_and_error_objects_stay_provider_failures() {
    match classify_provider_response(
        500,
        b"secret-body",
        UpstreamProtocolKind::ChatCompletions,
        true,
    )
    .unwrap_err()
    {
        ProtocolRequestError::Provider { status, message } => {
            assert_eq!(status, Some(500));
            assert_eq!(message, "upstream returned HTTP 500");
            assert!(!message.contains("secret-body"));
        }
        ProtocolRequestError::NotSent(message) => {
            panic!("completed provider status must stay a provider result: {message}")
        }
    }

    let leaked = br#"{"error":{"message":"sk-probe-secret"}}"#;
    match classify_provider_response(200, leaked, UpstreamProtocolKind::ChatCompletions, true)
        .unwrap_err()
    {
        ProtocolRequestError::Provider { message, .. } => {
            assert_eq!(message, "upstream returned a protocol error");
            assert!(!message.contains("sk-probe-secret"));
        }
        ProtocolRequestError::NotSent(message) => {
            panic!("error object must stay a provider result: {message}")
        }
    }

    let shaped = br#"{"choices":[{"message":{"content":"pong"}}],"error":null}"#;
    assert_eq!(
        classify_provider_response(200, shaped, UpstreamProtocolKind::ChatCompletions, false)
            .unwrap(),
        200
    );
}

#[test]
fn validated_hop_debug_redacts_the_secret() {
    let rendered = format!("{:?}", hop("http://127.0.0.1:9", "chat_completions"));
    assert!(rendered.contains("[redacted]"));
    assert!(!rendered.contains(HOP_SECRET));
}

#[tokio::test]
async fn validated_hop_posts_once_to_each_requested_protocol_path() {
    let (_dir, state) = open_state("paths");
    let server = serve(Reply::Status {
        code: 200,
        body: br#"{"ok":true}"#.to_vec(),
        extra_headers: String::new(),
    })
    .await;
    let decoy = serve(Reply::Status {
        code: 200,
        body: br#"{"decoy":true}"#.to_vec(),
        extra_headers: String::new(),
    })
    .await;
    let base = format!("{}/", server.origin);
    for protocol in ["chat_completions", "responses", "messages"] {
        let target = hop(&base, protocol);
        let response = post_minimal_hop(&state, &target, Duration::from_secs(2))
            .await
            .expect("one loopback response");
        assert_eq!(response.status, 200);
    }
    assert_eq!(server.hits.load(Ordering::SeqCst), 3);
    assert_eq!(decoy.hits.load(Ordering::SeqCst), 0);
    let requests = server.requests.lock().expect("requests").clone();
    assert_eq!(requests.len(), 3);
    let paths = ["/v1/chat/completions", "/v1/responses", "/v1/messages"];
    let kinds = [
        UpstreamProtocolKind::ChatCompletions,
        UpstreamProtocolKind::Responses,
        UpstreamProtocolKind::Messages,
    ];
    for ((request, path), protocol) in requests.iter().zip(paths).zip(kinds) {
        assert!(request.starts_with(&format!("POST {path} ")), "{request}");
        assert_eq!(
            header(request, "authorization"),
            format!("Bearer {HOP_SECRET}")
        );
        assert_eq!(header(request, "x-ocg-request-kind"), "validated");
        assert_eq!(header(request, "x-ocg-pinned-auth-id"), "auth-pin-1");
        assert_eq!(
            header(request, "x-ocg-validated-protocol"),
            protocol.as_str()
        );
        assert_eq!(header(request, "x-ocg-process-generation"), "17");
        assert_eq!(header(request, "x-ocg-projection-revision"), "23");
        uuid::Uuid::parse_str(header(request, "x-ocg-request-id")).unwrap();
        let body = request.split_once("\r\n\r\n").expect("body").1;
        let expected = crate::custom::minimal_verification_body(protocol, "public-alias")
            .unwrap_or_else(|error| panic!("verification body: {}", error.message));
        assert_eq!(body.as_bytes(), expected.as_slice());
        assert!(!request.contains("credential-not-a-header"));
        assert!(!request.contains("binding-not-a-header"));
        assert!(!request.contains("material-rev-not-a-header"));
        assert!(!request.contains(&decoy.origin));
    }
}

#[tokio::test]
async fn provider_error_and_redirect_do_not_send_a_second_request() {
    let (_dir, state) = open_state("once");
    let server = serve(Reply::Status {
        code: 500,
        body: br#"{"error":"upstream"}"#.to_vec(),
        extra_headers: String::new(),
    })
    .await;
    let target = hop(&server.origin, "chat_completions");
    let response = post_minimal_hop(&state, &target, Duration::from_secs(2))
        .await
        .expect("500 is a completed response");
    assert_eq!(response.status, 500);
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert_eq!(server.hits.load(Ordering::SeqCst), 1);

    let decoy = serve(Reply::Status {
        code: 200,
        body: br#"{"decoy":true}"#.to_vec(),
        extra_headers: String::new(),
    })
    .await;
    let redirect = serve(Reply::Status {
        code: 302,
        body: Vec::new(),
        extra_headers: format!("Location: {}/v1/chat/completions\r\n", decoy.origin),
    })
    .await;
    let target = hop(&redirect.origin, "chat_completions");
    match post_minimal_hop(&state, &target, Duration::from_secs(2)).await {
        Ok(response) => assert_eq!(response.status, 302),
        Err(error) => {
            let message = not_sent(error);
            assert!(!message.contains(HOP_SECRET));
        }
    }
    assert_eq!(redirect.hits.load(Ordering::SeqCst), 1);
    assert_eq!(decoy.hits.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn elapsed_or_rejected_hop_does_not_send_and_hides_the_secret() {
    let (_dir, state) = open_state("unsent");
    let server = serve(Reply::Hang).await;
    let target = hop(&server.origin, "chat_completions");
    let message = not_sent(
        post_minimal_hop(&state, &target, Duration::ZERO)
            .await
            .expect_err("zero timeout"),
    );
    assert!(message.contains("deadline already elapsed"));
    tokio::task::yield_now().await;
    assert_eq!(server.hits.load(Ordering::SeqCst), 0);

    let target = hop(&server.origin, "not-a-protocol");
    let message = not_sent(
        post_minimal_hop(&state, &target, Duration::from_millis(200))
            .await
            .expect_err("unknown protocol"),
    );
    assert!(message.contains("not a private bridge protocol"));
    assert_eq!(server.hits.load(Ordering::SeqCst), 0);

    let mut bad_pin = hop(&server.origin, "chat_completions");
    bad_pin.auth_id = "bad pin".into();
    let message = not_sent(
        post_minimal_hop(&state, &bad_pin, Duration::from_millis(200))
            .await
            .expect_err("header"),
    );
    assert_eq!(message, "validated hop headers were rejected");
    assert!(!message.contains(HOP_SECRET));
    tokio::task::yield_now().await;
    assert_eq!(server.hits.load(Ordering::SeqCst), 0);

    for origin in [
        "http://192.0.2.1:9",
        "http://user:pw@127.0.0.1:9",
        "http://127.0.0.1:9/v1?x=1",
        "https://example.com",
    ] {
        let target = hop(origin, "chat_completions");
        let message = not_sent(
            post_minimal_hop(&state, &target, Duration::from_millis(200))
                .await
                .expect_err("non loopback"),
        );
        assert!(
            message.contains("not a loopback URL"),
            "{origin}: {message}"
        );
        assert!(!message.contains(HOP_SECRET));
    }
    assert_eq!(server.hits.load(Ordering::SeqCst), 0);

    let slow = serve(Reply::Hang).await;
    let target = hop(&slow.origin, "chat_completions");
    let message = not_sent(
        post_minimal_hop(&state, &target, Duration::from_millis(200))
            .await
            .expect_err("hung hop"),
    );
    assert!(!message.contains(HOP_SECRET));
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert_eq!(slow.hits.load(Ordering::SeqCst), 1);

    let target = hop("http://127.0.0.1:1", "chat_completions");
    let message = not_sent(
        post_minimal_hop(&state, &target, Duration::from_millis(500))
            .await
            .expect_err("closed port"),
    );
    assert!(!message.contains(HOP_SECRET));
    assert!(!message.contains("not a loopback URL"));
}

#[tokio::test]
async fn pin_stub_refuses_before_any_send() {
    let (_dir, state) = open_state("pin-stub");
    let server = serve(Reply::Status {
        code: 200,
        body: br#"{"ok":true}"#.to_vec(),
        extra_headers: String::new(),
    })
    .await;
    let error = send_validated_generation(
        &state,
        &ValidatedGeneration {
            credential_id: "credential-not-a-header".into(),
            credential_version: 4,
            binding_id: "binding-not-a-header".into(),
            public_model: "public-alias".into(),
            protocol: UpstreamProtocolKind::ChatCompletions,
        },
        Duration::from_secs(2),
    )
    .await
    .expect_err("unenforced pin");
    assert!(not_sent(error).contains("validated protocol pin is not enforced"));
    tokio::task::yield_now().await;
    assert_eq!(server.hits.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn not_sent_stops_the_probe_batch_before_a_later_protocol() {
    let (_dir, state) = open_state("batch");
    let config = AppConfig::default();
    let one = [account("missing-probe-account")];
    let ctx = ProtocolProbeContext {
        state: &state,
        config: &config,
        accounts: &one,
        model_id: "public-alias",
        now: Utc::now(),
    };
    let mut loads = 0;
    let error = run_protocol_probes(
        &ctx,
        &ContractScope::provider("opencode"),
        &[
            UpstreamProtocolKind::ChatCompletions,
            UpstreamProtocolKind::Responses,
        ],
        |_| {
            loads += 1;
            Ok(None)
        },
    )
    .await
    .expect_err("missing credential");
    assert!(matches!(error, ProtocolProbeRunError::NotSent(_)));
    assert_eq!(loads, 1);

    let two = [account("a"), account("b")];
    let ctx = ProtocolProbeContext {
        accounts: &two,
        ..ctx
    };
    let error = run_protocol_probes(
        &ctx,
        &ContractScope::provider("opencode"),
        &[UpstreamProtocolKind::ChatCompletions],
        |_| panic!("a second account must not load evidence"),
    )
    .await
    .expect_err("two accounts");
    match error {
        ProtocolProbeRunError::NotSent(message) => {
            assert!(message.contains("one selected account"));
        }
        other => panic!("expected NotSent, got {other:?}"),
    }
}

#[test]
fn captured_deadline_replaces_an_outbound_caller_deadline() {
    let mut headers = reqwest::header::HeaderMap::new();
    headers.insert(
        "x-ocg-request-deadline",
        reqwest::header::HeaderValue::from_static("2099-01-01T00:00:00.000000000Z"),
    );
    let captured = chrono::DateTime::<chrono::Utc>::from_timestamp(1_759_545_723, 123_456_789)
        .expect("timestamp");
    super::seal_captured_deadline(&mut headers, captured).expect("seal");
    let wire = headers
        .get("x-ocg-request-deadline")
        .expect("deadline")
        .to_str()
        .expect("ascii");
    assert_eq!(
        wire,
        captured.to_rfc3339_opts(chrono::SecondsFormat::Nanos, true)
    );
    assert!(wire.ends_with(".123456789Z"), "{wire}");
    assert_ne!(wire, "2099-01-01T00:00:00.000000000Z");
    assert_eq!(headers.get_all("x-ocg-request-deadline").iter().count(), 1);
}

#[tokio::test]
async fn private_hop_sends_supplied_payload_or_minimal_defaults_with_registered_deadline() {
    let (_dir, state) = open_state("payload-deadline");
    let server = serve(Reply::Status {
        code: 200,
        body: br#"{"ok":true}"#.to_vec(),
        extra_headers: String::new(),
    })
    .await;
    let timeout = Duration::from_millis(800);
    let rejected = post_validated_hop(
        &state,
        &hop(&server.origin, "chat_completions"),
        timeout,
        VerificationOverrides {
            message: Some("custom prompt".into()),
            max_tokens: Some(0),
        },
    )
    .await
    .expect_err("zero maxTokens");
    assert!(not_sent(rejected).contains("maxTokens must be positive"));
    assert_eq!(server.hits.load(Ordering::SeqCst), 0);

    let cases: [(UpstreamProtocolKind, Option<&str>, Option<u32>); 9] = [
        (UpstreamProtocolKind::ChatCompletions, None, None),
        (
            UpstreamProtocolKind::ChatCompletions,
            Some("custom prompt"),
            Some(11u32),
        ),
        (
            UpstreamProtocolKind::ChatCompletions,
            Some("only message"),
            None,
        ),
        (UpstreamProtocolKind::Responses, None, None),
        (
            UpstreamProtocolKind::Responses,
            Some("custom prompt"),
            Some(11),
        ),
        (UpstreamProtocolKind::Responses, None, Some(11)),
        (UpstreamProtocolKind::Messages, None, None),
        (
            UpstreamProtocolKind::Messages,
            Some("custom prompt"),
            Some(11),
        ),
        (UpstreamProtocolKind::Messages, Some("only message"), None),
    ];
    for (protocol, message, max_tokens) in cases {
        let before = Utc::now();
        let response = post_validated_hop(
            &state,
            &hop(&server.origin, protocol.as_str()),
            timeout,
            VerificationOverrides {
                message: message.map(str::to_string),
                max_tokens,
            },
        )
        .await
        .expect("loopback hop");
        let after = Utc::now();
        assert_eq!(response.status, 200);
        let request = server
            .requests
            .lock()
            .expect("requests")
            .last()
            .cloned()
            .expect("captured hop");
        assert!(
            request.starts_with(&format!("POST {} ", protocol_path(protocol))),
            "{request}"
        );
        let body = request.split_once("\r\n\r\n").expect("body").1;
        assert_hop_body(body, protocol, message, max_tokens);
        let request_id = uuid::Uuid::parse_str(header(&request, "x-ocg-request-id")).unwrap();
        let registered = super::registered_deadline(request_id).expect("registered capture");
        let wire = header(&request, "x-ocg-request-deadline");
        assert_eq!(deadline_header_count(&request), 1, "{request}");
        assert_registered_deadline(before, after, registered, wire, timeout);
        assert!(!request.contains("credential-not-a-header"));
        assert!(!request.contains("2099-01-01"));
    }
}

fn protocol_path(protocol: UpstreamProtocolKind) -> &'static str {
    match protocol {
        UpstreamProtocolKind::ChatCompletions => "/v1/chat/completions",
        UpstreamProtocolKind::Responses => "/v1/responses",
        UpstreamProtocolKind::Messages => "/v1/messages",
    }
}

fn deadline_header_count(request: &str) -> usize {
    request
        .lines()
        .filter(|line| {
            line.split_once(':')
                .is_some_and(|(name, _)| name.eq_ignore_ascii_case("x-ocg-request-deadline"))
        })
        .count()
}

fn assert_hop_body(
    body: &str,
    protocol: UpstreamProtocolKind,
    message: Option<&str>,
    max_tokens: Option<u32>,
) {
    let parsed: serde_json::Value = serde_json::from_str(body).expect("hop json");
    assert_eq!(parsed["model"].as_str(), Some("public-alias"));
    assert_eq!(parsed["stream"], serde_json::json!(false));
    match protocol {
        UpstreamProtocolKind::ChatCompletions | UpstreamProtocolKind::Messages => {
            assert!(parsed.get("input").is_none(), "{parsed}");
            assert!(parsed.get("max_output_tokens").is_none(), "{parsed}");
            assert!(parsed.get("store").is_none(), "{parsed}");
            assert_eq!(parsed["messages"][0]["role"].as_str(), Some("user"));
            assert_eq!(
                parsed["messages"][0]["content"].as_str(),
                Some(message.unwrap_or("ping"))
            );
            assert_eq!(parsed["messages"].as_array().map(Vec::len), Some(1));
            assert_eq!(
                parsed["max_tokens"].as_u64(),
                Some(u64::from(max_tokens.unwrap_or(1)))
            );
        }
        UpstreamProtocolKind::Responses => {
            assert!(parsed.get("messages").is_none(), "{parsed}");
            assert!(parsed.get("max_tokens").is_none(), "{parsed}");
            assert_eq!(parsed["store"], serde_json::json!(false));
            assert_eq!(parsed["input"].as_str(), Some(message.unwrap_or("ping")));
            assert_eq!(
                parsed["max_output_tokens"].as_u64(),
                Some(u64::from(max_tokens.unwrap_or(16)))
            );
        }
    }
    if message.is_none() && max_tokens.is_none() {
        let minimal = crate::custom::minimal_verification_body(protocol, "public-alias")
            .unwrap_or_else(|error| panic!("{}", error.message));
        assert_eq!(body.as_bytes(), minimal.as_slice());
    }
}

fn assert_registered_deadline(
    before: chrono::DateTime<chrono::Utc>,
    after: chrono::DateTime<chrono::Utc>,
    registered: chrono::DateTime<chrono::Utc>,
    wire: &str,
    timeout: Duration,
) {
    let rendered = registered.to_rfc3339_opts(chrono::SecondsFormat::Nanos, true);
    assert_eq!(wire, rendered);
    let fraction = rendered.split_once('.').expect("nanosecond fraction").1;
    assert_eq!(fraction.trim_end_matches('Z').len(), 9, "{rendered}");
    assert!(rendered.ends_with('Z'), "{rendered}");
    let span = chrono::Duration::from_std(timeout).expect("timeout fits");
    assert!(
        registered >= before + span,
        "deadline lost the fractional timeout: registered={registered} before={before}"
    );
    let ceiling = after + span + chrono::Duration::milliseconds(50);
    assert!(
        registered <= ceiling,
        "deadline extended past the captured timeout: registered={registered} after={after}"
    );
}
