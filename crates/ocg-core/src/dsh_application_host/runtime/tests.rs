use super::*;
use serde_json::{Value, json};
use std::collections::VecDeque;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

const TOKEN: &str = "launch-token-SECRET-xyz";
const COOKIE_VALUE: &str = "v1.test-cookie-SECRET.signature";
const DIAGNOSTIC: &str = "diag-body-SECRET-do-not-echo";
const FOLLOWED: &str = "followed-leak-path";

#[derive(Clone)]
struct RecordedRequest {
    method: String,
    target: String,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

impl RecordedRequest {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find_map(|(key, value)| key.eq_ignore_ascii_case(name).then_some(value.as_str()))
    }

    fn json_body(&self) -> Value {
        serde_json::from_slice(&self.body).expect("request JSON")
    }
}

enum FakeAction {
    Exchange {
        location: String,
        cookie_value: String,
        extra_set_cookie: Option<String>,
    },
    RpcOk(Value),
    RpcMismatch,
    RpcError {
        code: &'static str,
        diagnostic: &'static str,
    },
    Status {
        status: u16,
        body: Vec<u8>,
        content_type: &'static str,
    },
    Oversize {
        bytes: usize,
    },
    Reset,
}

struct FakeServer {
    port: u16,
    ipv6: bool,
    log: Arc<Mutex<Vec<RecordedRequest>>>,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl FakeServer {
    fn start(script: Vec<FakeAction>) -> Self {
        Self::start_on("127.0.0.1:0", false, script)
    }

    fn start_v6(script: Vec<FakeAction>) -> Self {
        Self::start_on("[::1]:0", true, script)
    }

    fn start_on(addr: &str, ipv6: bool, script: Vec<FakeAction>) -> Self {
        let listener = TcpListener::bind(addr).expect("bind loopback");
        let port = listener.local_addr().expect("local addr").port();
        let log = Arc::new(Mutex::new(Vec::new()));
        let stop = Arc::new(AtomicBool::new(false));
        let thread_log = log.clone();
        let thread_stop = stop.clone();
        let script = Arc::new(Mutex::new(VecDeque::from(script)));
        let thread = thread::spawn(move || {
            while !thread_stop.load(Ordering::SeqCst) {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        let _ = stream.set_nodelay(true);
                        let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
                        match read_http_request(&mut stream) {
                            Ok(request) => {
                                let action = script.lock().expect("script").pop_front();
                                thread_log.lock().expect("log").push(request.clone());
                                if let Some(action) = action {
                                    write_action(&mut stream, port, &request, action);
                                }
                            }
                            Err(_) => {}
                        }
                    }
                    Err(_) => break,
                }
            }
        });
        Self {
            port,
            ipv6,
            log,
            stop,
            thread: Some(thread),
        }
    }

    fn origin(&self) -> String {
        if self.ipv6 {
            format!("http://[::1]:{}", self.port)
        } else {
            format!("http://127.0.0.1:{}", self.port)
        }
    }

    fn launch_url(&self) -> String {
        format!("{}/?token={TOKEN}", self.origin())
    }

    fn requests(&self) -> Vec<RecordedRequest> {
        self.log.lock().expect("log").drain(..).collect()
    }
}

impl Drop for FakeServer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if self.ipv6 {
            let _ = TcpStream::connect(("::1", self.port));
        } else {
            let _ = TcpStream::connect(("127.0.0.1", self.port));
        }
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn read_http_request(stream: &mut TcpStream) -> std::io::Result<RecordedRequest> {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 512];
    loop {
        let read = stream.read(&mut chunk)?;
        if read == 0 {
            break;
        }
        buf.extend_from_slice(&chunk[..read]);
        if buf.windows(4).any(|window| window == b"\r\n\r\n") {
            break;
        }
        if buf.len() > 64 * 1024 {
            return Err(std::io::Error::other("headers too large"));
        }
    }
    let header_end = buf
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .ok_or_else(|| std::io::Error::other("incomplete headers"))?;
    let header_bytes = &buf[..header_end];
    let mut leftover = buf[header_end + 4..].to_vec();
    let header_text = String::from_utf8_lossy(header_bytes);
    let mut lines = header_text.split("\r\n");
    let request_line = lines.next().unwrap_or("");
    let mut parts = request_line.split(' ');
    let method = parts.next().unwrap_or("").to_owned();
    let target = parts.next().unwrap_or("").to_owned();
    let mut headers = Vec::new();
    let mut content_length = 0usize;
    for line in lines {
        let Some((name, value)) = line.split_once(':') else {
            continue;
        };
        let value = value.trim().to_owned();
        if name.eq_ignore_ascii_case("content-length") {
            content_length = value.parse().unwrap_or(0);
        }
        headers.push((name.trim().to_owned(), value));
    }
    while leftover.len() < content_length {
        let read = stream.read(&mut chunk)?;
        if read == 0 {
            break;
        }
        leftover.extend_from_slice(&chunk[..read]);
    }
    leftover.truncate(content_length);
    Ok(RecordedRequest {
        method,
        target,
        headers,
        body: leftover,
    })
}

fn write_http(
    stream: &mut TcpStream,
    status: u16,
    reason: &str,
    headers: &[(&str, String)],
    body: &[u8],
) {
    let mut response = format!("HTTP/1.1 {status} {reason}\r\nConnection: close\r\n");
    let mut has_length = false;
    for (name, value) in headers {
        if name.eq_ignore_ascii_case("content-length") {
            has_length = true;
        }
        response.push_str(name);
        response.push_str(": ");
        response.push_str(value);
        response.push_str("\r\n");
    }
    if !has_length {
        response.push_str(&format!("Content-Length: {}\r\n", body.len()));
    }
    response.push_str("\r\n");
    let _ = stream.write_all(response.as_bytes());
    let _ = stream.write_all(body);
    let _ = stream.flush();
}

fn write_action(stream: &mut TcpStream, port: u16, request: &RecordedRequest, action: FakeAction) {
    match action {
        FakeAction::Exchange {
            location,
            cookie_value,
            extra_set_cookie,
        } => {
            let name = cookie_name_for_authority(&format!("127.0.0.1:{port}"));
            let mut headers = vec![
                ("Location", location),
                (
                    "Set-Cookie",
                    format!(
                        "{name}={cookie_value}; Max-Age=86400; Path=/; HttpOnly; SameSite=Strict"
                    ),
                ),
                ("Cache-Control", "no-store".into()),
            ];
            if let Some(extra) = extra_set_cookie {
                headers.push(("Set-Cookie", extra));
            }
            let headers: Vec<(&str, String)> = headers
                .iter()
                .map(|(name, value)| (*name, value.clone()))
                .collect();
            write_http(stream, 303, "See Other", &headers, &[]);
        }
        FakeAction::RpcOk(value) => write_rpc(stream, request, true, None, value),
        FakeAction::RpcMismatch => write_http(
            stream,
            200,
            "OK",
            &[("Content-Type", "application/json".into())],
            &serde_json::to_vec(&json!({
                "type": "server-response",
                "rpcId": "00000000-0000-4000-8000-000000000000",
                "result": { "ok": true, "value": [] }
            }))
            .expect("json"),
        ),
        FakeAction::RpcError { code, diagnostic } => write_rpc(
            stream,
            request,
            false,
            Some((code, diagnostic)),
            Value::Null,
        ),
        FakeAction::Status {
            status,
            body,
            content_type,
        } => write_http(
            stream,
            status,
            reason(status),
            &[("Content-Type", content_type.into())],
            &body,
        ),
        FakeAction::Oversize { bytes } => {
            let length = bytes.to_string();
            let _ = stream.write_all(
                format!(
                    "HTTP/1.1 200 OK\r\nConnection: close\r\nContent-Type: application/json\r\nContent-Length: {length}\r\n\r\n"
                )
                .as_bytes(),
            );
            let chunk = vec![b'x'; 1024.min(bytes)];
            let mut remaining = bytes;
            while remaining > 0 {
                let n = chunk.len().min(remaining);
                if stream.write_all(&chunk[..n]).is_err() {
                    break;
                }
                remaining -= n;
            }
        }
        FakeAction::Reset => {}
    }
}

fn write_rpc(
    stream: &mut TcpStream,
    request: &RecordedRequest,
    ok: bool,
    error: Option<(&str, &str)>,
    value: Value,
) {
    let envelope = request.json_body();
    let rpc_id = envelope
        .get("rpcId")
        .and_then(Value::as_str)
        .unwrap_or("missing");
    let result = if ok {
        json!({ "ok": true, "value": value })
    } else {
        let (code, diagnostic) = error.unwrap_or(("operation-error", DIAGNOSTIC));
        json!({
            "ok": false,
            "error": { "code": code, "message": diagnostic, "details": { "secret": diagnostic } }
        })
    };
    write_http(
        stream,
        200,
        "OK",
        &[("Content-Type", "application/json".into())],
        &serde_json::to_vec(&json!({
            "type": "server-response",
            "rpcId": rpc_id,
            "result": result
        }))
        .expect("json"),
    );
}

fn reason(status: u16) -> &'static str {
    match status {
        401 => "Unauthorized",
        403 => "Forbidden",
        500 => "Internal Server Error",
        _ => "OK",
    }
}

fn change(application: &str) -> Value {
    json!({
        "changed": application != "failed" && application != "cancelled",
        "application": application,
        "stage": "install",
        "target": "@open-console-gateway/dsh-plugin",
        "bundle": "@open-console-gateway/dsh-plugin",
        "error": if application == "failed" {
            json!({ "code": "operation-error", "diagnostic": DIAGNOSTIC })
        } else {
            Value::Null
        }
    })
}

fn cookie_for(origin: &str, value: &str) -> DshRuntimeCookie {
    let origin = DshRuntimeOrigin::parse(origin).expect("origin");
    let name = cookie_name_for_authority(origin.host_header());
    DshRuntimeCookie::parse(&origin, &format!("{name}={value}")).expect("cookie")
}

fn connect_session(server: &FakeServer) -> DshRuntimeClient {
    DshRuntimeClient::connect_session(&server.origin(), cookie_for(&server.origin(), COOKIE_VALUE))
        .expect("session")
}

fn assert_redacted(error: &DshRuntimeError, secrets: &[&str]) {
    let display = error.to_string();
    let debug = format!("{error:?}");
    for secret in secrets {
        assert!(
            !display.contains(secret),
            "Display leaked {secret}: {display}"
        );
        assert!(!debug.contains(secret), "Debug leaked {secret}: {debug}");
    }
}

#[test]
fn launch_url_rejects_non_loopback_shapes() {
    let rejected = [
        "https://127.0.0.1:1/?token=x",
        "http://localhost:1/?token=x",
        "http://127.0.0.1/?token=x",
        "http://127.0.0.1:1/path?token=x",
        "http://user:pass@127.0.0.1:1/?token=x",
        "http://127.0.0.1:1/?token=x#frag",
        "http://127.0.0.1:1/?token=x&extra=1",
        "http://127.0.0.1:1/?token=a&token=b",
        "http://127.0.0.1:1/?Token=x",
        "http://127.0.0.1:1/",
        "http://192.168.1.1:1/?token=x",
        "http://127.0.0.2:1/?token=x",
        "http://[::ffff:127.0.0.1]:1/?token=x",
        "http://[::1]/?token=x",
        "ws://127.0.0.1:1/?token=x",
        "http://127.0.0.1:0/?token=x",
    ];
    for url in rejected {
        let error = DshRuntimeClient::connect_launch_url(url).expect_err(url);
        assert_eq!(error.kind, DshRuntimeErrorKind::Invalid, "{url}");
        assert_redacted(&error, &[url, "token=x", "user:pass"]);
    }
}

#[test]
fn session_origin_rejects_token_query() {
    let origin = DshRuntimeOrigin::parse("http://127.0.0.1:3080").expect("origin");
    let cookie = DshRuntimeCookie::parse(
        &origin,
        &format!(
            "{}={COOKIE_VALUE}",
            cookie_name_for_authority(origin.host_header())
        ),
    )
    .expect("cookie");
    let error = DshRuntimeClient::connect_session("http://127.0.0.1:3080/?token=keep-out", cookie)
        .expect_err("tokenized session origin");
    assert_eq!(error.kind, DshRuntimeErrorKind::Invalid);
    assert_redacted(&error, &["keep-out"]);
}

#[test]
fn cookie_debug_hides_value_and_rejects_wrong_name() {
    let origin = DshRuntimeOrigin::parse("http://127.0.0.1:3080").expect("origin");
    let cookie = cookie_for("http://127.0.0.1:3080", COOKIE_VALUE);
    let debug = format!("{cookie:?}");
    assert_eq!(debug, "DshRuntimeCookie([redacted])");
    assert!(!debug.contains(COOKIE_VALUE));
    let error = DshRuntimeCookie::parse(&origin, &format!("dsh-auth-notahash={COOKIE_VALUE}"))
        .expect_err("wrong name");
    assert_eq!(error.kind, DshRuntimeErrorKind::Invalid);
    assert_redacted(&error, &[COOKIE_VALUE]);
}

#[test]
fn token_exchange_stores_clean_origin_and_exact_cookie() {
    let server = FakeServer::start(vec![
        FakeAction::Exchange {
            location: "./".into(),
            cookie_value: COOKIE_VALUE.into(),
            extra_set_cookie: None,
        },
        FakeAction::RpcOk(json!([])),
    ]);
    let client = DshRuntimeClient::connect_launch_url(&server.launch_url()).expect("exchange");
    assert_eq!(client.origin(), server.origin());
    assert!(!client.origin().contains("token"));
    assert!(!format!("{client:?}").contains(TOKEN));
    assert!(!format!("{client:?}").contains(COOKIE_VALUE));
    client.list_bundles().expect("list");
    let requests = server.requests();
    let expected_host = format!("127.0.0.1:{}", server.port);
    let expected_cookie = format!(
        "{}={COOKIE_VALUE}",
        cookie_name_for_authority(&expected_host)
    );
    assert_eq!(requests[0].method, "GET");
    assert_eq!(requests[0].target, format!("/?token={TOKEN}"));
    assert_eq!(requests[0].header("host"), Some(expected_host.as_str()));
    assert_eq!(requests[1].header("cookie"), Some(expected_cookie.as_str()));
}

#[test]
fn token_exchange_does_not_follow_redirect() {
    let server = FakeServer::start(vec![
        FakeAction::Exchange {
            location: format!("/{FOLLOWED}"),
            cookie_value: COOKIE_VALUE.into(),
            extra_set_cookie: Some(format!("other=other-{DIAGNOSTIC}")),
        },
        FakeAction::Status {
            status: 200,
            body: FOLLOWED.as_bytes().to_vec(),
            content_type: "text/plain",
        },
    ]);
    let client = DshRuntimeClient::connect_launch_url(&server.launch_url()).expect("exchange");
    assert_eq!(client.origin(), server.origin());
    let requests = server.requests();
    assert_eq!(requests.len(), 1);
    assert!(!requests[0].target.contains(FOLLOWED));
}

#[test]
fn list_bundles_sends_exact_envelope_and_cookie() {
    let server = FakeServer::start(vec![FakeAction::RpcOk(json!([{
        "name": "@open-console-gateway/dsh-plugin",
        "enabled": true,
        "installed": true,
        "removable": true,
        "version": "0.1.0",
        "optional": false,
        "rows": [],
        "overrides": [],
        "error": { "code": "operation-error", "diagnostic": DIAGNOSTIC }
    }]))]);
    let client = connect_session(&server);
    let bundles = client.list_bundles().expect("list");
    assert_eq!(bundles.len(), 1);
    assert_eq!(bundles[0].name, "@open-console-gateway/dsh-plugin");
    assert_eq!(bundles[0].error_code.as_deref(), Some("operation-error"));
    let request = &server.requests()[0];
    assert_eq!(request.method, "POST");
    assert_eq!(request.target, "/api/pluginManager/listBundles");
    assert!(
        request.header("content-type").is_some_and(|value| value
            .split(';')
            .next()
            .unwrap_or("")
            .trim()
            == "application/json")
    );
    let expected_cookie = format!(
        "{}={COOKIE_VALUE}",
        cookie_name_for_authority(&format!("127.0.0.1:{}", server.port))
    );
    assert_eq!(request.header("cookie"), Some(expected_cookie.as_str()));
    let body = request.json_body();
    assert_eq!(body["type"], "client-request");
    assert_eq!(body["method"], "pluginManager/listBundles");
    assert_eq!(body["payload"], json!({ "args": {} }));
    let rpc_id = body["rpcId"].as_str().expect("rpcId");
    uuid::Uuid::parse_str(rpc_id).expect("uuid rpcId");
    assert_eq!(body.as_object().expect("object").len(), 4);
}

#[test]
fn rpc_id_mismatch_is_unknown_without_body_secrets() {
    let server = FakeServer::start(vec![FakeAction::RpcMismatch]);
    let client = connect_session(&server);
    let error = client.list_bundles().expect_err("mismatch");
    assert_eq!(error.kind, DshRuntimeErrorKind::Unknown);
    assert_eq!(error.unknown_kind, Some(DshRuntimeUnknownKind::Protocol));
    assert_redacted(&error, &["00000000-0000-4000-8000-000000000000"]);
}

#[test]
fn status_401_and_403_are_redacted() {
    for status in [401u16, 403] {
        let server = FakeServer::start(vec![FakeAction::Status {
            status,
            body: json!({ "error": DIAGNOSTIC, "token": TOKEN, "cookie": COOKIE_VALUE })
                .to_string()
                .into_bytes(),
            content_type: "application/json",
        }]);
        let client = connect_session(&server);
        let error = client.list_bundles().expect_err("auth");
        assert_eq!(
            error.kind,
            if status == 401 {
                DshRuntimeErrorKind::Unauthenticated
            } else {
                DshRuntimeErrorKind::Forbidden
            }
        );
        assert_redacted(&error, &[DIAGNOSTIC, TOKEN, COOKIE_VALUE, &server.origin()]);
    }
}

#[test]
fn exchange_401_does_not_echo_token_or_url() {
    let server = FakeServer::start(vec![FakeAction::Status {
        status: 401,
        body: DIAGNOSTIC.as_bytes().to_vec(),
        content_type: "text/plain",
    }]);
    let error = DshRuntimeClient::connect_launch_url(&server.launch_url()).expect_err("401");
    assert_eq!(error.kind, DshRuntimeErrorKind::Unauthenticated);
    assert_redacted(
        &error,
        &[TOKEN, DIAGNOSTIC, &server.origin(), &server.launch_url()],
    );
}

#[test]
fn application_outcomes_are_typed_and_failed_is_definitive() {
    let variants = [
        ("applied", DshRuntimeApplication::Applied, false),
        (
            "restart-required",
            DshRuntimeApplication::RestartRequired,
            false,
        ),
        ("overridden", DshRuntimeApplication::Overridden, false),
        ("failed", DshRuntimeApplication::Failed, false),
        ("cancelled", DshRuntimeApplication::Cancelled, false),
    ];
    for (label, expected, _) in variants {
        let server = FakeServer::start(vec![FakeAction::RpcOk(change(label))]);
        let client = connect_session(&server);
        let result = client
            .install_bundle(
                "/abs/plugin",
                DshInstallOptions {
                    enabled: Some(true),
                    request_id: Some("install-req-1".into()),
                },
            )
            .expect(label);
        assert_eq!(result.request_id, "install-req-1");
        assert_eq!(result.change.application, expected);
        if expected == DshRuntimeApplication::Failed {
            assert_eq!(result.change.error_code.as_deref(), Some("operation-error"));
        }
        let request = &server.requests()[0];
        assert_eq!(request.target, "/api/pluginManager/installBundle");
        let body = request.json_body();
        assert_eq!(body["method"], "pluginManager/installBundle");
        assert_eq!(body["payload"]["args"]["spec"], "/abs/plugin");
        assert_eq!(
            body["payload"]["args"]["options"]["requestId"],
            "install-req-1"
        );
        assert_eq!(body["payload"]["args"]["options"]["enabled"], true);
    }
}

#[test]
fn unknown_application_is_transport_unknown_and_keeps_request_id() {
    let server = FakeServer::start(vec![FakeAction::RpcOk(json!({
        "changed": true,
        "application": "mystery",
        "stage": "install",
        "target": "pkg"
    }))]);
    let client = connect_session(&server);
    let error = client
        .install_bundle(
            "/abs/plugin",
            DshInstallOptions {
                request_id: Some("recover-me".into()),
                ..DshInstallOptions::default()
            },
        )
        .expect_err("unknown application");
    assert!(error.is_unknown());
    assert_eq!(error.request_id(), Some("recover-me"));
    assert_redacted(&error, &["mystery"]);
}

#[test]
fn remote_error_preserves_semantic_code_not_diagnostic() {
    let server = FakeServer::start(vec![FakeAction::RpcError {
        code: "not-bundle",
        diagnostic: DIAGNOSTIC,
    }]);
    let client = connect_session(&server);
    let error = client
        .remove_bundle("@open-console-gateway/dsh-plugin")
        .expect_err("remote");
    assert_eq!(error.kind, DshRuntimeErrorKind::Remote);
    assert_eq!(error.remote_code.as_deref(), Some("not-bundle"));
    assert!(!error.is_unknown());
    assert_redacted(&error, &[DIAGNOSTIC]);
}

#[test]
fn install_reset_is_unknown_and_keeps_request_id() {
    let server = FakeServer::start(vec![FakeAction::Reset]);
    let client = connect_session(&server);
    let error = client
        .install_bundle(
            "/abs/plugin",
            DshInstallOptions {
                request_id: Some("need-reconcile".into()),
                ..DshInstallOptions::default()
            },
        )
        .expect_err("reset");
    assert_eq!(error.kind, DshRuntimeErrorKind::Unknown);
    assert_eq!(error.request_id(), Some("need-reconcile"));
    assert_redacted(&error, &[TOKEN, COOKIE_VALUE, DIAGNOSTIC, &server.origin()]);
}

#[test]
fn oversize_response_is_bounded() {
    let server = FakeServer::start(vec![FakeAction::Oversize { bytes: 64 * 1024 }]);
    let client = DshRuntimeClient::connect_session_with_limits(
        &server.origin(),
        cookie_for(&server.origin(), COOKIE_VALUE),
        DshRuntimeLimits {
            max_body_bytes: 1024,
            ..DshRuntimeLimits::default()
        },
    )
    .expect("session");
    let error = client.list_bundles().expect_err("bounded");
    assert_eq!(error.kind, DshRuntimeErrorKind::Unknown);
    assert_eq!(error.unknown_kind, Some(DshRuntimeUnknownKind::Protocol));
    assert_eq!(error.message, MSG_BOUNDED);
}

#[test]
fn wait_for_install_null_keeps_request_id_path() {
    let server = FakeServer::start(vec![FakeAction::RpcOk(Value::Null)]);
    let client = connect_session(&server);
    assert_eq!(client.wait_for_install("r1").expect("null"), None);
}

#[test]
fn inspect_and_plugins_parse_fiber_phase() {
    let server = FakeServer::start(vec![
        FakeAction::RpcOk(json!({
            "status": "accepted",
            "kind": "path",
            "name": "@open-console-gateway/dsh-plugin",
            "version": "0.1.0",
            "bundle": true
        })),
        FakeAction::RpcOk(json!([{
            "moduleName": "@open-console-gateway/dsh-plugin",
            "enabled": true,
            "fiberPhase": "active"
        }])),
    ]);
    let client = connect_session(&server);
    match client.inspect("/abs/plugin").expect("inspect") {
        DshSpecInspection::Accepted { kind, bundle, .. } => {
            assert_eq!(kind, "path");
            assert_eq!(bundle, Some(true));
        }
        DshSpecInspection::Refused { .. } => panic!("expected accepted"),
    }
    let plugin = &client.list_plugins().expect("plugins")[0];
    assert_eq!(plugin.module_name, "@open-console-gateway/dsh-plugin");
    assert_eq!(plugin.fiber_phase.as_deref(), Some("active"));
}

#[test]
fn session_origin_accepts_loopback_ipv6() {
    let origin = DshRuntimeOrigin::parse("http://[::1]:3080").expect("ipv6 origin");
    assert_eq!(origin.as_str(), "http://[::1]:3080");
    assert_eq!(origin.host_header(), "[::1]:3080");
    let error = DshRuntimeClient::connect_launch_url("http://[::1]:19387/?token=loopback-token")
        .expect_err("no listener");
    assert_ne!(
        error.kind,
        DshRuntimeErrorKind::Invalid,
        "valid IPv6 loopback must not fail host_str matching"
    );
    let server = FakeServer::start_v6(vec![FakeAction::RpcOk(json!([]))]);
    let client = connect_session(&server);
    assert_eq!(client.origin(), server.origin());
    assert!(client.list_bundles().expect("list").is_empty());
}

#[test]
fn minted_browser_session_cookie_is_redacted_and_authority_bound() {
    let origin = DshRuntimeOrigin::parse("http://127.0.0.1:3080").expect("origin");
    let secret = [0x11u8; 32];
    let cookie =
        mint_browser_session_cookie(&origin, &secret, Some(1_700_000_000_000)).expect("mint");
    assert_eq!(cookie.name, cookie_name_for_authority(origin.host_header()));
    assert!(cookie.value.starts_with("v1."));
    let debug = format!("{cookie:?}");
    assert_eq!(debug, "DshRuntimeCookie([redacted])");
    assert!(!debug.contains(&cookie.value));
    assert!(!debug.contains("111111"));
    let client_debug = format!(
        "{:?}",
        DshRuntimeClient::connect_session(origin.as_str(), cookie).expect("session")
    );
    assert!(!client_debug.contains("v1."));
}

#[test]
fn spawn_blocking_uses_the_blocking_client_without_a_nested_runtime() {
    let server = FakeServer::start(vec![FakeAction::RpcOk(json!([]))]);
    let client = connect_session(&server);
    let bundles = std::thread::spawn(move || client.list_bundles())
        .join()
        .expect("join")
        .expect("list");
    assert!(bundles.is_empty());
}
