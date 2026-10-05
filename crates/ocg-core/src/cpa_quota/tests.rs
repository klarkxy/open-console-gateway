use super::*;
use crate::cpa_policy::{PolicyDocument, Reset, SETTINGS_KEY, Window, read_settings};
use chrono::{DateTime, Duration, TimeZone, Utc};
use rusqlite::{Connection, params};
use serde_json::{Value, json};
use std::path::PathBuf;

const SCHEMA: &str = "
CREATE TABLE settings (key TEXT PRIMARY KEY, value TEXT NOT NULL);
CREATE TABLE credentials (
    id TEXT PRIMARY KEY,
    legacy_account_id TEXT NOT NULL,
    provider_id TEXT NOT NULL,
    binding_id TEXT NOT NULL,
    credential_version INTEGER NOT NULL,
    key_cipher TEXT NOT NULL,
    enabled INTEGER NOT NULL,
    scope_json TEXT NOT NULL,
    destination_id TEXT NOT NULL,
    credential_purpose TEXT NOT NULL,
    quota_recovery_json TEXT
);
CREATE TABLE credential_grants (
    credential_id TEXT NOT NULL,
    kind TEXT NOT NULL,
    value TEXT NOT NULL
);
CREATE TABLE quota_pool_members (
    pool_id TEXT NOT NULL,
    account_id TEXT NOT NULL
);
";

struct DirGuard(PathBuf);

impl Drop for DirGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn open() -> (DirGuard, Connection) {
    let dir = std::env::temp_dir().join(format!("ocg-cpa-quota-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let conn = Connection::open(dir.join("quota.sqlite")).unwrap();
    conn.execute_batch(SCHEMA).unwrap();
    (DirGuard(dir), conn)
}

fn at(seconds: i64) -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 10, 4, 0, 0, 0).unwrap() + Duration::seconds(seconds)
}

fn insert(conn: &Connection, enabled: i64, scope: &str) {
    conn.execute(
        "INSERT INTO credentials (
            id, legacy_account_id, provider_id, binding_id, credential_version, key_cipher,
            enabled, scope_json, destination_id, credential_purpose, quota_recovery_json
         ) VALUES ('cid', 'legacy', 'opencode', 'bind', 3, 'cipher-v1', ?1, ?2, 'dest-1',
                   'inference', '{\"epoch\":7}')",
        params![enabled, scope],
    )
    .unwrap();
}

fn grant(conn: &Connection, kind: &str, value: &str) {
    conn.execute(
        "INSERT INTO credential_grants (credential_id, kind, value) VALUES ('cid', ?1, ?2)",
        params![kind, value],
    )
    .unwrap();
}

fn pool(conn: &Connection, pool_id: &str, account_id: &str) {
    conn.execute(
        "INSERT INTO quota_pool_members (pool_id, account_id) VALUES (?1, ?2)",
        params![pool_id, account_id],
    )
    .unwrap();
}

fn body(
    rolling: &str,
    rolling_at: DateTime<Utc>,
    weekly: &str,
    weekly_at: DateTime<Utc>,
    percent: &str,
) -> Vec<u8> {
    let monthly = at(40 * 24 * 60 * 60);
    format!(
        r#"{{"usage":{{"rolling":{{"status":"{rolling}","percent":{percent},"resetsAt":"{rolling_at}"}},"weekly":{{"status":"{weekly}","percent":0,"resetsAt":"{weekly_at}"}},"monthly":{{"status":"ok","percent":1,"resetsAt":"{monthly}"}}}},"note":"sk-planted-secret"}}"#,
        rolling_at = rolling_at.to_rfc3339(),
        weekly_at = weekly_at.to_rfc3339(),
        monthly = monthly.to_rfc3339(),
    )
    .into_bytes()
}

fn commit(
    conn: &Connection,
    id: &str,
    fetched_at: DateTime<Utc>,
    body: Vec<u8>,
) -> OfficialPlanCommit {
    OfficialPlanCommit {
        fence: capture_live_fence(conn, "cid").unwrap(),
        observation_id: id.to_string(),
        fetched_at,
        body,
        provider_id: OPENCODE_PROVIDER_ID.to_string(),
    }
}

fn apply(
    conn: &mut Connection,
    id: &str,
    fetched_at: DateTime<Utc>,
    body: Vec<u8>,
) -> Result<QuotaApply, PolicyFault> {
    let commit = commit(conn, id, fetched_at, body);
    apply_accepted(conn, &commit)
}

fn known(conn: &Connection, window: Window) -> Option<DateTime<Utc>> {
    let document = read_settings(conn, SETTINGS_KEY).unwrap();
    document.restrictions.iter().find_map(|row| {
        if row.window != window {
            return None;
        }
        match row.reset {
            Reset::Known { at } => Some(at),
            Reset::Unknown => None,
        }
    })
}

fn windows(conn: &Connection) -> Vec<Window> {
    read_settings(conn, SETTINGS_KEY)
        .unwrap()
        .restrictions
        .iter()
        .map(|row| row.window)
        .collect()
}

fn settings_text(conn: &Connection) -> String {
    conn.query_row(
        "SELECT value FROM settings WHERE key = ?1",
        [SETTINGS_KEY],
        |row| row.get(0),
    )
    .unwrap()
}

fn recovery_json(conn: &Connection) -> String {
    conn.query_row(
        "SELECT quota_recovery_json FROM credentials WHERE id = 'cid'",
        [],
        |row| row.get(0),
    )
    .unwrap()
}

#[test]
fn exact_resets_at_is_stored_and_minutes_are_not_the_deadline() {
    let (_dir, mut conn) = open();
    insert(&conn, 1, r#"{"kind":"all"}"#);
    grant(&conn, "endpoint_id", "ep-1");
    let rolling = at(90);
    let applied = apply(
        &mut conn,
        "obs-1",
        at(0),
        body("rate-limited", rolling, "ok", at(2 * 24 * 60 * 60), "0"),
    )
    .unwrap();
    assert_eq!(applied, QuotaApply::Applied);
    assert_eq!(known(&conn, Window::FiveHours), Some(rolling));
    assert!(known(&conn, Window::Week).is_none());
    assert!(!settings_text(&conn).contains("sk-planted-secret"));
    assert_eq!(recovery_json(&conn), r#"{"epoch":7}"#);
    let plan = present_credential(&conn, "cid", at(0)).unwrap().unwrap();
    assert_eq!(plan.card.status, PlanStatus::Waiting);
    assert_eq!(plan.card.window, PlanWindowLabel::FiveHours);
    assert_eq!(plan.card.resets_at, Some(rolling));
    assert_eq!(plan.card.next_retry_at, rolling);
}

#[test]
fn healthy_newer_observation_clears_only_the_matching_scope_and_window() {
    let (_dir, mut conn) = open();
    insert(&conn, 1, r#"{"kind":"only","models":["kept"]}"#);
    let first = at(90);
    let weekly = at(2 * 24 * 60 * 60);
    apply(
        &mut conn,
        "obs-1",
        at(0),
        body("rate-limited", first, "rate-limited", weekly, "100"),
    )
    .unwrap();
    let mut value: Value = serde_json::from_str(&settings_text(&conn)).unwrap();
    let mut extra = value["restrictions"][0].clone();
    extra["scope"]["public_model"] = json!("other");
    extra["observation_id"] = json!("outside-scope");
    extra["window"] = json!("week");
    value["restrictions"].as_array_mut().unwrap().push(extra);
    let mut outside_pool = value["restrictions"][0].clone();
    outside_pool["scope"]["subject"] = json!({
        "kind": "pool",
        "pool_id": "not-a-member",
        "pool_version": 2
    });
    outside_pool["scope"]["public_model"] = json!("kept");
    outside_pool["observation_id"] = json!("outside-pool");
    outside_pool["window"] = json!("month");
    value["restrictions"]
        .as_array_mut()
        .unwrap()
        .push(outside_pool);
    let text = value.to_string();
    PolicyDocument::from_json(&text).unwrap();
    conn.execute(
        "UPDATE settings SET value = ?1 WHERE key = ?2",
        params![text, SETTINGS_KEY],
    )
    .unwrap();

    let cleared = apply(
        &mut conn,
        "obs-2",
        at(30),
        body("ok", at(120), "rate-limited", weekly, "100"),
    )
    .unwrap();
    assert_eq!(cleared, QuotaApply::Applied);
    let document = read_settings(&conn, SETTINGS_KEY).unwrap();
    assert!(document.restrictions.iter().all(|row| {
        row.scope.public_model.as_deref() != Some("kept") || row.window != Window::FiveHours
    }));
    assert!(document.restrictions.iter().any(|row| {
        row.window == Window::Week && row.scope.public_model.as_deref() == Some("kept")
    }));
    assert_eq!(known(&conn, Window::Week), Some(weekly));
    assert!(
        document
            .restrictions
            .iter()
            .any(|row| row.scope.public_model.as_deref() == Some("other"))
    );
    assert!(document.restrictions.iter().any(|row| matches!(
        &row.scope.subject,
        crate::cpa_policy::Subject::Pool { pool_id, pool_version }
            if pool_id == "not-a-member" && *pool_version == 2
    )));
}

#[test]
fn stale_malformed_expired_and_replay_do_not_clear() {
    let (_dir, mut conn) = open();
    insert(&conn, 0, r#"{"kind":"all"}"#);
    let deadline = at(90);
    apply(
        &mut conn,
        "obs-1",
        at(0),
        body("rate-limited", deadline, "ok", at(86_400), "4"),
    )
    .unwrap();
    assert_eq!(known(&conn, Window::FiveHours), Some(deadline));

    let older = apply(
        &mut conn,
        "obs-old",
        at(0) - Duration::seconds(5),
        body("ok", at(30), "ok", at(86_400), "0"),
    )
    .unwrap();
    assert_eq!(older, QuotaApply::Applied);
    assert_eq!(known(&conn, Window::FiveHours), Some(deadline));

    let replay = apply(
        &mut conn,
        "obs-1",
        at(10),
        body("ok", at(30), "ok", at(86_400), "0"),
    )
    .unwrap();
    assert_eq!(replay, QuotaApply::Applied);
    assert_eq!(known(&conn, Window::FiveHours), Some(deadline));

    let expired = apply(
        &mut conn,
        "obs-expired",
        at(20),
        body("rate-limited", at(20), "ok", at(86_400), "0"),
    );
    assert!(expired.is_ok());
    assert_eq!(known(&conn, Window::FiveHours), Some(deadline));

    let malformed = apply(&mut conn, "obs-bad", at(21), b"{".to_vec());
    assert_eq!(malformed, Err(PolicyFault::Malformed));
    assert_eq!(known(&conn, Window::FiveHours), Some(deadline));

    let ordinary = apply(
        &mut conn,
        "obs-429",
        at(22),
        br#"{"error":"too many requests"}"#.to_vec(),
    );
    assert_eq!(ordinary, Err(PolicyFault::Malformed));
    assert_eq!(known(&conn, Window::FiveHours), Some(deadline));
    let saved = capture_live_fence(&conn, "cid").unwrap();

    conn.execute(
        "UPDATE credentials SET credential_version = 4 WHERE id = 'cid'",
        [],
    )
    .unwrap();
    let stale_version = apply_accepted(
        &mut conn,
        &saved_commit(&saved, "obs-stale-version", at(23)),
    )
    .unwrap();
    assert_eq!(stale_version, QuotaApply::Stale);
    conn.execute(
        "UPDATE credentials SET credential_version = 3, binding_id = 'other-bind' WHERE id = 'cid'",
        [],
    )
    .unwrap();
    let stale_binding = apply_accepted(
        &mut conn,
        &saved_commit(&saved, "obs-stale-binding", at(24)),
    )
    .unwrap();
    assert_eq!(stale_binding, QuotaApply::Stale);
    conn.execute(
        "UPDATE credentials SET binding_id = 'bind', key_cipher = 'cipher-v2' WHERE id = 'cid'",
        [],
    )
    .unwrap();
    let stale_key =
        apply_accepted(&mut conn, &saved_commit(&saved, "obs-stale-key", at(25))).unwrap();
    assert_eq!(stale_key, QuotaApply::Stale);
    conn.execute(
        "UPDATE credentials SET key_cipher = 'cipher-v1' WHERE id = 'cid'",
        [],
    )
    .unwrap();
    assert_eq!(recovery_json(&conn), r#"{"epoch":7}"#);
    grant(&conn, "origin", "https://loopback.invalid");
    let stale_grant = {
        let mut fence = capture_live_fence(&conn, "cid").unwrap();
        fence.origins.clear();
        apply_accepted(
            &mut conn,
            &OfficialPlanCommit {
                fence,
                observation_id: "obs-stale-grant".into(),
                fetched_at: at(26),
                body: body("ok", at(40), "ok", at(86_400), "0"),
                provider_id: OPENCODE_PROVIDER_ID.into(),
            },
        )
        .unwrap()
    };
    assert_eq!(stale_grant, QuotaApply::Stale);
    pool(&conn, "pool-a", "cid");
    let stale_pool = {
        let mut fence = capture_live_fence(&conn, "cid").unwrap();
        fence.pool_ids.clear();
        apply_accepted(
            &mut conn,
            &OfficialPlanCommit {
                fence,
                observation_id: "obs-stale-pool".into(),
                fetched_at: at(27),
                body: body("ok", at(40), "ok", at(86_400), "0"),
                provider_id: OPENCODE_PROVIDER_ID.into(),
            },
        )
        .unwrap()
    };
    assert_eq!(stale_pool, QuotaApply::Stale);
    conn.execute("DELETE FROM credentials WHERE id = 'cid'", [])
        .unwrap();
    let deleted = apply_accepted(
        &mut conn,
        &OfficialPlanCommit {
            fence: QuotaFence {
                credential_id: "cid".into(),
                legacy_account_id: "legacy".into(),
                credential_version: 3,
                provider_id: "opencode".into(),
                binding_id: "bind".into(),
                key_cipher: "cipher-v1".into(),
                scope: ModelScope::All,
                endpoint_ids: Vec::new(),
                origins: vec!["https://loopback.invalid".into()],
                pool_ids: vec!["pool-a".into()],
            },
            observation_id: "obs-deleted".into(),
            fetched_at: at(28),
            body: body("ok", at(40), "ok", at(86_400), "0"),
            provider_id: OPENCODE_PROVIDER_ID.into(),
        },
    )
    .unwrap();
    assert_eq!(deleted, QuotaApply::Stale);
    assert_eq!(known(&conn, Window::FiveHours), Some(deadline));
}

fn saved_commit(fence: &QuotaFence, id: &str, fetched_at: DateTime<Utc>) -> OfficialPlanCommit {
    OfficialPlanCommit {
        fence: fence.clone(),
        observation_id: id.to_string(),
        fetched_at,
        body: body("ok", at(40), "ok", at(86_400), "0"),
        provider_id: OPENCODE_PROVIDER_ID.to_string(),
    }
}

#[test]
fn later_deadline_extends_and_earlier_deadline_does_not_shorten() {
    let (_dir, mut conn) = open();
    insert(&conn, 1, r#"{"kind":"all"}"#);
    let first = at(120);
    let later = at(240);
    let earlier = at(150);
    apply(
        &mut conn,
        "obs-1",
        at(0),
        body("rate-limited", first, "ok", at(86_400), "0"),
    )
    .unwrap();
    apply(
        &mut conn,
        "obs-2",
        at(10),
        body("rate-limited", later, "ok", at(86_400), "0"),
    )
    .unwrap();
    assert_eq!(known(&conn, Window::FiveHours), Some(later));
    apply(
        &mut conn,
        "obs-3",
        at(20),
        body("rate-limited", earlier, "ok", at(86_400), "0"),
    )
    .unwrap();
    assert_eq!(known(&conn, Window::FiveHours), Some(later));
    assert_eq!(windows(&conn), vec![Window::FiveHours]);
}

#[test]
fn percent_and_healthy_status_do_not_mint_a_restriction() {
    let (_dir, mut conn) = open();
    insert(&conn, 1, r#"{"kind":"all"}"#);
    let applied = apply(
        &mut conn,
        "obs-healthy",
        at(0),
        body("ok", at(90), "ok", at(86_400), "100"),
    )
    .unwrap();
    assert_eq!(applied, QuotaApply::Applied);
    assert!(windows(&conn).is_empty());
    apply(
        &mut conn,
        "obs-limited",
        at(5),
        body("rate-limited", at(90), "ok", at(86_400), "0"),
    )
    .unwrap();
    assert_eq!(known(&conn, Window::FiveHours), Some(at(90)));
    apply(
        &mut conn,
        "obs-clear",
        at(6),
        body("ok", at(90), "ok", at(86_400), "100"),
    )
    .unwrap();
    assert!(windows(&conn).is_empty());
}

#[test]
fn manual_opportunity_clears_only_the_unknown_gap() {
    let (_dir, mut conn) = open();
    insert(&conn, 1, r#"{"kind":"all"}"#);
    let week = at(2 * 24 * 60 * 60);
    apply(
        &mut conn,
        "obs-week",
        at(0),
        body("ok", at(60), "rate-limited", week, "0"),
    )
    .unwrap();
    let mut value: Value = serde_json::from_str(&settings_text(&conn)).unwrap();
    let mut unknown = value["restrictions"][0].clone();
    unknown["window"] = json!("five_hours");
    unknown["reset"] = json!({"kind": "unknown"});
    unknown["observation_id"] = json!("obs-unknown");
    unknown["recovery"] = json!({"last_admit_at": at(0).to_rfc3339()});
    value["restrictions"].as_array_mut().unwrap().push(unknown);
    let text = value.to_string();
    PolicyDocument::from_json(&text).unwrap();
    conn.execute(
        "UPDATE settings SET value = ?1 WHERE key = ?2",
        params![text, SETTINGS_KEY],
    )
    .unwrap();

    let now = at(10);
    let plan = present_credential(&conn, "cid", now).unwrap().unwrap();
    assert_eq!(plan.card.status, PlanStatus::Waiting);
    assert_eq!(plan.card.resets_at, Some(week));
    assert_eq!(
        mark_manual_opportunity(&mut conn, "cid", now).unwrap(),
        ManualOpportunity::Opened
    );
    assert_eq!(known(&conn, Window::Week), Some(week));
    let opened = settings_text(&conn);
    assert!(!opened.contains("last_admit_at"));
    let plan = present_credential(&conn, "cid", now).unwrap().unwrap();
    assert_eq!(plan.card.status, PlanStatus::Waiting);
    assert_eq!(plan.card.next_retry_at, week);
    assert_eq!(
        mark_manual_opportunity(&mut conn, "cid", now).unwrap(),
        ManualOpportunity::Unchanged
    );
    assert_eq!(recovery_json(&conn), r#"{"epoch":7}"#);
}

#[test]
fn manual_opportunity_does_not_clear_a_live_inflight_attempt() {
    let (_dir, mut conn) = open();
    insert(&conn, 1, r#"{"kind":"all"}"#);
    apply(
        &mut conn,
        "obs-1",
        at(0),
        body("rate-limited", at(90), "ok", at(86_400), "0"),
    )
    .unwrap();
    let attempt_id = uuid::Uuid::new_v4();
    let mut value: Value = serde_json::from_str(&settings_text(&conn)).unwrap();
    value["restrictions"][0]["reset"] = json!({"kind": "unknown"});
    value["restrictions"][0]["recovery"] = json!({
        "last_admit_at": at(0).to_rfc3339(),
        "inflight": [{
            "request_id": uuid::Uuid::new_v4().to_string(),
            "attempt_id": attempt_id.to_string(),
            "at": at(0).to_rfc3339()
        }]
    });
    let text = value.to_string();
    let mut document = PolicyDocument::from_json(&text).unwrap();
    document.attempts.push(crate::cpa_policy::AdmittedAttempt {
        request_id: uuid::Uuid::new_v4(),
        attempt_id,
        credential_id: "cid".into(),
        credential_version: 3,
        provider_id: "opencode".into(),
        binding_id: "bind".into(),
        public_model: "kept".into(),
        upstream_model: "kept".into(),
        auth_id: "cid".into(),
        material_revision: "quota-fence".into(),
        registration_epoch: 0,
        process_generation: 1,
        projection_revision: 1,
        projection_digest: [9; 32],
        kind: crate::cpa_policy::SendKind::Accepted,
        admitted_at: at(0),
        retain_until: at(80),
        scopes: Vec::new(),
    });
    let stored = document.to_json().unwrap();
    conn.execute(
        "UPDATE settings SET value = ?1 WHERE key = ?2",
        params![stored, SETTINGS_KEY],
    )
    .unwrap();

    let now = at(10);
    let plan = present_credential(&conn, "cid", now).unwrap().unwrap();
    assert_eq!(plan.card.status, PlanStatus::Probing);
    assert!(plan.rows.iter().any(|row| row.probe_in_flight));
    assert_eq!(
        mark_manual_opportunity(&mut conn, "cid", now).unwrap(),
        ManualOpportunity::Opened
    );
    let plan = present_credential(&conn, "cid", now).unwrap().unwrap();
    assert_eq!(plan.card.status, PlanStatus::Probing);
    assert!(settings_text(&conn).contains(&attempt_id.to_string()));
    assert!(!settings_text(&conn).contains("last_admit_at"));
}

#[test]
fn apply_on_the_caller_connection_persists_file_and_memory() {
    let (_dir, conn) = open();
    insert(&conn, 1, r#"{"kind":"all"}"#);
    let file_commit = commit(
        &conn,
        "obs-share",
        at(0),
        body("rate-limited", at(90), "ok", at(86_400), "0"),
    );
    assert_eq!(
        apply_sharing(&conn, &file_commit).unwrap(),
        QuotaApply::Applied
    );
    assert_eq!(known(&conn, Window::FiveHours), Some(at(90)));

    let memory = Connection::open_in_memory().unwrap();
    memory.execute_batch(SCHEMA).unwrap();
    insert(&memory, 1, r#"{"kind":"all"}"#);
    let memory_commit = commit(
        &memory,
        "obs-memory",
        at(0),
        body("rate-limited", at(90), "ok", at(86_400), "0"),
    );
    assert_eq!(
        apply_sharing(&memory, &memory_commit).unwrap(),
        QuotaApply::Applied
    );
    assert_eq!(known(&memory, Window::FiveHours), Some(at(90)));
}

#[test]
fn empty_scope_is_stale_and_does_not_clear() {
    let (_dir, mut conn) = open();
    insert(&conn, 1, r#"{"kind":"all"}"#);
    apply(
        &mut conn,
        "obs-1",
        at(0),
        body("rate-limited", at(90), "ok", at(86_400), "0"),
    )
    .unwrap();
    conn.execute(
        "UPDATE credentials SET scope_json = '{\"kind\":\"only\",\"models\":[]}' WHERE id = 'cid'",
        [],
    )
    .unwrap();
    let fence = QuotaFence {
        credential_id: "cid".into(),
        legacy_account_id: "legacy".into(),
        credential_version: 3,
        provider_id: "opencode".into(),
        binding_id: "bind".into(),
        key_cipher: "cipher-v1".into(),
        scope: ModelScope::Only { models: Vec::new() },
        endpoint_ids: Vec::new(),
        origins: Vec::new(),
        pool_ids: Vec::new(),
    };
    let outcome = apply_accepted(
        &mut conn,
        &OfficialPlanCommit {
            fence,
            observation_id: "obs-empty-scope".into(),
            fetched_at: at(15),
            body: body("ok", at(40), "ok", at(86_400), "0"),
            provider_id: OPENCODE_PROVIDER_ID.into(),
        },
    )
    .unwrap();
    assert_eq!(outcome, QuotaApply::Stale);
    assert_eq!(known(&conn, Window::FiveHours), Some(at(90)));
}
