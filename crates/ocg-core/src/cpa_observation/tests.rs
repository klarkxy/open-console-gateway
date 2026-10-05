use chrono::{DateTime, Utc};
use rusqlite::Connection;
use serde_json::{Value, json};
use uuid::Uuid;

use super::{
    AdmittedAttemptContext, AttemptErrorCode, AttemptOutcome, AttemptRecord, CapturedClientKey,
    ObservationError, StrictAttemptResult, record_result_on,
};
use crate::cpa_policy::SendKind;

const SECRET_BODY: &str = "upstream-secret-body";
const SECRET_TOKEN: &str = "secret-token-value";

const ROWS_SQL: &str = "SELECT id, timestamp, model, account_id, account_name,
       client_key_id, client_key_name, route_account_id, provider_id, credential_account_id,
       status, http_status, route,
       prompt_tokens, completion_tokens, cached_tokens, cache_creation_tokens, cost,
       raw_cost_usd, quota_debit, effective_paid_cost_usd,
       pricing_revision_id, quota_multiplier, local_adjustment_multiplier, service_tier,
       cost_state, error_message, request_id, attempt, error_source, error_stage, duration_ms,
       diagnostic_json, requested_model, resolved_alias, upstream_model,
       native_cost_value, native_cost_unit, native_cost_currency
FROM forward_logs
ORDER BY id ASC";

struct Row {
    id: i64,
    timestamp: String,
    model: String,
    account_id: String,
    account_name: String,
    client_key_id: Option<String>,
    client_key_name: Option<String>,
    route_account_id: Option<String>,
    provider_id: Option<String>,
    credential_account_id: Option<String>,
    status: String,
    http_status: Option<i32>,
    route: String,
    prompt_tokens: i64,
    completion_tokens: i64,
    cached_tokens: i64,
    cache_creation_tokens: i64,
    cost: f64,
    raw_cost_usd: Option<f64>,
    quota_debit: Option<f64>,
    effective_paid_cost_usd: Option<f64>,
    pricing_revision_id: Option<String>,
    quota_multiplier: Option<f64>,
    local_adjustment_multiplier: Option<f64>,
    service_tier: Option<String>,
    cost_state: String,
    error_message: Option<String>,
    request_id: Option<String>,
    attempt: Option<i64>,
    error_source: Option<String>,
    error_stage: Option<String>,
    duration_ms: Option<i64>,
    diagnostic_json: Option<String>,
    requested_model: Option<String>,
    resolved_alias: Option<String>,
    upstream_model: Option<String>,
    native_cost_value: Option<f64>,
    native_cost_unit: Option<String>,
    native_cost_currency: Option<String>,
}

fn harness() -> Connection {
    let conn = Connection::open_in_memory().expect("memory db");
    conn.execute_batch(
        "CREATE TABLE forward_logs (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            timestamp TEXT NOT NULL,
            model TEXT NOT NULL,
            account_id TEXT NOT NULL,
            account_name TEXT NOT NULL,
            client_key_id TEXT,
            client_key_name TEXT,
            route_account_id TEXT,
            provider_id TEXT,
            credential_account_id TEXT,
            status TEXT NOT NULL,
            http_status INTEGER,
            route TEXT NOT NULL DEFAULT '',
            prompt_tokens INTEGER NOT NULL DEFAULT 0,
            completion_tokens INTEGER NOT NULL DEFAULT 0,
            cached_tokens INTEGER NOT NULL DEFAULT 0,
            cache_creation_tokens INTEGER NOT NULL DEFAULT 0,
            cost REAL NOT NULL DEFAULT 0,
            raw_cost_usd REAL,
            quota_debit REAL,
            effective_paid_cost_usd REAL,
            pricing_revision_id TEXT,
            quota_multiplier REAL,
            local_adjustment_multiplier REAL,
            service_tier TEXT,
            cost_state TEXT NOT NULL DEFAULT 'not_applicable',
            error_message TEXT,
            request_id TEXT,
            attempt INTEGER,
            error_source TEXT,
            error_stage TEXT,
            duration_ms INTEGER,
            diagnostic_json TEXT,
            requested_model TEXT,
            resolved_alias TEXT,
            upstream_model TEXT,
            native_cost_value REAL,
            native_cost_unit TEXT,
            native_cost_currency TEXT
        );",
    )
    .expect("forward_logs");
    conn
}

fn started() -> DateTime<Utc> {
    DateTime::parse_from_rfc3339("2026-10-04T01:02:03Z")
        .expect("clock")
        .with_timezone(&Utc)
}

fn id(last: u8) -> Uuid {
    Uuid::parse_str(&format!("00000000-0000-4000-8000-0000000000{last:02x}")).expect("uuid")
}

fn accepted(ordinal: u32, attempt: u8) -> AdmittedAttemptContext {
    AdmittedAttemptContext {
        request_id: id(0x10),
        client_trace_id: None,
        attempt_id: id(attempt),
        ordinal,
        started_at: started(),
        provider_id: "opencode".to_string(),
        credential_id: "cred-1".to_string(),
        credential_version: 7,
        binding_id: "bind-1".to_string(),
        auth_id: "auth-1".to_string(),
        material_revision: "mat-1".to_string(),
        registration_epoch: 3,
        public_model: "public-model".to_string(),
        upstream_model: "upstream-model".to_string(),
        kind: SendKind::Accepted,
        client_key: Some(CapturedClientKey {
            id: "key-1".to_string(),
            name: Some("Key One".to_string()),
        }),
    }
}

fn validated(ordinal: u32, attempt: u8) -> AdmittedAttemptContext {
    let mut admitted = accepted(ordinal, attempt);
    admitted.kind = SendKind::Validated;
    admitted.client_key = None;
    admitted
}

fn attempt_result(
    sent: bool,
    status: Option<u16>,
    body_complete: bool,
    stream_started: bool,
    outcome: AttemptOutcome,
    error_code: AttemptErrorCode,
) -> StrictAttemptResult {
    StrictAttemptResult {
        sent,
        status,
        body_complete,
        stream_started,
        outcome,
        error_code,
        observation_id: None,
        observed_at: None,
    }
}

fn clean_success() -> StrictAttemptResult {
    attempt_result(
        true,
        Some(200),
        true,
        false,
        AttemptOutcome::Success,
        AttemptErrorCode::None,
    )
}

fn kind_text(kind: SendKind) -> &'static str {
    match kind {
        SendKind::Accepted => "accepted",
        SendKind::Validated => "validated",
    }
}

fn outcome_text(outcome: AttemptOutcome) -> &'static str {
    match outcome {
        AttemptOutcome::Success => "success",
        AttemptOutcome::ExplicitRejection => "explicit_rejection",
        AttemptOutcome::Uncertain => "uncertain",
        AttemptOutcome::Cancelled => "cancelled",
        AttemptOutcome::Deadline => "deadline",
        AttemptOutcome::LocalFailure => "local_failure",
    }
}

fn error_text(code: AttemptErrorCode) -> &'static str {
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

fn wire_body(
    admitted: &AdmittedAttemptContext,
    result: &StrictAttemptResult,
    usage: Option<Value>,
    observation: Option<Value>,
) -> String {
    let mut value = json!({
        "requestId": admitted.request_id.to_string(),
        "attemptId": admitted.attempt_id.to_string(),
        "providerId": admitted.provider_id,
        "credentialId": admitted.credential_id,
        "credentialVersion": admitted.credential_version.to_string(),
        "publicModel": admitted.public_model,
        "upstreamModel": admitted.upstream_model,
        "authId": admitted.auth_id,
        "materialRevision": admitted.material_revision,
        "registrationEpoch": admitted.registration_epoch.to_string(),
        "kind": kind_text(admitted.kind),
        "sent": result.sent,
        "bodyComplete": result.body_complete,
        "streamStarted": result.stream_started,
        "status": result.status,
        "outcome": outcome_text(result.outcome),
        "errorCode": error_text(result.error_code),
        "responseBody": SECRET_BODY,
        "headers": {"Authorization": format!("Bearer {SECRET_TOKEN}")},
    });
    if let Some(usage) = usage {
        value["reportedUsage"] = usage;
    }
    if let Some(observation) = observation {
        value["observation"] = observation;
    }
    value.to_string()
}

fn count(conn: &Connection) -> i64 {
    conn.query_row("SELECT COUNT(*) FROM forward_logs", [], |row| row.get(0))
        .expect("count")
}

fn priced_cost(conn: &Connection) -> f64 {
    conn.query_row(
        "SELECT COALESCE(SUM(cost), 0) FROM forward_logs
         WHERE cost_state IN ('priced', 'legacy_estimate')",
        [],
        |row| row.get(0),
    )
    .expect("priced")
}

fn counted_tokens(conn: &Connection) -> i64 {
    conn.query_row(
        "SELECT COALESCE(SUM(prompt_tokens + completion_tokens), 0) FROM forward_logs
         WHERE prompt_tokens + completion_tokens > 0",
        [],
        |row| row.get(0),
    )
    .expect("tokens")
}

fn success_rows(conn: &Connection) -> i64 {
    conn.query_row(
        "SELECT COUNT(*) FROM forward_logs WHERE status IN ('success', 'success_unpriced')",
        [],
        |row| row.get(0),
    )
    .expect("success filter")
}

fn rows(conn: &Connection) -> Vec<Row> {
    let mut stmt = conn.prepare(ROWS_SQL).expect("prepare");
    let mapped = stmt
        .query_map([], |row| {
            Ok(Row {
                id: row.get(0)?,
                timestamp: row.get(1)?,
                model: row.get(2)?,
                account_id: row.get(3)?,
                account_name: row.get(4)?,
                client_key_id: row.get(5)?,
                client_key_name: row.get(6)?,
                route_account_id: row.get(7)?,
                provider_id: row.get(8)?,
                credential_account_id: row.get(9)?,
                status: row.get(10)?,
                http_status: row.get(11)?,
                route: row.get(12)?,
                prompt_tokens: row.get(13)?,
                completion_tokens: row.get(14)?,
                cached_tokens: row.get(15)?,
                cache_creation_tokens: row.get(16)?,
                cost: row.get(17)?,
                raw_cost_usd: row.get(18)?,
                quota_debit: row.get(19)?,
                effective_paid_cost_usd: row.get(20)?,
                pricing_revision_id: row.get(21)?,
                quota_multiplier: row.get(22)?,
                local_adjustment_multiplier: row.get(23)?,
                service_tier: row.get(24)?,
                cost_state: row.get(25)?,
                error_message: row.get(26)?,
                request_id: row.get(27)?,
                attempt: row.get(28)?,
                error_source: row.get(29)?,
                error_stage: row.get(30)?,
                duration_ms: row.get(31)?,
                diagnostic_json: row.get(32)?,
                requested_model: row.get(33)?,
                resolved_alias: row.get(34)?,
                upstream_model: row.get(35)?,
                native_cost_value: row.get(36)?,
                native_cost_unit: row.get(37)?,
                native_cost_currency: row.get(38)?,
            })
        })
        .expect("query");
    mapped.map(|row| row.expect("row")).collect()
}

fn only(conn: &Connection) -> Row {
    let found = rows(conn);
    assert_eq!(found.len(), 1, "expected one forward log");
    found.into_iter().next().expect("row")
}

fn diagnostic_of(row: &Row) -> Value {
    serde_json::from_str(row.diagnostic_json.as_deref().expect("diagnostic")).expect("json")
}

fn record_id(record: AttemptRecord) -> i64 {
    match record {
        AttemptRecord::Inserted(id) | AttemptRecord::Existing(id) => id,
    }
}

fn assert_no_secrets(conn: &Connection) {
    let blob: String = conn
        .query_row(
            "SELECT COALESCE(group_concat(
                timestamp || model || account_id || account_name ||
                IFNULL(client_key_id, '') || IFNULL(client_key_name, '') ||
                IFNULL(provider_id, '') || IFNULL(credential_account_id, '') ||
                status || IFNULL(route, '') || IFNULL(error_message, '') ||
                IFNULL(request_id, '') || IFNULL(error_source, '') || IFNULL(error_stage, '') ||
                IFNULL(diagnostic_json, '') || IFNULL(requested_model, '') ||
                IFNULL(resolved_alias, '') || IFNULL(upstream_model, '') ||
                IFNULL(native_cost_unit, '') || IFNULL(native_cost_currency, '') ||
                IFNULL(pricing_revision_id, '') || IFNULL(service_tier, '') ||
                IFNULL(route_account_id, '')
            ), '') FROM forward_logs",
            [],
            |row| row.get(0),
        )
        .expect("dump");
    assert!(!blob.contains(SECRET_BODY));
    assert!(!blob.contains(SECRET_TOKEN));
    assert!(!blob.contains("responseBody"));
    assert!(!blob.contains("Authorization"));
}

fn assert_no_binding_column(conn: &Connection) {
    let mut stmt = conn
        .prepare("PRAGMA table_info(forward_logs)")
        .expect("pragma");
    let names = stmt
        .query_map([], |row| row.get::<_, String>(1))
        .expect("names")
        .map(|name| name.expect("column"))
        .collect::<Vec<_>>();
    assert!(names.iter().all(|name| name != "binding_id"));
}

fn assert_frozen(row: &Row, admitted: &AdmittedAttemptContext) {
    let private_id = admitted.request_id.to_string();
    let attempt_id = admitted.attempt_id.to_string();
    let display_id = admitted
        .client_trace_id
        .clone()
        .unwrap_or_else(|| private_id.clone());
    assert_eq!(row.timestamp, admitted.started_at.to_rfc3339());
    assert_eq!(row.model, admitted.public_model);
    assert_eq!(row.account_id, admitted.credential_id);
    assert_eq!(row.account_name, admitted.credential_id);
    assert_eq!(row.route_account_id, None);
    assert_eq!(
        row.provider_id.as_deref(),
        Some(admitted.provider_id.as_str())
    );
    assert_eq!(
        row.credential_account_id.as_deref(),
        Some(admitted.credential_id.as_str())
    );
    assert_eq!(row.route, "");
    assert_eq!(row.cached_tokens, 0);
    assert_eq!(row.cache_creation_tokens, 0);
    assert_eq!(row.cost, 0.0);
    assert_eq!(row.raw_cost_usd, None);
    assert_eq!(row.quota_debit, None);
    assert_eq!(row.effective_paid_cost_usd, None);
    assert_eq!(row.pricing_revision_id, None);
    assert_eq!(row.quota_multiplier, None);
    assert_eq!(row.local_adjustment_multiplier, None);
    assert_eq!(row.service_tier, None);
    assert_eq!(row.cost_state, "unknown");
    assert_eq!(row.request_id.as_deref(), Some(display_id.as_str()));
    assert_eq!(row.attempt, Some(i64::from(admitted.ordinal)));
    assert_eq!(row.error_source.as_deref(), Some("cpa"));
    assert_eq!(row.duration_ms, None);
    assert_eq!(
        row.requested_model.as_deref(),
        Some(admitted.public_model.as_str())
    );
    assert_eq!(row.resolved_alias, None);
    assert_eq!(
        row.upstream_model.as_deref(),
        Some(admitted.upstream_model.as_str())
    );
    assert_eq!(row.native_cost_value, None);
    assert_eq!(row.native_cost_unit, None);
    assert_eq!(row.native_cost_currency, None);
    match &admitted.client_key {
        Some(key) => {
            assert_eq!(row.client_key_id.as_deref(), Some(key.id.as_str()));
            assert_eq!(row.client_key_name.as_deref(), key.name.as_deref());
        }
        None => {
            assert_eq!(row.client_key_id, None);
            assert_eq!(row.client_key_name, None);
        }
    }

    let diagnostic = diagnostic_of(row);
    let object = diagnostic.as_object().expect("object");
    for key in [
        "response_body",
        "responseBody",
        "headers",
        "authorization",
        "Authorization",
        "retry_after",
        "retryAfter",
        "fingerprint",
        "captured_key_fingerprint",
    ] {
        assert!(!object.contains_key(key), "{key}");
    }
    assert_eq!(diagnostic["version"], 1);
    assert_eq!(diagnostic["source"], "cpa_result");
    assert_eq!(diagnostic["request_id"], private_id.as_str());
    match &admitted.client_trace_id {
        Some(trace) => assert_eq!(diagnostic["client_trace_id"], trace.as_str()),
        None => assert!(diagnostic["client_trace_id"].is_null()),
    }
    assert_eq!(diagnostic["attempt_id"], attempt_id.as_str());
    assert_eq!(
        diagnostic["ordinal"].as_u64(),
        Some(u64::from(admitted.ordinal))
    );
    assert_eq!(diagnostic["kind"], kind_text(admitted.kind));
    assert_eq!(diagnostic["provider_id"], admitted.provider_id.as_str());
    assert_eq!(diagnostic["credential_id"], admitted.credential_id.as_str());
    assert_eq!(
        diagnostic["credential_version"]
            .as_str()
            .and_then(|text| text.parse().ok()),
        Some(admitted.credential_version)
    );
    assert_eq!(diagnostic["binding_id"], admitted.binding_id.as_str());
    assert_eq!(diagnostic["auth_id"], admitted.auth_id.as_str());
    assert_eq!(
        diagnostic["material_revision"],
        admitted.material_revision.as_str()
    );
    assert_eq!(
        diagnostic["registration_epoch"]
            .as_str()
            .and_then(|text| text.parse().ok()),
        Some(admitted.registration_epoch)
    );
    assert_eq!(diagnostic["public_model"], admitted.public_model.as_str());
    assert_eq!(
        diagnostic["upstream_model"],
        admitted.upstream_model.as_str()
    );
    assert_eq!(
        diagnostic["started_at"],
        admitted.started_at.to_rfc3339().as_str()
    );
    match &admitted.client_key {
        Some(key) => assert_eq!(diagnostic["client_key_id"], key.id.as_str()),
        None => assert!(diagnostic["client_key_id"].is_null()),
    }
    assert_eq!(diagnostic["cost_state"], "unknown");
    assert_eq!(diagnostic["admission"], false);
    assert_eq!(diagnostic["cache_reported"], false);
    assert_eq!(diagnostic["native_usd_reported"], false);
}

fn commit(
    conn: &mut Connection,
    admitted: &AdmittedAttemptContext,
    result: &StrictAttemptResult,
    body: &str,
) -> Result<AttemptRecord, ObservationError> {
    let tx = conn.transaction().expect("transaction");
    let recorded = record_result_on(&tx, admitted, result, body)?;
    tx.commit().expect("commit");
    Ok(recorded)
}

fn persist(
    conn: &mut Connection,
    admitted: &AdmittedAttemptContext,
    result: &StrictAttemptResult,
    body: &str,
) -> AttemptRecord {
    let recorded = commit(conn, admitted, result, body).expect("record");
    assert_no_secrets(conn);
    assert_no_binding_column(conn);
    assert_eq!(priced_cost(conn), 0.0);
    recorded
}

fn reject(
    conn: &mut Connection,
    admitted: &AdmittedAttemptContext,
    result: &StrictAttemptResult,
    body: &str,
) -> ObservationError {
    let err = commit(conn, admitted, result, body).expect_err("error");
    assert_eq!(count(conn), 0);
    err
}

#[test]
fn rollback_drops_the_log_with_the_same_transaction_and_commit_keeps_one() {
    let mut conn = harness();
    conn.execute_batch("CREATE TABLE side_receipt (note TEXT NOT NULL);")
        .expect("side");
    let admitted = accepted(1, 1);
    let result = clean_success();
    let body = wire_body(
        &admitted,
        &result,
        Some(json!({"inputTokens": 3, "outputTokens": 4})),
        None,
    );
    {
        let tx = conn.transaction().expect("transaction");
        tx.execute("INSERT INTO side_receipt (note) VALUES ('restriction')", [])
            .expect("side insert");
        let inserted = record_result_on(&tx, &admitted, &result, &body).expect("record");
        assert!(matches!(inserted, AttemptRecord::Inserted(_)));
        tx.rollback().expect("rollback");
    }
    assert_eq!(count(&conn), 0);
    let side: i64 = conn
        .query_row("SELECT COUNT(*) FROM side_receipt", [], |row| row.get(0))
        .expect("side count");
    assert_eq!(side, 0);

    {
        let tx = conn.transaction().expect("transaction");
        tx.execute("INSERT INTO side_receipt (note) VALUES ('restriction')", [])
            .expect("side insert");
        record_result_on(&tx, &admitted, &result, &body).expect("record");
        tx.commit().expect("commit");
    }
    assert_eq!(count(&conn), 1);
    let side: i64 = conn
        .query_row("SELECT COUNT(*) FROM side_receipt", [], |row| row.get(0))
        .expect("side count");
    assert_eq!(side, 1);
    let row = only(&conn);
    assert_frozen(&row, &admitted);
    assert_eq!(row.prompt_tokens, 3);
    assert_eq!(row.completion_tokens, 4);
    assert_no_secrets(&conn);
}

#[test]
fn replayed_callback_keeps_the_original_tokens_and_attribution() {
    let mut conn = harness();
    let admitted = accepted(1, 1);
    let result = clean_success();
    let body = wire_body(
        &admitted,
        &result,
        Some(json!({"inputTokens": 3, "outputTokens": 9})),
        None,
    );
    let tx = conn.transaction().expect("transaction");
    let first = record_result_on(&tx, &admitted, &result, &body).expect("first");
    let mut changed = admitted.clone();
    changed.public_model = "other-model".to_string();
    let mut replay = serde_json::from_str::<Value>(&body).expect("json");
    replay["publicModel"] = json!("other-model");
    replay["reportedUsage"] = json!({"inputTokens": 100, "outputTokens": 100});
    let second = record_result_on(&tx, &changed, &result, &replay.to_string()).expect("replay");
    let third = record_result_on(&tx, &admitted, &result, "{").expect("broken replay");
    assert_eq!(record_id(first), record_id(second));
    assert_eq!(record_id(first), record_id(third));
    assert!(matches!(second, AttemptRecord::Existing(_)));
    assert!(matches!(third, AttemptRecord::Existing(_)));
    tx.commit().expect("commit");

    assert_eq!(count(&conn), 1);
    let row = only(&conn);
    assert_frozen(&row, &admitted);
    assert_eq!(row.model, "public-model");
    assert_eq!(row.prompt_tokens, 3);
    assert_eq!(row.completion_tokens, 9);
    let diagnostic = diagnostic_of(&row);
    assert_eq!(diagnostic["usage"]["input_tokens"], 3);
    assert_eq!(diagnostic["usage"]["output_tokens"], 9);
    assert_eq!(diagnostic["usage_state"], "reported");
    assert_no_secrets(&conn);
}

#[test]
fn a_result_that_disagrees_with_the_admitted_attempt_writes_nothing() {
    let mut conn = harness();
    let admitted = accepted(1, 1);
    let result = clean_success();
    let mut body =
        serde_json::from_str::<Value>(&wire_body(&admitted, &result, None, None)).expect("json");
    body["providerId"] = json!("other-provider");
    let err = reject(&mut conn, &admitted, &result, &body.to_string());
    assert_eq!(err, ObservationError::Mismatch);
}

#[test]
fn ordinal_zero_is_rejected_before_the_body_is_parsed() {
    let mut conn = harness();
    let mut admitted = accepted(1, 1);
    admitted.ordinal = 0;
    let err = reject(&mut conn, &admitted, &clean_success(), "not-json");
    assert_eq!(err, ObservationError::Identity);
}

#[test]
fn accepted_attempt_without_a_client_key_writes_nothing() {
    let mut conn = harness();
    let mut admitted = accepted(1, 1);
    admitted.client_key = None;
    let err = reject(&mut conn, &admitted, &clean_success(), "not-json");
    assert_eq!(err, ObservationError::Identity);
}

#[test]
fn validated_attempt_with_a_client_key_writes_nothing() {
    let mut conn = harness();
    let mut admitted = validated(1, 1);
    admitted.client_key = Some(CapturedClientKey {
        id: "key-1".to_string(),
        name: None,
    });
    let err = reject(&mut conn, &admitted, &clean_success(), "not-json");
    assert_eq!(err, ObservationError::Identity);
}

#[test]
fn empty_client_key_name_writes_nothing() {
    let mut conn = harness();
    let mut admitted = accepted(1, 1);
    admitted.client_key = Some(CapturedClientKey {
        id: "key-1".to_string(),
        name: Some(String::new()),
    });
    let err = reject(&mut conn, &admitted, &clean_success(), "not-json");
    assert_eq!(err, ObservationError::Identity);
}

#[test]
fn a_control_character_in_the_provider_id_writes_nothing() {
    let mut conn = harness();
    let mut admitted = accepted(1, 1);
    admitted.provider_id = "open\ncode".to_string();
    let err = reject(&mut conn, &admitted, &clean_success(), "not-json");
    assert_eq!(err, ObservationError::Identity);
}

#[test]
fn validated_attempt_stores_a_null_client_key() {
    let mut conn = harness();
    let admitted = validated(1, 1);
    let result = clean_success();
    let body = wire_body(&admitted, &result, None, None);
    let recorded = persist(&mut conn, &admitted, &result, &body);
    assert!(matches!(recorded, AttemptRecord::Inserted(_)));
    let row = only(&conn);
    assert_frozen(&row, &admitted);
    assert_eq!(row.client_key_id, None);
    assert_eq!(row.client_key_name, None);
    assert_eq!(row.status, "success");
    assert!(diagnostic_of(&row)["client_key_id"].is_null());
    let tables: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = 'access_keys'",
            [],
            |row| row.get(0),
        )
        .expect("tables");
    assert_eq!(tables, 0);
}

#[test]
fn foreign_observation_id_is_dropped_and_the_attempt_is_kept() {
    let mut conn = harness();
    let admitted = accepted(1, 1);
    let foreign = id(0x99);
    let fetched = DateTime::parse_from_rfc3339("2026-10-04T09:09:09Z")
        .expect("clock")
        .with_timezone(&Utc);
    let mut result = clean_success();
    result.observation_id = Some(foreign.to_string());
    result.observed_at = Some(fetched);
    let observation = json!({
        "id": foreign.to_string(),
        "fetchedAt": "2026-10-04T09:09:09Z",
    });
    let body = wire_body(&admitted, &result, None, Some(observation));
    persist(&mut conn, &admitted, &result, &body);
    let row = only(&conn);
    assert_frozen(&row, &admitted);
    let diagnostic = diagnostic_of(&row);
    assert_eq!(diagnostic["observation_stamp"], "rejected");
    assert!(diagnostic["observation_id"].is_null());
    assert!(diagnostic["observed_at"].is_null());
    let stored = row.diagnostic_json.expect("diagnostic");
    assert!(!stored.contains(&foreign.to_string()));
    assert!(!stored.contains("09:09:09"));
}

#[test]
fn matched_observation_stamp_stores_the_canonical_attempt_id() {
    let mut conn = harness();
    let admitted = accepted(1, 1);
    let mut result = clean_success();
    result.observation_id = Some(admitted.attempt_id.to_string());
    result.observed_at = Some(started());
    let observation = json!({
        "id": admitted.attempt_id.to_string(),
        "fetchedAt": "2026-10-04T01:02:03Z",
    });
    let body = wire_body(&admitted, &result, None, Some(observation));
    persist(&mut conn, &admitted, &result, &body);
    let diagnostic = diagnostic_of(&only(&conn));
    assert_eq!(diagnostic["observation_stamp"], "matched");
    assert_eq!(
        diagnostic["observation_id"],
        admitted.attempt_id.to_string().as_str()
    );
    assert_eq!(diagnostic["observed_at"], started().to_rfc3339().as_str());
}

#[test]
fn observation_stamp_disagreement_writes_nothing() {
    let mut conn = harness();
    let admitted = accepted(1, 1);
    let mut result = clean_success();
    result.observation_id = Some(id(0x99).to_string());
    result.observed_at = Some(started());
    let observation = json!({
        "id": admitted.attempt_id.to_string(),
        "fetchedAt": "2026-10-04T01:02:03Z",
    });
    let body = wire_body(&admitted, &result, None, Some(observation));
    let err = reject(&mut conn, &admitted, &result, &body);
    assert_eq!(err, ObservationError::Mismatch);
}

#[test]
fn an_extra_observation_field_writes_nothing() {
    let mut conn = harness();
    let admitted = accepted(1, 1);
    let mut result = clean_success();
    result.observation_id = Some(admitted.attempt_id.to_string());
    result.observed_at = Some(started());
    let observation = json!({
        "id": admitted.attempt_id.to_string(),
        "fetchedAt": "2026-10-04T01:02:03Z",
        "note": "extra",
    });
    let body = wire_body(&admitted, &result, None, Some(observation));
    let err = reject(&mut conn, &admitted, &result, &body);
    assert_eq!(err, ObservationError::Mismatch);
}

#[test]
fn same_ordinal_different_attempt_conflicts_and_leaves_the_first_row() {
    let mut conn = harness();
    let first = accepted(1, 1);
    let result = clean_success();
    let body = wire_body(
        &first,
        &result,
        Some(json!({"inputTokens": 2, "outputTokens": 2})),
        None,
    );
    persist(&mut conn, &first, &result, &body);
    let second = accepted(1, 2);
    let other = wire_body(
        &second,
        &result,
        Some(json!({"inputTokens": 8, "outputTokens": 8})),
        None,
    );
    let err = commit(&mut conn, &second, &result, &other).expect_err("conflict");
    assert_eq!(err, ObservationError::Conflict);
    assert_eq!(count(&conn), 1);
    let row = only(&conn);
    assert_frozen(&row, &first);
    assert_eq!(row.prompt_tokens, 2);
    assert_eq!(row.completion_tokens, 2);
}

#[test]
fn cleared_diagnostic_does_not_insert_a_second_ordinal_row() {
    let mut conn = harness();
    let admitted = accepted(1, 1);
    let result = clean_success();
    let body = wire_body(
        &admitted,
        &result,
        Some(json!({"inputTokens": 5, "outputTokens": 6})),
        None,
    );
    let id = record_id(persist(&mut conn, &admitted, &result, &body));
    conn.execute(
        "UPDATE forward_logs SET diagnostic_json = NULL WHERE id = ?1",
        [id],
    )
    .expect("clear");
    let replay = commit(&mut conn, &admitted, &result, "{").expect("replay");
    assert!(matches!(replay, AttemptRecord::Existing(found) if found == id));
    let other = accepted(1, 2);
    let other_body = wire_body(
        &other,
        &result,
        Some(json!({"inputTokens": 1, "outputTokens": 1})),
        None,
    );
    let blocked = commit(&mut conn, &other, &result, &other_body).expect("blocked");
    assert!(matches!(blocked, AttemptRecord::Existing(found) if found == id));
    assert_eq!(count(&conn), 1);
    let row = only(&conn);
    assert_eq!(row.prompt_tokens, 5);
    assert_eq!(row.completion_tokens, 6);
    assert_eq!(row.model, "public-model");
    assert_eq!(row.diagnostic_json, None);
}

#[test]
fn explicit_zero_tokens_stay_reported_zeros() {
    let mut conn = harness();
    let admitted = accepted(1, 1);
    let result = clean_success();
    let body = wire_body(
        &admitted,
        &result,
        Some(json!({"inputTokens": 0, "outputTokens": 0})),
        None,
    );
    persist(&mut conn, &admitted, &result, &body);
    let row = only(&conn);
    assert_frozen(&row, &admitted);
    assert_eq!(row.prompt_tokens, 0);
    assert_eq!(row.completion_tokens, 0);
    let diagnostic = diagnostic_of(&row);
    assert_eq!(diagnostic["usage"]["input_tokens"], 0);
    assert_eq!(diagnostic["usage"]["output_tokens"], 0);
    assert!(!diagnostic["usage"]["input_tokens"].is_null());
    assert_eq!(diagnostic["usage_state"], "reported");
    assert_eq!(counted_tokens(&conn), 0);
}

#[test]
fn omitted_or_null_reported_usage_stays_unknown() {
    let mut conn = harness();
    let result = clean_success();
    let omitted = accepted(1, 1);
    persist(
        &mut conn,
        &omitted,
        &result,
        &wire_body(&omitted, &result, None, None),
    );
    let absent = accepted(2, 2);
    persist(
        &mut conn,
        &absent,
        &result,
        &wire_body(&absent, &result, Some(Value::Null), None),
    );
    assert_eq!(count(&conn), 2);
    assert_eq!(counted_tokens(&conn), 0);
    for (row, admitted) in rows(&conn).into_iter().zip([omitted, absent]) {
        assert_frozen(&row, &admitted);
        assert_eq!(row.prompt_tokens, 0);
        assert_eq!(row.completion_tokens, 0);
        let diagnostic = diagnostic_of(&row);
        assert!(diagnostic["usage"]["input_tokens"].is_null());
        assert!(diagnostic["usage"]["output_tokens"].is_null());
        assert_eq!(diagnostic["usage_state"], "unknown");
    }
}

#[test]
fn partial_input_stores_the_reported_side_only() {
    let mut conn = harness();
    let admitted = accepted(1, 1);
    let result = clean_success();
    let body = wire_body(&admitted, &result, Some(json!({"inputTokens": 4})), None);
    persist(&mut conn, &admitted, &result, &body);
    let row = only(&conn);
    assert_frozen(&row, &admitted);
    assert_eq!(row.prompt_tokens, 4);
    assert_eq!(row.completion_tokens, 0);
    let diagnostic = diagnostic_of(&row);
    assert_eq!(diagnostic["usage"]["input_tokens"], 4);
    assert!(diagnostic["usage"]["output_tokens"].is_null());
    assert_eq!(diagnostic["usage_state"], "partial");
    assert_eq!(counted_tokens(&conn), 4);
}

#[test]
fn reported_usage_outside_the_wire_bounds_writes_nothing() {
    let result = clean_success();
    let cases = [
        json!({"inputTokens": 50_000_001_u64, "outputTokens": 0}),
        json!({"inputTokens": 1, "cachedTokens": 1}),
        json!({"inputTokens": null, "outputTokens": 1}),
        json!({"inputTokens": "1", "outputTokens": 1}),
        json!({"inputTokens": 1.5, "outputTokens": 1}),
        json!([1, 2]),
    ];
    for usage in cases {
        let mut conn = harness();
        let admitted = accepted(1, 1);
        let body = wire_body(&admitted, &result, Some(usage), None);
        let err = reject(&mut conn, &admitted, &result, &body);
        assert_eq!(err, ObservationError::Usage);
    }
}

#[test]
fn the_wire_token_cap_is_stored() {
    let mut conn = harness();
    let admitted = accepted(1, 1);
    let result = clean_success();
    let body = wire_body(
        &admitted,
        &result,
        Some(json!({"inputTokens": 50_000_000_u64, "outputTokens": 0})),
        None,
    );
    persist(&mut conn, &admitted, &result, &body);
    let row = only(&conn);
    assert_eq!(row.prompt_tokens, 50_000_000);
    assert_eq!(row.completion_tokens, 0);
    assert_eq!(diagnostic_of(&row)["usage"]["input_tokens"], 50_000_000_u64);
    assert_eq!(diagnostic_of(&row)["usage_state"], "reported");
}

#[test]
fn a_completed_stream_with_usage_is_success() {
    let mut conn = harness();
    let admitted = accepted(1, 1);
    let result = attempt_result(
        true,
        Some(200),
        true,
        true,
        AttemptOutcome::Success,
        AttemptErrorCode::None,
    );
    let body = wire_body(
        &admitted,
        &result,
        Some(json!({"inputTokens": 11, "outputTokens": 4})),
        None,
    );
    persist(&mut conn, &admitted, &result, &body);
    let row = only(&conn);
    assert_frozen(&row, &admitted);
    assert_eq!(row.status, "success");
    assert_eq!(row.http_status, Some(200));
    assert_eq!(row.error_message, None);
    assert_eq!(row.error_stage.as_deref(), Some("complete"));
    assert_eq!(row.prompt_tokens, 11);
    assert_eq!(row.completion_tokens, 4);
    let diagnostic = diagnostic_of(&row);
    assert_eq!(diagnostic["completeness"], "complete");
    assert_eq!(diagnostic["usage_state"], "reported");
    assert_eq!(success_rows(&conn), 1);
    assert_eq!(counted_tokens(&conn), 15);
}

#[test]
fn a_truncated_stream_is_outcome_unknown() {
    let mut conn = harness();
    let admitted = accepted(1, 1);
    let result = attempt_result(
        true,
        Some(200),
        false,
        true,
        AttemptOutcome::Success,
        AttemptErrorCode::None,
    );
    let body = wire_body(&admitted, &result, None, None);
    persist(&mut conn, &admitted, &result, &body);
    let row = only(&conn);
    assert_frozen(&row, &admitted);
    assert_eq!(row.status, "outcome_unknown");
    assert_eq!(row.error_message.as_deref(), Some("truncated"));
    assert_eq!(row.error_stage.as_deref(), Some("truncated"));
    assert_eq!(diagnostic_of(&row)["completeness"], "truncated");
    assert_eq!(diagnostic_of(&row)["outcome"], "success");
    assert_eq!(success_rows(&conn), 0);
}

#[test]
fn stream_lost_stays_truncated_when_the_body_is_marked_complete() {
    let mut conn = harness();
    let admitted = accepted(1, 1);
    let result = attempt_result(
        true,
        Some(200),
        true,
        true,
        AttemptOutcome::Success,
        AttemptErrorCode::StreamLost,
    );
    let body = wire_body(&admitted, &result, None, None);
    persist(&mut conn, &admitted, &result, &body);
    let row = only(&conn);
    assert_eq!(row.status, "outcome_unknown");
    assert_eq!(row.error_stage.as_deref(), Some("truncated"));
    assert_eq!(diagnostic_of(&row)["error_code"], "stream_lost");
    assert_eq!(success_rows(&conn), 0);
}

#[test]
fn body_lost_uncertain_and_explicit_rejection_stay_distinct() {
    let mut conn = harness();
    let cases: [(
        AdmittedAttemptContext,
        StrictAttemptResult,
        &str,
        &str,
        Option<i32>,
    ); 3] = [
        (
            accepted(1, 1),
            attempt_result(
                true,
                None,
                false,
                false,
                AttemptOutcome::Uncertain,
                AttemptErrorCode::BodyLost,
            ),
            "outcome_unknown",
            "body_lost",
            None,
        ),
        (
            accepted(2, 2),
            attempt_result(
                true,
                Some(500),
                true,
                false,
                AttemptOutcome::Uncertain,
                AttemptErrorCode::Unknown,
            ),
            "outcome_unknown",
            "uncertain",
            Some(500),
        ),
        (
            accepted(3, 3),
            attempt_result(
                true,
                Some(429),
                true,
                false,
                AttemptOutcome::ExplicitRejection,
                AttemptErrorCode::ProviderRejected,
            ),
            "explicit_rejection",
            "explicit_rejection",
            Some(429),
        ),
    ];
    for (admitted, result, status, stage, http) in &cases {
        let body = wire_body(admitted, result, None, None);
        persist(&mut conn, admitted, result, &body);
        let row = rows(&conn).into_iter().last().expect("row");
        assert_frozen(&row, admitted);
        assert_eq!(row.status, *status);
        assert_eq!(row.error_stage.as_deref(), Some(*stage));
        assert_eq!(row.error_message.as_deref(), Some(*stage));
        assert_eq!(row.http_status, *http);
        assert_eq!(diagnostic_of(&row)["completeness"], *stage);
    }
    assert_eq!(count(&conn), 3);
    assert_eq!(success_rows(&conn), 0);
    let stages = rows(&conn)
        .into_iter()
        .map(|row| row.error_stage.expect("stage"))
        .collect::<Vec<_>>();
    assert_eq!(stages, ["body_lost", "uncertain", "explicit_rejection"]);
}

#[test]
fn definite_rejection_statuses_are_explicit() {
    let mut conn = harness();
    for (ordinal, status) in [(1_u32, 401_u16), (2, 403), (3, 429)] {
        let admitted = accepted(ordinal, u8::try_from(ordinal).expect("ordinal"));
        let result = attempt_result(
            true,
            Some(status),
            true,
            false,
            AttemptOutcome::ExplicitRejection,
            AttemptErrorCode::ProviderRejected,
        );
        persist(
            &mut conn,
            &admitted,
            &result,
            &wire_body(&admitted, &result, None, None),
        );
    }
    assert_eq!(count(&conn), 3);
    assert!(
        rows(&conn)
            .iter()
            .all(|row| row.status == "explicit_rejection")
    );
    assert_eq!(success_rows(&conn), 0);
}

#[test]
fn a_non_failover_explicit_status_stays_uncertain() {
    let mut conn = harness();
    let admitted = accepted(1, 1);
    let result = attempt_result(
        true,
        Some(404),
        true,
        false,
        AttemptOutcome::ExplicitRejection,
        AttemptErrorCode::ProviderRejected,
    );
    persist(
        &mut conn,
        &admitted,
        &result,
        &wire_body(&admitted, &result, None, None),
    );
    let row = only(&conn);
    assert_eq!(row.status, "outcome_unknown");
    assert_eq!(row.http_status, Some(404));
    assert_eq!(row.error_stage.as_deref(), Some("uncertain"));
    let diagnostic = diagnostic_of(&row);
    assert_eq!(diagnostic["outcome"], "explicit_rejection");
    assert_eq!(diagnostic["error_code"], "provider_rejected");
    assert_eq!(diagnostic["completeness"], "uncertain");
}

#[test]
fn cancelled_result_keeps_the_captured_client_key() {
    let mut conn = harness();
    let admitted = accepted(1, 1);
    let result = attempt_result(
        true,
        None,
        false,
        true,
        AttemptOutcome::Cancelled,
        AttemptErrorCode::Cancelled,
    );
    persist(
        &mut conn,
        &admitted,
        &result,
        &wire_body(&admitted, &result, None, None),
    );
    let row = only(&conn);
    assert_frozen(&row, &admitted);
    assert_eq!(row.status, "cancelled");
    assert_eq!(row.http_status, None);
    assert_eq!(row.client_key_id.as_deref(), Some("key-1"));
    assert_eq!(row.client_key_name.as_deref(), Some("Key One"));
    assert_eq!(row.error_stage.as_deref(), Some("cancelled"));
    let tables: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = 'access_keys'",
            [],
            |row| row.get(0),
        )
        .expect("tables");
    assert_eq!(tables, 0);
}

#[test]
fn deadline_wins_over_a_lost_body() {
    let mut conn = harness();
    let admitted = accepted(1, 1);
    let result = attempt_result(
        true,
        None,
        false,
        false,
        AttemptOutcome::Deadline,
        AttemptErrorCode::Deadline,
    );
    persist(
        &mut conn,
        &admitted,
        &result,
        &wire_body(&admitted, &result, None, None),
    );
    let row = only(&conn);
    assert_eq!(row.status, "deadline");
    assert_eq!(row.http_status, None);
    assert_eq!(row.error_stage.as_deref(), Some("deadline"));
    assert_eq!(diagnostic_of(&row)["completeness"], "deadline");
}

#[test]
fn unsent_validation_is_an_error() {
    let mut conn = harness();
    let admitted = accepted(1, 1);
    let result = attempt_result(
        false,
        None,
        false,
        false,
        AttemptOutcome::LocalFailure,
        AttemptErrorCode::Validation,
    );
    persist(
        &mut conn,
        &admitted,
        &result,
        &wire_body(&admitted, &result, None, None),
    );
    let row = only(&conn);
    assert_frozen(&row, &admitted);
    assert_eq!(row.status, "error");
    assert_eq!(row.http_status, None);
    assert_eq!(row.error_message.as_deref(), Some("validation"));
    assert_eq!(row.error_stage.as_deref(), Some("validation"));
    assert_eq!(diagnostic_of(&row)["completeness"], "validation");
}

#[test]
fn a_sent_local_failure_is_uncertain() {
    let mut conn = harness();
    let admitted = accepted(1, 1);
    let result = attempt_result(
        true,
        Some(200),
        true,
        false,
        AttemptOutcome::LocalFailure,
        AttemptErrorCode::Transport,
    );
    persist(
        &mut conn,
        &admitted,
        &result,
        &wire_body(&admitted, &result, None, None),
    );
    let row = only(&conn);
    assert_eq!(row.status, "outcome_unknown");
    assert_eq!(row.error_stage.as_deref(), Some("uncertain"));
    let diagnostic = diagnostic_of(&row);
    assert_eq!(diagnostic["outcome"], "local_failure");
    assert_eq!(diagnostic["error_code"], "transport");
    assert_eq!(diagnostic["completeness"], "uncertain");
    assert_eq!(success_rows(&conn), 0);
}

#[test]
fn usage_observation_on_a_complete_success_stores_the_tokens() {
    let mut conn = harness();
    let admitted = accepted(1, 1);
    let result = attempt_result(
        true,
        Some(200),
        true,
        true,
        AttemptOutcome::Success,
        AttemptErrorCode::UsageObservation,
    );
    let body = wire_body(
        &admitted,
        &result,
        Some(json!({"inputTokens": 8, "outputTokens": 2})),
        None,
    );
    persist(&mut conn, &admitted, &result, &body);
    let row = only(&conn);
    assert_frozen(&row, &admitted);
    assert_eq!(row.status, "success");
    assert_eq!(row.error_message, None);
    assert_eq!(row.error_stage.as_deref(), Some("complete"));
    assert_eq!(row.prompt_tokens, 8);
    assert_eq!(row.completion_tokens, 2);
    let diagnostic = diagnostic_of(&row);
    assert_eq!(diagnostic["error_code"], "usage_observation");
    assert_eq!(diagnostic["usage_state"], "reported");
    assert_eq!(diagnostic["completeness"], "complete");
    assert_eq!(success_rows(&conn), 1);
}

#[test]
fn two_attempt_ids_under_one_request_are_two_rows() {
    let mut conn = harness();
    let result = clean_success();
    let first = accepted(1, 1);
    let second = accepted(2, 2);
    persist(
        &mut conn,
        &first,
        &result,
        &wire_body(&first, &result, None, None),
    );
    persist(
        &mut conn,
        &second,
        &result,
        &wire_body(&second, &result, None, None),
    );
    assert_eq!(count(&conn), 2);
    let found = rows(&conn);
    assert_eq!(found[0].request_id, found[1].request_id);
    assert_ne!(found[0].attempt, found[1].attempt);
    assert_eq!(
        diagnostic_of(&found[0])["attempt_id"],
        first.attempt_id.to_string().as_str()
    );
    assert_eq!(
        diagnostic_of(&found[1])["attempt_id"],
        second.attempt_id.to_string().as_str()
    );
    assert_frozen(&found[0], &first);
    assert_frozen(&found[1], &second);
}

#[test]
fn an_uppercase_attempt_id_is_stored_in_canonical_form() {
    let mut conn = harness();
    let admitted = accepted(1, 1);
    let result = clean_success();
    let mut body =
        serde_json::from_str::<Value>(&wire_body(&admitted, &result, None, None)).expect("json");
    body["requestId"] = json!(admitted.request_id.to_string().to_ascii_uppercase());
    body["attemptId"] = json!(admitted.attempt_id.to_string().to_ascii_uppercase());
    let inserted = persist(&mut conn, &admitted, &result, &body.to_string());
    let row = only(&conn);
    let stored_request = row.request_id.clone().expect("request");
    assert_eq!(stored_request, admitted.request_id.to_string());
    assert_eq!(stored_request, stored_request.to_ascii_lowercase());
    assert_eq!(
        diagnostic_of(&row)["attempt_id"],
        admitted.attempt_id.to_string().as_str()
    );
    let replay = commit(
        &mut conn,
        &admitted,
        &result,
        &wire_body(&admitted, &result, None, None),
    )
    .expect("replay");
    assert!(matches!(replay, AttemptRecord::Existing(id) if id == record_id(inserted)));
    assert_eq!(count(&conn), 1);
}

#[test]
fn a_leading_zero_credential_version_writes_nothing() {
    let mut conn = harness();
    let admitted = accepted(1, 1);
    let result = clean_success();
    let mut body =
        serde_json::from_str::<Value>(&wire_body(&admitted, &result, None, None)).expect("json");
    body["credentialVersion"] = json!("07");
    let err = reject(&mut conn, &admitted, &result, &body.to_string());
    assert_eq!(err, ObservationError::Mismatch);
}

#[test]
fn http_status_above_599_writes_nothing() {
    let mut conn = harness();
    let admitted = accepted(1, 1);
    let result = attempt_result(
        true,
        Some(600),
        true,
        false,
        AttemptOutcome::Success,
        AttemptErrorCode::None,
    );
    let body = wire_body(&admitted, &result, None, None);
    let err = reject(&mut conn, &admitted, &result, &body);
    assert_eq!(err, ObservationError::Mismatch);
}

fn public_trace(last: u8) -> String {
    format!("ocg-{}", id(last))
}

fn request_rows(conn: &Connection, request_id: &str) -> i64 {
    conn.query_row(
        "SELECT COUNT(*) FROM forward_logs WHERE request_id = ?1",
        [request_id],
        |row| row.get(0),
    )
    .expect("request rows")
}

#[test]
fn a_public_trace_is_the_log_request_id_and_the_private_uuid_stays_diagnostic() {
    let mut conn = harness();
    let trace = public_trace(0x21);
    let mut first = accepted(1, 1);
    first.client_trace_id = Some(trace.clone());
    let mut second = accepted(2, 2);
    second.client_trace_id = Some(trace.clone());
    let result = clean_success();
    persist(
        &mut conn,
        &first,
        &result,
        &wire_body(&first, &result, None, None),
    );
    persist(
        &mut conn,
        &second,
        &result,
        &wire_body(&second, &result, None, None),
    );
    assert_eq!(count(&conn), 2);
    assert_eq!(request_rows(&conn, &trace), 2);
    assert_eq!(request_rows(&conn, &first.request_id.to_string()), 0);
    for (row, admitted) in rows(&conn).into_iter().zip([&first, &second]) {
        assert_frozen(&row, admitted);
        assert_eq!(row.request_id.as_deref(), Some(trace.as_str()));
        let diagnostic = diagnostic_of(&row);
        assert_eq!(
            diagnostic["request_id"],
            admitted.request_id.to_string().as_str()
        );
        assert_eq!(diagnostic["client_trace_id"], trace.as_str());
        assert_eq!(diagnostic["public_model"], "public-model");
        assert_eq!(diagnostic["upstream_model"], "upstream-model");
    }
}

#[test]
fn replay_and_a_cleared_diagnostic_stay_on_the_public_trace() {
    let mut conn = harness();
    let trace = public_trace(0x22);
    let mut admitted = accepted(1, 1);
    admitted.client_trace_id = Some(trace.clone());
    let result = clean_success();
    let body = wire_body(
        &admitted,
        &result,
        Some(json!({"inputTokens": 6, "outputTokens": 1})),
        None,
    );
    let inserted = record_id(persist(&mut conn, &admitted, &result, &body));
    let mut mixed = admitted.clone();
    let uuid = trace
        .strip_prefix("ocg-")
        .expect("prefix")
        .to_ascii_uppercase();
    mixed.client_trace_id = Some(format!("ocg-{uuid}"));
    let replay = commit(&mut conn, &mixed, &result, "{").expect("case fold");
    assert!(matches!(replay, AttemptRecord::Existing(id) if id == inserted));
    assert_eq!(count(&conn), 1);
    assert_eq!(only(&conn).request_id.as_deref(), Some(trace.as_str()));
    assert_eq!(only(&conn).prompt_tokens, 6);

    conn.execute(
        "UPDATE forward_logs SET diagnostic_json = NULL WHERE id = ?1",
        [inserted],
    )
    .expect("clear");
    let again = commit(&mut conn, &admitted, &result, "{").expect("cleared");
    assert!(matches!(again, AttemptRecord::Existing(id) if id == inserted));
    let mut other = accepted(1, 2);
    other.client_trace_id = Some(trace.clone());
    let blocked = commit(
        &mut conn,
        &other,
        &result,
        &wire_body(
            &other,
            &result,
            Some(json!({"inputTokens": 9, "outputTokens": 9})),
            None,
        ),
    )
    .expect("ordinal");
    assert!(matches!(blocked, AttemptRecord::Existing(id) if id == inserted));
    assert_eq!(count(&conn), 1);
    assert_eq!(request_rows(&conn, &trace), 1);
    assert_eq!(request_rows(&conn, &admitted.request_id.to_string()), 0);
    let row = only(&conn);
    assert_eq!(row.request_id.as_deref(), Some(trace.as_str()));
    assert_eq!(row.prompt_tokens, 6);
    assert_eq!(row.diagnostic_json, None);
}

#[test]
fn a_result_body_trace_does_not_become_the_log_request_id() {
    let mut conn = harness();
    let admitted = accepted(1, 1);
    let result = clean_success();
    let decoy = public_trace(0x2a);
    let mut body =
        serde_json::from_str::<Value>(&wire_body(&admitted, &result, None, None)).expect("json");
    body["clientTraceId"] = json!(decoy);
    body["x-ocg-request-id"] = json!(decoy);
    persist(&mut conn, &admitted, &result, &body.to_string());
    let row = only(&conn);
    assert_frozen(&row, &admitted);
    assert_eq!(
        row.request_id.as_deref(),
        Some(admitted.request_id.to_string().as_str())
    );
    assert!(diagnostic_of(&row)["client_trace_id"].is_null());
    let stored = row.diagnostic_json.expect("diagnostic");
    assert!(!stored.contains(&decoy));
    assert!(!row.request_id.as_deref().unwrap().contains(&decoy));
    assert_eq!(request_rows(&conn, &decoy), 0);
}

#[test]
fn the_result_request_id_stays_the_private_uuid_when_a_public_trace_is_present() {
    let mut conn = harness();
    let mut admitted = accepted(1, 1);
    let trace = public_trace(0x23);
    admitted.client_trace_id = Some(trace.clone());
    let result = clean_success();
    let mut body =
        serde_json::from_str::<Value>(&wire_body(&admitted, &result, None, None)).expect("json");
    body["requestId"] = json!(trace);
    let err = reject(&mut conn, &admitted, &result, &body.to_string());
    assert_eq!(err, ObservationError::Mismatch);
    assert_eq!(request_rows(&conn, &trace), 0);
}

#[test]
fn a_trace_that_is_not_a_generated_ocg_id_writes_nothing() {
    let result = clean_success();
    let private_id = id(0x10).to_string();
    let cases = [
        "client-supplied",
        private_id.as_str(),
        "ocg-not-a-uuid",
        "",
        "ocg-00000000-0000-4000-8000-000000000021-extra",
        "OCG-00000000-0000-4000-8000-000000000021",
    ];
    for trace in cases {
        let mut conn = harness();
        let mut admitted = accepted(1, 1);
        admitted.client_trace_id = Some(trace.to_string());
        let err = reject(&mut conn, &admitted, &result, "not-json");
        assert_eq!(err, ObservationError::Identity);
    }
}

#[test]
fn a_missing_log_table_is_a_store_error() {
    let mut conn = Connection::open_in_memory().expect("memory db");
    let admitted = accepted(1, 1);
    let result = clean_success();
    let body = wire_body(&admitted, &result, None, None);
    let err = commit(&mut conn, &admitted, &result, &body).expect_err("store");
    assert_eq!(err, ObservationError::Store);
}
