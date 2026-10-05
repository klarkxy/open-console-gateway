//! Per-attempt CPA observation on the existing `forward_logs` rows.
//!
//! [`record_result_on`] writes one admitted attempt inside the caller's SQLite
//! transaction. It does not admit, select, price, or read the correlation map.
//! The lifecycle owner supplies the snapshot taken when that attempt was allowed.

#![allow(dead_code)]

use chrono::{DateTime, Utc};
use rusqlite::Transaction;
use serde_json::{Map, Value};
use uuid::Uuid;

use crate::cpa_policy::{MAX_REQUEST_BYTES, SendKind};
use crate::db::log_store::{
    cpa_forward_logs_for_ordinal_on, find_forward_log_attempt_on, insert_forward_log_on,
};
use ocg_infra::sqlite_logs::ForwardLogInsertRow;

const MAX_ID_BYTES: usize = 128;
const MAX_MODEL_BYTES: usize = 256;
const MAX_DIAGNOSTIC_BYTES: usize = 4 * 1024;
const MAX_REPORTED_TOKENS: u64 = 50_000_000;

/// Immutable identity captured when an attempt was actually allowed.
///
/// A result body cannot build this value. Rejected admits have no snapshot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdmittedAttemptContext {
    /// Private CPA correlation UUID. The result body's `requestId` matches this value.
    pub request_id: Uuid,
    /// Generated `ocg-` trace from the public `X-OCG-Request-Id` header.
    /// `None` leaves the private UUID in `forward_logs.request_id`.
    pub client_trace_id: Option<String>,
    pub attempt_id: Uuid,
    pub ordinal: u32,
    pub started_at: DateTime<Utc>,
    pub provider_id: String,
    pub credential_id: String,
    pub credential_version: u64,
    pub binding_id: String,
    pub auth_id: String,
    pub material_revision: String,
    pub registration_epoch: u64,
    pub public_model: String,
    pub upstream_model: String,
    pub kind: SendKind,
    /// Client key captured with the allow. Validated control attempts have none.
    pub client_key: Option<CapturedClientKey>,
}

/// Key id and display name from the allow-time pin. Not a secret and not a fingerprint.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CapturedClientKey {
    pub id: String,
    pub name: Option<String>,
}

/// Result flags copied from the strict policy parse. The response body is not a field.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StrictAttemptResult {
    pub sent: bool,
    pub status: Option<u16>,
    pub body_complete: bool,
    pub stream_started: bool,
    pub outcome: AttemptOutcome,
    pub error_code: AttemptErrorCode,
    pub observation_id: Option<String>,
    pub observed_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AttemptOutcome {
    Success,
    ExplicitRejection,
    Uncertain,
    Cancelled,
    Deadline,
    LocalFailure,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AttemptErrorCode {
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
pub enum AttemptRecord {
    Inserted(i64),
    Existing(i64),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ObservationError {
    /// The snapshot is not an allowed attempt identity.
    Identity,
    /// The raw result does not match the snapshot or the typed result.
    Mismatch,
    /// `reportedUsage` failed the same bounds the policy wire already applies.
    Usage,
    /// This request ordinal already belongs to another CPA attempt.
    Conflict,
    /// The log statement failed. The caller must not roll back policy.
    Store,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Completeness {
    Complete,
    ExplicitRejection,
    Cancelled,
    Deadline,
    BodyLost,
    Truncated,
    Uncertain,
    Validation,
    Transport,
}

#[derive(Clone, Copy)]
enum Stamp {
    Absent,
    Matched,
    Rejected,
}

struct ReportedUsage {
    input: Option<u64>,
    output: Option<u64>,
    stamp: Stamp,
}

/// Record one correlated CPA result on `tx`.
///
/// A second call for the same displayed request id and attempt id does not
/// insert or update. The displayed id is the generated public trace when the
/// snapshot has one, and the private correlation UUID otherwise. The diagnostic
/// keeps the private UUID. Unknown token counts stay unknown. Cost and native
/// USD stay unreported. The upstream body is not stored.
pub fn record_result_on(
    tx: &Transaction<'_>,
    admitted: &AdmittedAttemptContext,
    result: &StrictAttemptResult,
    validated_raw_body: &str,
) -> Result<AttemptRecord, ObservationError> {
    validate_context(admitted)?;
    let display_id = display_request_id(admitted)?;
    let attempt_id = admitted.attempt_id.to_string();
    if let Some(id) = find_forward_log_attempt_on(tx, &display_id, &attempt_id)
        .map_err(|_| ObservationError::Store)?
    {
        return Ok(AttemptRecord::Existing(id));
    }
    match ordinal_owner(tx, &display_id, i64::from(admitted.ordinal), &attempt_id)? {
        OrdinalOwner::Existing(id) => return Ok(AttemptRecord::Existing(id)),
        OrdinalOwner::Conflict => return Err(ObservationError::Conflict),
        OrdinalOwner::Free => {}
    }

    let usage = parse_validated_body(admitted, result, validated_raw_body)?;
    let class = classify(result);
    let started_at = admitted.started_at.to_rfc3339();
    let diagnostic = diagnostic_json(admitted, result, &usage, class, &started_at)?;
    let row = insert_row(
        admitted,
        result,
        &usage,
        class,
        &diagnostic,
        &started_at,
        &display_id,
    );
    let id = insert_forward_log_on(tx, &row).map_err(|_| ObservationError::Store)?;
    Ok(AttemptRecord::Inserted(id))
}

enum OrdinalOwner {
    Free,
    Existing(i64),
    Conflict,
}

fn ordinal_owner(
    tx: &Transaction<'_>,
    request_id: &str,
    ordinal: i64,
    attempt_id: &str,
) -> Result<OrdinalOwner, ObservationError> {
    let rows = cpa_forward_logs_for_ordinal_on(tx, request_id, ordinal)
        .map_err(|_| ObservationError::Store)?;
    if rows.is_empty() {
        return Ok(OrdinalOwner::Free);
    }
    if rows
        .iter()
        .any(|(_, stored)| stored.as_ref().is_some_and(|value| value != attempt_id))
    {
        return Ok(OrdinalOwner::Conflict);
    }
    Ok(OrdinalOwner::Existing(rows[0].0))
}

fn validate_context(admitted: &AdmittedAttemptContext) -> Result<(), ObservationError> {
    if admitted.ordinal == 0 {
        return Err(ObservationError::Identity);
    }
    display_request_id(admitted)?;
    let ids = [
        admitted.provider_id.as_str(),
        admitted.credential_id.as_str(),
        admitted.binding_id.as_str(),
        admitted.auth_id.as_str(),
        admitted.material_revision.as_str(),
    ];
    if ids.into_iter().any(|value| !bounded(value, MAX_ID_BYTES)) {
        return Err(ObservationError::Identity);
    }
    if !bounded(&admitted.public_model, MAX_MODEL_BYTES)
        || !bounded(&admitted.upstream_model, MAX_MODEL_BYTES)
    {
        return Err(ObservationError::Identity);
    }
    match (&admitted.kind, &admitted.client_key) {
        (SendKind::Accepted, Some(key)) if bounded(&key.id, MAX_ID_BYTES) => {
            if let Some(name) = &key.name {
                if !bounded(name, MAX_ID_BYTES) {
                    return Err(ObservationError::Identity);
                }
            }
            Ok(())
        }
        (SendKind::Validated, None) => Ok(()),
        _ => Err(ObservationError::Identity),
    }
}

fn bounded(value: &str, max: usize) -> bool {
    !value.is_empty() && value.len() <= max && !value.chars().any(char::is_control)
}

fn display_request_id(admitted: &AdmittedAttemptContext) -> Result<String, ObservationError> {
    match admitted.client_trace_id.as_deref() {
        None => Ok(admitted.request_id.to_string()),
        Some(trace) => canonical_client_trace(trace),
    }
}

/// `RequestTrace` emits `ocg-` plus a hyphenated UUID. Anything else is not that trace.
fn canonical_client_trace(trace: &str) -> Result<String, ObservationError> {
    let Some(rest) = trace.strip_prefix("ocg-") else {
        return Err(ObservationError::Identity);
    };
    if rest.len() != 36 {
        return Err(ObservationError::Identity);
    }
    let parsed = Uuid::parse_str(rest).map_err(|_| ObservationError::Identity)?;
    Ok(format!("ocg-{parsed}"))
}

fn parse_validated_body(
    admitted: &AdmittedAttemptContext,
    result: &StrictAttemptResult,
    raw: &str,
) -> Result<ReportedUsage, ObservationError> {
    if raw.len() > MAX_REQUEST_BYTES {
        return Err(ObservationError::Mismatch);
    }
    let value: Value = serde_json::from_str(raw).map_err(|_| ObservationError::Mismatch)?;
    let object = value.as_object().ok_or(ObservationError::Mismatch)?;
    require_uuid(object, "requestId", admitted.request_id)?;
    require_uuid(object, "attemptId", admitted.attempt_id)?;
    require_text(object, "providerId", &admitted.provider_id)?;
    require_text(object, "credentialId", &admitted.credential_id)?;
    require_decimal(object, "credentialVersion", admitted.credential_version)?;
    require_text(object, "publicModel", &admitted.public_model)?;
    require_executed_upstream(object, admitted)?;
    require_text(object, "authId", &admitted.auth_id)?;
    require_text(object, "materialRevision", &admitted.material_revision)?;
    require_decimal(object, "registrationEpoch", admitted.registration_epoch)?;
    require_text(object, "kind", kind_wire(admitted.kind))?;
    if json_bool(object, "sent")? != result.sent
        || json_bool(object, "bodyComplete")? != result.body_complete
        || json_bool(object, "streamStarted")? != result.stream_started
        || json_status(object)? != result.status
        || json_text(object, "outcome")? != outcome_wire(result.outcome)
        || json_text(object, "errorCode")? != error_wire(result.error_code)
    {
        return Err(ObservationError::Mismatch);
    }
    let body_stamp = json_observation(object)?;
    if !stamps_agree(body_stamp.as_ref(), result) {
        return Err(ObservationError::Mismatch);
    }
    let (input, output) = reported_tokens(object.get("reportedUsage"))?;
    let stamp = match body_stamp {
        None => Stamp::Absent,
        Some((id, _)) => match Uuid::parse_str(&id) {
            Ok(parsed) if parsed == admitted.attempt_id => Stamp::Matched,
            _ => Stamp::Rejected,
        },
    };
    Ok(ReportedUsage {
        input,
        output,
        stamp,
    })
}

fn require_uuid(
    object: &Map<String, Value>,
    key: &str,
    expected: Uuid,
) -> Result<(), ObservationError> {
    let text = json_text(object, key)?;
    let parsed = Uuid::parse_str(text).map_err(|_| ObservationError::Mismatch)?;
    if parsed == expected {
        Ok(())
    } else {
        Err(ObservationError::Mismatch)
    }
}

fn require_executed_upstream(
    object: &Map<String, Value>,
    admitted: &AdmittedAttemptContext,
) -> Result<(), ObservationError> {
    let reported = json_text(object, "upstreamModel")?;
    if reported == admitted.upstream_model
        || (admitted.public_model != admitted.upstream_model && reported == admitted.public_model)
    {
        return Ok(());
    }
    Err(ObservationError::Mismatch)
}

fn require_text(
    object: &Map<String, Value>,
    key: &str,
    expected: &str,
) -> Result<(), ObservationError> {
    if json_text(object, key)? == expected {
        Ok(())
    } else {
        Err(ObservationError::Mismatch)
    }
}

fn require_decimal(
    object: &Map<String, Value>,
    key: &str,
    expected: u64,
) -> Result<(), ObservationError> {
    let text = json_text(object, key)?;
    if text.len() > 20
        || text.is_empty()
        || !text.bytes().all(|byte| byte.is_ascii_digit())
        || (text.len() > 1 && text.starts_with('0'))
    {
        return Err(ObservationError::Mismatch);
    }
    match text.parse::<u64>() {
        Ok(value) if value == expected => Ok(()),
        _ => Err(ObservationError::Mismatch),
    }
}

fn json_text<'a>(object: &'a Map<String, Value>, key: &str) -> Result<&'a str, ObservationError> {
    object
        .get(key)
        .and_then(Value::as_str)
        .ok_or(ObservationError::Mismatch)
}

fn json_bool(object: &Map<String, Value>, key: &str) -> Result<bool, ObservationError> {
    object
        .get(key)
        .and_then(Value::as_bool)
        .ok_or(ObservationError::Mismatch)
}

fn json_status(object: &Map<String, Value>) -> Result<Option<u16>, ObservationError> {
    match object.get("status") {
        None => Err(ObservationError::Mismatch),
        Some(Value::Null) => Ok(None),
        Some(Value::Number(number)) => {
            let status = number.as_u64().ok_or(ObservationError::Mismatch)?;
            if status > 599 {
                return Err(ObservationError::Mismatch);
            }
            Ok(Some(status as u16))
        }
        Some(_) => Err(ObservationError::Mismatch),
    }
}

fn json_observation(
    object: &Map<String, Value>,
) -> Result<Option<(String, DateTime<Utc>)>, ObservationError> {
    match object.get("observation") {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Object(stamp)) => {
            if stamp.keys().any(|key| key != "id" && key != "fetchedAt") {
                return Err(ObservationError::Mismatch);
            }
            let id = stamp
                .get("id")
                .and_then(Value::as_str)
                .ok_or(ObservationError::Mismatch)?;
            if id.is_empty() || id.len() > MAX_ID_BYTES {
                return Err(ObservationError::Mismatch);
            }
            let fetched = stamp
                .get("fetchedAt")
                .and_then(Value::as_str)
                .ok_or(ObservationError::Mismatch)?;
            let observed_at = DateTime::parse_from_rfc3339(fetched)
                .map(|value| value.with_timezone(&Utc))
                .map_err(|_| ObservationError::Mismatch)?;
            Ok(Some((id.to_string(), observed_at)))
        }
        Some(_) => Err(ObservationError::Mismatch),
    }
}

fn stamps_agree(body: Option<&(String, DateTime<Utc>)>, result: &StrictAttemptResult) -> bool {
    match (body, &result.observation_id, &result.observed_at) {
        (None, None, None) => true,
        (Some((id, at)), Some(typed_id), Some(typed_at)) => id == typed_id && at == typed_at,
        _ => false,
    }
}

fn reported_tokens(value: Option<&Value>) -> Result<(Option<u64>, Option<u64>), ObservationError> {
    match value {
        None | Some(Value::Null) => Ok((None, None)),
        Some(Value::Object(usage)) => {
            if usage
                .keys()
                .any(|key| key != "inputTokens" && key != "outputTokens")
            {
                return Err(ObservationError::Usage);
            }
            Ok((
                one_token(usage.get("inputTokens"))?,
                one_token(usage.get("outputTokens"))?,
            ))
        }
        Some(_) => Err(ObservationError::Usage),
    }
}

fn one_token(value: Option<&Value>) -> Result<Option<u64>, ObservationError> {
    match value {
        None => Ok(None),
        Some(Value::Number(number)) => match number.as_u64() {
            Some(count) if count <= MAX_REPORTED_TOKENS => Ok(Some(count)),
            _ => Err(ObservationError::Usage),
        },
        Some(_) => Err(ObservationError::Usage),
    }
}

fn classify(result: &StrictAttemptResult) -> Completeness {
    let truncated = result.error_code == AttemptErrorCode::StreamLost
        || (result.stream_started && !result.body_complete);
    let body_lost = result.error_code == AttemptErrorCode::BodyLost
        || (result.sent && !result.body_complete && !result.stream_started);
    match result.outcome {
        AttemptOutcome::Cancelled => Completeness::Cancelled,
        AttemptOutcome::Deadline => Completeness::Deadline,
        _ if truncated => Completeness::Truncated,
        _ if body_lost => Completeness::BodyLost,
        AttemptOutcome::Uncertain => Completeness::Uncertain,
        AttemptOutcome::Success => {
            let lost_status = result.sent && result.status.is_none();
            let clean_code = matches!(
                result.error_code,
                AttemptErrorCode::None | AttemptErrorCode::UsageObservation
            );
            if result.body_complete && !lost_status && clean_code {
                Completeness::Complete
            } else {
                Completeness::Uncertain
            }
        }
        AttemptOutcome::ExplicitRejection => {
            let definite = result.sent
                && result.body_complete
                && !result.stream_started
                && matches!(result.status, Some(401 | 403 | 429));
            if definite {
                Completeness::ExplicitRejection
            } else {
                Completeness::Uncertain
            }
        }
        AttemptOutcome::LocalFailure => {
            if result.sent
                || result.stream_started
                || (result.body_complete && result.status.is_some())
            {
                Completeness::Uncertain
            } else {
                match result.error_code {
                    AttemptErrorCode::Validation => Completeness::Validation,
                    AttemptErrorCode::Transport => Completeness::Transport,
                    _ => Completeness::Uncertain,
                }
            }
        }
    }
}

fn status_column(class: Completeness) -> &'static str {
    match class {
        Completeness::Complete => "success",
        Completeness::ExplicitRejection => "explicit_rejection",
        Completeness::Cancelled => "cancelled",
        Completeness::Deadline => "deadline",
        Completeness::BodyLost | Completeness::Truncated | Completeness::Uncertain => {
            "outcome_unknown"
        }
        Completeness::Validation | Completeness::Transport => "error",
    }
}

fn class_code(class: Completeness) -> &'static str {
    match class {
        Completeness::Complete => "complete",
        Completeness::ExplicitRejection => "explicit_rejection",
        Completeness::Cancelled => "cancelled",
        Completeness::Deadline => "deadline",
        Completeness::BodyLost => "body_lost",
        Completeness::Truncated => "truncated",
        Completeness::Uncertain => "uncertain",
        Completeness::Validation => "validation",
        Completeness::Transport => "transport",
    }
}

fn diagnostic_json(
    admitted: &AdmittedAttemptContext,
    result: &StrictAttemptResult,
    usage: &ReportedUsage,
    class: Completeness,
    started_at: &str,
) -> Result<String, ObservationError> {
    let (observation_id, observed_at, observation_stamp) = match usage.stamp {
        Stamp::Absent => (Value::Null, Value::Null, "absent"),
        Stamp::Rejected => (Value::Null, Value::Null, "rejected"),
        Stamp::Matched => {
            let Some(at) = result.observed_at else {
                return Err(ObservationError::Mismatch);
            };
            (
                Value::String(admitted.attempt_id.to_string()),
                Value::String(at.to_rfc3339()),
                "matched",
            )
        }
    };
    let client_trace = client_trace_json(admitted)?;
    let body = serde_json::json!({
        "version": 1,
        "source": "cpa_result",
        "request_id": admitted.request_id.to_string(),
        "client_trace_id": client_trace,
        "attempt_id": admitted.attempt_id.to_string(),
        "ordinal": admitted.ordinal,
        "kind": kind_wire(admitted.kind),
        "provider_id": admitted.provider_id,
        "credential_id": admitted.credential_id,
        "credential_version": admitted.credential_version.to_string(),
        "binding_id": admitted.binding_id,
        "auth_id": admitted.auth_id,
        "material_revision": admitted.material_revision,
        "registration_epoch": admitted.registration_epoch.to_string(),
        "public_model": admitted.public_model,
        "upstream_model": admitted.upstream_model,
        "started_at": started_at,
        "client_key_id": admitted.client_key.as_ref().map(|key| key.id.as_str()),
        "sent": result.sent,
        "body_complete": result.body_complete,
        "stream_started": result.stream_started,
        "http_status": result.status,
        "outcome": outcome_wire(result.outcome),
        "error_code": error_wire(result.error_code),
        "completeness": class_code(class),
        "usage": {
            "input_tokens": token_json(usage.input),
            "output_tokens": token_json(usage.output),
        },
        "usage_state": usage_state(usage.input, usage.output),
        "cache_reported": false,
        "cost_state": "unknown",
        "native_usd_reported": false,
        "admission": false,
        "observation_id": observation_id,
        "observed_at": observed_at,
        "observation_stamp": observation_stamp,
    });
    let encoded = serde_json::to_string(&body).map_err(|_| ObservationError::Store)?;
    if encoded.len() > MAX_DIAGNOSTIC_BYTES {
        return Err(ObservationError::Identity);
    }
    Ok(encoded)
}

fn client_trace_json(admitted: &AdmittedAttemptContext) -> Result<Value, ObservationError> {
    match admitted.client_trace_id.as_deref() {
        None => Ok(Value::Null),
        Some(trace) => Ok(Value::String(canonical_client_trace(trace)?)),
    }
}

fn token_json(value: Option<u64>) -> Value {
    match value {
        Some(count) => Value::from(count),
        None => Value::Null,
    }
}

fn usage_state(input: Option<u64>, output: Option<u64>) -> &'static str {
    match (input, output) {
        (Some(_), Some(_)) => "reported",
        (None, None) => "unknown",
        _ => "partial",
    }
}

fn insert_row<'a>(
    admitted: &'a AdmittedAttemptContext,
    result: &StrictAttemptResult,
    usage: &ReportedUsage,
    class: Completeness,
    diagnostic: &'a str,
    started_at: &'a str,
    request_id: &'a str,
) -> ForwardLogInsertRow<'a> {
    let (client_key_id, client_key_name) = match &admitted.client_key {
        Some(key) => (Some(key.id.as_str()), key.name.as_deref()),
        None => (None, None),
    };
    ForwardLogInsertRow {
        timestamp: started_at,
        model: &admitted.public_model,
        account_id: &admitted.credential_id,
        account_name: &admitted.credential_id,
        client_key_id,
        client_key_name,
        route_account_id: None,
        provider_id: Some(&admitted.provider_id),
        credential_account_id: Some(&admitted.credential_id),
        status: status_column(class),
        http_status: result.status.map(i32::from),
        route: "",
        prompt_tokens: token_column(usage.input),
        completion_tokens: token_column(usage.output),
        cached_tokens: 0,
        cache_creation_tokens: 0,
        cost: 0.0,
        raw_cost_usd: None,
        quota_debit: None,
        effective_paid_cost_usd: None,
        pricing_revision_id: None,
        quota_multiplier: None,
        local_adjustment_multiplier: None,
        service_tier: None,
        cost_state: "unknown",
        error_message: (class != Completeness::Complete).then_some(class_code(class)),
        request_id: Some(request_id),
        attempt: Some(i64::from(admitted.ordinal)),
        error_source: Some("cpa"),
        error_stage: Some(class_code(class)),
        duration_ms: None,
        diagnostic_json: Some(diagnostic),
        requested_model: Some(&admitted.public_model),
        resolved_alias: None,
        upstream_model: Some(&admitted.upstream_model),
        native_cost_value: None,
        native_cost_unit: None,
        native_cost_currency: None,
    }
}

fn token_column(value: Option<u64>) -> i64 {
    match value {
        Some(count) => i64::try_from(count).unwrap_or(0),
        None => 0,
    }
}

fn kind_wire(kind: SendKind) -> &'static str {
    match kind {
        SendKind::Accepted => "accepted",
        SendKind::Validated => "validated",
    }
}

fn outcome_wire(outcome: AttemptOutcome) -> &'static str {
    match outcome {
        AttemptOutcome::Success => "success",
        AttemptOutcome::ExplicitRejection => "explicit_rejection",
        AttemptOutcome::Uncertain => "uncertain",
        AttemptOutcome::Cancelled => "cancelled",
        AttemptOutcome::Deadline => "deadline",
        AttemptOutcome::LocalFailure => "local_failure",
    }
}

fn error_wire(code: AttemptErrorCode) -> &'static str {
    match code {
        AttemptErrorCode::Validation => "validation",
        AttemptErrorCode::Transport => "transport",
        AttemptErrorCode::ProviderRejected => "provider_rejected",
        AttemptErrorCode::BodyLost => "body_lost",
        AttemptErrorCode::StreamLost => "stream_lost",
        AttemptErrorCode::Cancelled => "cancelled",
        AttemptErrorCode::Deadline => "deadline",
        AttemptErrorCode::UsageObservation => "usage_observation",
        AttemptErrorCode::Parser => "parser",
        AttemptErrorCode::None => "none",
        AttemptErrorCode::Unknown => "unknown",
    }
}

#[cfg(test)]
#[path = "cpa_observation/tests.rs"]
mod tests;
