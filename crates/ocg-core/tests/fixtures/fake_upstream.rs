//! Deterministic loopback upstream for gateway integration tests.
//!
//! Keep this fixture protocol-neutral: client-format tests choose the route and
//! body they need, while the fixture records the outbound request verbatim.
//! In particular, `x_goog_api_key` is deliberately captured so Zen Free tests
//! can assert that no account credential is sent upstream.

use axum::Router;
use axum::body::Body;
use axum::extract::{OriginalUri, State};
use axum::http::{HeaderMap, Method, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::any;
use std::collections::{HashMap, VecDeque};
use std::convert::Infallible;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

#[derive(Clone)]
pub(crate) struct FakeReply {
    pub status: u16,
    pub body: &'static str,
}

#[derive(Clone, Debug)]
pub(crate) struct FakeCall {
    pub key: String,
    pub method: Method,
    pub path: String,
    pub authorization: Option<String>,
    pub x_api_key: Option<String>,
    pub x_goog_api_key: Option<String>,
    pub anthropic_version: Option<String>,
    pub body: String,
    pub accept_encoding: Option<String>,
    pub conversation_header: Option<String>,
    pub opencode_session: Option<String>,
    pub opencode_client: Option<String>,
    pub opencode_request: Option<String>,
    pub opencode_project: Option<String>,
    pub session_id: Option<String>,
    pub session_affinity: Option<String>,
    pub user_agent: Option<String>,
    /// Verbatim inbound `Cookie` header; tests assert inference egress never
    /// carries one. Not every suite reads it, hence the allow.
    #[allow(dead_code)]
    pub cookie: Option<String>,
}

type Replies = Arc<Mutex<HashMap<String, VecDeque<FakeReply>>>>;
pub(crate) type FakeCalls = Arc<Mutex<Vec<FakeCall>>>;
pub(crate) type DelayedChunks = Vec<(Duration, &'static str)>;
pub(crate) type DelayedResponses = Arc<Mutex<VecDeque<DelayedChunks>>>;

/// One inbound HTTP hit on a journaled loopback listener.
///
/// `seq` is a process-local monotonic index shared by every listener attached
/// to the same [`SharedJournal`], so arrival order is not reconstructed from
/// concatenated per-server counts.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Arrival {
    pub seq: u64,
    pub listener: String,
    pub elapsed_ms: u128,
    pub key: String,
    pub method: Method,
    pub path: String,
    pub authorization: Option<String>,
    pub x_api_key: Option<String>,
    pub anthropic_version: Option<String>,
    pub body: String,
}

/// Chronological journal shared by independent loopback upstreams.
#[derive(Clone)]
pub(crate) struct SharedJournal {
    inner: Arc<Mutex<Vec<Arrival>>>,
    start: Instant,
}

impl SharedJournal {
    pub(crate) fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(Vec::new())),
            start: Instant::now(),
        }
    }

    pub(crate) fn record(
        &self,
        listener: &str,
        key: String,
        method: Method,
        path: String,
        authorization: Option<String>,
        x_api_key: Option<String>,
        anthropic_version: Option<String>,
        body: String,
    ) {
        let mut arrivals = self.inner.lock().expect("arrival journal lock");
        let seq = arrivals.len() as u64;
        arrivals.push(Arrival {
            seq,
            listener: listener.to_owned(),
            elapsed_ms: self.start.elapsed().as_millis(),
            key,
            method,
            path,
            authorization,
            x_api_key,
            anthropic_version,
            body,
        });
    }

    pub(crate) fn snapshot(&self) -> Vec<Arrival> {
        self.inner.lock().expect("arrival journal lock").clone()
    }

    pub(crate) fn listeners(&self) -> Vec<String> {
        self.snapshot()
            .into_iter()
            .map(|arrival| arrival.listener)
            .collect()
    }

    pub(crate) fn keys(&self) -> Vec<String> {
        self.snapshot()
            .into_iter()
            .map(|arrival| arrival.key)
            .collect()
    }

    pub(crate) fn paths(&self) -> Vec<String> {
        self.snapshot()
            .into_iter()
            .map(|arrival| arrival.path)
            .collect()
    }
}

#[derive(Clone)]
struct FakeState {
    replies: Replies,
    calls: FakeCalls,
    delay: Duration,
    journal: Option<SharedJournal>,
    listener: String,
}

#[derive(Clone)]
struct DelayedState {
    status: StatusCode,
    content_type: &'static str,
    responses: DelayedResponses,
    calls: Arc<AtomicUsize>,
}

/// Start a loopback-only upstream accepting every client/upstream route.
///
/// Replies are selected by Bearer, `x-api-key`, then `x-goog-api-key`, and
/// repeated once their queue is exhausted. An unexpected credential receives a
/// deterministic 500 response instead of making a real network request.
pub(crate) async fn start_fake_upstream(
    replies: HashMap<String, VecDeque<FakeReply>>,
) -> (String, FakeCalls, tokio::sync::oneshot::Sender<()>) {
    start_fake_upstream_with_delay(replies, Duration::ZERO).await
}

pub(crate) async fn start_fake_upstream_with_delay(
    replies: HashMap<String, VecDeque<FakeReply>>,
    delay: Duration,
) -> (String, FakeCalls, tokio::sync::oneshot::Sender<()>) {
    start_fake_upstream_inner(replies, delay, None, String::new()).await
}

/// Start a loopback upstream that appends every hit to `journal` under `label`.
///
/// Independent listeners share one journal so tests can assert actual arrival
/// order across distinct TCP endpoints.
pub(crate) async fn start_fake_upstream_on_journal(
    label: impl Into<String>,
    replies: HashMap<String, VecDeque<FakeReply>>,
    journal: SharedJournal,
) -> (String, FakeCalls, tokio::sync::oneshot::Sender<()>) {
    start_fake_upstream_inner(replies, Duration::ZERO, Some(journal), label.into()).await
}

async fn start_fake_upstream_inner(
    replies: HashMap<String, VecDeque<FakeReply>>,
    delay: Duration,
    journal: Option<SharedJournal>,
    listener: String,
) -> (String, FakeCalls, tokio::sync::oneshot::Sender<()>) {
    let calls = Arc::new(Mutex::new(Vec::new()));
    let app = Router::new()
        .fallback(any(fake_reply))
        .layer(axum::extract::DefaultBodyLimit::max(64 * 1024 * 1024))
        .with_state(FakeState {
            replies: Arc::new(Mutex::new(replies)),
            calls: calls.clone(),
            delay,
            journal,
            listener,
        });
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .expect("fake upstream loopback listener should bind");
    let address = listener
        .local_addr()
        .expect("fake upstream listener should have an address");
    let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel();
    tokio::spawn(async move {
        let server = axum::serve(listener, app).with_graceful_shutdown(async move {
            let _ = shutdown_rx.await;
        });
        let _ = server.await;
    });
    (format!("http://{address}"), calls, shutdown_tx)
}

/// Serve one or more deliberately chunked responses over every local route.
/// This models SSE usage chunks and timeout boundaries without sleeping in
/// production code or contacting a provider.
pub(crate) async fn start_delayed_fake_upstream(
    status: StatusCode,
    content_type: &'static str,
    responses: Vec<DelayedChunks>,
) -> (String, Arc<AtomicUsize>, tokio::sync::oneshot::Sender<()>) {
    assert!(
        !responses.is_empty(),
        "fake delayed response sequence is required"
    );
    let calls = Arc::new(AtomicUsize::new(0));
    let app = Router::new()
        .fallback(any(delayed_reply))
        .with_state(DelayedState {
            status,
            content_type,
            responses: Arc::new(Mutex::new(VecDeque::from(responses))),
            calls: calls.clone(),
        });
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .expect("fake delayed upstream loopback listener should bind");
    let address = listener
        .local_addr()
        .expect("fake delayed upstream listener should have an address");
    let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel();
    tokio::spawn(async move {
        let server = axum::serve(listener, app).with_graceful_shutdown(async move {
            let _ = shutdown_rx.await;
        });
        let _ = server.await;
    });
    (format!("http://{address}"), calls, shutdown_tx)
}

/// Write an incomplete raw HTTP response and close the socket immediately.
/// Callers can place visible SSE output before the close to exercise both sides
/// of the gateway's downstream-output retry boundary.
pub(crate) async fn start_raw_disconnect_upstream(
    response: Vec<u8>,
) -> (String, Arc<AtomicUsize>, tokio::sync::oneshot::Sender<()>) {
    let calls = Arc::new(AtomicUsize::new(0));
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .expect("fake disconnect upstream loopback listener should bind");
    let address = listener
        .local_addr()
        .expect("fake disconnect upstream listener should have an address");
    let (shutdown_tx, mut shutdown_rx) = tokio::sync::oneshot::channel();
    let calls_for_server = calls.clone();
    tokio::spawn(async move {
        loop {
            tokio::select! {
                _ = &mut shutdown_rx => break,
                accepted = listener.accept() => {
                    let Ok((mut socket, _)) = accepted else { break };
                    calls_for_server.fetch_add(1, Ordering::Relaxed);
                    let mut request = vec![0_u8; 16 * 1024];
                    let _ = socket.read(&mut request).await;
                    let _ = socket.write_all(&response).await;
                    let _ = socket.shutdown().await;
                }
            }
        }
    });
    (format!("http://{address}"), calls, shutdown_tx)
}

async fn fake_reply(
    State(state): State<FakeState>,
    OriginalUri(uri): OriginalUri,
    method: Method,
    headers: HeaderMap,
    body: String,
) -> impl IntoResponse {
    let authorization = header(&headers, axum::http::header::AUTHORIZATION);
    let x_api_key = header(&headers, "x-api-key");
    let x_goog_api_key = header(&headers, "x-goog-api-key");
    let key = authorization
        .as_deref()
        .and_then(|value| value.strip_prefix("Bearer "))
        .or(x_api_key.as_deref())
        .or(x_goog_api_key.as_deref())
        .unwrap_or_default()
        .to_owned();

    let path = uri.path().to_owned();
    let anthropic_version = header(&headers, "anthropic-version");
    if let Some(journal) = &state.journal {
        journal.record(
            &state.listener,
            key.clone(),
            method.clone(),
            path.clone(),
            authorization.clone(),
            x_api_key.clone(),
            anthropic_version.clone(),
            body.clone(),
        );
    }
    state
        .calls
        .lock()
        .expect("fake call log lock")
        .push(FakeCall {
            key: key.clone(),
            method,
            path,
            authorization,
            x_api_key,
            x_goog_api_key,
            anthropic_version,
            body,
            accept_encoding: header(&headers, axum::http::header::ACCEPT_ENCODING),
            conversation_header: header(&headers, "x-ocg-conversation-id"),
            opencode_session: header(&headers, "x-opencode-session"),
            opencode_client: header(&headers, "x-opencode-client"),
            opencode_request: header(&headers, "x-opencode-request"),
            opencode_project: header(&headers, "x-opencode-project"),
            session_id: header(&headers, "x-session-id"),
            session_affinity: header(&headers, "x-session-affinity"),
            user_agent: header(&headers, axum::http::header::USER_AGENT),
            cookie: header(&headers, "cookie"),
        });

    if !state.delay.is_zero() {
        tokio::time::sleep(state.delay).await;
    }

    let reply = {
        let mut replies = state.replies.lock().expect("fake reply queue lock");
        let queue = replies.entry(key).or_insert_with(|| {
            VecDeque::from([FakeReply {
                status: StatusCode::INTERNAL_SERVER_ERROR.as_u16(),
                body: r#"{"error":"unexpected fake upstream credential"}"#,
            }])
        });
        if queue.len() > 1 {
            queue.pop_front().expect("non-empty fake reply queue")
        } else {
            queue.front().expect("non-empty fake reply queue").clone()
        }
    };
    let content_type = if reply.body.starts_with("data:") || reply.body.starts_with("event:") {
        "text/event-stream"
    } else {
        "application/json"
    };
    (
        StatusCode::from_u16(reply.status).expect("valid fake upstream status"),
        [("content-type", content_type)],
        reply.body,
    )
}

async fn delayed_reply(State(state): State<DelayedState>) -> Response {
    state.calls.fetch_add(1, Ordering::Relaxed);
    let chunks = {
        let mut responses = state.responses.lock().expect("fake delayed response lock");
        if responses.len() > 1 {
            responses
                .pop_front()
                .expect("non-empty delayed response queue")
        } else {
            responses
                .front()
                .expect("non-empty delayed response queue")
                .clone()
        }
    };
    let stream = futures_util::stream::unfold(VecDeque::from(chunks), |mut chunks| async move {
        let (delay, chunk) = chunks.pop_front()?;
        tokio::time::sleep(delay).await;
        Some((
            Ok::<_, Infallible>(bytes::Bytes::from_static(chunk.as_bytes())),
            chunks,
        ))
    });
    Response::builder()
        .status(state.status)
        .header("content-type", state.content_type)
        .body(Body::from_stream(stream))
        .expect("fake delayed response should build")
}

fn header(headers: &HeaderMap, name: impl axum::http::header::AsHeaderName) -> Option<String> {
    headers
        .get(name)
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned)
}
