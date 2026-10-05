use super::{
    CapturedDeadline, ClientAccess, IntentRecord, OwnedHop, PublicHop, PublicPath,
    forward_for_test, forward_observing, forward_public, forward_query, public_target,
};
use crate::cpa_runtime::fingerprint_key;
use crate::gateway::diagnostics::RequestTrace;
use crate::gateway::handler::{
    authenticate_client, chat_completions, gemini_model_action, messages, messages_count_tokens,
    responses,
};
use crate::gateway::protocol::{parse_client_request, parse_gemini_request};
use crate::gateway_keys::{
    CredentialEntry, CredentialSnapshot, KeyStore, PRIMARY_KEY_ID, refresh_snapshot,
};
use crate::kernel::protocol::ApiFormat;
use crate::models::SubGatewayKey;
use crate::state::{CoreState, CoreStateInner};
use axum::Router;
use axum::body::{Body, Bytes};
use axum::extract::{Extension, Path, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use axum::response::Response;
use axum::routing::post;
use futures_util::StreamExt;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

const MODEL: &str = "hy4-preview";
const SECRET: &str = "hop-fixture-secret";
const CLIENT_REQUEST_ID: &str = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";
const GEMINI_BODY: &str = r#"{"contents":[{"role":"user","parts":[{"text":"hi"}]}]}"#;

struct StateDir {
    state: Option<CoreState>,
    dir: Option<std::path::PathBuf>,
}

impl StateDir {
    fn open() -> Self {
        let mut dir = std::env::temp_dir();
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        dir.push(format!("ocg-cpa-ingress-{nanos}"));
        std::fs::create_dir_all(&dir).expect("test dir");
        let db = crate::db::Database::open(dir.clone()).expect("test database");
        let cipher: std::sync::Arc<dyn crate::crypto::KeyCipher + Send + Sync> =
            std::sync::Arc::new(crate::crypto::StaticKeyCipher::new("test"));
        let state = CoreStateInner::new(db, dir.clone(), cipher).expect("state");
        Self {
            state: Some(std::sync::Arc::new(state)),
            dir: Some(dir),
        }
    }

    fn state(&self) -> CoreState {
        self.state.clone().expect("state")
    }

    fn with_key(&self) {
        let mut snapshot = CredentialSnapshot::new();
        snapshot.insert(
            "ocg-primary".to_string(),
            CredentialEntry {
                id: PRIMARY_KEY_ID.to_string(),
                name: "Primary".to_string(),
            },
        );
        *self.state().credential_snapshot.write() = snapshot;
    }
}

impl Drop for StateDir {
    fn drop(&mut self) {
        self.state.take();
        if let Some(dir) = self.dir.take() {
            std::fs::remove_dir_all(dir).ok();
        }
    }
}

struct Hit {
    path: String,
    query: String,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

#[derive(Clone, Default)]
struct Sink {
    hits: Arc<Mutex<Vec<Hit>>>,
}

struct Server {
    origin: String,
    handle: tokio::task::JoinHandle<()>,
}

impl Drop for Server {
    fn drop(&mut self) {
        self.handle.abort();
    }
}

fn ahead(duration: Duration) -> CapturedDeadline {
    CapturedDeadline {
        wall: chrono::Utc::now() + chrono::Duration::from_std(duration).expect("duration"),
        mono: tokio::time::Instant::now() + duration,
    }
}

fn elapsed() -> CapturedDeadline {
    CapturedDeadline {
        wall: chrono::Utc::now() - chrono::Duration::seconds(1),
        mono: tokio::time::Instant::now(),
    }
}

fn owned(origin: &str) -> OwnedHop {
    OwnedHop::fixture(origin, SECRET, 41, 7)
}

fn header_map(pairs: &[(&str, &str)]) -> HeaderMap {
    let mut headers = HeaderMap::new();
    for (name, value) in pairs {
        headers.insert(
            axum::http::HeaderName::from_bytes(name.as_bytes()).expect("header name"),
            HeaderValue::from_str(value).expect("header value"),
        );
    }
    headers
}

fn scaffold_access() -> ClientAccess {
    ClientAccess {
        key_id: "key-1".to_string(),
        captured_key_fingerprint: "ab".repeat(32),
    }
}

fn chat_body() -> Bytes {
    Bytes::from(format!(
        r#"{{"model":"{MODEL}","messages":[{{"role":"user","content":"hi"}}]}}"#
    ))
}

fn chat_hop(headers: HeaderMap, query: &str) -> PublicHop {
    let body = chat_body();
    let parsed = parse_client_request(ApiFormat::ChatCompletions, body.clone()).expect("chat");
    PublicHop {
        trace: RequestTrace::new(),
        headers,
        body,
        parsed,
        client: scaffold_access(),
        path: PublicPath::ChatCompletions,
        query: query.to_string(),
    }
}

fn bearer(value: &str) -> HeaderMap {
    let mut headers = HeaderMap::new();
    headers.insert(
        axum::http::header::AUTHORIZATION,
        HeaderValue::from_str(&format!("Bearer {value}")).expect("bearer"),
    );
    headers
}

fn hit_header<'a>(hit: &'a Hit, name: &str) -> Option<&'a str> {
    hit.headers
        .iter()
        .find(|(key, _)| key.eq_ignore_ascii_case(name))
        .map(|(_, value)| value.as_str())
}

async fn text(response: Response) -> (StatusCode, String) {
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), 64 * 1024)
        .await
        .expect("response body");
    (status, String::from_utf8_lossy(&bytes).into_owned())
}

async fn serve(app: Router) -> Server {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let origin = format!("http://{}", listener.local_addr().expect("addr"));
    let handle = tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    Server { origin, handle }
}

fn recording(status: StatusCode, content_type: &'static str, body: &'static str) -> (Sink, Router) {
    let sink = Sink::default();
    let app = Router::new()
        .fallback(
            move |State(sink): State<Sink>, request: axum::extract::Request| async move {
                record(sink, request, status, content_type, body).await
            },
        )
        .with_state(sink.clone());
    (sink, app)
}

async fn record(
    sink: Sink,
    request: axum::extract::Request,
    status: StatusCode,
    content_type: &'static str,
    body: &'static str,
) -> Response {
    let path = request.uri().path().to_string();
    let query = request.uri().query().unwrap_or("").to_string();
    let headers = request
        .headers()
        .iter()
        .map(|(name, value)| {
            (
                name.as_str().to_string(),
                value.to_str().unwrap_or("").to_string(),
            )
        })
        .collect();
    let bytes = axum::body::to_bytes(request.into_body(), 1024 * 1024)
        .await
        .expect("request body");
    sink.hits.lock().expect("hits").push(Hit {
        path,
        query,
        headers,
        body: bytes.to_vec(),
    });
    Response::builder()
        .status(status)
        .header(axum::http::header::CONTENT_TYPE, content_type)
        .body(Body::from(body))
        .expect("fixture response")
}

#[test]
fn public_target_keeps_path_and_query() {
    let (path, query) = public_target(
        "/v1beta/models/hy4-preview:streamGenerateContent?alt=sse&key=client-key-value",
    );
    assert_eq!(path, "/v1beta/models/hy4-preview:streamGenerateContent");
    assert_eq!(query, "alt=sse&key=client-key-value");
    assert_eq!(
        public_target("/v1/chat/completions"),
        ("/v1/chat/completions".to_string(), String::new())
    );
}

#[test]
fn gemini_alias_hops_on_the_child_v1beta_path() {
    assert_eq!(
        super::gemini_path("/v1/models/cli-test-model:generateContent").as_deref(),
        Some("/v1beta/models/cli-test-model:generateContent")
    );
    assert_eq!(
        super::gemini_path("/v1/models/cli-test-model:streamGenerateContent").as_deref(),
        Some("/v1beta/models/cli-test-model:streamGenerateContent")
    );
    assert_eq!(
        super::gemini_path("/v1/models/cli-test-model:countTokens").as_deref(),
        Some("/v1beta/models/cli-test-model:countTokens")
    );
    assert_eq!(
        super::gemini_path("/v1beta/models/cli-test-model:generateContent").as_deref(),
        Some("/v1beta/models/cli-test-model:generateContent")
    );
    assert_eq!(super::gemini_path("/v1/models"), None);
    assert_eq!(
        super::gemini_path("/v1/models/cli-test-model:embedContent"),
        None
    );
}

#[test]
fn query_strips_credential_parameters_only() {
    assert_eq!(
        forward_query(
            "alt=sse&key=client-key-value&auth_token=client-token&api_key=k&api-key=k&x-api-key=k&x-goog-api-key=k&pinned_auth_id=pin&pinned-auth-id=pin&x=1"
        ),
        "alt=sse&x=1"
    );
    assert_eq!(forward_query("keyboard=1&API_KEY=k"), "keyboard=1");
}

#[test]
fn debug_redacts_the_hop_secret() {
    let owned = OwnedHop::fixture("http://127.0.0.1:9", SECRET, 41, 7);
    let rendered = format!("{owned:?}");
    assert!(rendered.contains("[redacted]"));
    assert!(!rendered.contains(SECRET));
}

#[tokio::test]
async fn unready_child_is_explicitly_unavailable() {
    let dir = StateDir::open();
    let hop = chat_hop(HeaderMap::new(), "");
    let (status, body) = text(forward_public(dir.state(), hop).await).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert!(body.contains("owned CPA child is missing"), "{body}");
    assert!(!body.contains(SECRET));
}

#[tokio::test]
async fn gemini_count_uses_the_hop_and_embed_stays_unimplemented() {
    let dir = StateDir::open();
    dir.with_key();
    let headers = header_map(&[("authorization", "Bearer ocg-primary")]);
    let mut count_trace = RequestTrace::new();
    count_trace.path = format!("/v1beta/models/{MODEL}:countTokens?alt=sse");
    let count = gemini_model_action(
        State(dir.state()),
        Extension(count_trace),
        Path(format!("{MODEL}:countTokens")),
        headers.clone(),
        Bytes::from(GEMINI_BODY),
    )
    .await;
    let (status, body) = text(count).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert!(body.contains("owned CPA child is missing"), "{body}");
    assert!(!body.contains("local estimation"), "{body}");

    let mut embed_trace = RequestTrace::new();
    embed_trace.path = format!("/v1beta/models/{MODEL}:embedContent");
    let embed = gemini_model_action(
        State(dir.state()),
        Extension(embed_trace),
        Path(format!("{MODEL}:embedContent")),
        headers,
        Bytes::from(GEMINI_BODY),
    )
    .await;
    let (status, body) = text(embed).await;
    assert_eq!(status, StatusCode::NOT_IMPLEMENTED);
    assert!(body.contains("embeddings"), "{body}");
}

#[tokio::test]
async fn sdk_count_handler_does_not_generate_when_the_child_is_unready() {
    let dir = StateDir::open();
    dir.with_key();
    let response = messages_count_tokens(
        State(dir.state()),
        Extension(RequestTrace::new()),
        header_map(&[("x-api-key", "ocg-primary")]),
        Bytes::from(format!(
            r#"{{"model":"{MODEL}","messages":[{{"role":"user","content":"hi"}}]}}"#
        )),
    )
    .await;
    let (status, body) = text(response).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert!(body.contains("owned CPA child is missing"), "{body}");
}

#[tokio::test]
async fn unknown_model_and_failed_constraints_do_not_send() {
    let dir = StateDir::open();
    let (sink, app) = recording(StatusCode::CREATED, "application/json", r#"{"kept":true}"#);
    let server = serve(app).await;
    let mut hop = chat_hop(HeaderMap::new(), "");
    hop.body = Bytes::from(r#"{"model":"not-a-public-model","messages":[]}"#);
    hop.parsed = parse_client_request(ApiFormat::ChatCompletions, hop.body.clone()).expect("parse");
    let cancels = Arc::new(AtomicU32::new(0));
    let (status, body) = text(
        forward_for_test(
            dir.state(),
            hop,
            owned(&server.origin),
            ahead(Duration::from_secs(5)),
            cancels.clone(),
        )
        .await,
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(body.contains("unknown model"), "{body}");
    assert!(sink.hits.lock().expect("hits").is_empty());
    assert_eq!(cancels.load(Ordering::SeqCst), 0);

    let mut responses = chat_hop(HeaderMap::new(), "");
    responses.body = Bytes::from(format!(r#"{{"model":"{MODEL}","input":"hi"}}"#));
    responses.parsed =
        parse_client_request(ApiFormat::Responses, responses.body.clone()).expect("responses");
    responses.path = PublicPath::Responses;
    let (status, body) = text(
        forward_for_test(
            dir.state(),
            responses,
            owned(&server.origin),
            ahead(Duration::from_secs(5)),
            cancels.clone(),
        )
        .await,
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(body.contains("store=false"), "{body}");
    assert!(sink.hits.lock().expect("hits").is_empty());
    assert_eq!(cancels.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn raw_public_request_is_one_hop_and_the_child_payload_is_preserved() {
    let dir = StateDir::open();
    let (sink, app) = recording(StatusCode::CREATED, "application/json", r#"{"kept":true}"#);
    let server = serve(app).await;
    let headers = header_map(&[
        ("authorization", "Bearer client-key"),
        ("cookie", "session=client"),
        ("x-api-key", "client-key"),
        ("x-goog-api-key", "client-key"),
        ("x-ocg-request-id", CLIENT_REQUEST_ID),
        ("x-ocg-process-generation", "1"),
        ("x-ocg-projection-revision", "2"),
        ("pinned_auth_id", "client-picked"),
        ("x-pinned-auth-id", "client-picked"),
        ("x-client-trace", "keep-me"),
    ]);
    let hop = chat_hop(
        headers,
        "alt=sse&key=client-key-value&auth_token=client-token&x=1",
    );
    let public_trace = hop.trace.request_id.clone();
    let sent = hop.body.clone();
    let cancels = Arc::new(AtomicU32::new(0));
    let response = forward_for_test(
        dir.state(),
        hop,
        owned(&server.origin),
        ahead(Duration::from_secs(5)),
        cancels.clone(),
    )
    .await;
    assert_eq!(
        response
            .headers()
            .get(axum::http::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok()),
        Some("application/json")
    );
    let (status, body) = text(response).await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(body, r#"{"kept":true}"#);
    assert!(!body.contains(SECRET));
    let hits = sink.hits.lock().expect("hits");
    assert_eq!(hits.len(), 1);
    let hit = &hits[0];
    assert_eq!(hit.path, "/v1/chat/completions");
    assert_eq!(hit.body, sent.as_ref());
    assert!(hit.body.windows(MODEL.len()).any(|w| w == MODEL.as_bytes()));
    assert_eq!(hit.query, "alt=sse&x=1");
    assert!(!hit.query.contains("client-key-value"));
    assert!(!hit.query.contains("client-token"));
    assert_eq!(
        hit_header(hit, "authorization"),
        Some("Bearer hop-fixture-secret")
    );
    assert_ne!(hit_header(hit, "x-ocg-request-id"), Some(CLIENT_REQUEST_ID));
    let request_id = hit_header(hit, "x-ocg-request-id").expect("generated id");
    assert_eq!(request_id.len(), 36);
    let private_id = uuid::Uuid::parse_str(request_id).expect("private correlation id");
    assert_ne!(public_trace, CLIENT_REQUEST_ID);
    assert!(public_trace.starts_with("ocg-"));
    assert_eq!(
        crate::cpa_execution::correlation_client_trace(&dir.state(), private_id).as_deref(),
        Some(public_trace.as_str())
    );
    assert_eq!(
        crate::cpa_execution::correlation_authorization_kind(&dir.state(), private_id),
        Some("client")
    );
    assert_eq!(hit_header(hit, "x-ocg-process-generation"), Some("41"));
    assert_eq!(hit_header(hit, "x-ocg-projection-revision"), Some("7"));
    assert_eq!(hit_header(hit, "x-client-trace"), Some("keep-me"));
    assert!(hit_header(hit, "cookie").is_none());
    assert!(hit_header(hit, "x-api-key").is_none());
    assert!(hit_header(hit, "x-goog-api-key").is_none());
    assert!(hit_header(hit, "pinned_auth_id").is_none());
    assert!(hit_header(hit, "x-pinned-auth-id").is_none());
    assert_eq!(cancels.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn child_error_and_redirect_are_single_requests() {
    let dir = StateDir::open();
    let (sink, app) = recording(
        StatusCode::INTERNAL_SERVER_ERROR,
        "application/json",
        r#"{"error":"nope"}"#,
    );
    let server = serve(app).await;
    let cancels = Arc::new(AtomicU32::new(0));
    let (status, body) = text(
        forward_for_test(
            dir.state(),
            chat_hop(HeaderMap::new(), ""),
            owned(&server.origin),
            ahead(Duration::from_secs(5)),
            cancels.clone(),
        )
        .await,
    )
    .await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(body, r#"{"error":"nope"}"#);
    assert_eq!(sink.hits.lock().expect("hits").len(), 1);
    assert_eq!(cancels.load(Ordering::SeqCst), 0);

    let redirects = Arc::new(AtomicU32::new(0));
    let app = Router::new()
        .fallback(|State(counter): State<Arc<AtomicU32>>| async move {
            counter.fetch_add(1, Ordering::SeqCst);
            Response::builder()
                .status(StatusCode::FOUND)
                .header("location", "/v1/chat/completions")
                .body(Body::from("stop"))
                .unwrap()
        })
        .with_state(redirects.clone());
    let server = serve(app).await;
    let (status, body) = text(
        forward_for_test(
            dir.state(),
            chat_hop(HeaderMap::new(), ""),
            owned(&server.origin),
            ahead(Duration::from_secs(5)),
            Arc::new(AtomicU32::new(0)),
        )
        .await,
    )
    .await;
    assert_eq!(status, StatusCode::FOUND);
    assert_eq!(body, "stop");
    assert_eq!(redirects.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn stream_preserves_content_type_and_every_chunk() {
    let dir = StateDir::open();
    let hits = Arc::new(AtomicU32::new(0));
    let app = Router::new()
        .route(
            "/v1/chat/completions",
            post(
                |State(seen): State<Arc<AtomicU32>>, request: axum::extract::Request| async move {
                    seen.fetch_add(1, Ordering::SeqCst);
                    let _ = axum::body::to_bytes(request.into_body(), 1024 * 1024).await;
                    let chunks = futures_util::stream::iter([
                        Ok::<Bytes, std::io::Error>(Bytes::from("data: one\n\n")),
                        Ok(Bytes::from("data: two\n\n")),
                    ]);
                    Response::builder()
                        .header(axum::http::header::CONTENT_TYPE, "text/event-stream")
                        .body(Body::from_stream(chunks))
                        .unwrap()
                },
            ),
        )
        .with_state(hits.clone());
    let server = serve(app).await;
    let mut hop = chat_hop(HeaderMap::new(), "");
    hop.body = Bytes::from(format!(
        r#"{{"model":"{MODEL}","stream":true,"messages":[{{"role":"user","content":"hi"}}]}}"#
    ));
    hop.parsed =
        parse_client_request(ApiFormat::ChatCompletions, hop.body.clone()).expect("stream");
    let response = forward_for_test(
        dir.state(),
        hop,
        owned(&server.origin),
        ahead(Duration::from_secs(5)),
        Arc::new(AtomicU32::new(0)),
    )
    .await;
    assert_eq!(
        response
            .headers()
            .get(axum::http::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok()),
        Some("text/event-stream")
    );
    let (status, body) = text(response).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, "data: one\n\ndata: two\n\n");
    assert_eq!(hits.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn absolute_deadline_covers_headers_and_a_stalled_stream() {
    let dir = StateDir::open();
    let hits = Arc::new(AtomicU32::new(0));
    let app = Router::new()
        .fallback(|State(seen): State<Arc<AtomicU32>>| async move {
            seen.fetch_add(1, Ordering::SeqCst);
            tokio::time::sleep(Duration::from_secs(5)).await;
            Response::new(Body::from("late"))
        })
        .with_state(hits.clone());
    let server = serve(app).await;
    let cancels = Arc::new(AtomicU32::new(0));
    let (status, _) = text(
        tokio::time::timeout(
            Duration::from_secs(3),
            forward_for_test(
                dir.state(),
                chat_hop(HeaderMap::new(), ""),
                owned(&server.origin),
                ahead(Duration::from_millis(400)),
                cancels.clone(),
            ),
        )
        .await
        .expect("header deadline"),
    )
    .await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(hits.load(Ordering::SeqCst), 1);
    assert_eq!(cancels.load(Ordering::SeqCst), 1);

    let hits = Arc::new(AtomicU32::new(0));
    let app = Router::new()
        .route(
            "/v1/chat/completions",
            post(|State(seen): State<Arc<AtomicU32>>| async move {
                seen.fetch_add(1, Ordering::SeqCst);
                let tail = futures_util::stream::once(async {
                    tokio::time::sleep(Duration::from_secs(8)).await;
                    Ok::<Bytes, std::io::Error>(Bytes::from("data: TAIL\n\n"))
                });
                let stream = futures_util::stream::once(async {
                    Ok::<Bytes, std::io::Error>(Bytes::from("data: head\n\n"))
                })
                .chain(tail);
                Response::builder()
                    .header(axum::http::header::CONTENT_TYPE, "text/event-stream")
                    .body(Body::from_stream(stream))
                    .unwrap()
            }),
        )
        .with_state(hits.clone());
    let server = serve(app).await;
    let cancels = Arc::new(AtomicU32::new(0));
    let response = tokio::time::timeout(
        Duration::from_secs(4),
        forward_for_test(
            dir.state(),
            chat_hop(HeaderMap::new(), ""),
            owned(&server.origin),
            ahead(Duration::from_millis(700)),
            cancels.clone(),
        ),
    )
    .await
    .expect("stream deadline");
    let collected = axum::body::to_bytes(response.into_body(), 64 * 1024).await;
    if let Ok(bytes) = &collected {
        let text = String::from_utf8_lossy(bytes);
        assert!(!text.contains("TAIL"), "{text}");
    }
    assert_eq!(hits.load(Ordering::SeqCst), 1);
    assert_eq!(cancels.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn expired_deadline_does_not_send() {
    let dir = StateDir::open();
    let (sink, app) = recording(StatusCode::CREATED, "application/json", r#"{"kept":true}"#);
    let server = serve(app).await;
    let cancels = Arc::new(AtomicU32::new(0));
    let (status, _) = text(
        forward_for_test(
            dir.state(),
            chat_hop(HeaderMap::new(), ""),
            owned(&server.origin),
            elapsed(),
            cancels.clone(),
        )
        .await,
    )
    .await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert!(sink.hits.lock().expect("hits").is_empty());
    assert_eq!(cancels.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn transport_failure_sends_one_request() {
    let dir = StateDir::open();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let origin = format!("http://{}", listener.local_addr().expect("addr"));
    let accepts = Arc::new(AtomicU32::new(0));
    let seen = accepts.clone();
    let background = tokio::spawn(async move {
        loop {
            let Ok((mut socket, _)) = listener.accept().await else {
                break;
            };
            seen.fetch_add(1, Ordering::SeqCst);
            let mut buf = [0_u8; 2048];
            let _ = tokio::time::timeout(Duration::from_secs(2), async {
                use tokio::io::AsyncReadExt;
                let _ = socket.read(&mut buf).await;
            })
            .await;
            drop(socket);
        }
    });
    let (status, body) = text(
        tokio::time::timeout(
            Duration::from_secs(3),
            forward_for_test(
                dir.state(),
                chat_hop(HeaderMap::new(), ""),
                owned(&origin),
                ahead(Duration::from_secs(2)),
                Arc::new(AtomicU32::new(0)),
            ),
        )
        .await
        .expect("transport"),
    )
    .await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert!(!body.contains(SECRET));
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(accepts.load(Ordering::SeqCst), 1);
    background.abort();
}

#[tokio::test]
async fn dropping_the_hop_closes_the_connection_and_cancels_correlation() {
    let dir = StateDir::open();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let origin = format!("http://{}", listener.local_addr().expect("addr"));
    let closed = Arc::new(AtomicBool::new(false));
    let done = closed.clone();
    let accepted = Arc::new(AtomicBool::new(false));
    let started = accepted.clone();
    let background = tokio::spawn(async move {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let Ok((mut socket, _)) = listener.accept().await else {
            return;
        };
        started.store(true, Ordering::SeqCst);
        let mut buf = [0_u8; 4096];
        let _ = tokio::time::timeout(Duration::from_secs(2), socket.read(&mut buf)).await;
        let _ = socket
            .write_all(
                b"HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ntransfer-encoding: chunked\r\n\r\n5\r\nhello\r\n",
            )
            .await;
        loop {
            match socket.read(&mut buf).await {
                Ok(0) | Err(_) => {
                    done.store(true, Ordering::SeqCst);
                    break;
                }
                Ok(_) => {}
            }
        }
    });
    let cancels = Arc::new(AtomicU32::new(0));
    let state = dir.state();
    let task = tokio::spawn(forward_for_test(
        state,
        chat_hop(HeaderMap::new(), ""),
        owned(&origin),
        ahead(Duration::from_secs(30)),
        cancels.clone(),
    ));
    let became_accepted = tokio::time::timeout(Duration::from_secs(2), async {
        while !accepted.load(Ordering::SeqCst) {
            tokio::task::yield_now().await;
        }
    })
    .await;
    assert!(became_accepted.is_ok(), "fixture did not accept");
    task.abort();
    let _ = task.await;
    let became_closed = tokio::time::timeout(Duration::from_secs(2), async {
        while !closed.load(Ordering::SeqCst) {
            tokio::task::yield_now().await;
        }
    })
    .await;
    assert!(became_closed.is_ok(), "hop connection stayed open");
    assert_eq!(cancels.load(Ordering::SeqCst), 1);
    background.abort();
}

#[tokio::test]
async fn count_routes_do_not_call_generation() {
    let dir = StateDir::open();
    let sink = Sink::default();
    let recorded = sink.clone();
    let app = Router::new()
        .fallback(
            move |State(sink): State<Sink>, request: axum::extract::Request| async move {
                let path = request.uri().path().to_string();
                let count = path.ends_with("/count_tokens") || path.ends_with(":countTokens");
                let body = if count {
                    r#"{"input_tokens":7}"#
                } else {
                    r#"{"generated":true}"#
                };
                record(sink, request, StatusCode::OK, "application/json", body).await
            },
        )
        .with_state(recorded);
    let server = serve(app).await;
    let cancels = Arc::new(AtomicU32::new(0));

    let messages = Bytes::from(format!(
        r#"{{"model":"{MODEL}","messages":[{{"role":"user","content":"hi"}}]}}"#
    ));
    let parsed = parse_client_request(ApiFormat::Messages, messages.clone()).expect("messages");
    let (status, body) = text(
        forward_for_test(
            dir.state(),
            PublicHop {
                trace: RequestTrace::new(),
                headers: HeaderMap::new(),
                body: messages.clone(),
                parsed,
                client: scaffold_access(),
                path: PublicPath::MessagesCount,
                query: String::new(),
            },
            owned(&server.origin),
            ahead(Duration::from_secs(5)),
            cancels.clone(),
        )
        .await,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, r#"{"input_tokens":7}"#);

    let gemini = Bytes::from(GEMINI_BODY);
    let parsed = parse_gemini_request(MODEL.to_string(), false, gemini.clone()).expect("gemini");
    let (status, body) = text(
        forward_for_test(
            dir.state(),
            PublicHop {
                trace: RequestTrace::new(),
                headers: HeaderMap::new(),
                body: gemini.clone(),
                parsed,
                client: scaffold_access(),
                path: PublicPath::Gemini(format!("/v1beta/models/{MODEL}:countTokens")),
                query: "alt=sse".to_string(),
            },
            owned(&server.origin),
            ahead(Duration::from_secs(5)),
            cancels,
        )
        .await,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, r#"{"input_tokens":7}"#);
    let hits = sink.hits.lock().expect("hits");
    assert_eq!(hits.len(), 2);
    assert_eq!(hits[0].path, "/v1/messages/count_tokens");
    assert_eq!(hits[0].body, messages.as_ref());
    assert!(hits.iter().all(|hit| hit.path != "/v1/messages"));
    assert_eq!(hits[1].path, "/v1beta/models/hy4-preview:countTokens");
    assert_eq!(hits[1].query, "alt=sse");
    assert_eq!(hits[1].body, gemini.as_ref());
    assert!(!hits[1].body.windows(7).any(|window| window == b"\"model\""));
    assert!(hits.iter().all(|hit| !hit.path.contains("generateContent")));
    assert!(hits.iter().all(|hit| {
        !hit.body
            .windows(13)
            .any(|window| window == b"\"generated\"")
    }));
}

#[tokio::test]
async fn responses_store_false_uses_the_responses_path() {
    let dir = StateDir::open();
    let (sink, app) = recording(StatusCode::OK, "application/json", r#"{"id":"resp"}"#);
    let server = serve(app).await;
    let body = Bytes::from(format!(
        r#"{{"model":"{MODEL}","store":false,"input":"hi"}}"#
    ));
    let parsed = parse_client_request(ApiFormat::Responses, body.clone()).expect("responses");
    let (status, payload) = text(
        forward_for_test(
            dir.state(),
            PublicHop {
                trace: RequestTrace::new(),
                headers: HeaderMap::new(),
                body: body.clone(),
                parsed,
                client: scaffold_access(),
                path: PublicPath::Responses,
                query: String::new(),
            },
            owned(&server.origin),
            ahead(Duration::from_secs(5)),
            Arc::new(AtomicU32::new(0)),
        )
        .await,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(payload, r#"{"id":"resp"}"#);
    let hits = sink.hits.lock().expect("hits");
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].path, "/v1/responses");
    assert_eq!(hits[0].body, body.as_ref());
}

async fn assert_unauthorized(response: Response) {
    let (status, body) = text(response).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "{body}");
    assert!(body.contains("invalid gateway key"), "{body}");
    assert!(!body.contains("owned CPA child"), "{body}");
}

fn sub_key(id: &str, value: &str, enabled: bool) -> SubGatewayKey {
    SubGatewayKey {
        id: id.to_string(),
        name: id.to_string(),
        key: value.to_string(),
        enabled,
        deleted_at: None,
        created_at: chrono::Utc::now(),
    }
}

fn put_snapshot(state: &CoreState, value: &str, id: &str) {
    let mut snapshot = CredentialSnapshot::new();
    snapshot.insert(
        value.to_string(),
        CredentialEntry {
            id: id.to_string(),
            name: "client".to_string(),
        },
    );
    *state.credential_snapshot.write() = snapshot;
}

#[tokio::test]
async fn matched_fingerprint_survives_same_id_rotation() {
    const KEY_ID: &str = "44444444-4444-4444-8444-444444444444";
    const FIRST: &str = "matched-access-key-v1";
    const SECOND: &str = "rotated-access-key-v2";
    let dir = StateDir::open();
    put_snapshot(&dir.state(), FIRST, KEY_ID);
    let headers = bearer(FIRST);
    let captured = authenticate_client(&headers, &dir.state()).expect("matched key");
    assert_eq!(captured.key_id, KEY_ID);
    assert_eq!(captured.captured_key_fingerprint, fingerprint_key(FIRST));

    put_snapshot(&dir.state(), SECOND, KEY_ID);
    assert!(authenticate_client(&headers, &dir.state()).is_none());
    let rotated = authenticate_client(&bearer(SECOND), &dir.state()).expect("rotated value");
    assert_eq!(rotated.key_id, KEY_ID);
    assert_eq!(rotated.captured_key_fingerprint, fingerprint_key(SECOND));
    assert_ne!(
        captured.captured_key_fingerprint,
        rotated.captured_key_fingerprint
    );

    let (sink, app) = recording(StatusCode::CREATED, "application/json", r#"{"kept":true}"#);
    let server = serve(app).await;
    let intents = Arc::new(Mutex::new(Vec::<IntentRecord>::new()));
    let cancels = Arc::new(AtomicU32::new(0));
    let mut hop = chat_hop(headers, "");
    hop.client = captured;
    let (status, body) = text(
        forward_observing(
            dir.state(),
            hop,
            owned(&server.origin),
            ahead(Duration::from_secs(5)),
            cancels.clone(),
            intents.clone(),
        )
        .await,
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    assert_eq!(sink.hits.lock().expect("hits").len(), 1);
    assert_eq!(cancels.load(Ordering::SeqCst), 0);
    let recorded = intents.lock().expect("intents").clone();
    assert_eq!(recorded.len(), 1);
    assert!(!recorded[0].validated);
    assert_eq!(recorded[0].key_id, KEY_ID);
    assert_eq!(recorded[0].fingerprint, fingerprint_key(FIRST));
    assert_ne!(recorded[0].fingerprint, fingerprint_key(SECOND));
    assert_eq!(recorded[0].requested_model, MODEL);
}

#[tokio::test]
async fn unknown_deleted_and_disabled_keys_do_not_hop() {
    let dir = StateDir::open();
    let state = dir.state();
    assert_unauthorized(
        chat_completions(
            State(state.clone()),
            Extension(RequestTrace::new()),
            bearer("not-a-key"),
            Bytes::new(),
        )
        .await,
    )
    .await;
    assert_unauthorized(
        responses(
            State(state.clone()),
            Extension(RequestTrace::new()),
            bearer("not-a-key"),
            Bytes::new(),
        )
        .await,
    )
    .await;
    assert_unauthorized(
        messages(
            State(state.clone()),
            Extension(RequestTrace::new()),
            header_map(&[("x-api-key", "not-a-key")]),
            Bytes::new(),
        )
        .await,
    )
    .await;
    assert_unauthorized(
        messages_count_tokens(
            State(state.clone()),
            Extension(RequestTrace::new()),
            header_map(&[("x-goog-api-key", "not-a-key")]),
            Bytes::new(),
        )
        .await,
    )
    .await;
    let mut generate = RequestTrace::new();
    generate.path = format!("/v1beta/models/{MODEL}:generateContent");
    assert_unauthorized(
        gemini_model_action(
            State(state.clone()),
            Extension(generate),
            Path(format!("{MODEL}:generateContent")),
            bearer("not-a-key"),
            Bytes::from(GEMINI_BODY),
        )
        .await,
    )
    .await;
    let mut count = RequestTrace::new();
    count.path = format!("/v1beta/models/{MODEL}:countTokens");
    assert_unauthorized(
        gemini_model_action(
            State(state.clone()),
            Extension(count),
            Path(format!("{MODEL}:countTokens")),
            bearer("not-a-key"),
            Bytes::from(GEMINI_BODY),
        )
        .await,
    )
    .await;

    const DISABLED_ID: &str = "22222222-2222-4222-8222-222222222222";
    const DISABLED: &str = "ocg-disabled-value";
    KeyStore::insert_sub_gateway_key(&state, &sub_key(DISABLED_ID, DISABLED, true))
        .expect("enabled key");
    refresh_snapshot(&state);
    assert!(state.credential_snapshot.read().contains_key(DISABLED));
    let live = chat_completions(
        State(state.clone()),
        Extension(RequestTrace::new()),
        bearer(DISABLED),
        chat_body(),
    )
    .await;
    let (status, body) = text(live).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{body}");
    assert!(body.contains("owned CPA child is missing"), "{body}");
    assert!(KeyStore::set_sub_gateway_key_enabled(&state, DISABLED_ID, false).expect("disable"));
    refresh_snapshot(&state);
    assert!(!state.credential_snapshot.read().contains_key(DISABLED));
    assert!(authenticate_client(&bearer(DISABLED), &state).is_none());
    assert_unauthorized(
        chat_completions(
            State(state.clone()),
            Extension(RequestTrace::new()),
            bearer(DISABLED),
            chat_body(),
        )
        .await,
    )
    .await;

    const DELETED_ID: &str = "33333333-3333-4333-8333-333333333333";
    const DELETED: &str = "ocg-deleted-value";
    KeyStore::insert_sub_gateway_key(&state, &sub_key(DELETED_ID, DELETED, true))
        .expect("live key");
    refresh_snapshot(&state);
    assert!(state.credential_snapshot.read().contains_key(DELETED));
    assert!(
        KeyStore::soft_delete_sub_gateway_key(&state, DELETED_ID, chrono::Utc::now())
            .expect("delete")
    );
    refresh_snapshot(&state);
    assert!(!state.credential_snapshot.read().contains_key(DELETED));
    assert!(authenticate_client(&bearer(DELETED), &state).is_none());
    assert_unauthorized(
        messages(
            State(state),
            Extension(RequestTrace::new()),
            header_map(&[("x-api-key", DELETED)]),
            chat_body(),
        )
        .await,
    )
    .await;
}

#[tokio::test]
async fn injected_private_headers_stay_client_authorization() {
    const KEY_ID: &str = "55555555-5555-4555-8555-555555555555";
    const VALUE: &str = "matched-access-key-v1";
    let dir = StateDir::open();
    put_snapshot(&dir.state(), VALUE, KEY_ID);
    let mut headers = bearer(VALUE);
    headers.insert(
        "pinned_auth_id",
        HeaderValue::from_static("pin-from-client"),
    );
    headers.insert("x-ocg-kind", HeaderValue::from_static("validated"));
    headers.insert("x-ocg-protocol", HeaderValue::from_static("messages"));
    headers.insert(
        "x-ocg-requested-protocol",
        HeaderValue::from_static("messages"),
    );
    headers.insert(
        "x-ocg-validation-kind",
        HeaderValue::from_static("validated"),
    );
    headers.insert("kind", HeaderValue::from_static("keep-kind"));
    headers.insert("x-client-trace", HeaderValue::from_static("keep-me"));
    let captured = authenticate_client(&headers, &dir.state()).expect("client match");
    assert_eq!(captured.key_id, KEY_ID);
    assert_ne!(captured.key_id, "pin-from-client");
    assert_eq!(captured.captured_key_fingerprint, fingerprint_key(VALUE));

    let query = [
        "alt=sse",
        "key=leak-key",
        "auth_token=leak-token",
        "api_key=leak-api-key",
        "api-key=leak-api-dash",
        "x-api-key=leak-x-api",
        "x-goog-api-key=leak-goog",
        "pinned_auth_id=pin-from-client",
        "pinned-auth-id=pin-dash",
        "keyboard=1",
    ]
    .join("&");
    let mut hop = chat_hop(headers, &query);
    hop.client = captured.clone();
    let (sink, app) = recording(StatusCode::CREATED, "application/json", r#"{"kept":true}"#);
    let server = serve(app).await;
    let intents = Arc::new(Mutex::new(Vec::<IntentRecord>::new()));
    let (status, body) = text(
        forward_observing(
            dir.state(),
            hop,
            owned(&server.origin),
            ahead(Duration::from_secs(5)),
            Arc::new(AtomicU32::new(0)),
            intents.clone(),
        )
        .await,
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let hits = sink.hits.lock().expect("hits");
    assert_eq!(hits.len(), 1);
    let hit = &hits[0];
    assert_eq!(hit.query, "alt=sse&keyboard=1");
    for leaked in [
        "leak-key",
        "leak-token",
        "leak-api-key",
        "leak-api-dash",
        "leak-x-api",
        "leak-goog",
        "pin-from-client",
        "pin-dash",
        VALUE,
    ] {
        assert!(!hit.query.contains(leaked), "{leaked} in {}", hit.query);
    }
    assert_eq!(
        hit_header(hit, "authorization"),
        Some("Bearer hop-fixture-secret")
    );
    assert_eq!(hit_header(hit, "kind"), Some("keep-kind"));
    assert_eq!(hit_header(hit, "x-client-trace"), Some("keep-me"));
    for name in [
        "pinned_auth_id",
        "x-ocg-kind",
        "x-ocg-protocol",
        "x-ocg-requested-protocol",
        "x-ocg-validation-kind",
        "x-api-key",
        "x-goog-api-key",
    ] {
        assert!(hit_header(hit, name).is_none(), "{name}");
    }
    assert!(hit.headers.iter().all(|(name, _)| {
        let name = name.to_ascii_lowercase();
        !name.contains("pinned_auth")
            && !name.contains("pinned-auth")
            && (!name.starts_with("x-ocg-")
                || matches!(
                    name.as_str(),
                    "x-ocg-request-id"
                        | "x-ocg-process-generation"
                        | "x-ocg-projection-revision"
                        | "x-ocg-request-deadline"
                ))
    }));
    let recorded = intents.lock().expect("intents").clone();
    assert_eq!(recorded.len(), 1);
    assert!(!recorded[0].validated);
    assert_eq!(recorded[0].key_id, KEY_ID);
    assert_eq!(recorded[0].fingerprint, captured.captured_key_fingerprint);
    assert_eq!(recorded[0].requested_model, MODEL);
}

fn fixed_deadline(wall: &str) -> CapturedDeadline {
    CapturedDeadline {
        wall: chrono::DateTime::parse_from_rfc3339(wall)
            .expect("deadline")
            .with_timezone(&chrono::Utc),
        mono: tokio::time::Instant::now() + Duration::from_secs(30),
    }
}

fn rfc3339_nanos(deadline: chrono::DateTime<chrono::Utc>) -> String {
    deadline.to_rfc3339_opts(chrono::SecondsFormat::Nanos, true)
}

#[tokio::test]
async fn generated_deadline_matches_the_registered_capture() {
    const NANOS: &str = "2030-01-02T03:04:05.123456789Z";
    const ZERO: &str = "2030-01-02T03:04:05.000000000Z";
    const LONGER: &str = "2099-12-31T23:59:59.999999999Z";
    const EXPIRED: &str = "2000-01-01T00:00:00.000000000Z";
    const KEY_ID: &str = "66666666-6666-4666-8666-666666666666";
    const VALUE: &str = "deadline-access-key-v1";
    let dir = StateDir::open();
    put_snapshot(&dir.state(), VALUE, KEY_ID);
    let (sink, app) = recording(StatusCode::CREATED, "application/json", r#"{"kept":true}"#);
    let server = serve(app).await;
    let intents = Arc::new(Mutex::new(Vec::<IntentRecord>::new()));
    let cases = [(NANOS, LONGER), (ZERO, EXPIRED)];
    for (index, (captured, foreign)) in cases.into_iter().enumerate() {
        let mut headers = bearer(VALUE);
        headers.insert(
            "x-ocg-request-deadline",
            HeaderValue::from_str(foreign).expect("foreign deadline"),
        );
        headers.insert(
            "pinned_auth_id",
            HeaderValue::from_static("pin-from-client"),
        );
        headers.insert("x-ocg-request-kind", HeaderValue::from_static("validated"));
        headers.insert(
            "x-ocg-pinned-auth-id",
            HeaderValue::from_static("pin-from-client"),
        );
        headers.insert(
            "x-ocg-validated-protocol",
            HeaderValue::from_static("chat_completions"),
        );
        headers.insert("x-api-key", HeaderValue::from_static(VALUE));
        let client = authenticate_client(&headers, &dir.state()).expect("client match");
        let query = format!("key={VALUE}&alt=sse");
        let mut hop = chat_hop(headers, &query);
        hop.client = client.clone();
        let cancels = Arc::new(AtomicU32::new(0));
        let (status, body) = text(
            forward_observing(
                dir.state(),
                hop,
                owned(&server.origin),
                fixed_deadline(captured),
                cancels.clone(),
                intents.clone(),
            )
            .await,
        )
        .await;
        assert_eq!(status, StatusCode::CREATED, "{body}");
        assert_eq!(cancels.load(Ordering::SeqCst), 0);
        let hits = sink.hits.lock().expect("hits");
        assert_eq!(hits.len(), index + 1);
        let hit = &hits[index];
        assert_eq!(hit.path, "/v1/chat/completions");
        assert_eq!(hit.query, "alt=sse");
        assert!(!hit.query.contains(VALUE));
        let deadlines: Vec<_> = hit
            .headers
            .iter()
            .filter(|(name, _)| name.eq_ignore_ascii_case("x-ocg-request-deadline"))
            .map(|(_, value)| value.as_str())
            .collect();
        let recorded = intents.lock().expect("intents");
        let intent = &recorded[index];
        let registered = rfc3339_nanos(intent.deadline);
        assert_eq!(deadlines.len(), 1);
        assert_eq!(deadlines[0], registered.as_str());
        assert_eq!(registered, captured);
        assert_ne!(registered, foreign);
        assert_eq!(
            intent.deadline,
            chrono::DateTime::parse_from_rfc3339(captured)
                .expect("captured")
                .with_timezone(&chrono::Utc)
        );
        assert!(!intent.validated);
        assert_eq!(intent.key_id, KEY_ID);
        assert_eq!(intent.fingerprint, client.captured_key_fingerprint);
        assert_eq!(intent.requested_model, MODEL);
        assert_eq!(
            hit_header(hit, "authorization"),
            Some("Bearer hop-fixture-secret")
        );
        for name in [
            "pinned_auth_id",
            "x-ocg-request-kind",
            "x-ocg-pinned-auth-id",
            "x-ocg-validated-protocol",
            "x-api-key",
        ] {
            assert!(hit_header(hit, name).is_none(), "{name}");
        }
        for (name, value) in &hit.headers {
            assert_ne!(value, foreign, "{name}");
            assert!(!value.contains(VALUE), "{name}");
            assert!(!value.contains("pin-from-client"), "{name}");
            assert!(!value.contains("chat_completions"), "{name}");
        }
        let payload = String::from_utf8_lossy(&hit.body);
        assert!(!payload.contains(VALUE));
        assert!(!payload.contains(foreign));
    }
}

#[tokio::test]
async fn invalid_public_trace_cancels_before_the_hop() {
    let dir = StateDir::open();
    let (sink, app) = recording(StatusCode::CREATED, "application/json", r#"{"kept":true}"#);
    let server = serve(app).await;
    let intents = Arc::new(Mutex::new(Vec::<IntentRecord>::new()));
    let cancels = Arc::new(AtomicU32::new(0));
    let mut hop = chat_hop(HeaderMap::new(), "");
    hop.trace.request_id = "not-a-public-trace".to_string();
    let (status, body) = text(
        forward_observing(
            dir.state(),
            hop,
            owned(&server.origin),
            ahead(Duration::from_secs(5)),
            cancels.clone(),
            intents.clone(),
        )
        .await,
    )
    .await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{body}");
    assert!(body.contains("correlation_trace"), "{body}");
    assert!(sink.hits.lock().expect("hits").is_empty());
    let recorded = intents.lock().expect("intents");
    assert_eq!(recorded.len(), 1);
    assert!(!recorded[0].validated);
    assert_eq!(recorded[0].requested_model, MODEL);
    assert_eq!(
        crate::cpa_execution::correlation_cancelled(&dir.state(), recorded[0].request_id),
        Some(true)
    );
    assert_eq!(
        crate::cpa_execution::correlation_authorization_kind(&dir.state(), recorded[0].request_id),
        Some("client")
    );
    assert!(
        crate::cpa_execution::correlation_client_trace(&dir.state(), recorded[0].request_id)
            .is_none()
    );
    assert_eq!(cancels.load(Ordering::SeqCst), 1);
}
