use super::{ApiRequest, Invocation, execute, invoke, schema_json};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

struct Captured {
    header: String,
    body: Vec<u8>,
}

fn content_length(header: &str) -> usize {
    header
        .lines()
        .find_map(|line| {
            let (name, value) = line.split_once(':')?;
            name.eq_ignore_ascii_case("content-length")
                .then(|| value.trim().parse::<usize>().ok())
                .flatten()
        })
        .unwrap_or(0)
}

async fn read_request(stream: &mut tokio::net::TcpStream) -> Captured {
    let mut buf = Vec::new();
    let mut end = None;
    loop {
        let mut tmp = [0u8; 2048];
        let read = stream.read(&mut tmp).await.unwrap();
        if read == 0 {
            break;
        }
        buf.extend_from_slice(&tmp[..read]);
        if let Some(pos) = buf.windows(4).position(|window| window == b"\r\n\r\n") {
            end = Some(pos);
            break;
        }
    }
    let end = end.unwrap_or(buf.len());
    let header = String::from_utf8_lossy(&buf[..end]).into_owned();
    let mut body = if end + 4 <= buf.len() {
        buf[end + 4..].to_vec()
    } else {
        Vec::new()
    };
    let length = content_length(&header);
    while body.len() < length {
        let mut tmp = [0u8; 2048];
        let read = stream.read(&mut tmp).await.unwrap();
        if read == 0 {
            break;
        }
        body.extend_from_slice(&tmp[..read]);
    }
    body.truncate(length);
    Captured { header, body }
}

fn response(status: u16, headers: &[(&str, &str)], body: &[u8]) -> Vec<u8> {
    let mut text = format!(
        "HTTP/1.1 {status} TEST\r\nContent-Length: {}\r\nConnection: close\r\n",
        body.len()
    );
    for (name, value) in headers {
        text.push_str(name);
        text.push_str(": ");
        text.push_str(value);
        text.push_str("\r\n");
    }
    text.push_str("\r\n");
    let mut bytes = text.into_bytes();
    bytes.extend_from_slice(body);
    bytes
}

async fn serve(
    handler: impl Fn(Captured) -> Vec<u8> + Send + Sync + 'static,
) -> (String, Arc<AtomicUsize>, tokio::task::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let hits = Arc::new(AtomicUsize::new(0));
    let recorded = Arc::clone(&hits);
    let task = tokio::spawn(async move {
        loop {
            let Ok((mut stream, _)) = listener.accept().await else {
                break;
            };
            recorded.fetch_add(1, Ordering::SeqCst);
            let captured = read_request(&mut stream).await;
            let bytes = handler(captured);
            let _ = stream.write_all(&bytes).await;
        }
    });
    (format!("http://127.0.0.1:{port}"), hits, task)
}

fn temp_dir() -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("ocg-cli-api-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

struct FirstChunk {
    seen: Arc<tokio::sync::Notify>,
    buf: Vec<u8>,
    told: bool,
}

impl super::BodySink for FirstChunk {
    fn write_chunk(&mut self, bytes: &[u8]) -> std::io::Result<()> {
        self.buf.extend_from_slice(bytes);
        if !self.told && self.buf.windows(3).any(|window| window == b"one") {
            self.told = true;
            self.seen.notify_one();
        }
        Ok(())
    }
}

#[test]
fn schema_json_prints_the_offline_catalog() {
    let v4: serde_json::Value = serde_json::from_str(&schema_json("v4").unwrap()).unwrap();
    let v3: serde_json::Value = serde_json::from_str(&schema_json("v3").unwrap()).unwrap();
    assert_eq!(v4["title"], "DashboardApiV4");
    assert_eq!(v3["title"], "DashboardApiV3");
    assert_eq!(schema_json("v2").unwrap_err().code, "invalidRequest");
    assert_eq!(schema_json("v2").unwrap_err().exit_code, 2);
}

#[tokio::test]
async fn invalid_endpoint_json_and_route_never_connect() {
    let (endpoint, hits, task) = serve(|_| response(500, &[], b"no")).await;
    let bad_endpoint = endpoint.replacen("http://", "http://user:secret@", 1);
    let error = execute(
        ApiRequest {
            endpoint: bad_endpoint,
            method: "GET".into(),
            path: "/dashboard/api/v4/contract".into(),
            body: None,
            cas_current: false,
            bearer: None,
            session_file: None,
        },
        &mut Vec::new(),
    )
    .await
    .unwrap_err();
    assert_eq!(error.code, "invalidRequest");
    assert!(!error.to_string().contains("secret"));

    let error = execute(
        ApiRequest {
            endpoint: endpoint.clone(),
            method: "POST".into(),
            path: "/dashboard/api/v4/accounts".into(),
            body: Some(b"[1]".to_vec()),
            cas_current: true,
            bearer: None,
            session_file: None,
        },
        &mut Vec::new(),
    )
    .await
    .unwrap_err();
    assert_eq!(error.code, "invalidRequest");
    assert!(error.message.contains("JSON object"));

    let error = execute(
        ApiRequest {
            endpoint,
            method: "GET".into(),
            path: "/dashboard/api/v3/accounts".into(),
            body: None,
            cas_current: false,
            bearer: None,
            session_file: None,
        },
        &mut Vec::new(),
    )
    .await
    .unwrap_err();
    assert!(error.message.contains("/dashboard/api/v4"));
    assert_eq!(hits.load(Ordering::SeqCst), 0);
    task.abort();
}

#[tokio::test]
async fn redirects_are_not_followed_and_stdout_stays_empty() {
    let hits = Arc::new(AtomicUsize::new(0));
    let seen = Arc::clone(&hits);
    let (endpoint, connections, task) = serve(move |captured| {
        if captured.header.contains(" /dashboard/api/v4/secret ") {
            seen.fetch_add(1, Ordering::SeqCst);
            return response(200, &[], b"followed");
        }
        response(302, &[("Location", "/dashboard/api/v4/secret")], b"go-away")
    })
    .await;
    let mut sink = Vec::new();
    let error = execute(
        ApiRequest {
            endpoint,
            method: "POST".into(),
            path: "/dashboard/api/v4/contract".into(),
            body: Some(b"{}".to_vec()),
            cas_current: false,
            bearer: None,
            session_file: None,
        },
        &mut sink,
    )
    .await
    .unwrap_err();
    assert_eq!(error.code, "redirectRefused");
    assert_eq!(error.status, Some(302));
    assert_eq!(error.exit_code, 3);
    assert_ne!(error.exit_code, 302);
    assert!(sink.is_empty());
    assert_eq!(hits.load(Ordering::SeqCst), 0);
    assert_eq!(connections.load(Ordering::SeqCst), 1);
    task.abort();
}

#[tokio::test]
async fn cas_keeps_supplied_expectations_and_injects_only_missing_fields() {
    let calls = Arc::new(Mutex::new(Vec::<Captured>::new()));
    let recorded = Arc::clone(&calls);
    let (endpoint, _, task) = serve(move |captured| {
        let path = captured
            .header
            .lines()
            .next()
            .unwrap_or("")
            .split_whitespace()
            .nth(1)
            .unwrap_or("")
            .to_string();
        let method = captured
            .header
            .lines()
            .next()
            .unwrap_or("")
            .split_whitespace()
            .next()
            .unwrap_or("")
            .to_string();
        if method == "GET" && path == "/dashboard/api/v4/auth/status" {
            recorded.lock().unwrap().push(captured);
            return response(
                200,
                &[("Content-Type", "application/json")],
                br#"{"local":true,"initialized":false,"authenticated":false,"revision":5,"processGeneration":9}"#,
            );
        }
        recorded.lock().unwrap().push(captured);
        if path.ends_with("/pricing/multipliers") || path.ends_with("/pricing/refresh") {
            return response(200, &[("Content-Type", "application/json")], b"{}");
        }
        let body = recorded.lock().unwrap().last().unwrap().body.clone();
        let value: serde_json::Value = serde_json::from_slice(&body).unwrap_or(serde_json::json!({}));
        let revision = value.get("expectedRevision").and_then(|item| item.as_u64());
        let generation = value.get("processGeneration").and_then(|item| item.as_u64());
        if revision != Some(5) || generation != Some(9) {
            return response(
                409,
                &[("Content-Type", "application/json")],
                br#"{"code":"revisionConflict","message":"stale","currentRevision":5,"processGeneration":9}"#,
            );
        }
        response(
            200,
            &[("Content-Type", "application/json")],
            br#"{"account":{"id":"acc","name":"go"},"revision":6,"processGeneration":9}"#,
        )
    })
    .await;

    let stale = execute(
        ApiRequest {
            endpoint: endpoint.clone(),
            method: "POST".into(),
            path: "/dashboard/api/v4/accounts".into(),
            body: Some(
                br#"{"name":"go","key":"sk-test","expectedRevision":1,"processGeneration":2}"#
                    .to_vec(),
            ),
            cas_current: true,
            bearer: None,
            session_file: None,
        },
        &mut Vec::new(),
    )
    .await
    .unwrap_err();
    assert_eq!(stale.status, Some(409));
    assert_eq!(stale.exit_code, 3);
    assert_ne!(stale.exit_code, 0);
    assert_eq!(stale.code, "revisionConflict");
    assert_eq!(stale.current_revision, Some(5));
    {
        let recorded = calls.lock().unwrap();
        assert_eq!(
            recorded.len(),
            1,
            "a supplied expectation must not fetch or retry"
        );
        assert!(
            std::str::from_utf8(&recorded[0].body)
                .unwrap()
                .contains("\"expectedRevision\":1")
        );
    }

    let mut sink = Vec::new();
    execute(
        ApiRequest {
            endpoint: endpoint.clone(),
            method: "post".into(),
            path: "/dashboard/api/v4/accounts".into(),
            body: Some(br#"{"name":"go","key":"sk-test"}"#.to_vec()),
            cas_current: true,
            bearer: None,
            session_file: None,
        },
        &mut sink,
    )
    .await
    .unwrap();
    assert_eq!(
        sink,
        br#"{"account":{"id":"acc","name":"go"},"revision":6,"processGeneration":9}"#
    );
    {
        let recorded = calls.lock().unwrap();
        assert_eq!(recorded.len(), 3);
        assert!(
            recorded[1]
                .header
                .starts_with("GET /dashboard/api/v4/auth/status ")
        );
        assert!(
            !recorded
                .iter()
                .any(|item| item.header.contains("/contract"))
        );
        let injected = std::str::from_utf8(&recorded[2].body).unwrap();
        assert!(injected.contains("\"expectedRevision\":5"));
        assert!(injected.contains("\"processGeneration\":9"));
        assert!(!injected.contains("expectedProviderPricingRevision"));
    }

    execute(
        ApiRequest {
            endpoint: endpoint.clone(),
            method: "POST".into(),
            path: "/dashboard/api/v4/accounts".into(),
            body: Some(
                br#"{"name":"go","key":"sk-test","expectedRevision":null,"processGeneration":9}"#
                    .to_vec(),
            ),
            cas_current: true,
            bearer: None,
            session_file: None,
        },
        &mut Vec::new(),
    )
    .await
    .unwrap_err();
    {
        let recorded = calls.lock().unwrap();
        assert_eq!(recorded.len(), 4, "null expectations are not replaced");
        assert!(
            std::str::from_utf8(&recorded[3].body)
                .unwrap()
                .contains("null")
        );
    }

    execute(
        ApiRequest {
            endpoint: endpoint.clone(),
            method: "PUT".into(),
            path: "/dashboard/api/v4/providers/opencode/pricing/multipliers".into(),
            body: Some(b"{}".to_vec()),
            cas_current: true,
            bearer: None,
            session_file: None,
        },
        &mut Vec::new(),
    )
    .await
    .unwrap();
    execute(
        ApiRequest {
            endpoint,
            method: "POST".into(),
            path: "/dashboard/api/v4/providers/opencode/pricing/refresh".into(),
            body: Some(b"{}".to_vec()),
            cas_current: true,
            bearer: None,
            session_file: None,
        },
        &mut Vec::new(),
    )
    .await
    .unwrap();
    let recorded = calls.lock().unwrap();
    let multipliers = recorded
        .iter()
        .find(|item| item.header.starts_with("PUT "))
        .unwrap();
    let multipliers = std::str::from_utf8(&multipliers.body).unwrap();
    assert!(multipliers.contains("\"expectedRevision\":5"));
    assert!(multipliers.contains("\"processGeneration\":9"));
    assert!(!multipliers.contains("expectedPricingRevision"));
    assert!(!multipliers.contains("expectedProviderPricingRevision"));
    let refresh = recorded
        .iter()
        .find(|item| item.header.contains("pricing/refresh"))
        .unwrap();
    let refresh = std::str::from_utf8(&refresh.body).unwrap();
    assert!(!refresh.contains("expectedProviderPricingRevision"));
    assert!(refresh.contains("\"expectedRevision\":5"));
    task.abort();
}

#[tokio::test]
async fn key_file_is_sent_once_and_never_echoed() {
    let secret = "super-secret-gateway-key".to_string();
    let expected = secret.clone();
    let (endpoint, hits, task) = serve(move |captured| {
        let bearer = captured.header.lines().find_map(|line| {
            let (name, value) = line.split_once(':')?;
            name.eq_ignore_ascii_case("authorization")
                .then(|| value.trim().to_string())
        });
        let expected_header = format!("Bearer {expected}");
        let names = captured
            .header
            .lines()
            .filter_map(|line| line.split_once(':').map(|(name, _)| name))
            .collect::<Vec<_>>();
        assert_eq!(
            bearer.as_deref(),
            Some(expected_header.as_str()),
            "header names: {names:?}"
        );
        let message = format!(r#"{{"code":"unauthorized","message":"bad {expected}"}}"#);
        response(
            401,
            &[("Content-Type", "application/json")],
            message.as_bytes(),
        )
    })
    .await;
    let dir = temp_dir();
    let key_file = dir.join("gateway.key");
    std::fs::write(&key_file, format!("{secret}\n")).unwrap();
    let output = dir.join("out.json");
    let mut sink = Vec::new();
    let error = invoke(
        Invocation {
            endpoint: endpoint.clone(),
            method: "POST".into(),
            path: "/v1/chat/completions".into(),
            input: None,
            output: Some(output.clone()),
            cas_current: false,
            key_file: Some(key_file.clone()),
            session_file: None,
        },
        &mut sink,
    )
    .await
    .unwrap_err();
    assert_eq!(error.status, Some(401));
    assert!(error.message.contains("[redacted]"));
    assert!(!error.to_string().contains(&secret));
    assert!(sink.is_empty());
    assert!(!output.exists());
    assert_eq!(hits.load(Ordering::SeqCst), 1);

    let error = execute(
        ApiRequest {
            endpoint,
            method: "GET".into(),
            path: "/dashboard/api/v4/contract".into(),
            body: None,
            cas_current: false,
            bearer: Some(secret.clone()),
            session_file: None,
        },
        &mut sink,
    )
    .await
    .unwrap_err();
    assert_eq!(error.code, "invalidRequest");
    assert!(!error.to_string().contains(&secret));
    assert_eq!(hits.load(Ordering::SeqCst), 1);
    task.abort();
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn success_output_is_the_raw_body_and_leaves_stdout_empty() {
    let (endpoint, _, task) = serve(|_| response(200, &[], b"{\"ok\":true}\n")).await;
    let dir = temp_dir();
    let output = dir.join("body.json");
    let mut sink = Vec::new();
    let success = invoke(
        Invocation {
            endpoint,
            method: "GET".into(),
            path: "/dashboard/api/v4/contract".into(),
            input: None,
            output: Some(output.clone()),
            cas_current: false,
            key_file: None,
            session_file: None,
        },
        &mut sink,
    )
    .await
    .unwrap();
    assert_eq!(success.status, 200);
    assert!(sink.is_empty());
    assert_eq!(std::fs::read(&output).unwrap(), b"{\"ok\":true}\n");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&output).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
    }
    task.abort();
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn session_cookie_is_stored_deleted_and_kept_off_inference() {
    let (endpoint, _, task) = serve(|captured| {
        if captured.header.starts_with("GET /v1/models ") {
            assert!(!captured.header.to_ascii_lowercase().contains("cookie"));
            return response(200, &[], b"{}");
        }
        if captured.header.to_ascii_lowercase().contains("cookie:") {
            return response(
                200,
                &[(
                    "Set-Cookie",
                    "ocg_dashboard_session=; Max-Age=0; Path=/dashboard",
                )],
                b"cleared",
            );
        }
        response(
            200,
            &[(
                "Set-Cookie",
                "ocg_dashboard_session=sekret-value; HttpOnly; Path=/dashboard",
            )],
            b"set",
        )
    })
    .await;
    let dir = temp_dir();
    let session = dir.join("session.json");
    let mut sink = Vec::new();
    invoke(
        Invocation {
            endpoint: endpoint.clone(),
            method: "GET".into(),
            path: "/dashboard/api/v4/contract".into(),
            input: None,
            output: None,
            cas_current: false,
            key_file: None,
            session_file: Some(session.clone()),
        },
        &mut sink,
    )
    .await
    .unwrap();
    let stored = std::fs::read_to_string(&session).unwrap();
    assert!(stored.contains("sekret-value"));
    assert!(
        !String::from_utf8(sink.clone())
            .unwrap()
            .contains("sekret-value")
    );

    sink.clear();
    invoke(
        Invocation {
            endpoint: endpoint.clone(),
            method: "GET".into(),
            path: "/v1/models".into(),
            input: None,
            output: None,
            cas_current: false,
            key_file: None,
            session_file: Some(session.clone()),
        },
        &mut sink,
    )
    .await
    .unwrap();
    assert_eq!(sink, b"{}");
    assert!(
        std::fs::read_to_string(&session)
            .unwrap()
            .contains("sekret-value")
    );

    sink.clear();
    invoke(
        Invocation {
            endpoint: endpoint.clone(),
            method: "GET".into(),
            path: "/dashboard/api/v4/contract".into(),
            input: None,
            output: None,
            cas_current: false,
            key_file: None,
            session_file: Some(session.clone()),
        },
        &mut sink,
    )
    .await
    .unwrap();
    assert_eq!(sink, b"cleared");
    assert!(
        !std::fs::read_to_string(&session)
            .unwrap()
            .contains("sekret-value")
    );

    let wrong = dir.join("other.json");
    std::fs::write(
        &wrong,
        br#"{"version":1,"endpoint":"http://127.0.0.1:1","cookies":[]}"#,
    )
    .unwrap();
    let error = invoke(
        Invocation {
            endpoint,
            method: "GET".into(),
            path: "/dashboard/api/v4/contract".into(),
            input: None,
            output: None,
            cas_current: false,
            key_file: None,
            session_file: Some(wrong.clone()),
        },
        &mut Vec::new(),
    )
    .await
    .unwrap_err();
    assert!(error.message.contains("bound to"));
    assert!(
        std::fs::read_to_string(&wrong)
            .unwrap()
            .contains("127.0.0.1:1")
    );
    task.abort();
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn inference_stream_reaches_the_sink_before_the_body_ends() {
    let seen = Arc::new(tokio::sync::Notify::new());
    let waiter = Arc::clone(&seen);
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let _ = read_request(&mut stream).await;
        stream
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n")
            .await
            .unwrap();
        stream.write_all(b"b\r\ndata: one\n\n\r\n").await.unwrap();
        stream.flush().await.unwrap();
        tokio::time::timeout(Duration::from_secs(2), waiter.notified())
            .await
            .expect("client buffered the stream");
        stream
            .write_all(b"b\r\ndata: two\n\n\r\n0\r\n\r\n")
            .await
            .unwrap();
    });

    let mut sink = FirstChunk {
        seen,
        buf: Vec::new(),
        told: false,
    };
    let input = temp_dir().join("request.json");
    std::fs::write(&input, br#"{"stream":true}"#).unwrap();
    invoke(
        Invocation {
            endpoint: format!("http://127.0.0.1:{port}"),
            method: "POST".into(),
            path: "/v1/chat/completions".into(),
            input: Some(input.clone()),
            output: None,
            cas_current: false,
            key_file: None,
            session_file: None,
        },
        &mut sink,
    )
    .await
    .unwrap();
    let text = String::from_utf8(sink.buf).unwrap();
    assert!(text.find("one").unwrap() < text.find("two").unwrap());
    server.abort();
    let _ = std::fs::remove_dir_all(input.parent().unwrap());
}

#[tokio::test]
async fn http_512_exits_nonzero_and_keeps_the_status() {
    let (endpoint, _, task) = serve(|_| {
        response(
            512,
            &[("Content-Type", "application/json")],
            br#"{"code":"weird","message":"nope"}"#,
        )
    })
    .await;
    let error = execute(
        ApiRequest {
            endpoint,
            method: "GET".into(),
            path: "/dashboard/api/v4/contract".into(),
            body: None,
            cas_current: false,
            bearer: None,
            session_file: None,
        },
        &mut Vec::new(),
    )
    .await
    .unwrap_err();
    assert_eq!(error.status, Some(512));
    assert_eq!(error.exit_code, 3);
    assert_ne!(error.exit_code, 0);
    assert_ne!(error.exit_code, 512);
    assert!(error.stderr_line().contains("\"status\":512"));
    task.abort();
}

#[tokio::test]
async fn stdout_redacts_secrets_and_output_keeps_the_raw_body() {
    let body = br#"{"connection":{"primaryKey":"sk-primary-value","subKeys":[{"id":"sub","value":"sk-sub-value","name":"visible"}]},"secret":"cpa-one-time-secret","password":"hunter2-password","name":"visible"}"#;
    let payload = body.to_vec();
    let (endpoint, _, task) =
        serve(move |_| response(200, &[("Content-Type", "application/json")], &payload)).await;
    let mut stdout = Vec::new();
    execute(
        ApiRequest {
            endpoint: endpoint.clone(),
            method: "GET".into(),
            path: "/dashboard/api/v4/connection".into(),
            body: None,
            cas_current: false,
            bearer: None,
            session_file: None,
        },
        &mut stdout,
    )
    .await
    .unwrap();
    let text = String::from_utf8(stdout).unwrap();
    assert!(!text.contains("sk-primary-value"));
    assert!(!text.contains("sk-sub-value"));
    assert!(!text.contains("cpa-one-time-secret"));
    assert!(!text.contains("hunter2-password"));
    assert!(text.contains("visible"));
    assert!(text.contains("[redacted]"));

    let dir = temp_dir();
    let output = dir.join("private.json");
    let success = invoke(
        Invocation {
            endpoint,
            method: "GET".into(),
            path: "/dashboard/api/v4/connection".into(),
            input: None,
            output: Some(output.clone()),
            cas_current: false,
            key_file: None,
            session_file: None,
        },
        &mut Vec::new(),
    )
    .await
    .unwrap();
    assert_eq!(success.status, 200);
    assert_eq!(std::fs::read(&output).unwrap(), body);
    task.abort();
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn inference_bytes_pass_through_and_host_errors_hide_input_secrets() {
    let password = "correct-horse-battery";
    let api_key = "sk-echo-api-key";
    let secret_input = "secret-input-value";
    let echoed = format!("rejected {password} {api_key} {secret_input}");
    let (endpoint, hits, task) = serve(move |captured| {
        if captured.header.starts_with("POST /v1/chat/completions ") {
            return response(
                200,
                &[("Content-Type", "application/json")],
                br#"{"primaryKey":"sk-primary-value","secret":"leave-inference-bytes"}"#,
            );
        }
        let message = serde_json::json!({
            "code": "badRequest",
            "message": echoed.clone(),
        });
        response(
            400,
            &[("Content-Type", "application/json")],
            message.to_string().as_bytes(),
        )
    })
    .await;
    let mut inference = Vec::new();
    execute(
        ApiRequest {
            endpoint: endpoint.clone(),
            method: "POST".into(),
            path: "/v1/chat/completions".into(),
            body: Some(br#"{"stream":false}"#.to_vec()),
            cas_current: false,
            bearer: None,
            session_file: None,
        },
        &mut inference,
    )
    .await
    .unwrap();
    let inference = String::from_utf8(inference).unwrap();
    assert!(inference.contains("sk-primary-value"));
    assert!(inference.contains("leave-inference-bytes"));

    let request = serde_json::json!({
        "password": password,
        "apiKey": api_key,
        "secretInput": secret_input,
    });
    let error = execute(
        ApiRequest {
            endpoint,
            method: "POST".into(),
            path: "/dashboard/api/v4/accounts".into(),
            body: Some(serde_json::to_vec(&request).unwrap()),
            cas_current: false,
            bearer: None,
            session_file: None,
        },
        &mut Vec::new(),
    )
    .await
    .unwrap_err();
    let text = error.to_string();
    assert_eq!(error.status, Some(400));
    assert_eq!(error.exit_code, 3);
    assert!(text.contains("[redacted]"));
    assert!(!text.contains(password), "{text}");
    assert!(!text.contains(api_key), "{text}");
    assert!(!text.contains(secret_input), "{text}");
    assert_eq!(hits.load(Ordering::SeqCst), 2);
    task.abort();
}

#[tokio::test]
async fn failed_request_keeps_output_and_success_io_failure_is_not_replayed() {
    let hits = Arc::new(AtomicUsize::new(0));
    let seen = Arc::clone(&hits);
    let (endpoint, _, task) = serve(move |_| {
        let attempt = seen.fetch_add(1, Ordering::SeqCst);
        if attempt == 0 {
            return response(
                500,
                &[("Content-Type", "application/json")],
                br#"{"code":"bad","message":"no"}"#,
            );
        }
        response(
            200,
            &[("Content-Type", "application/json")],
            br#"{"ok":true}"#,
        )
    })
    .await;
    let dir = temp_dir();
    let output = dir.join("body.json");
    std::fs::write(&output, b"keep-me").unwrap();
    let error = invoke(
        Invocation {
            endpoint: endpoint.clone(),
            method: "POST".into(),
            path: "/dashboard/api/v4/accounts".into(),
            input: None,
            output: Some(output.clone()),
            cas_current: false,
            key_file: None,
            session_file: None,
        },
        &mut Vec::new(),
    )
    .await
    .unwrap_err();
    assert_eq!(error.status, Some(500));
    assert_eq!(std::fs::read(&output).unwrap(), b"keep-me");

    std::fs::create_dir(dir.join("body.json.ocg-tmp")).unwrap();
    let error = invoke(
        Invocation {
            endpoint,
            method: "POST".into(),
            path: "/dashboard/api/v4/accounts".into(),
            input: None,
            output: Some(output.clone()),
            cas_current: false,
            key_file: None,
            session_file: None,
        },
        &mut Vec::new(),
    )
    .await
    .unwrap_err();
    assert_eq!(error.status, Some(200));
    assert_eq!(error.exit_code, 5);
    assert_eq!(error.code, "acceptedNotSaved");
    assert!(error.message.contains("already accepted"));
    assert!(error.message.contains("not replayed"));
    assert_eq!(std::fs::read(&output).unwrap(), b"keep-me");
    assert_eq!(hits.load(Ordering::SeqCst), 2);
    task.abort();
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn accepted_response_save_failure_discards_temporary_body_without_replay() {
    let dir = temp_dir();
    let output = dir.join("body.json");
    let blocked_output = output.clone();
    let (endpoint, hits, task) = serve(move |_| {
        std::fs::create_dir(&blocked_output).unwrap();
        response(
            200,
            &[("Content-Type", "application/json")],
            br#"{"apiKey":"synthetic-response-secret"}"#,
        )
    })
    .await;
    let error = invoke(
        Invocation {
            endpoint,
            method: "POST".into(),
            path: "/dashboard/api/v4/accounts".into(),
            input: None,
            output: Some(output.clone()),
            cas_current: false,
            key_file: None,
            session_file: None,
        },
        &mut Vec::new(),
    )
    .await
    .unwrap_err();
    assert_eq!(error.status, Some(200));
    assert_eq!(error.exit_code, 5);
    assert_eq!(error.code, "acceptedNotSaved");
    assert_eq!(hits.load(Ordering::SeqCst), 1);
    assert!(output.is_dir());
    assert!(!super::sibling_temp(&output).exists());
    task.abort();
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn host_error_hides_set_cookie_and_session_save_is_not_replayed() {
    let cookie = "cookie-secret-value";
    let (endpoint, hits, task) = serve(move |captured| {
        if captured
            .header
            .starts_with("POST /dashboard/api/v4/accounts ")
        {
            return response(
                200,
                &[(
                    "Set-Cookie",
                    "ocg_dashboard_session=cookie-secret-value; HttpOnly; Path=/dashboard",
                )],
                br#"{"ok":true}"#,
            );
        }
        let message = format!("rejected {cookie}\nSet-Cookie: ocg_dashboard_session={cookie}");
        let body = serde_json::json!({"code": "badRequest", "message": message});
        response(
            400,
            &[(
                "Set-Cookie",
                "ocg_dashboard_session=cookie-secret-value; HttpOnly; Path=/dashboard",
            )],
            body.to_string().as_bytes(),
        )
    })
    .await;

    let error = execute(
        ApiRequest {
            endpoint: endpoint.clone(),
            method: "GET".into(),
            path: "/dashboard/api/v4/contract".into(),
            body: None,
            cas_current: false,
            bearer: None,
            session_file: None,
        },
        &mut Vec::new(),
    )
    .await
    .unwrap_err();
    let text = error.to_string();
    assert_eq!(error.status, Some(400));
    assert_eq!(error.exit_code, 3);
    assert!(text.contains("[redacted]"));
    assert!(!text.contains(cookie), "{text}");
    assert!(!text.to_ascii_lowercase().contains("set-cookie"), "{text}");

    let dir = temp_dir();
    let session = dir.join("session.json");
    std::fs::write(
        &session,
        format!(r#"{{"version":1,"endpoint":"{endpoint}","cookies":[]}}"#),
    )
    .unwrap();
    std::fs::create_dir(dir.join("session.tmp")).unwrap();
    let saved = execute(
        ApiRequest {
            endpoint,
            method: "POST".into(),
            path: "/dashboard/api/v4/accounts".into(),
            body: Some(br#"{"name":"go"}"#.to_vec()),
            cas_current: false,
            bearer: None,
            session_file: Some(session),
        },
        &mut Vec::new(),
    )
    .await
    .unwrap_err();
    assert_eq!(saved.status, Some(200));
    assert_eq!(saved.exit_code, 5);
    assert_eq!(saved.code, "acceptedNotSaved");
    assert!(saved.message.contains("already accepted"));
    assert!(saved.message.contains("not replayed"));
    assert!(!saved.to_string().contains(cookie), "{saved}");
    assert_eq!(hits.load(Ordering::SeqCst), 2);
    task.abort();
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn credential_echo_redacts_inference_key_and_short_secrets() {
    let inference_key = "k9mm";
    let password = "k9";
    let api_key = "z";
    let secret = "pqr";
    let (endpoint, hits, task) = serve(move |captured| {
        if captured.header.starts_with("GET ") {
            let body = format!(
                r#"{{"inferenceKey":"{inference_key}","password":"{password}","apiKey":"{api_key}","secret":"{secret}","name":"visible"}}"#
            );
            return response(
                200,
                &[("Content-Type", "application/json")],
                body.as_bytes(),
            );
        }
        let value: serde_json::Value = serde_json::from_slice(&captured.body).unwrap();
        let message = format!(
            "inferenceKey=BEGIN{{{}}}END password=BEGIN{{{}}}END apiKey=BEGIN{{{}}}END secret=BEGIN{{{}}}END",
            value["inferenceKey"].as_str().unwrap(),
            value["password"].as_str().unwrap(),
            value["apiKey"].as_str().unwrap(),
            value["secret"].as_str().unwrap()
        );
        let body = serde_json::json!({
            "code": "badRequest",
            "message": message,
        });
        response(
            400,
            &[("Content-Type", "application/json")],
            body.to_string().as_bytes(),
        )
    })
    .await;

    let mut stdout = Vec::new();
    execute(
        ApiRequest {
            endpoint: endpoint.clone(),
            method: "GET".into(),
            path: "/dashboard/api/v4/contract".into(),
            body: None,
            cas_current: false,
            bearer: None,
            session_file: None,
        },
        &mut stdout,
    )
    .await
    .unwrap();
    let catalog: serde_json::Value = serde_json::from_slice(&stdout).unwrap();
    assert_eq!(catalog["inferenceKey"], "[redacted]");
    assert_eq!(catalog["password"], "[redacted]");
    assert_eq!(catalog["apiKey"], "[redacted]");
    assert_eq!(catalog["secret"], "[redacted]");
    assert_eq!(catalog["name"], "visible");
    assert!(!stdout.windows(4).any(|window| window == b"k9mm"));
    assert!(!stdout.windows(3).any(|window| window == b"pqr"));

    let request = serde_json::json!({
        "inferenceKey": inference_key,
        "password": password,
        "apiKey": api_key,
        "secret": secret,
    });
    let error = execute(
        ApiRequest {
            endpoint,
            method: "POST".into(),
            path: "/dashboard/api/v4/accounts".into(),
            body: Some(serde_json::to_vec(&request).unwrap()),
            cas_current: false,
            bearer: None,
            session_file: None,
        },
        &mut Vec::new(),
    )
    .await
    .unwrap_err();
    assert_eq!(error.status, Some(400));
    assert_eq!(error.exit_code, 3);
    assert_eq!(error.code, "badRequest");
    let diagnostic: serde_json::Value = serde_json::from_str(&error.stderr_line()).unwrap();
    let message = diagnostic["message"].as_str().unwrap();
    assert_eq!(message, error.message);
    for label in ["inferenceKey", "password", "apiKey", "secret"] {
        assert_eq!(
            echoed_slot(message, label),
            "[redacted]",
            "echoed {label} leaked in {message}"
        );
    }
    assert!(!message.contains(inference_key), "{message}");
    assert!(!message.contains(secret), "{message}");
    assert_eq!(hits.load(Ordering::SeqCst), 2);
    task.abort();
}

fn echoed_slot(message: &str, label: &str) -> String {
    let marker = format!("{label}=BEGIN{{");
    let start = message
        .find(&marker)
        .unwrap_or_else(|| panic!("missing {label} in {message}"))
        + marker.len();
    let rest = &message[start..];
    let end = rest
        .find("}END")
        .unwrap_or_else(|| panic!("missing end for {label} in {message}"));
    rest[..end].to_string()
}

#[cfg(windows)]
#[tokio::test]
async fn credential_output_dacl_after_create_and_replacement() {
    let body = br#"{"saved":"private-body"}"#;
    let (endpoint, hits, task) =
        serve(move |_| response(200, &[("Content-Type", "application/json")], body)).await;
    let dir = temp_dir();
    let created = dir.join("created.json");
    let created_ok = invoke(
        Invocation {
            endpoint: endpoint.clone(),
            method: "GET".into(),
            path: "/dashboard/api/v4/contract".into(),
            input: None,
            output: Some(created.clone()),
            cas_current: false,
            key_file: None,
            session_file: None,
        },
        &mut Vec::new(),
    )
    .await
    .unwrap();
    assert_eq!(created_ok.status, 200);
    assert_eq!(std::fs::read(&created).unwrap(), body);
    crate::session_file::assert_private_dacl(&created);

    let replaced = dir.join("replaced.json");
    std::fs::write(&replaced, b"broad-acl").unwrap();
    let inherited = crate::session_file::dacl_report(&replaced);
    assert!(
        !inherited.is_private(),
        "a normal create must not already have the private DACL: {inherited:?}"
    );
    let replaced_ok = invoke(
        Invocation {
            endpoint,
            method: "GET".into(),
            path: "/dashboard/api/v4/contract".into(),
            input: None,
            output: Some(replaced.clone()),
            cas_current: false,
            key_file: None,
            session_file: None,
        },
        &mut Vec::new(),
    )
    .await
    .unwrap();
    assert_eq!(replaced_ok.status, 200);
    assert_eq!(std::fs::read(&replaced).unwrap(), body);
    crate::session_file::assert_private_dacl(&replaced);
    assert_eq!(hits.load(Ordering::SeqCst), 2);
    task.abort();
    let _ = std::fs::remove_dir_all(dir);
}
