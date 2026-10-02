//! Shared user-operation and logical-request log types.
//!
//! Wire names are camelCase. Operation metadata is an allowlist: field names,
//! effect counts, a revision, a compensation flag, and opaque related ids.
//! Callers supply `actor_id` only when they already know a non-secret
//! identifier. These types never mint an actor from a session or bearer token.

use chrono::{DateTime, SecondsFormat, Utc};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::models::UNATTRIBUTED_KEY_FILTER;
use crate::redaction::redact_text;

pub const MAX_PAGE_LIMIT: i64 = 200;
pub const DEFAULT_PAGE_LIMIT: i64 = 100;
pub const MAX_PAGE_OFFSET: i64 = 100_000;
pub const MAX_CODE_LEN: usize = 64;
pub const MAX_OPAQUE_ID_LEN: usize = 128;
pub const MAX_FILTER_LEN: usize = 256;
pub const MAX_CHANGED_FIELDS: usize = 32;
pub const MAX_FIELD_NAME_LEN: usize = 64;
pub const MAX_RELATED_IDS: usize = 16;
pub const MAX_REQUEST_ID_LEN: usize = 256;

const LOGICAL_STATUSES: &[&str] = &[
    "success",
    "error",
    "client_error",
    "streaming",
    "outcome_unknown",
    "cancelled",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
#[schemars(rename_all = "snake_case")]
pub enum OperationSource {
    Dashboard,
    Cli,
    Desktop,
}

impl OperationSource {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Dashboard => "dashboard",
            Self::Cli => "cli",
            Self::Desktop => "desktop",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
#[schemars(rename_all = "snake_case")]
pub enum OperationOutcome {
    Pending,
    Success,
    Rejected,
    Failed,
    Partial,
    Compensated,
}

impl OperationOutcome {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Success => "success",
            Self::Rejected => "rejected",
            Self::Failed => "failed",
            Self::Partial => "partial",
            Self::Compensated => "compensated",
        }
    }

    pub fn is_terminal(self) -> bool {
        !matches!(self, Self::Pending)
    }

    pub fn parse(value: &str) -> Option<Self> {
        Some(match value {
            "pending" => Self::Pending,
            "success" => Self::Success,
            "rejected" => Self::Rejected,
            "failed" => Self::Failed,
            "partial" => Self::Partial,
            "compensated" => Self::Compensated,
            _ => return None,
        })
    }
}

/// Allowlisted, non-secret facts about one user operation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Default)]
#[serde(rename_all = "camelCase", default)]
#[schemars(rename_all = "camelCase", deny_unknown_fields)]
pub struct OperationMetadata {
    pub changed_fields: Vec<String>,
    pub requested_count: Option<u32>,
    pub completed_count: Option<u32>,
    pub failed_count: Option<u32>,
    pub revision: Option<u64>,
    pub compensated: Option<bool>,
    pub related_ids: Vec<String>,
}

/// One user-operation receipt. `completed_at` is null only while `pending`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
#[schemars(rename_all = "camelCase", deny_unknown_fields)]
pub struct OperationLog {
    pub operation_id: String,
    pub started_at: DateTime<Utc>,
    pub completed_at: Option<DateTime<Utc>>,
    pub action: String,
    pub source: OperationSource,
    pub actor_id: Option<String>,
    pub subject_type: Option<String>,
    pub subject_id: Option<String>,
    pub outcome: OperationOutcome,
    pub reason_code: Option<String>,
    pub metadata: OperationMetadata,
}

/// Terminal facts applied once to the pending row for `operation_id`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OperationFinish {
    pub completed_at: DateTime<Utc>,
    pub outcome: OperationOutcome,
    pub reason_code: Option<String>,
    pub metadata: OperationMetadata,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[schemars(rename_all = "camelCase", deny_unknown_fields)]
pub struct OperationLogQuery {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub offset: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub action: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<OperationSource>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outcome: Option<OperationOutcome>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subject_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subject_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start_time: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub end_time: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
#[schemars(rename_all = "camelCase", deny_unknown_fields)]
pub struct OperationLogPage {
    pub items: Vec<OperationLog>,
    pub total: i64,
    pub limit: i64,
    pub offset: i64,
}

/// One logical request projected from `forward_logs` attempts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
#[schemars(rename_all = "camelCase", deny_unknown_fields)]
pub struct RequestLog {
    pub request_key: String,
    pub request_id: Option<String>,
    pub timestamp: String,
    pub status: String,
    pub http_status: Option<i32>,
    pub model: String,
    pub requested_model: Option<String>,
    pub resolved_alias: Option<String>,
    pub upstream_model: Option<String>,
    /// Upstream attempts. A pre-upstream `attempt = 0` row contributes zero.
    pub attempt_count: i64,
    /// Stored `forward_logs` rows in this logical request, including attempt 0.
    pub recorded_row_count: i64,
    pub prompt_tokens: i64,
    pub completion_tokens: i64,
    pub cached_tokens: i64,
    /// Newest attempt's cumulative elapsed, only when that attempt recorded one.
    pub duration_ms: Option<i64>,
    pub is_legacy: bool,
    pub client_key_id: Option<String>,
    pub client_key_name: Option<String>,
    pub provider_id: Option<String>,
    pub account_id: String,
    pub account_name: String,
    pub route_account_id: Option<String>,
    pub credential_account_id: Option<String>,
    pub route: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Default)]
#[serde(rename_all = "camelCase")]
#[schemars(rename_all = "camelCase", deny_unknown_fields)]
pub struct RequestLogSummary {
    pub total_requests: i64,
    pub total_attempts: i64,
    pub prompt_tokens: i64,
    pub completion_tokens: i64,
    pub cached_tokens: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
#[schemars(rename_all = "camelCase", deny_unknown_fields)]
pub struct RequestLogPage {
    pub items: Vec<RequestLog>,
    pub total: i64,
    pub limit: i64,
    pub offset: i64,
    pub summary: RequestLogSummary,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[schemars(rename_all = "camelCase", deny_unknown_fields)]
pub struct RequestLogQuery {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub offset: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub route_account_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub credential_account_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start_time: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub end_time: Option<String>,
}

#[derive(Debug)]
pub enum LogReadError {
    Invalid(String),
    Store(anyhow::Error),
}

impl std::fmt::Display for LogReadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Invalid(message) => f.write_str(message),
            Self::Store(error) => write!(f, "{error}"),
        }
    }
}

impl From<anyhow::Error> for LogReadError {
    fn from(error: anyhow::Error) -> Self {
        Self::Store(error)
    }
}

impl From<rusqlite::Error> for LogReadError {
    fn from(error: rusqlite::Error) -> Self {
        Self::Store(error.into())
    }
}

#[derive(Debug, Clone)]
pub(crate) struct NormalizedOperationQuery {
    pub limit: i64,
    pub offset: i64,
    pub action: Option<String>,
    pub source: Option<OperationSource>,
    pub outcome: Option<OperationOutcome>,
    pub subject_type: Option<String>,
    pub subject_id: Option<String>,
    pub start_time: Option<String>,
    pub end_time: Option<String>,
}

#[derive(Debug, Clone)]
pub(crate) struct NormalizedRequestQuery {
    pub request_id: Option<String>,
    pub limit: i64,
    pub offset: i64,
    pub status: Option<String>,
    pub provider_id: Option<String>,
    pub account_id: Option<String>,
    pub route_account_id: Option<String>,
    pub credential_account_id: Option<String>,
    pub key_id: Option<String>,
    pub unattributed_key: bool,
    pub model: Option<String>,
    pub start_time: Option<String>,
    pub end_time: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CanonicalOperation {
    pub operation_id: String,
    pub started_at: String,
    pub completed_at: Option<String>,
    pub action: String,
    pub source: OperationSource,
    pub actor_id: Option<String>,
    pub subject_type: Option<String>,
    pub subject_id: Option<String>,
    pub outcome: OperationOutcome,
    pub reason_code: Option<String>,
    pub metadata_json: String,
}

/// Group key for one `forward_logs` row.
///
/// A non-blank `request_id` keeps its exact bytes after `request:`. Null and
/// blank ids each stand alone as `legacy:<rowid>`, so a stored id of
/// `legacy:1` cannot collide with row 1.
pub fn request_log_key(request_id: Option<&str>, row_id: i64) -> String {
    if let Some(request_id) = request_id.filter(|value| !value.trim().is_empty()) {
        format!("request:{request_id}")
    } else {
        format!("legacy:{row_id}")
    }
}

pub fn parse_request_log_key(value: &str) -> Result<String, String> {
    if value.len() > MAX_REQUEST_ID_LEN + "request:".len() || value.chars().any(char::is_control) {
        return Err("invalid request_key".into());
    }
    if let Some(id) = value.strip_prefix("legacy:") {
        if !(1..=19).contains(&id.len())
            || !id.bytes().all(|byte| byte.is_ascii_digit())
            || id.starts_with('0')
        {
            return Err("invalid request_key".into());
        }
        let row_id: i64 = id.parse().map_err(|_| "invalid request_key".to_string())?;
        if row_id <= 0 {
            return Err("invalid request_key".into());
        }
        return Ok(format!("legacy:{row_id}"));
    }
    if let Some(request_id) = value.strip_prefix("request:") {
        if request_id.trim().is_empty() || request_id.len() > MAX_REQUEST_ID_LEN {
            return Err("invalid request_key".into());
        }
        return Ok(format!("request:{request_id}"));
    }
    Err("invalid request_key".into())
}

/// Logical outcome. Every stored `success_*` variant is success; other
/// statuses, including `streaming` and `outcome_unknown`, stay as recorded.
pub fn logical_request_status(status: &str) -> &str {
    if status == "success" || status.starts_with("success_") {
        "success"
    } else {
        status
    }
}

pub(crate) fn normalize_operation_query(
    query: &OperationLogQuery,
) -> Result<NormalizedOperationQuery, String> {
    let (limit, offset) = normalize_page(query.limit, query.offset)?;
    let (start_time, end_time) =
        normalize_window(query.start_time.as_deref(), query.end_time.as_deref())?;
    Ok(NormalizedOperationQuery {
        limit,
        offset,
        action: optional_code(query.action.as_deref(), "action")?,
        source: query.source,
        outcome: query.outcome,
        subject_type: optional_code(query.subject_type.as_deref(), "subject_type")?,
        subject_id: optional_opaque(query.subject_id.as_deref(), "subject_id", false)?,
        start_time,
        end_time,
    })
}

pub(crate) fn normalize_request_query(
    query: &RequestLogQuery,
) -> Result<NormalizedRequestQuery, String> {
    let (limit, offset) = normalize_page(query.limit, query.offset)?;
    let (start_time, end_time) =
        normalize_window(query.start_time.as_deref(), query.end_time.as_deref())?;
    let status = match query.status.as_deref() {
        None => None,
        Some(status) if LOGICAL_STATUSES.contains(&status) => Some(status.to_string()),
        Some(_) => return Err("invalid query status".into()),
    };
    let key_id = query
        .key_id
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let unattributed_key = key_id == Some(UNATTRIBUTED_KEY_FILTER);
    let key_id = if unattributed_key {
        None
    } else {
        optional_filter(key_id, "key_id")?
    };
    Ok(NormalizedRequestQuery {
        request_id: query
            .request_id
            .as_deref()
            .filter(|value| !value.trim().is_empty())
            .map(|value| {
                if value.len() > MAX_REQUEST_ID_LEN || value.chars().any(char::is_control) {
                    Err("invalid query request_id".to_string())
                } else {
                    Ok(value.to_string())
                }
            })
            .transpose()?,
        limit,
        offset,
        status,
        provider_id: optional_filter(query.provider_id.as_deref(), "provider_id")?,
        account_id: optional_filter(query.account_id.as_deref(), "account_id")?,
        route_account_id: optional_filter(query.route_account_id.as_deref(), "route_account_id")?,
        credential_account_id: optional_filter(
            query.credential_account_id.as_deref(),
            "credential_account_id",
        )?,
        key_id,
        unattributed_key,
        model: optional_filter(query.model.as_deref(), "model")?,
        start_time,
        end_time,
    })
}

pub(crate) fn canonicalize_operation(
    operation: &OperationLog,
) -> Result<CanonicalOperation, String> {
    validate_times(operation.outcome, operation.completed_at)?;
    let metadata = sanitize_metadata_write(&operation.metadata)?;
    Ok(CanonicalOperation {
        operation_id: canonical_operation_id(&operation.operation_id)?,
        started_at: format_time(operation.started_at),
        completed_at: operation.completed_at.map(format_time),
        action: required_code(&operation.action, "action")?,
        source: operation.source,
        actor_id: optional_actor(operation.actor_id.as_deref())?,
        subject_type: optional_code(operation.subject_type.as_deref(), "subject_type")?,
        subject_id: optional_opaque(operation.subject_id.as_deref(), "subject_id", false)?,
        outcome: operation.outcome,
        reason_code: optional_code(operation.reason_code.as_deref(), "reason_code")?,
        metadata_json: serde_json::to_string(&metadata)
            .map_err(|_| "invalid operation metadata")?,
    })
}

pub(crate) fn canonicalize_finish(
    operation_id: &str,
    finish: &OperationFinish,
) -> Result<(String, CanonicalFinish), String> {
    if !finish.outcome.is_terminal() {
        return Err("invalid operation outcome".into());
    }
    let metadata = sanitize_metadata_write(&finish.metadata)?;
    Ok((
        canonical_operation_id(operation_id)?,
        CanonicalFinish {
            completed_at: format_time(finish.completed_at),
            outcome: finish.outcome,
            reason_code: optional_code(finish.reason_code.as_deref(), "reason_code")?,
            metadata_json: serde_json::to_string(&metadata)
                .map_err(|_| "invalid operation metadata")?,
        },
    ))
}

#[derive(Debug, Clone)]
pub(crate) struct CanonicalFinish {
    pub completed_at: String,
    pub outcome: OperationOutcome,
    pub reason_code: Option<String>,
    pub metadata_json: String,
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn operation_from_stored(
    operation_id: String,
    started_at: String,
    completed_at: Option<String>,
    action: String,
    source: String,
    actor_id: Option<String>,
    subject_type: Option<String>,
    subject_id: Option<String>,
    outcome: String,
    reason_code: Option<String>,
    metadata_json: String,
) -> anyhow::Result<OperationLog> {
    let outcome = OperationOutcome::parse(&outcome)
        .ok_or_else(|| anyhow::anyhow!("invalid stored operation outcome"))?;
    let source = match source.as_str() {
        "dashboard" => OperationSource::Dashboard,
        "cli" => OperationSource::Cli,
        "desktop" => OperationSource::Desktop,
        _ => anyhow::bail!("invalid stored operation source"),
    };
    let completed_at = completed_at
        .map(|value| parse_stored_time(&value))
        .transpose()?;
    Ok(OperationLog {
        operation_id: safe_operation_id(&operation_id),
        started_at: parse_stored_time(&started_at)?,
        completed_at,
        action: safe_code_or_redacted(&action),
        source,
        actor_id: actor_id.as_deref().and_then(safe_actor_read),
        subject_type: subject_type.as_deref().and_then(safe_code_read),
        subject_id: subject_id.as_deref().and_then(safe_opaque_read),
        outcome,
        reason_code: reason_code.as_deref().and_then(safe_code_read),
        metadata: metadata_from_stored(&metadata_json),
    })
}

fn metadata_from_stored(metadata_json: &str) -> OperationMetadata {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(metadata_json) else {
        return OperationMetadata::default();
    };
    let Some(object) = value.as_object() else {
        return OperationMetadata::default();
    };
    let changed_fields = object
        .get("changedFields")
        .and_then(|value| value.as_array())
        .map(|values| {
            values
                .iter()
                .filter_map(|value| value.as_str())
                .filter_map(safe_field_name_read)
                .take(MAX_CHANGED_FIELDS)
                .collect()
        })
        .unwrap_or_default();
    let related_ids = object
        .get("relatedIds")
        .and_then(|value| value.as_array())
        .map(|values| {
            values
                .iter()
                .filter_map(|value| value.as_str())
                .filter_map(safe_opaque_read)
                .take(MAX_RELATED_IDS)
                .collect()
        })
        .unwrap_or_default();
    OperationMetadata {
        changed_fields,
        requested_count: bounded_count(object.get("requestedCount")),
        completed_count: bounded_count(object.get("completedCount")),
        failed_count: bounded_count(object.get("failedCount")),
        revision: object.get("revision").and_then(serde_json::Value::as_u64),
        compensated: object
            .get("compensated")
            .and_then(serde_json::Value::as_bool),
        related_ids,
    }
}

fn bounded_count(value: Option<&serde_json::Value>) -> Option<u32> {
    let number = value?.as_u64()?;
    u32::try_from(number).ok()
}

fn sanitize_metadata_write(metadata: &OperationMetadata) -> Result<OperationMetadata, String> {
    if metadata.changed_fields.len() > MAX_CHANGED_FIELDS {
        return Err("invalid operation metadata".into());
    }
    if metadata.related_ids.len() > MAX_RELATED_IDS {
        return Err("invalid operation metadata".into());
    }
    let mut changed_fields = Vec::with_capacity(metadata.changed_fields.len());
    for name in &metadata.changed_fields {
        changed_fields.push(required_field_name(name)?);
    }
    let mut related_ids = Vec::with_capacity(metadata.related_ids.len());
    for id in &metadata.related_ids {
        related_ids.push(
            optional_opaque(Some(id), "related_id", false)?.ok_or("invalid operation metadata")?,
        );
    }
    let metadata = OperationMetadata {
        changed_fields,
        requested_count: metadata.requested_count,
        completed_count: metadata.completed_count,
        failed_count: metadata.failed_count,
        revision: metadata.revision,
        compensated: metadata.compensated,
        related_ids,
    };
    let encoded = serde_json::to_string(&metadata).map_err(|_| "invalid operation metadata")?;
    if encoded.len() > 4096 {
        return Err("invalid operation metadata".into());
    }
    Ok(metadata)
}

fn validate_times(
    outcome: OperationOutcome,
    completed_at: Option<DateTime<Utc>>,
) -> Result<(), String> {
    if outcome.is_terminal() == completed_at.is_some() {
        Ok(())
    } else {
        Err("invalid operation outcome".into())
    }
}

fn canonical_operation_id(value: &str) -> Result<String, String> {
    let id = uuid::Uuid::parse_str(value.trim()).map_err(|_| "invalid operation_id")?;
    Ok(id.hyphenated().to_string())
}

fn safe_operation_id(value: &str) -> String {
    uuid::Uuid::parse_str(value)
        .map(|id| id.hyphenated().to_string())
        .unwrap_or_else(|_| "00000000-0000-0000-0000-000000000000".to_string())
}

fn format_time(value: DateTime<Utc>) -> String {
    value.to_rfc3339_opts(SecondsFormat::Millis, true)
}

fn parse_stored_time(value: &str) -> anyhow::Result<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(value)
        .map(|value| value.with_timezone(&Utc))
        .map_err(|error| anyhow::anyhow!("invalid stored operation time: {error}"))
}

fn normalize_page(limit: Option<i64>, offset: Option<i64>) -> Result<(i64, i64), String> {
    let limit = limit.unwrap_or(DEFAULT_PAGE_LIMIT);
    let offset = offset.unwrap_or(0);
    if !(1..=MAX_PAGE_LIMIT).contains(&limit) {
        return Err("invalid query limit".into());
    }
    if !(0..=MAX_PAGE_OFFSET).contains(&offset) {
        return Err("invalid query offset".into());
    }
    Ok((limit, offset))
}

fn normalize_window(
    start_time: Option<&str>,
    end_time: Option<&str>,
) -> Result<(Option<String>, Option<String>), String> {
    let start_time = optional_time(start_time, "start_time")?;
    let end_time = optional_time(end_time, "end_time")?;
    if start_time
        .as_ref()
        .zip(end_time.as_ref())
        .is_some_and(|(start, end)| start > end)
    {
        return Err("invalid query time range".into());
    }
    Ok((start_time, end_time))
}

fn optional_time(value: Option<&str>, name: &str) -> Result<Option<String>, String> {
    let Some(value) = value.map(str::trim).filter(|value| !value.is_empty()) else {
        return Ok(None);
    };
    let parsed = DateTime::parse_from_rfc3339(value)
        .map(|time| {
            time.with_timezone(&Utc)
                .to_rfc3339_opts(SecondsFormat::Millis, true)
        })
        .map_err(|_| format!("invalid query {name}"))?;
    Ok(Some(parsed))
}

fn optional_code(value: Option<&str>, name: &str) -> Result<Option<String>, String> {
    match value.map(str::trim).filter(|value| !value.is_empty()) {
        None => Ok(None),
        Some(value) => required_code(value, name).map(Some),
    }
}

fn required_code(value: &str, name: &str) -> Result<String, String> {
    if value.len() > MAX_CODE_LEN || !is_stable_code(value) || contains_secret(value) {
        return Err(format!("invalid operation {name}"));
    }
    Ok(value.to_string())
}

fn optional_filter(value: Option<&str>, name: &str) -> Result<Option<String>, String> {
    match value.map(str::trim).filter(|value| !value.is_empty()) {
        None => Ok(None),
        Some(value) if value.len() <= MAX_FILTER_LEN && !value.chars().any(char::is_control) => {
            Ok(Some(value.to_string()))
        }
        Some(_) => Err(format!("invalid query {name}")),
    }
}

fn optional_actor(value: Option<&str>) -> Result<Option<String>, String> {
    optional_opaque(value, "actor_id", true)
}

fn optional_opaque(value: Option<&str>, name: &str, actor: bool) -> Result<Option<String>, String> {
    let Some(value) = value.map(str::trim).filter(|value| !value.is_empty()) else {
        return Ok(None);
    };
    if !is_opaque_id(value) || contains_secret(value) || (actor && is_session_token_shape(value)) {
        return Err(format!("invalid operation {name}"));
    }
    Ok(Some(value.to_string()))
}

pub(crate) fn operation_subject(value: &str) -> Option<String> {
    optional_opaque(Some(value), "subject_id", false)
        .ok()
        .flatten()
}

fn required_field_name(value: &str) -> Result<String, String> {
    if value.len() > MAX_FIELD_NAME_LEN
        || !is_field_name(value)
        || crate::redaction::is_sensitive_key(value)
        || contains_secret(value)
    {
        return Err("invalid operation metadata".into());
    }
    Ok(value.to_string())
}

fn safe_code_or_redacted(value: &str) -> String {
    safe_code_read(value).unwrap_or_else(|| "redacted".to_string())
}

fn safe_code_read(value: &str) -> Option<String> {
    let value = value.trim();
    (value.len() <= MAX_CODE_LEN && is_stable_code(value) && !contains_secret(value))
        .then(|| value.to_string())
}

fn safe_actor_read(value: &str) -> Option<String> {
    let value = value.trim();
    (is_opaque_id(value) && !contains_secret(value) && !is_session_token_shape(value))
        .then(|| value.to_string())
}

fn safe_opaque_read(value: &str) -> Option<String> {
    let value = value.trim();
    (is_opaque_id(value) && !contains_secret(value)).then(|| value.to_string())
}

fn safe_field_name_read(value: &str) -> Option<String> {
    (value.len() <= MAX_FIELD_NAME_LEN
        && is_field_name(value)
        && !crate::redaction::is_sensitive_key(value)
        && !contains_secret(value))
    .then(|| value.to_string())
}

fn is_stable_code(value: &str) -> bool {
    let mut parts = value.split('.');
    let Some(first) = parts.next() else {
        return false;
    };
    is_code_part(first) && parts.all(is_code_part)
}

fn is_code_part(value: &str) -> bool {
    let mut chars = value.chars();
    matches!(chars.next(), Some(ch) if ch.is_ascii_lowercase())
        && chars.all(|ch| ch.is_ascii_alphanumeric() || ch == '_')
}

fn is_field_name(value: &str) -> bool {
    let mut chars = value.chars();
    matches!(chars.next(), Some(ch) if ch.is_ascii_alphabetic())
        && chars.all(|ch| ch.is_ascii_alphanumeric() || ch == '_')
}

fn is_opaque_id(value: &str) -> bool {
    let mut chars = value.chars();
    matches!(chars.next(), Some(ch) if ch.is_ascii_alphanumeric())
        && value.len() <= MAX_OPAQUE_ID_LEN
        && chars.all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '.' | ':' | '@' | '-'))
}

fn is_session_token_shape(value: &str) -> bool {
    value.len() == 32 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn contains_secret(value: &str) -> bool {
    let lower = value.to_ascii_lowercase();
    lower.contains("sk-")
        || lower.contains("bearer")
        || lower.contains("authorization")
        || lower.contains("api_key")
        || lower.contains("api-key")
        || lower.contains("-----begin")
        || lower.starts_with("eyj")
        || redact_text(value) != value
}
