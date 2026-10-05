//! Typed v1 policy callback bodies. Parsing is bounded and errors stay redacted.

use chrono::{DateTime, Utc};
use rusqlite::Transaction;
use serde::Serialize;
use serde_json::{Map, Value, json};
use uuid::Uuid;

use super::store::{PolicyFault, SendKind};
use crate::cpa_projection::{MAX_NATIVE_TARGETS, NativeEndpointPin};

pub const MAX_REQUEST_BYTES: usize = 128 * 1024;
pub const MAX_RESPONSE_BYTES: usize = 16 * 1024;
pub const MAX_EVIDENCE_BODY_BYTES: usize = 64 * 1024;
pub const MAX_TOKEN_BYTES: usize = 256;
const MAX_ID_BYTES: usize = 128;
const MAX_MODEL_BYTES: usize = 256;
const MAX_HEADERS: usize = 8;
const MAX_HEADER_BYTES: usize = 128;
const PROTOCOL_VERSION: u64 = 1;
const CALLABLE_PROTOCOLS: [&str; 3] = ["chat_completions", "responses", "messages"];
const GENERATION_KINDS: [&str; 7] = [
    "execute",
    "refresh-resend",
    "stream",
    "stream-refresh",
    "stream-bootstrap",
    "internal",
    "count-tokens",
];
const PIN_KEYS: [&str; 5] = [
    "protocol",
    "endpointId",
    "origin",
    "endpointFingerprint",
    "httpMethod",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Action {
    Allow,
    Skip,
    Stop,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Reason {
    Eligible,
    Restricted,
    ExplicitRejection,
    ProviderRejected,
    Uncertain,
    Cancelled,
    Deadline,
    LocalValidation,
    UnsentTransport,
    Completed,
    Unauthorized,
    Malformed,
    Oversize,
    ProjectionFence,
    RequestFence,
    ModelFence,
    IdentityFence,
    Uncorrelated,
    Unavailable,
    StaleCredential,
    Deleted,
    Rebound,
    NotGranted,
    Disabled,
    UnknownReset,
    Ready,
    UsageObservation,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Decision {
    pub action: Action,
    pub reason: Reason,
    pub restriction_deadline: Option<DateTime<Utc>>,
    pub unavailable: bool,
    /// Allow-time native pins. Null for skip, stop, ready, API, no-auth, and local count.
    pub(crate) endpoint_pins: Option<Vec<NativeEndpointPin>>,
}

impl Decision {
    pub fn stop(reason: Reason) -> Self {
        Self {
            action: Action::Stop,
            reason,
            restriction_deadline: None,
            unavailable: matches!(reason, Reason::Unavailable),
            endpoint_pins: None,
        }
    }

    pub fn unavailable() -> Self {
        Self::stop(Reason::Unavailable)
    }

    pub fn skip(reason: Reason, deadline: Option<DateTime<Utc>>) -> Self {
        Self {
            action: Action::Skip,
            reason,
            restriction_deadline: deadline,
            unavailable: false,
            endpoint_pins: None,
        }
    }

    pub fn allow(reason: Reason) -> Self {
        Self {
            action: Action::Allow,
            reason,
            restriction_deadline: None,
            unavailable: false,
            endpoint_pins: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReadyReport {
    pub policy_ready: bool,
    pub process_generation: u64,
    pub projection_revision: u64,
    pub projection_digest: [u8; 32],
    pub reason: Reason,
    pub unavailable: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReplyClass {
    Decision,
    Ready,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PolicyReply {
    pub status: u16,
    pub bytes: Vec<u8>,
    pub decision: Option<Decision>,
    pub ready: Option<ReadyReport>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppliedProjection {
    pub process_generation: u64,
    pub revision: u64,
    pub digest: [u8; 32],
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttemptIdentity {
    pub request_id: Uuid,
    pub attempt_id: Uuid,
    pub auth_id: String,
    pub credential_id: String,
    pub credential_version: u64,
    pub provider_id: String,
    pub public_model: String,
    pub upstream_model: String,
    pub binding_id: String,
    pub material_revision: String,
    pub registration_epoch: u64,
    pub kind: SendKind,
}

/// Explicit pool membership. An empty model list does not mean every model.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PoolMembership {
    pub pool_id: String,
    pub pool_version: u64,
    pub public_models: Vec<String>,
    pub all_models: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeclaredSubject {
    Credential,
    Pool { pool_id: String, pool_version: u64 },
}

/// One scope the owner declares for this credential. Model is optional and
/// independent of whether the subject is a credential or a pool.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeclaredScope {
    pub subject: DeclaredSubject,
    pub public_model: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CurrentCredential {
    pub credential_id: String,
    pub credential_version: u64,
    pub provider_id: String,
    pub binding_id: String,
    pub material_revision: String,
    pub registration_epoch: u64,
    pub auth_id: String,
    pub memberships: Vec<PoolMembership>,
    pub scopes: Vec<DeclaredScope>,
    pub granted: bool,
    pub enabled: bool,
    pub deleted: bool,
    pub rebound: bool,
}

/// Captured attempt plus live credential facts.
///
/// `applied` and `attempt` are the callback's captured fences. `current` is
/// re-read inside the settings transaction. Epoch and material are fences on
/// the attempt, not the durable quota subject.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PolicyFacts {
    pub applied: AppliedProjection,
    pub attempt: Option<AttemptIdentity>,
    pub current: Option<CurrentCredential>,
    pub deadline_at: DateTime<Utc>,
    pub now: DateTime<Utc>,
    /// Caller intent failed. Admit stops the logical request before provider skip.
    pub caller_stop: Option<Reason>,
    /// Validated control pin. A provider skip becomes a stop so another auth is not chosen.
    pub validated_pin: bool,
}

impl PolicyFacts {
    pub fn invalid(&self) -> bool {
        if let Some(attempt) = &self.attempt {
            if invalid_attempt(attempt) {
                return true;
            }
        }
        if let Some(current) = &self.current {
            if invalid_current(current) {
                return true;
            }
        }
        false
    }
}

fn invalid_attempt(attempt: &AttemptIdentity) -> bool {
    invalid_id(&attempt.auth_id)
        || invalid_id(&attempt.credential_id)
        || invalid_id(&attempt.provider_id)
        || invalid_id(&attempt.binding_id)
        || invalid_id(&attempt.material_revision)
        || invalid_model(&attempt.public_model)
        || invalid_model(&attempt.upstream_model)
}

fn invalid_current(current: &CurrentCredential) -> bool {
    invalid_id(&current.credential_id)
        || invalid_id(&current.provider_id)
        || invalid_id(&current.binding_id)
        || invalid_id(&current.material_revision)
        || invalid_id(&current.auth_id)
        || current.memberships.iter().any(|member| {
            invalid_id(&member.pool_id)
                || member
                    .public_models
                    .iter()
                    .any(|model| invalid_model(model))
        })
        || current.scopes.iter().any(|scope| {
            let bad_model = scope
                .public_model
                .as_ref()
                .is_some_and(|model| invalid_model(model));
            let bad_pool = match &scope.subject {
                DeclaredSubject::Credential => false,
                DeclaredSubject::Pool { pool_id, .. } => invalid_id(pool_id),
            };
            bad_model || bad_pool
        })
}

fn invalid_id(value: &str) -> bool {
    value.is_empty() || value.len() > MAX_ID_BYTES
}

fn invalid_model(value: &str) -> bool {
    value.is_empty() || value.len() > MAX_MODEL_BYTES
}

/// Re-read live credential facts on the settings transaction.
///
/// Implementations must not take another connection lock, call the policy
/// service, or perform network I/O. `applied` and `attempt` stay the captured
/// callback fences; `current` is the live row.
pub trait CurrentFacts {
    fn revalidate(&mut self, tx: &Transaction<'_>) -> Result<PolicyFacts, PolicyFault>;

    /// Store the attempt identity after the admit decision is allow.
    ///
    /// `Ok(None)` is an API, no-auth, or local-only count allow. A native
    /// network allow returns the filtered pin vector. The default stores
    /// nothing. A result must not call this.
    fn freeze_admitted_attempt(
        &mut self,
        _tx: &Transaction<'_>,
        _identity: &AttemptIdentity,
        _allowed_at: DateTime<Utc>,
    ) -> Result<Option<Vec<NativeEndpointPin>>, PolicyFault> {
        Ok(None)
    }

    /// Allow-time pins for this attempt. `None` means this implementation
    /// did not freeze a vector. `Some(None)` is a frozen null allow.
    fn frozen_endpoint_pins(&self, _attempt_id: Uuid) -> Option<Option<Vec<NativeEndpointPin>>> {
        None
    }

    /// Pending and Absent OAuth lose restriction authority. The default
    /// keeps the existing quota path for facts that do not carry presence.
    fn retains_quota_restriction_authority(&self) -> bool {
        true
    }

    /// Write the allow-time observation on this settings transaction.
    ///
    /// A store failure must return `Ok` so the policy decision still commits.
    fn record_admitted_result(
        &mut self,
        _tx: &Transaction<'_>,
        _decision: &Decision,
        _identity: &AttemptIdentity,
        _result: &ResultBody,
        _raw_body: &str,
    ) -> Result<(), PolicyFault> {
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OpportunityPolicy {
    pub gap_seconds: u64,
    pub max_inflight: u32,
    pub max_per_request: u32,
}

impl OpportunityPolicy {
    pub const fn default_bounded() -> Self {
        Self {
            gap_seconds: 90,
            max_inflight: 1,
            max_per_request: 3,
        }
    }

    pub fn check(&self) -> Result<(), PolicyFault> {
        let gap_ok = (1..=24 * 60 * 60).contains(&self.gap_seconds);
        let inflight_ok = (1..=32).contains(&self.max_inflight);
        let request_ok = (1..=32).contains(&self.max_per_request);
        if gap_ok && inflight_ok && request_ok {
            Ok(())
        } else {
            Err(PolicyFault::Malformed)
        }
    }
}

impl Default for OpportunityPolicy {
    fn default() -> Self {
        Self::default_bounded()
    }
}

/// Stable error codes shared with the Go host. Raw `http_*` prefixes are rejected.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorCode {
    Validation,
    Transport,
    ProviderRejected,
    BodyLost,
    StreamLost,
    Cancelled,
    Deadline,
    UsageObservation,
    Parser,
    None,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    Success,
    ExplicitRejection,
    Uncertain,
    Cancelled,
    Deadline,
    LocalFailure,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResultObservation {
    pub id: String,
    pub fetched_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResultBody {
    pub sent: bool,
    pub status: Option<u16>,
    pub body_complete: bool,
    pub stream_started: bool,
    pub outcome: Outcome,
    pub error_code: ErrorCode,
    pub response_body: String,
    pub retry_after: Option<String>,
    pub observation: Option<ResultObservation>,
    /// Matched native dispatch. Null is not a matched dispatch and is not a count.
    pub endpoint_pin: Option<NativeEndpointPin>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectionFields {
    pub process_generation: u64,
    pub projection_revision: u64,
    pub projection_digest: [u8; 32],
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttemptFields {
    pub request_id: Uuid,
    pub attempt_id: Uuid,
    pub auth_id: String,
    pub credential_id: String,
    pub credential_version: u64,
    pub provider_id: String,
    pub public_model: String,
    pub upstream_model: String,
    pub registration_epoch: u64,
    pub material_revision: String,
    pub kind: SendKind,
    /// Admit only. Results leave both unset.
    pub callable_protocol: Option<String>,
    pub generation_kind: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RequestKind {
    Admit,
    Ready,
    Result(ResultBody),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedRequest {
    pub projection: ProjectionFields,
    pub attempt: Option<AttemptFields>,
    pub kind: RequestKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParseFault {
    Malformed,
    Oversize,
}

pub fn parse_request(body: &[u8]) -> Result<ParsedRequest, ParseFault> {
    if body.len() > MAX_REQUEST_BYTES {
        return Err(ParseFault::Oversize);
    }
    let value: Value = serde_json::from_slice(body).map_err(|_| ParseFault::Malformed)?;
    let object = value.as_object().ok_or(ParseFault::Malformed)?;
    if object.get("protocolVersion").and_then(Value::as_u64) != Some(PROTOCOL_VERSION) {
        return Err(ParseFault::Malformed);
    }
    let operation = object
        .get("operation")
        .and_then(Value::as_str)
        .ok_or(ParseFault::Malformed)?;
    let projection = parse_projection(object)?;
    let mut attempt = match operation {
        "ready" => parse_optional_attempt(object)?,
        "admit" | "result" => Some(parse_required_attempt(object)?),
        _ => return Err(ParseFault::Malformed),
    };
    if operation == "admit" {
        require_admit_pair(object, attempt.as_mut().ok_or(ParseFault::Malformed)?)?;
    }
    let kind = match operation {
        "admit" => RequestKind::Admit,
        "ready" => RequestKind::Ready,
        "result" => RequestKind::Result(parse_result(object)?),
        _ => return Err(ParseFault::Malformed),
    };
    reject_unexpected(object, operation)?;
    Ok(ParsedRequest {
        projection,
        attempt,
        kind,
    })
}

fn parse_projection(object: &Map<String, Value>) -> Result<ProjectionFields, ParseFault> {
    Ok(ProjectionFields {
        process_generation: decimal(object.get("processGeneration"))?,
        projection_revision: decimal(object.get("projectionRevision"))?,
        projection_digest: digest(object.get("projectionDigest"))?,
    })
}

fn parse_required_attempt(object: &Map<String, Value>) -> Result<AttemptFields, ParseFault> {
    parse_attempt(object)?.ok_or(ParseFault::Malformed)
}

fn parse_optional_attempt(
    object: &Map<String, Value>,
) -> Result<Option<AttemptFields>, ParseFault> {
    let present = ATTEMPT_KEYS.iter().any(|key| object.contains_key(*key));
    if !present {
        return Ok(None);
    }
    parse_attempt(object)
}

fn parse_attempt(object: &Map<String, Value>) -> Result<Option<AttemptFields>, ParseFault> {
    Ok(Some(AttemptFields {
        request_id: uuid_field(object.get("requestId"))?,
        attempt_id: uuid_field(object.get("attemptId"))?,
        auth_id: bounded_string(object.get("authId"), MAX_ID_BYTES)?,
        credential_id: bounded_string(object.get("credentialId"), MAX_ID_BYTES)?,
        credential_version: decimal(object.get("credentialVersion"))?,
        provider_id: bounded_string(object.get("providerId"), MAX_ID_BYTES)?,
        public_model: bounded_string(object.get("publicModel"), MAX_MODEL_BYTES)?,
        upstream_model: bounded_string(object.get("upstreamModel"), MAX_MODEL_BYTES)?,
        registration_epoch: decimal(object.get("registrationEpoch"))?,
        material_revision: bounded_string(object.get("materialRevision"), MAX_ID_BYTES)?,
        kind: send_kind(object.get("kind"))?,
        callable_protocol: None,
        generation_kind: None,
    }))
}

fn require_admit_pair(
    object: &Map<String, Value>,
    attempt: &mut AttemptFields,
) -> Result<(), ParseFault> {
    let protocol = object
        .get("callableProtocol")
        .and_then(Value::as_str)
        .ok_or(ParseFault::Malformed)?;
    let kind = object
        .get("generationKind")
        .and_then(Value::as_str)
        .ok_or(ParseFault::Malformed)?;
    if !CALLABLE_PROTOCOLS.contains(&protocol) || !GENERATION_KINDS.contains(&kind) {
        return Err(ParseFault::Malformed);
    }
    attempt.callable_protocol = Some(protocol.to_string());
    attempt.generation_kind = Some(kind.to_string());
    Ok(())
}

const ATTEMPT_KEYS: [&str; 11] = [
    "requestId",
    "attemptId",
    "authId",
    "credentialId",
    "credentialVersion",
    "providerId",
    "publicModel",
    "upstreamModel",
    "registrationEpoch",
    "materialRevision",
    "kind",
];

fn parse_result(object: &Map<String, Value>) -> Result<ResultBody, ParseFault> {
    let response_body = object
        .get("responseBody")
        .and_then(Value::as_str)
        .ok_or(ParseFault::Malformed)?;
    if response_body.len() > MAX_EVIDENCE_BODY_BYTES {
        return Err(ParseFault::Oversize);
    }
    let endpoint_pin = parse_endpoint_pin(object.get("endpointPin"))?;
    let status = match object.get("status") {
        None => return Err(ParseFault::Malformed),
        Some(Value::Null) => None,
        Some(Value::Number(number)) => {
            let status = number.as_u64().ok_or(ParseFault::Malformed)?;
            if status > 599 {
                return Err(ParseFault::Malformed);
            }
            Some(status as u16)
        }
        Some(_) => return Err(ParseFault::Malformed),
    };
    Ok(ResultBody {
        sent: bool_field(object.get("sent"))?,
        status,
        body_complete: bool_field(object.get("bodyComplete"))?,
        stream_started: bool_field(object.get("streamStarted"))?,
        outcome: outcome(object.get("outcome"))?,
        error_code: error_code(object.get("errorCode"))?,
        response_body: response_body.to_string(),
        retry_after: retry_after(object.get("headers"))?,
        observation: observation(object.get("observation"))?,
        endpoint_pin,
    })
}

fn parse_endpoint_pin(value: Option<&Value>) -> Result<Option<NativeEndpointPin>, ParseFault> {
    match value {
        None => Err(ParseFault::Malformed),
        Some(Value::Null) => Ok(None),
        Some(Value::Object(object)) => {
            if object.len() != PIN_KEYS.len()
                || object.keys().any(|key| !PIN_KEYS.contains(&key.as_str()))
            {
                return Err(ParseFault::Malformed);
            }
            let protocol = bounded_string(object.get("protocol"), MAX_ID_BYTES)?;
            let endpoint_id = bounded_string(object.get("endpointId"), MAX_ID_BYTES)?;
            let origin = bounded_string(object.get("origin"), MAX_ID_BYTES)?;
            let endpoint_fingerprint =
                bounded_string(object.get("endpointFingerprint"), MAX_ID_BYTES)?;
            let http_method = bounded_string(object.get("httpMethod"), MAX_ID_BYTES)?;
            if !CALLABLE_PROTOCOLS.contains(&protocol.as_str()) || http_method != "POST" {
                return Err(ParseFault::Malformed);
            }
            if endpoint_fingerprint.len() != 64
                || !endpoint_fingerprint
                    .bytes()
                    .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
            {
                return Err(ParseFault::Malformed);
            }
            Ok(Some(NativeEndpointPin {
                protocol,
                endpoint_id,
                origin,
                endpoint_fingerprint,
                http_method,
            }))
        }
        Some(_) => Err(ParseFault::Malformed),
    }
}

fn reject_unexpected(object: &Map<String, Value>, operation: &str) -> Result<(), ParseFault> {
    let mut allowed = vec![
        "protocolVersion",
        "operation",
        "processGeneration",
        "projectionRevision",
        "projectionDigest",
    ];
    if operation != "ready" || ATTEMPT_KEYS.iter().any(|key| object.contains_key(*key)) {
        allowed.extend(ATTEMPT_KEYS);
    }
    if operation == "admit" {
        allowed.extend(["callableProtocol", "generationKind"]);
    }
    if operation == "result" {
        allowed.extend([
            "sent",
            "status",
            "bodyComplete",
            "streamStarted",
            "outcome",
            "errorCode",
            "responseBody",
            "headers",
            "reportedUsage",
            "observation",
            "endpointPin",
        ]);
    }
    if object.keys().any(|key| !allowed.contains(&key.as_str())) {
        return Err(ParseFault::Malformed);
    }
    if operation == "result" {
        ignore_reported_usage(object.get("reportedUsage"))?;
    }
    Ok(())
}

fn ignore_reported_usage(value: Option<&Value>) -> Result<(), ParseFault> {
    match value {
        None | Some(Value::Null) => Ok(()),
        Some(Value::Object(usage)) => {
            for key in ["inputTokens", "outputTokens"] {
                if let Some(token) = usage.get(key) {
                    match token.as_u64() {
                        Some(count) if count <= 50_000_000 => {}
                        _ => return Err(ParseFault::Malformed),
                    }
                }
            }
            if usage
                .keys()
                .any(|key| key != "inputTokens" && key != "outputTokens")
            {
                return Err(ParseFault::Malformed);
            }
            Ok(())
        }
        Some(_) => Err(ParseFault::Malformed),
    }
}

fn observation(value: Option<&Value>) -> Result<Option<ResultObservation>, ParseFault> {
    match value {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Object(object)) => {
            if object.keys().any(|key| key != "id" && key != "fetchedAt") {
                return Err(ParseFault::Malformed);
            }
            let id = bounded_string(object.get("id"), MAX_ID_BYTES)?;
            let fetched = object
                .get("fetchedAt")
                .and_then(Value::as_str)
                .ok_or(ParseFault::Malformed)?;
            let fetched_at = DateTime::parse_from_rfc3339(fetched)
                .map(|stamp| stamp.with_timezone(&Utc))
                .map_err(|_| ParseFault::Malformed)?;
            Ok(Some(ResultObservation { id, fetched_at }))
        }
        Some(_) => Err(ParseFault::Malformed),
    }
}

fn retry_after(value: Option<&Value>) -> Result<Option<String>, ParseFault> {
    let Some(value) = value else {
        return Ok(None);
    };
    if value.is_null() {
        return Ok(None);
    }
    let headers = value.as_object().ok_or(ParseFault::Malformed)?;
    if headers.len() > MAX_HEADERS {
        return Err(ParseFault::Malformed);
    }
    let mut retry = None;
    for (name, header) in headers {
        if name.len() > MAX_HEADER_BYTES {
            return Err(ParseFault::Malformed);
        }
        let text = header.as_str().ok_or(ParseFault::Malformed)?;
        if text.len() > MAX_HEADER_BYTES {
            return Err(ParseFault::Malformed);
        }
        if name.eq_ignore_ascii_case("retry-after") {
            retry = Some(text.to_string());
        }
    }
    Ok(retry)
}

fn decimal(value: Option<&Value>) -> Result<u64, ParseFault> {
    let text = value.and_then(Value::as_str).ok_or(ParseFault::Malformed)?;
    if text.is_empty() || text.len() > 20 || !text.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(ParseFault::Malformed);
    }
    if text.len() > 1 && text.starts_with('0') {
        return Err(ParseFault::Malformed);
    }
    text.parse().map_err(|_| ParseFault::Malformed)
}

fn digest(value: Option<&Value>) -> Result<[u8; 32], ParseFault> {
    let text = value.and_then(Value::as_str).ok_or(ParseFault::Malformed)?;
    if text.len() != 64 || !text.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(ParseFault::Malformed);
    }
    let mut out = [0u8; 32];
    hex::decode_to_slice(text, &mut out).map_err(|_| ParseFault::Malformed)?;
    Ok(out)
}

fn uuid_field(value: Option<&Value>) -> Result<Uuid, ParseFault> {
    let text = value.and_then(Value::as_str).ok_or(ParseFault::Malformed)?;
    Uuid::parse_str(text).map_err(|_| ParseFault::Malformed)
}

fn bounded_string(value: Option<&Value>, max: usize) -> Result<String, ParseFault> {
    let text = value.and_then(Value::as_str).ok_or(ParseFault::Malformed)?;
    if text.is_empty() || text.len() > max {
        return Err(ParseFault::Malformed);
    }
    Ok(text.to_string())
}

fn bool_field(value: Option<&Value>) -> Result<bool, ParseFault> {
    value.and_then(Value::as_bool).ok_or(ParseFault::Malformed)
}

fn outcome(value: Option<&Value>) -> Result<Outcome, ParseFault> {
    match value.and_then(Value::as_str) {
        Some("success") => Ok(Outcome::Success),
        Some("explicit_rejection") => Ok(Outcome::ExplicitRejection),
        Some("uncertain") => Ok(Outcome::Uncertain),
        Some("cancelled") => Ok(Outcome::Cancelled),
        Some("deadline") => Ok(Outcome::Deadline),
        Some("local_failure") => Ok(Outcome::LocalFailure),
        _ => Err(ParseFault::Malformed),
    }
}

fn send_kind(value: Option<&Value>) -> Result<SendKind, ParseFault> {
    match value.and_then(Value::as_str) {
        Some("accepted") => Ok(SendKind::Accepted),
        Some("validated") => Ok(SendKind::Validated),
        _ => Err(ParseFault::Malformed),
    }
}

fn error_code(value: Option<&Value>) -> Result<ErrorCode, ParseFault> {
    match value.and_then(Value::as_str) {
        Some("validation") => Ok(ErrorCode::Validation),
        Some("transport") => Ok(ErrorCode::Transport),
        Some("provider_rejected") => Ok(ErrorCode::ProviderRejected),
        Some("body_lost") => Ok(ErrorCode::BodyLost),
        Some("stream_lost") => Ok(ErrorCode::StreamLost),
        Some("cancelled") => Ok(ErrorCode::Cancelled),
        Some("deadline") => Ok(ErrorCode::Deadline),
        Some("usage_observation") => Ok(ErrorCode::UsageObservation),
        Some("parser") => Ok(ErrorCode::Parser),
        Some("none") => Ok(ErrorCode::None),
        Some("unknown") => Ok(ErrorCode::Unknown),
        _ => Err(ParseFault::Malformed),
    }
}

const DECISION_FALLBACK: &[u8] =
    br#"{"protocolVersion":1,"action":"stop","reason":"unavailable","unavailable":true,"endpointPins":null}"#;
const READY_FALLBACK: &[u8] = br#"{"protocolVersion":1,"policyReady":false,"processGeneration":"0","projectionRevision":"0","projectionDigest":"0000000000000000000000000000000000000000000000000000000000000000","reason":"unavailable","unavailable":true,"endpointPins":null}"#;

pub fn encode_decision(status: u16, decision: Decision, _class: ReplyClass) -> PolicyReply {
    let pins = match endpoint_pins_json(
        decision.action == Action::Allow,
        decision.endpoint_pins.as_deref(),
    ) {
        Some(pins) => pins,
        None => {
            return PolicyReply {
                status,
                bytes: DECISION_FALLBACK.to_vec(),
                decision: Some(Decision::unavailable()),
                ready: None,
            };
        }
    };
    let body = json!({
        "protocolVersion": PROTOCOL_VERSION,
        "action": decision.action,
        "reason": decision.reason,
        "restrictionDeadline": decision.restriction_deadline.map(|deadline| deadline.to_rfc3339()),
        "unavailable": decision.unavailable,
        "endpointPins": pins,
    });
    PolicyReply {
        status,
        bytes: bounded_bytes(&body, DECISION_FALLBACK),
        decision: Some(decision),
        ready: None,
    }
}

pub fn encode_ready(report: ReadyReport) -> PolicyReply {
    let body = json!({
        "protocolVersion": PROTOCOL_VERSION,
        "policyReady": report.policy_ready,
        "processGeneration": report.process_generation.to_string(),
        "projectionRevision": report.projection_revision.to_string(),
        "projectionDigest": hex::encode(report.projection_digest),
        "reason": report.reason,
        "unavailable": report.unavailable,
        "endpointPins": Value::Null,
    });
    let decision = if report.policy_ready {
        Decision::allow(Reason::Ready)
    } else if report.unavailable {
        Decision::unavailable()
    } else {
        Decision::stop(report.reason)
    };
    PolicyReply {
        status: 200,
        bytes: bounded_bytes(&body, READY_FALLBACK),
        decision: Some(decision),
        ready: Some(report),
    }
}

/// `None` refuses a present vector that is empty, oversized, or not distinct.
fn endpoint_pins_json(allow: bool, pins: Option<&[NativeEndpointPin]>) -> Option<Value> {
    if !allow {
        return Some(Value::Null);
    }
    let Some(pins) = pins else {
        return Some(Value::Null);
    };
    if pins.is_empty() || pins.len() > MAX_NATIVE_TARGETS || !distinct_pins(pins) {
        return None;
    }
    serde_json::to_value(pins).ok()
}

fn distinct_pins(pins: &[NativeEndpointPin]) -> bool {
    let mut seen = Vec::with_capacity(pins.len());
    for pin in pins {
        if pin.protocol.is_empty()
            || pin.endpoint_id.is_empty()
            || pin.origin.is_empty()
            || pin.http_method != "POST"
            || pin.endpoint_fingerprint.len() != 64
        {
            return false;
        }
        if seen.iter().any(|saved: &NativeEndpointPin| saved == pin) {
            return false;
        }
        seen.push(pin.clone());
    }
    true
}

fn bounded_bytes(body: &Value, fallback: &'static [u8]) -> Vec<u8> {
    let bytes = serde_json::to_vec(body).unwrap_or_else(|_| fallback.to_vec());
    if bytes.len() <= MAX_RESPONSE_BYTES {
        bytes
    } else {
        fallback.to_vec()
    }
}

/// Direct official Go usage observation. This is not an inference result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OfficialQuotaObservation {
    pub observation_id: String,
    pub fetched_at: DateTime<Utc>,
    pub body: Vec<u8>,
    pub provider_id: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QuotaApply {
    Applied,
    Stale,
}
