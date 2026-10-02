//! Authenticated reads for operation receipts and logical request groups.
//!
//! Rows stay as stored. These handlers scrub every decrypted account secret
//! before the V3 forward-log converter maps an attempt. Model names are not
//! passed through pattern redaction, which would reflow their whitespace.

use std::collections::BTreeMap;

use axum::Json;
use axum::extract::{FromRequestParts, Path, Query, State};
use axum::http::request::Parts;
use schemars::JsonSchema;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

use crate::control::observability::{self, EnrichedForwardLog};
use crate::dashboard_v3::observability::forward_log_from_enriched;
use crate::dashboard_v3::{ForwardLog, V3ApiError};
use crate::db::request_logs::RequestAttemptRecord;
use crate::log_types::{
    LogReadError, OperationLog, OperationLogPage, OperationLogQuery, RequestLog, RequestLogPage,
    RequestLogQuery,
};
use crate::state::CoreState;

/// Attempt detail reuses the V3 forward-log DTO.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
#[schemars(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct RequestAttempts {
    pub items: Vec<ForwardLog>,
}

pub(crate) struct LogQuery<T>(pub T);

impl<T> FromRequestParts<CoreState> for LogQuery<T>
where
    T: DeserializeOwned + Send,
{
    type Rejection = V3ApiError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &CoreState,
    ) -> Result<Self, Self::Rejection> {
        Query::<T>::try_from_uri(&parts.uri)
            .map(|Query(value)| Self(value))
            .map_err(|_| V3ApiError::invalid_request_at(state, "invalid query"))
    }
}

pub(crate) async fn list_operations(
    State(state): State<CoreState>,
    LogQuery(query): LogQuery<OperationLogQuery>,
) -> Result<Json<OperationLogPage>, V3ApiError> {
    tokio::task::spawn_blocking(move || read_operation_logs(&state, &query))
        .await
        .map_err(|_| V3ApiError::internal("log reader failed"))?
        .map(Json)
}

pub(crate) async fn list_requests(
    State(state): State<CoreState>,
    LogQuery(query): LogQuery<RequestLogQuery>,
) -> Result<Json<RequestLogPage>, V3ApiError> {
    tokio::task::spawn_blocking(move || read_request_logs(&state, &query))
        .await
        .map_err(|_| V3ApiError::internal("log reader failed"))?
        .map(Json)
}

pub(crate) async fn list_request_attempts(
    State(state): State<CoreState>,
    Path(request_key): Path<String>,
) -> Result<Json<RequestAttempts>, V3ApiError> {
    tokio::task::spawn_blocking(move || read_request_attempts(&state, &request_key))
        .await
        .map_err(|_| V3ApiError::internal("log reader failed"))?
        .map(Json)
}

pub(crate) fn read_operation_logs(
    state: &CoreState,
    query: &OperationLogQuery,
) -> Result<OperationLogPage, V3ApiError> {
    let (mut page, secrets) = {
        let conn = read_connection(state)?;
        let snapshot = conn.unchecked_transaction().map_err(V3ApiError::internal)?;
        let secrets = account_secrets(state, &snapshot)?;
        let page = crate::db::operation_logs::query_operation_logs_on(&snapshot, query)
            .map_err(|error| map_log_read(state, error))?;
        (page, secrets)
    };
    for item in &mut page.items {
        scrub_operation(item, &secrets);
    }
    Ok(page)
}

pub(crate) fn read_request_logs(
    state: &CoreState,
    query: &RequestLogQuery,
) -> Result<RequestLogPage, V3ApiError> {
    let (mut page, secrets) = {
        let conn = read_connection(state)?;
        let snapshot = conn.unchecked_transaction().map_err(V3ApiError::internal)?;
        let secrets = account_secrets(state, &snapshot)?;
        let page = crate::db::request_logs::query_request_logs_on(&snapshot, query)
            .map_err(|error| map_log_read(state, error))?;
        (page, secrets)
    };
    for item in &mut page.items {
        scrub_request(item, &secrets);
    }
    Ok(page)
}

pub(crate) fn read_request_attempts(
    state: &CoreState,
    request_key: &str,
) -> Result<RequestAttempts, V3ApiError> {
    let (attempts, secrets) = {
        let conn = read_connection(state)?;
        let snapshot = conn.unchecked_transaction().map_err(V3ApiError::internal)?;
        let secrets = account_secrets(state, &snapshot)?;
        let attempts = crate::db::request_logs::query_request_attempts_on(&snapshot, request_key)
            .map_err(|error| map_log_read(state, error))?;
        (attempts, secrets)
    };
    Ok(RequestAttempts {
        items: attempts
            .into_iter()
            .map(|attempt| scrub_attempt(attempt, &secrets))
            .collect(),
    })
}

fn account_secrets(
    state: &CoreState,
    conn: &rusqlite::Connection,
) -> Result<BTreeMap<String, String>, V3ApiError> {
    observability::dashboard_account_secrets_on(conn, |cipher| state.decrypt_key(cipher).ok())
        .map_err(V3ApiError::internal)
}

fn read_connection(state: &CoreState) -> Result<rusqlite::Connection, V3ApiError> {
    // The live CoreState owns the database lifetime guard. Opening a reader
    // performs no initialization, migration, recovery or history pruning.
    // Its WAL snapshot keeps secrets, totals and page rows consistent while
    // unrelated mutations and request receipts use the original writer.
    let conn = rusqlite::Connection::open_with_flags(
        state.data_dir.join("data.sqlite"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(V3ApiError::internal)?;
    conn.busy_timeout(std::time::Duration::from_secs(1))
        .map_err(V3ApiError::internal)?;
    Ok(conn)
}

fn map_log_read(state: &CoreState, error: LogReadError) -> V3ApiError {
    match error {
        LogReadError::Invalid(message) => V3ApiError::invalid_request_at(state, message),
        LogReadError::Store(error) => V3ApiError::internal(error),
    }
}

fn scrub(text: &str, secrets: &BTreeMap<String, String>) -> String {
    observability::redact_known_secrets(text, secrets)
}

fn scrub_opt(text: Option<String>, secrets: &BTreeMap<String, String>) -> Option<String> {
    text.map(|text| scrub(&text, secrets))
}

fn scrub_operation(item: &mut OperationLog, secrets: &BTreeMap<String, String>) {
    item.action = scrub(&item.action, secrets);
    item.actor_id = scrub_opt(item.actor_id.take(), secrets);
    item.subject_type = scrub_opt(item.subject_type.take(), secrets);
    item.subject_id = scrub_opt(item.subject_id.take(), secrets);
    item.reason_code = scrub_opt(item.reason_code.take(), secrets);
    for field in &mut item.metadata.changed_fields {
        *field = scrub(field, secrets);
    }
    for id in &mut item.metadata.related_ids {
        *id = scrub(id, secrets);
    }
}

fn scrub_request(item: &mut RequestLog, secrets: &BTreeMap<String, String>) {
    item.request_key = scrub(&item.request_key, secrets);
    item.request_id = scrub_opt(item.request_id.take(), secrets);
    item.model = scrub(&item.model, secrets);
    item.requested_model = scrub_opt(item.requested_model.take(), secrets);
    item.resolved_alias = scrub_opt(item.resolved_alias.take(), secrets);
    item.upstream_model = scrub_opt(item.upstream_model.take(), secrets);
    item.client_key_id = scrub_opt(item.client_key_id.take(), secrets);
    item.client_key_name = scrub_opt(item.client_key_name.take(), secrets);
    item.provider_id = scrub_opt(item.provider_id.take(), secrets);
    item.account_id = scrub(&item.account_id, secrets);
    item.account_name = scrub(&item.account_name, secrets);
    item.route_account_id = scrub_opt(item.route_account_id.take(), secrets);
    item.credential_account_id = scrub_opt(item.credential_account_id.take(), secrets);
    item.route = scrub(&item.route, secrets);
}

fn scrub_attempt(record: RequestAttemptRecord, secrets: &BTreeMap<String, String>) -> ForwardLog {
    let RequestAttemptRecord {
        mut log,
        mut attribution,
    } = record;
    log.model = scrub(&log.model, secrets);
    log.account_id = scrub(&log.account_id, secrets);
    log.account_name = scrub(&log.account_name, secrets);
    log.route_account_id = scrub_opt(log.route_account_id.take(), secrets);
    log.provider_id = scrub_opt(log.provider_id.take(), secrets);
    log.credential_account_id = scrub_opt(log.credential_account_id.take(), secrets);
    log.client_key_id = scrub_opt(log.client_key_id.take(), secrets);
    log.client_key_name = scrub_opt(log.client_key_name.take(), secrets);
    log.route = scrub(&log.route, secrets);
    log.error_message = scrub_opt(log.error_message.take(), secrets);
    log.request_id = scrub_opt(log.request_id.take(), secrets);
    log.error_source = scrub_opt(log.error_source.take(), secrets);
    log.error_stage = scrub_opt(log.error_stage.take(), secrets);
    log.service_tier = scrub_opt(log.service_tier.take(), secrets);
    log.pricing_revision_id = scrub_opt(log.pricing_revision_id.take(), secrets);
    log.diagnostic = observability::redact_diagnostic(log.diagnostic.take(), secrets.values());
    attribution.requested_model = scrub_opt(attribution.requested_model.take(), secrets);
    attribution.resolved_alias = scrub_opt(attribution.resolved_alias.take(), secrets);
    attribution.upstream_model = scrub_opt(attribution.upstream_model.take(), secrets);
    attribution.native_cost_unit = scrub_opt(attribution.native_cost_unit.take(), secrets);
    attribution.native_cost_currency = scrub_opt(attribution.native_cost_currency.take(), secrets);
    forward_log_from_enriched(EnrichedForwardLog {
        log,
        requested_model: attribution.requested_model,
        resolved_alias: attribution.resolved_alias,
        upstream_model: attribution.upstream_model,
        native_cost_value: attribution.native_cost_value,
        native_cost_unit: attribution.native_cost_unit,
        native_cost_currency: attribution.native_cost_currency,
    })
}

#[cfg(test)]
mod tests;
