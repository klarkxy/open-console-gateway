//! Read projection over `forward_logs`.
//!
//! Rows are grouped in SQL before filters and pagination. Identity predicates
//! must all match one existing attempt; the selected logical request then
//! keeps every attempt. The newest attempt, including `streaming` and
//! `outcome_unknown`, supplies the logical outcome. `attempt = 0` adds no
//! upstream attempt. Duration is that newest row's `duration_ms` only.

use rusqlite::{params_from_iter, types::Value};

use super::{Database, forward_log_from_row};
use crate::log_types::{
    LogReadError, RequestLog, RequestLogPage, RequestLogQuery, RequestLogSummary,
    normalize_request_query, parse_request_log_key,
};
use crate::models::{ForwardLog, ForwardLogNativeAttribution};

const REQUEST_GROUPS: &str = r#"
WITH newest AS (
    SELECT
        request_key, timestamp, logical_status, http_status, model,
        requested_model, resolved_alias, upstream_model, duration_ms, request_id,
        is_legacy, client_key_id, client_key_name, provider_id, account_id,
        account_name, route_account_id, credential_account_id, route
    FROM (
        SELECT
            request_group_key AS request_key,
            timestamp,
            CASE
                WHEN status = 'success' OR status LIKE 'success\_%' ESCAPE '\'
                THEN 'success'
                ELSE status
            END AS logical_status,
            http_status,
            model,
            NULLIF(requested_model, '') AS requested_model,
            NULLIF(resolved_alias, '') AS resolved_alias,
            NULLIF(upstream_model, '') AS upstream_model,
            duration_ms,
            CASE
                WHEN request_group_key LIKE 'request:%' THEN request_id
                ELSE NULL
            END AS request_id,
            CASE
                WHEN request_group_key LIKE 'legacy:%' THEN 1
                ELSE 0
            END AS is_legacy,
            client_key_id,
            client_key_name,
            provider_id,
            account_id,
            account_name,
            route_account_id,
            credential_account_id,
            route,
            ROW_NUMBER() OVER (
                PARTITION BY request_group_key
                ORDER BY COALESCE(attempt, -1) DESC, timestamp DESC, id DESC
            ) AS rn
        FROM forward_logs
    )
    WHERE rn = 1
),
agg AS (
    SELECT
        request_group_key AS request_key,
        COUNT(*) AS recorded_row_count,
        COALESCE(SUM(CASE
            WHEN attempt = 0 THEN 0
            -- Older local-rejection producers used attempt=1, despite having
            -- no selected upstream credential or route. Preserve those rows
            -- while counting their actual sends honestly.
            WHEN attempt IS NOT NULL AND account_id = '' AND route = '' THEN 0
            -- These selected-route receipts are explicitly emitted before
            -- transport starts. Keep their sequence for logical outcome,
            -- without pretending a rejected preparation sent upstream.
            WHEN error_source = 'gateway' AND error_stage IN (
                'credential', 'local_policy_skip', 'recovery_capacity',
                'resource_wait', 'request_budget'
            ) THEN 0
            ELSE 1
        END), 0) AS attempt_count,
        COALESCE(SUM(prompt_tokens), 0) AS prompt_tokens,
        COALESCE(SUM(completion_tokens), 0) AS completion_tokens,
        COALESCE(SUM(cached_tokens), 0) AS cached_tokens
    FROM forward_logs
    GROUP BY request_group_key
),
grouped AS (
    SELECT
        newest.request_key AS request_key,
        newest.timestamp AS timestamp,
        newest.logical_status AS logical_status,
        newest.http_status AS http_status,
        newest.model AS model,
        newest.requested_model AS requested_model,
        newest.resolved_alias AS resolved_alias,
        newest.upstream_model AS upstream_model,
        newest.duration_ms AS duration_ms,
        newest.request_id AS request_id,
        newest.is_legacy AS is_legacy,
        newest.client_key_id AS client_key_id,
        newest.client_key_name AS client_key_name,
        newest.provider_id AS provider_id,
        newest.account_id AS account_id,
        newest.account_name AS account_name,
        newest.route_account_id AS route_account_id,
        newest.credential_account_id AS credential_account_id,
        newest.route AS route,
        agg.recorded_row_count AS recorded_row_count,
        agg.attempt_count AS attempt_count,
        agg.prompt_tokens AS prompt_tokens,
        agg.completion_tokens AS completion_tokens,
        agg.cached_tokens AS cached_tokens
    FROM newest
    JOIN agg ON agg.request_key = newest.request_key
)
"#;

const ATTEMPT_SELECT: &str =
    "SELECT id, timestamp, model, account_id, account_name, status, http_status, route,
    prompt_tokens, completion_tokens, cached_tokens, cache_creation_tokens, cost,
    pricing_revision_id, quota_multiplier, local_adjustment_multiplier,
    service_tier, cost_state, error_message, request_id, attempt,
    error_source, error_stage, duration_ms, diagnostic_json,
    client_key_id, client_key_name, route_account_id, provider_id,
    credential_account_id, raw_cost_usd, quota_debit, effective_paid_cost_usd,
    requested_model, resolved_alias, upstream_model,
    native_cost_value, native_cost_unit, native_cost_currency
    FROM forward_logs";

pub(crate) struct RequestAttemptRecord {
    pub log: ForwardLog,
    pub attribution: ForwardLogNativeAttribution,
}

impl Database {
    pub fn query_request_logs(
        &self,
        query: &RequestLogQuery,
    ) -> std::result::Result<RequestLogPage, LogReadError> {
        query_request_logs_on(&self.conn, query)
    }

    #[cfg(test)]
    pub(crate) fn explain_request_logs(&self, query: &RequestLogQuery) -> anyhow::Result<String> {
        let query = normalize_request_query(query).map_err(|message| anyhow::anyhow!(message))?;
        let (identity_sql, identity_params) = identity_exists(&query);
        let mut params = vec![
            text_or_null(query.status.as_deref()),
            text_or_null(query.start_time.as_deref()),
            text_or_null(query.end_time.as_deref()),
        ];
        params.extend(identity_params);
        params.push(Value::Integer(query.limit));
        params.push(Value::Integer(query.offset));
        let sql = format!(
            "EXPLAIN QUERY PLAN
             {REQUEST_GROUPS}
             SELECT request_key FROM grouped
             WHERE (?1 IS NULL OR logical_status = ?1)
               AND (?2 IS NULL OR julianday(timestamp) >= julianday(?2))
               AND (?3 IS NULL OR julianday(timestamp) <= julianday(?3))
               {identity_sql}
             ORDER BY timestamp DESC, request_key ASC
             LIMIT ? OFFSET ?"
        );
        explain(&self.conn, &sql, &params)
    }

    #[cfg(test)]
    pub(crate) fn explain_request_attempts(&self, request_key: &str) -> anyhow::Result<String> {
        let request_key =
            parse_request_log_key(request_key).map_err(|message| anyhow::anyhow!(message))?;
        explain(
            &self.conn,
            "EXPLAIN QUERY PLAN
             SELECT id FROM forward_logs
             WHERE request_group_key = ?1
             ORDER BY COALESCE(attempt, -1) ASC, id ASC",
            &[Value::Text(request_key)],
        )
    }
}

#[cfg(test)]
fn explain(conn: &rusqlite::Connection, sql: &str, params: &[Value]) -> anyhow::Result<String> {
    let mut stmt = conn.prepare(sql)?;
    let rows = stmt.query_map(params_from_iter(params.iter()), |row| {
        let mut columns = Vec::new();
        for index in 0..row.as_ref().column_count() {
            columns.push(row.get::<_, String>(index).unwrap_or_default());
        }
        Ok(columns.join("|"))
    })?;
    let mut lines = Vec::new();
    for row in rows {
        lines.push(row?);
    }
    Ok(lines.join("\n"))
}

fn identity_exists(query: &crate::log_types::NormalizedRequestQuery) -> (String, Vec<Value>) {
    let mut predicates = Vec::new();
    let mut params = Vec::new();
    let mut push_text = |sql: &str, value: &str| {
        predicates.push(sql.to_string());
        params.push(Value::Text(value.to_string()));
    };
    if let Some(value) = &query.request_id {
        push_text("attempt.request_id = ?", value);
    }
    if let Some(value) = &query.provider_id {
        push_text("attempt.provider_id = ?", value);
    }
    if let Some(value) = &query.account_id {
        push_text("attempt.account_id = ?", value);
    }
    if let Some(value) = &query.route_account_id {
        push_text("attempt.route_account_id = ?", value);
    }
    if let Some(value) = &query.credential_account_id {
        push_text("attempt.credential_account_id = ?", value);
    }
    if query.unattributed_key {
        predicates.push("attempt.client_key_id IS NULL".to_string());
    } else if let Some(value) = &query.key_id {
        push_text("attempt.client_key_id = ?", value);
    }
    if let Some(value) = &query.model {
        predicates.push(
            "(attempt.model = ? OR attempt.requested_model = ? OR attempt.resolved_alias = ? OR attempt.upstream_model = ?)"
                .to_string(),
        );
        for _ in 0..4 {
            params.push(Value::Text(value.clone()));
        }
    }
    if predicates.is_empty() {
        return (String::new(), Vec::new());
    }
    (
        format!(
            " AND EXISTS (
                SELECT 1 FROM forward_logs attempt
                WHERE attempt.request_group_key = grouped.request_key
                  AND {}
             )",
            predicates.join(" AND ")
        ),
        params,
    )
}

fn text_or_null(value: Option<&str>) -> Value {
    value.map_or(Value::Null, |value| Value::Text(value.to_string()))
}

fn request_log_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<RequestLog> {
    Ok(RequestLog {
        request_key: row.get(0)?,
        request_id: row.get(1)?,
        timestamp: row.get(2)?,
        status: row.get(3)?,
        http_status: row.get(4)?,
        model: row.get(5)?,
        requested_model: row.get(6)?,
        resolved_alias: row.get(7)?,
        upstream_model: row.get(8)?,
        attempt_count: row.get(9)?,
        recorded_row_count: row.get(10)?,
        prompt_tokens: row.get(11)?,
        completion_tokens: row.get(12)?,
        cached_tokens: row.get(13)?,
        duration_ms: row.get(14)?,
        is_legacy: row.get::<_, i64>(15)? != 0,
        client_key_id: row.get(16)?,
        client_key_name: row.get(17)?,
        provider_id: row.get(18)?,
        account_id: row.get(19)?,
        account_name: row.get(20)?,
        route_account_id: row.get(21)?,
        credential_account_id: row.get(22)?,
        route: row.get(23)?,
    })
}

fn attempt_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<RequestAttemptRecord> {
    let log = forward_log_from_row(row)?;
    let attribution = ForwardLogNativeAttribution {
        requested_model: row.get(33)?,
        resolved_alias: row.get(34)?,
        upstream_model: row.get(35)?,
        native_cost_value: row.get(36)?,
        native_cost_unit: row.get(37)?,
        native_cost_currency: row.get(38)?,
    };
    Ok(RequestAttemptRecord { log, attribution })
}

pub(crate) fn query_request_logs_on(
    conn: &rusqlite::Connection,
    query: &RequestLogQuery,
) -> std::result::Result<RequestLogPage, LogReadError> {
    let query = normalize_request_query(query).map_err(LogReadError::Invalid)?;
    let (identity_sql, identity_params) = identity_exists(&query);
    let mut filter_params = vec![
        text_or_null(query.status.as_deref()),
        text_or_null(query.start_time.as_deref()),
        text_or_null(query.end_time.as_deref()),
    ];
    filter_params.extend(identity_params);
    let where_sql = format!(
        " WHERE (?1 IS NULL OR logical_status = ?1)
            AND (?2 IS NULL OR julianday(timestamp) >= julianday(?2))
            AND (?3 IS NULL OR julianday(timestamp) <= julianday(?3))
            {identity_sql}"
    );
    let summary = conn
        .query_row(
            &format!(
                "{REQUEST_GROUPS}
                 SELECT COUNT(*),
                        COALESCE(SUM(attempt_count), 0),
                        COALESCE(SUM(prompt_tokens), 0),
                        COALESCE(SUM(completion_tokens), 0),
                        COALESCE(SUM(cached_tokens), 0)
                 FROM grouped
                 {where_sql}"
            ),
            params_from_iter(filter_params.iter()),
            |row| {
                Ok(RequestLogSummary {
                    total_requests: row.get(0)?,
                    total_attempts: row.get(1)?,
                    prompt_tokens: row.get(2)?,
                    completion_tokens: row.get(3)?,
                    cached_tokens: row.get(4)?,
                })
            },
        )
        .map_err(LogReadError::from)?;
    let mut item_params = filter_params;
    item_params.push(Value::Integer(query.limit));
    item_params.push(Value::Integer(query.offset));
    let mut stmt = conn
        .prepare(&format!(
            "{REQUEST_GROUPS}
             SELECT request_key, request_id, timestamp, logical_status, http_status, model,
                    requested_model, resolved_alias, upstream_model, attempt_count,
                    recorded_row_count, prompt_tokens, completion_tokens, cached_tokens,
                    duration_ms, is_legacy, client_key_id, client_key_name, provider_id,
                    account_id, account_name, route_account_id, credential_account_id, route
             FROM grouped
             {where_sql}
             ORDER BY timestamp DESC, request_key ASC
             LIMIT ? OFFSET ?"
        ))
        .map_err(LogReadError::from)?;
    let items = stmt
        .query_map(params_from_iter(item_params.iter()), request_log_from_row)
        .map_err(LogReadError::from)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(LogReadError::from)?;
    Ok(RequestLogPage {
        items,
        total: summary.total_requests,
        limit: query.limit,
        offset: query.offset,
        summary,
    })
}

pub(crate) fn query_request_attempts_on(
    conn: &rusqlite::Connection,
    request_key: &str,
) -> std::result::Result<Vec<RequestAttemptRecord>, LogReadError> {
    let request_key = parse_request_log_key(request_key).map_err(LogReadError::Invalid)?;
    let mut stmt = conn
        .prepare(&format!(
            "{ATTEMPT_SELECT}
             WHERE request_group_key = ?1
             ORDER BY COALESCE(attempt, -1) ASC, id ASC"
        ))
        .map_err(LogReadError::from)?;
    stmt.query_map([request_key], attempt_from_row)
        .map_err(LogReadError::from)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(LogReadError::from)
}
