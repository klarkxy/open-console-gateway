//! Private policy callback. Unknown admit and result requests fail closed.
//!
//! Capture uses the strict `cpa_policy::parse_request` decoder. SQLite work
//! inside the two-second budget cannot be cancelled; a timed-out task may
//! still commit, and this process then stays unavailable until a later ready
//! succeeds.

use super::identity::{CapturedAttempt, FenceMode, LiveFacts, PinnedAttempt};
use crate::cpa_policy::{
    MAX_REQUEST_BYTES, PolicyDocument, PolicyFault, PolicyService, PolicyStore, Reason,
    RequestKind, SETTINGS_KEY, authorize_token, parse_request, read_transaction, transact_settings,
};
use crate::state::{CoreState, CoreStateInner};
use axum::body::Body;
use axum::extract::{ConnectInfo, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use bytes::Bytes;
use chrono::{DateTime, Utc};
use rusqlite::Transaction;
use std::collections::{HashMap, HashSet};
use std::net::SocketAddr;
use std::sync::{Arc, Weak};
use std::time::Duration;
use uuid::Uuid;

pub(super) struct SqlitePolicyStore {
    state: Weak<CoreStateInner>,
}

impl SqlitePolicyStore {
    pub(super) fn new(state: &CoreState) -> Self {
        Self {
            state: Arc::downgrade(state),
        }
    }
}

impl PolicyStore for SqlitePolicyStore {
    fn read(
        &self,
        read: &mut dyn for<'tx> FnMut(
            &Transaction<'tx>,
            &PolicyDocument,
        ) -> Result<(), PolicyFault>,
    ) -> Result<(), PolicyFault> {
        let state = self.state.upgrade().ok_or(PolicyFault::Unavailable)?;
        let mut db = state.db.lock();
        read_transaction(&mut db.conn, SETTINGS_KEY, read)
    }

    fn update(
        &self,
        mutate: &mut dyn for<'tx> FnMut(
            &Transaction<'tx>,
            &mut PolicyDocument,
        ) -> Result<(), PolicyFault>,
    ) -> Result<(), PolicyFault> {
        let state = self.state.upgrade().ok_or(PolicyFault::Unavailable)?;
        let mut db = state.db.lock();
        transact_settings(&mut db.conn, SETTINGS_KEY, mutate)
    }
}

pub(super) type LivePolicy = PolicyService<SqlitePolicyStore>;

pub async fn policy_callback(
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    State(state): State<CoreState>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    if !peer.ip().is_loopback() {
        return json_response(StatusCode::FORBIDDEN, FORBIDDEN);
    }
    let Some(context) = state.cpa_execution.callback_context() else {
        return json_response(StatusCode::UNAUTHORIZED, UNAUTHORIZED);
    };
    let provided = bearer(headers.get(header::AUTHORIZATION));
    if !authorize_token(context.token.as_bytes(), provided.as_bytes()) {
        return json_response(StatusCode::UNAUTHORIZED, UNAUTHORIZED);
    }
    let origin = headers
        .get(header::ORIGIN)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("");
    if origin.is_empty() || origin != context.origin {
        return json_response(StatusCode::FORBIDDEN, FORBIDDEN);
    }
    if body.len() > MAX_REQUEST_BYTES {
        return json_response(StatusCode::PAYLOAD_TOO_LARGE, OVERSIZE);
    }
    let now = Utc::now();
    let captured = capture_request(&body);
    let mut tracked: Option<(Uuid, AttemptOperation, Uuid)> = None;
    let observation_fault = Arc::new(parking_lot::Mutex::new(None));
    let facts = match &captured {
        Ok(Captured::Ready(ready)) => LiveFacts {
            mode: FenceMode::Ready,
            attempt: None,
            ready: Some(ready.clone()),
            deadline: now + chrono::Duration::seconds(60),
            now,
            cipher: Arc::clone(&state.cipher),
            pin: Arc::new(parking_lot::Mutex::new(HashMap::new())),
            gate_intent: false,
            authorization: None,
            requested_model: String::new(),
            client_trace_id: None,
            observation_fault: Arc::clone(&observation_fault),
            restriction_authority: true,
        },
        Ok(Captured::Attempt {
            request_id,
            operation,
            attempt,
        }) => {
            let Some(correlation) = state.cpa_execution.correlation_for(*request_id) else {
                return json_response(StatusCode::OK, MALFORMED);
            };
            if correlation.cancelled && *operation == AttemptOperation::Admit {
                return json_response(StatusCode::OK, CANCELLED);
            }
            if *operation == AttemptOperation::Result
                && correlation.published.lock().contains(&attempt.attempt_id)
            {
                return json_response(StatusCode::OK, UNCORRELATED);
            }
            tracked = Some((*request_id, *operation, attempt.attempt_id));
            LiveFacts {
                mode: FenceMode::Applied,
                attempt: Some(attempt.clone()),
                ready: None,
                deadline: correlation.deadline,
                now,
                cipher: Arc::clone(&state.cipher),
                pin: Arc::clone(&correlation.pins),
                gate_intent: *operation == AttemptOperation::Admit,
                authorization: correlation.authorization.clone(),
                requested_model: correlation.requested_model.clone(),
                client_trace_id: correlation.client_trace_id.clone(),
                observation_fault: Arc::clone(&observation_fault),
                restriction_authority: true,
            }
        }
        Err(_) => LiveFacts {
            mode: FenceMode::Applied,
            attempt: None,
            ready: None,
            deadline: now,
            now,
            cipher: Arc::clone(&state.cipher),
            pin: Arc::new(parking_lot::Mutex::new(HashMap::new())),
            gate_intent: false,
            authorization: None,
            requested_model: String::new(),
            client_trace_id: None,
            observation_fault: Arc::clone(&observation_fault),
            restriction_authority: true,
        },
    };
    let service = context.service;
    let token = context.token;
    let body = body.to_vec();
    let joined = tokio::time::timeout(
        Duration::from_secs(2),
        tokio::task::spawn_blocking(move || {
            let mut facts = facts;
            service.handle(&token, &body, &mut facts)
        }),
    )
    .await;
    let reply = match joined {
        Ok(Ok(reply)) => {
            if let Some(message) = *observation_fault.lock() {
                state.log_runtime_event("error", "cpa", message);
            }
            reply
        }
        Ok(Err(_)) | Err(_) => {
            super::note_unavailable(&state);
            return json_response(StatusCode::OK, UNAVAILABLE);
        }
    };
    if reply
        .decision
        .as_ref()
        .is_some_and(|decision| decision.unavailable)
    {
        super::note_unavailable(&state);
    }
    if let Some((request_id, operation, attempt_id)) = tracked {
        if let Some(decision) = reply.decision.clone() {
            if operation == AttemptOperation::Result && consumes_attempt(decision.reason) {
                state.cpa_execution.note_published(request_id, attempt_id);
            }
            state.cpa_execution.remember_decision(request_id, decision);
        }
    }
    let status = StatusCode::from_u16(reply.status).unwrap_or(StatusCode::OK);
    json_bytes(status, reply.bytes)
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum AttemptOperation {
    Admit,
    Result,
}

#[derive(Clone)]
enum Captured {
    Ready(super::identity::ReadyIdentity),
    Attempt {
        request_id: Uuid,
        operation: AttemptOperation,
        attempt: CapturedAttempt,
    },
}

fn capture_request(body: &[u8]) -> Result<Captured, ()> {
    let request = parse_request(body).map_err(|_| ())?;
    let operation = match &request.kind {
        RequestKind::Ready => {
            return Ok(Captured::Ready(super::identity::ReadyIdentity {
                generation: request.projection.process_generation,
                revision: request.projection.projection_revision,
                digest: hex::encode(request.projection.projection_digest),
            }));
        }
        RequestKind::Admit => AttemptOperation::Admit,
        RequestKind::Result(_) => AttemptOperation::Result,
    };
    let attempt = request.attempt.ok_or(())?;
    Ok(Captured::Attempt {
        request_id: attempt.request_id,
        operation,
        attempt: CapturedAttempt {
            request_id: attempt.request_id,
            attempt_id: attempt.attempt_id,
            auth_id: attempt.auth_id,
            credential_id: attempt.credential_id,
            credential_version: attempt.credential_version,
            provider_id: attempt.provider_id,
            public_model: attempt.public_model,
            upstream_model: attempt.upstream_model,
            registration_epoch: attempt.registration_epoch,
            material_revision: attempt.material_revision,
            kind: attempt.kind,
            callable_protocol: attempt.callable_protocol.unwrap_or_default(),
            generation_kind: attempt.generation_kind.unwrap_or_default(),
        },
    })
}

fn consumes_attempt(reason: Reason) -> bool {
    crate::cpa_policy::result_consumes_attempt(reason)
}

fn bearer(value: Option<&header::HeaderValue>) -> String {
    let Some(value) = value.and_then(|value| value.to_str().ok()) else {
        return String::new();
    };
    value
        .strip_prefix("Bearer ")
        .or_else(|| value.strip_prefix("bearer "))
        .unwrap_or("")
        .trim()
        .to_string()
}

fn json_response(status: StatusCode, body: &'static str) -> Response {
    json_bytes(status, body.as_bytes().to_vec())
}

fn json_bytes(status: StatusCode, bytes: Vec<u8>) -> Response {
    let bytes = if bytes.len() > 16 * 1024 {
        UNAVAILABLE.as_bytes().to_vec()
    } else {
        bytes
    };
    #[cfg(test)]
    LAST_CALLBACK.with(|cell| {
        *cell.borrow_mut() = Some((
            status.as_u16(),
            String::from_utf8_lossy(&bytes).into_owned(),
        ));
    });
    Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::CACHE_CONTROL, "no-store")
        .body(Body::from(bytes))
        .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())
}

const FORBIDDEN: &str = r#"{"protocolVersion":1,"action":"stop","reason":"unauthorized","unavailable":false,"endpointPins":null}"#;
const UNAUTHORIZED: &str = r#"{"protocolVersion":1,"action":"stop","reason":"unauthorized","unavailable":false,"endpointPins":null}"#;
const OVERSIZE: &str = r#"{"protocolVersion":1,"action":"stop","reason":"oversize","unavailable":false,"endpointPins":null}"#;
const MALFORMED: &str = r#"{"protocolVersion":1,"action":"stop","reason":"malformed","unavailable":false,"endpointPins":null}"#;
const UNAVAILABLE: &str = r#"{"protocolVersion":1,"action":"stop","reason":"unavailable","unavailable":true,"endpointPins":null}"#;
const CANCELLED: &str = r#"{"protocolVersion":1,"action":"stop","reason":"cancelled","restrictionDeadline":null,"unavailable":false,"endpointPins":null}"#;
const UNCORRELATED: &str = r#"{"protocolVersion":1,"action":"stop","reason":"uncorrelated","restrictionDeadline":null,"unavailable":false,"endpointPins":null}"#;

#[cfg(test)]
thread_local! {
    static LAST_CALLBACK: std::cell::RefCell<Option<(u16, String)>> = const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
pub(super) fn take_callback() -> Option<(u16, String)> {
    LAST_CALLBACK.with(|cell| cell.borrow_mut().take())
}

impl std::fmt::Debug for CallbackContext {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("CallbackContext")
            .field("origin", &self.origin)
            .field("token", &"[redacted]")
            .finish()
    }
}

pub(super) struct CallbackContext {
    pub service: Arc<LivePolicy>,
    pub origin: String,
    pub token: String,
}

pub(super) struct CorrelationView {
    pub deadline: DateTime<Utc>,
    pub cancelled: bool,
    pub pins: Arc<parking_lot::Mutex<HashMap<Uuid, PinnedAttempt>>>,
    pub published: Arc<parking_lot::Mutex<HashSet<Uuid>>>,
    pub authorization: Option<super::CorrelationAuthorization>,
    pub requested_model: String,
    pub client_trace_id: Option<String>,
}
