//! Behavioral tests split from the root db test module.

use super::tests::*;
use super::*;
use std::fs;

#[test]
pub(super) fn forward_log_route_defaults_empty_and_round_trips_explicit_labels() {
    let dir = temp_data_dir("v24-route-column");
    let db = Database::open(dir.clone()).unwrap();

    // Omitting route on the current schema keeps its empty default.
    db.conn
        .execute(
            "INSERT INTO forward_logs
                 (timestamp, model, account_id, account_name, status, cost_state)
                 VALUES ('2026-01-01T00:00:00Z', 'glm-5.3', 'a1', 'a1', 'success',
                         'legacy_estimate')",
            [],
        )
        .unwrap();

    let mut modern = forward_log("a1", "success", 0.5);
    modern.route = "proxy".to_string();
    modern.model = "gpt-5.6-luna".to_string();
    db.log_forward(&modern).unwrap();

    let logs = db.list_forward_logs(10).unwrap();
    assert_eq!(logs.len(), 2);
    let historical = logs.iter().find(|log| log.model == "glm-5.3").unwrap();
    assert_eq!(historical.route, "");
    let labeled = logs.iter().find(|log| log.model == "gpt-5.6-luna").unwrap();
    assert_eq!(labeled.route, "proxy");

    // The paginated query surface exposes the same column.
    let page = db
        .query_forward_logs(ForwardLogQueryOptions {
            limit: 10,
            offset: 0,
            status: None,
            account_id: None,
            provider_id: None,
            route_account_id: None,
            credential_account_id: None,
            model: None,
            key_id: None,
            request_id: None,
            start_time: None,
            end_time: None,
            sort_by: None,
            sort_order: None,
        })
        .unwrap();
    assert_eq!(page.items.len(), 2);
    assert!(page.items.iter().all(|log| {
        (log.model == "glm-5.3" && log.route.is_empty())
            || (log.model == "gpt-5.6-luna" && log.route == "proxy")
    }));

    drop(db);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
pub(super) fn diagnostic_retention_removes_only_old_json() {
    let dir = temp_data_dir("diagnostic-retention");
    let db = Database::open(dir.clone()).expect("db should open");
    db.conn
        .execute_batch(
            "INSERT INTO forward_logs
                    (timestamp, model, account_id, account_name, status, error_message,
                     request_id, attempt, error_source, error_stage, duration_ms, diagnostic_json)
                 VALUES
                    (datetime('now', '-31 days'), 'old', 'a', 'A', 'client_error', 'keep me',
                     'ocg-old', 1, 'upstream', 'upstream_http', 12, '{\"old\":true}'),
                    (datetime('now', '-29 days'), 'new', 'a', 'A', 'client_error', 'keep new',
                     'ocg-new', 1, 'upstream', 'upstream_http', 13, '{\"new\":true}');
                 INSERT INTO gateway_logs
                    (level, category, message, created_at, request_id, error_source,
                     error_stage, duration_ms, diagnostic_json)
                 VALUES
                    ('warn', 'gateway', 'old gateway', datetime('now', '-31 days'),
                     'ocg-gateway-old', 'client', 'parse', 5, '{\"old\":true}'),
                    ('warn', 'gateway', 'new gateway', datetime('now', '-29 days'),
                     'ocg-gateway-new', 'client', 'parse', 6, '{\"new\":true}');",
        )
        .expect("diagnostic rows should insert");
    drop(db);

    let db = Database::open(dir.clone()).expect("db reopen should apply retention");
    let (old_detail, old_id, old_error, old_source): (Option<String>, String, String, String) = db
        .conn
        .query_row(
            "SELECT diagnostic_json, request_id, error_message, error_source
                 FROM forward_logs WHERE request_id='ocg-old'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .expect("old row should remain");
    assert!(old_detail.is_none());
    assert_eq!(old_id, "ocg-old");
    assert_eq!(old_error, "keep me");
    assert_eq!(old_source, "upstream");
    let new_detail: Option<String> = db
        .conn
        .query_row(
            "SELECT diagnostic_json FROM forward_logs WHERE request_id='ocg-new'",
            [],
            |row| row.get(0),
        )
        .expect("new detail should load");
    assert!(new_detail.is_some());
    let gateway_details: (Option<String>, Option<String>) = db
        .conn
        .query_row(
            "SELECT
                    (SELECT diagnostic_json FROM gateway_logs WHERE request_id='ocg-gateway-old'),
                    (SELECT diagnostic_json FROM gateway_logs WHERE request_id='ocg-gateway-new')",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .expect("gateway details should load");
    assert!(gateway_details.0.is_none());
    assert!(gateway_details.1.is_some());

    drop(db);
    fs::remove_dir_all(dir).expect("test data dir should be removed");
}

#[test]
pub(super) fn forward_log_time_filter_compares_rfc3339_offsets_by_instant() {
    let dir = temp_data_dir("forward-log-offset-filter");
    let db = Database::open(dir.clone()).expect("db should open");
    db.conn
        .execute(
            "INSERT INTO forward_logs
                 (timestamp, model, account_id, account_name, status, cost)
                 VALUES (?1, 'inside', 'a', 'a', 'success', 1)",
            ["2026-07-17T04:15:00Z"],
        )
        .expect("inside log should save");
    db.conn
        .execute(
            "INSERT INTO forward_logs
                 (timestamp, model, account_id, account_name, status, cost)
                 VALUES (?1, 'outside', 'a', 'a', 'success', 2)",
            ["2026-07-17T03:30:00Z"],
        )
        .expect("outside log should save");

    let page = db
        .query_forward_logs(ForwardLogQueryOptions {
            limit: 20,
            offset: 0,
            status: None,
            account_id: None,
            provider_id: None,
            route_account_id: None,
            credential_account_id: None,
            model: None,
            key_id: None,
            request_id: None,
            start_time: Some("2026-07-17T12:00:00+08:00"),
            end_time: Some("2026-07-17T12:30:00+08:00"),
            sort_by: Some("cost"),
            sort_order: Some("asc"),
        })
        .expect("offset filter should query");
    assert_eq!(page.summary.total_requests, 1);
    assert_eq!(page.items.len(), 1);
    assert_eq!(page.items[0].model, "inside");

    drop(db);
    fs::remove_dir_all(dir).expect("test data dir should be removed");
}

#[test]
pub(super) fn forward_logs_can_sort_by_attempt() {
    let dir = temp_data_dir("forward-log-attempt-sort");
    let db = Database::open(dir.clone()).expect("db should open");
    for attempt in [2, 1] {
        db.conn
            .execute(
                "INSERT INTO forward_logs
                     (timestamp, model, account_id, account_name, status, cost, attempt)
                     VALUES ('2026-07-23T00:00:00Z', ?1, 'a', 'a', 'client_error', 0, ?2)",
                params![format!("attempt-{attempt}"), attempt],
            )
            .expect("forward log should save");
    }

    let page = db
        .query_forward_logs(ForwardLogQueryOptions {
            limit: 20,
            offset: 0,
            status: None,
            account_id: None,
            provider_id: None,
            route_account_id: None,
            credential_account_id: None,
            model: None,
            key_id: None,
            request_id: None,
            start_time: None,
            end_time: None,
            sort_by: Some("attempt"),
            sort_order: Some("asc"),
        })
        .expect("attempt sort should query");
    assert_eq!(
        page.items
            .iter()
            .filter_map(|log| log.attempt)
            .collect::<Vec<_>>(),
        [1, 2]
    );

    drop(db);
    fs::remove_dir_all(dir).expect("test data dir should be removed");
}

#[test]
pub(super) fn forward_logs_success_filter_includes_unpriced_rows() {
    let dir = temp_data_dir("forward-success-includes-unpriced");
    let db = Database::open(dir.clone()).unwrap();
    db.log_forward(&forward_log("acct", "success", 1.0))
        .unwrap();
    db.log_forward(&forward_log("acct", "success_unpriced", 2.0))
        .unwrap();
    db.log_forward(&forward_log("acct", "error", 4.0)).unwrap();

    let query = |status: Option<&str>| {
        db.query_forward_logs(ForwardLogQueryOptions {
            status,
            ..empty_forward_query()
        })
        .unwrap()
    };

    let success = query(Some("success"));
    assert_eq!(success.summary.total_requests, 2);
    let mut statuses = success
        .items
        .iter()
        .map(|log| log.status.as_str())
        .collect::<Vec<_>>();
    statuses.sort_unstable();
    assert_eq!(statuses, ["success", "success_unpriced"]);

    let unpriced = query(Some("success_unpriced"));
    assert_eq!(unpriced.summary.total_requests, 1);
    assert_eq!(unpriced.items[0].status, "success_unpriced");

    let errors = query(Some("error"));
    assert_eq!(errors.summary.total_requests, 1);
    assert_eq!(errors.items[0].status, "error");

    drop(db);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
pub(super) fn forward_logs_filter_by_key_and_unattributed_sentinel() {
    let dir = temp_data_dir("forward-key-filter");
    let db = Database::open(dir.clone()).unwrap();
    db.log_forward(&attributed_log("acct", Some("key-a"), 1.0))
        .unwrap();
    db.log_forward(&attributed_log("acct", Some("key-b"), 2.0))
        .unwrap();
    db.log_forward(&attributed_log("acct", None, 4.0)).unwrap();

    let query = |key_id: Option<&str>| {
        db.query_forward_logs(ForwardLogQueryOptions {
            limit: 50,
            offset: 0,
            status: None,
            account_id: None,
            provider_id: None,
            route_account_id: None,
            credential_account_id: None,
            model: None,
            key_id,
            request_id: None,
            start_time: None,
            end_time: None,
            sort_by: Some("cost"),
            sort_order: Some("asc"),
        })
        .unwrap()
    };

    let all = query(None);
    assert_eq!(all.summary.total_requests, 3);
    assert_eq!(all.items.len(), 3);

    let key_a = query(Some("key-a"));
    assert_eq!(key_a.summary.total_requests, 1);
    assert_eq!(key_a.summary.cost, 1.0);
    assert_eq!(key_a.items[0].client_key_id.as_deref(), Some("key-a"));
    assert_eq!(key_a.items[0].client_key_name.as_deref(), Some("Key-key-a"));

    let unattributed = query(Some(UNATTRIBUTED_KEY_FILTER));
    assert_eq!(unattributed.summary.total_requests, 1);
    assert_eq!(unattributed.summary.cost, 4.0);
    assert!(unattributed.items[0].client_key_id.is_none());

    let keys = db.list_forward_log_keys().unwrap();
    assert_eq!(keys.len(), 2);
    assert!(
        keys.iter()
            .any(|key| key.id == "key-a" && key.name == "Key-key-a")
    );
    assert!(keys.iter().any(|key| key.id == "key-b"));

    drop(db);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
pub(super) fn forward_logs_filter_by_provider_attribution_before_pagination() {
    let dir = temp_data_dir("forward-provider-filter");
    let db = Database::open(dir.clone()).unwrap();
    let insert =
        |model: &str, provider_id: &str, route_account_id: &str, credential_account_id: &str| {
            let mut log = forward_log(credential_account_id, "success", 1.0);
            log.model = model.into();
            log.provider_id = Some(provider_id.into());
            log.route_account_id = Some(route_account_id.into());
            log.credential_account_id = Some(credential_account_id.into());
            db.log_forward(&log).unwrap();
        };

    insert("go-a", "opencode", "go-a", "go-a");
    insert("go-b", "opencode", "go-b", "go-b");
    // A Zen route may deliberately debit an OpenCode credential account.
    insert("zen", "opencode-zen-free", "zen-free", "go-a");
    // These newer rows would hide OpenCode Go rows if filtering happened
    // after LIMIT/OFFSET.
    insert("goat-a", "command-code", "goat-a", "goat-a");
    insert("goat-b", "command-code", "goat-b", "goat-b");

    let query = |limit: i64,
                 offset: i64,
                 provider_id: Option<&str>,
                 route_account_id: Option<&str>,
                 credential_account_id: Option<&str>| {
        db.query_forward_logs(ForwardLogQueryOptions {
            limit,
            offset,
            status: None,
            account_id: None,
            provider_id,
            route_account_id,
            credential_account_id,
            model: None,
            key_id: None,
            request_id: None,
            start_time: None,
            end_time: None,
            sort_by: None,
            sort_order: None,
        })
        .unwrap()
    };

    let first_go = query(1, 0, Some("opencode"), None, None);
    assert_eq!(first_go.summary.total_requests, 2);
    assert_eq!(first_go.items[0].model, "go-b");
    let second_go = query(1, 1, Some("opencode"), None, None);
    assert_eq!(second_go.summary.total_requests, 2);
    assert_eq!(second_go.items[0].model, "go-a");

    let routed_zen = query(10, 0, Some("opencode-zen-free"), Some("zen-free"), None);
    assert_eq!(routed_zen.summary.total_requests, 1);
    assert_eq!(routed_zen.items[0].model, "zen");
    assert_eq!(
        routed_zen.items[0].credential_account_id.as_deref(),
        Some("go-a")
    );

    let credential_go_a = query(10, 0, None, None, Some("go-a"));
    assert_eq!(credential_go_a.summary.total_requests, 2);
    assert_eq!(
        credential_go_a
            .items
            .iter()
            .map(|log| log.model.as_str())
            .collect::<Vec<_>>(),
        ["zen", "go-a"]
    );

    let goat = query(10, 0, Some("command-code"), None, None);
    assert_eq!(goat.summary.total_requests, 2);
    assert_eq!(
        goat.items
            .iter()
            .map(|log| log.route_account_id.as_deref())
            .collect::<Vec<_>>(),
        [Some("goat-b"), Some("goat-a")]
    );

    drop(db);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
pub(super) fn forward_logs_model_filter_matches_each_identity_and_legacy_fallback() {
    let dir = temp_data_dir("forward-model-identity-filter");
    let db = Database::open(dir.clone()).unwrap();

    let mut legacy = forward_log("acct", "success", 1.0);
    legacy.model = "needle".into();
    legacy.prompt_tokens = 1;
    let legacy_id = db.log_forward(&legacy).unwrap();
    clear_v23_identity(&db, legacy_id);

    let mut requested_only = forward_log("acct", "success", 2.0);
    requested_only.model = "legacy-req".into();
    requested_only.prompt_tokens = 2;
    let requested_id = insert_identity_log(
        &db,
        requested_only,
        Some("needle"),
        Some("alias-req"),
        Some("up-req"),
    );

    let mut alias_only = forward_log("acct", "success", 3.0);
    alias_only.model = "legacy-alias".into();
    alias_only.prompt_tokens = 3;
    let alias_id = insert_identity_log(
        &db,
        alias_only,
        Some("req-alias"),
        Some("needle"),
        Some("up-alias"),
    );

    let mut upstream_only = forward_log("acct", "success", 4.0);
    upstream_only.model = "legacy-up".into();
    upstream_only.prompt_tokens = 4;
    let upstream_id = insert_identity_log(
        &db,
        upstream_only,
        Some("req-up"),
        Some("alias-up"),
        Some("needle"),
    );

    let mut empty_v23 = forward_log("acct", "success", 5.0);
    empty_v23.model = "kept-empty".into();
    empty_v23.prompt_tokens = 5;
    insert_identity_log(&db, empty_v23, Some(""), Some(""), Some(""));

    let mut other = forward_log("acct", "success", 100.0);
    other.model = "other-legacy".into();
    other.prompt_tokens = 100;
    insert_identity_log(
        &db,
        other,
        Some("other-req"),
        Some("other-alias"),
        Some("other-up"),
    );

    let mut overlap = forward_log("acct", "success", 6.0);
    overlap.model = "needle".into();
    overlap.prompt_tokens = 6;
    let overlap_id =
        insert_identity_log(&db, overlap, Some("needle"), Some("needle"), Some("needle"));

    let page = db
        .query_forward_logs(ForwardLogQueryOptions {
            model: Some("needle"),
            sort_by: Some("cost"),
            sort_order: Some("asc"),
            ..empty_forward_query()
        })
        .unwrap();
    let ids = page.items.iter().map(|log| log.id).collect::<Vec<_>>();
    assert_eq!(
        ids,
        [legacy_id, requested_id, alias_id, upstream_id, overlap_id]
    );
    assert_eq!(page.summary.total_requests, 5);
    assert_eq!(page.summary.prompt_tokens, 16);
    assert!((page.summary.cost - 16.0).abs() < f64::EPSILON);

    let requested = db
        .query_forward_logs(ForwardLogQueryOptions {
            model: Some("legacy-req"),
            ..empty_forward_query()
        })
        .unwrap();
    assert_eq!(
        requested.items.iter().map(|log| log.id).collect::<Vec<_>>(),
        [requested_id]
    );

    let alias = db
        .query_forward_logs(ForwardLogQueryOptions {
            model: Some("alias-req"),
            ..empty_forward_query()
        })
        .unwrap();
    assert_eq!(
        alias.items.iter().map(|log| log.id).collect::<Vec<_>>(),
        [requested_id]
    );

    let empty_identity = db
        .query_forward_logs(ForwardLogQueryOptions {
            model: Some("kept-empty"),
            ..empty_forward_query()
        })
        .unwrap();
    assert_eq!(empty_identity.summary.total_requests, 1);
    assert_eq!(empty_identity.items[0].model, "kept-empty");

    let missing = db
        .query_forward_logs(ForwardLogQueryOptions {
            model: Some("missing"),
            ..empty_forward_query()
        })
        .unwrap();
    assert!(missing.items.is_empty());
    assert_eq!(missing.summary.total_requests, 0);

    let substring = db
        .query_forward_logs(ForwardLogQueryOptions {
            model: Some("need"),
            ..empty_forward_query()
        })
        .unwrap();
    assert!(substring.items.is_empty());
    assert_eq!(substring.summary.total_requests, 0);

    drop(db);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
pub(super) fn forward_logs_model_filter_ands_other_filters_before_pagination() {
    let dir = temp_data_dir("forward-model-combo-filter");
    let db = Database::open(dir.clone()).unwrap();
    let inside = DateTime::parse_from_rfc3339("2026-07-17T04:15:00Z")
        .unwrap()
        .with_timezone(&Utc);
    let outside = DateTime::parse_from_rfc3339("2026-07-17T03:30:00Z")
        .unwrap()
        .with_timezone(&Utc);

    let matching = |suffix: &str, cost: f64| {
        let mut log = forward_log("acct", "success", cost);
        log.model = format!("legacy-{suffix}");
        log.provider_id = Some("opencode".into());
        log.client_key_id = Some("key-a".into());
        log.client_key_name = Some("Key-a".into());
        log.timestamp = inside;
        log.prompt_tokens = cost as i64;
        insert_identity_log(
            &db,
            log,
            Some("req-other"),
            Some("needle"),
            Some("up-other"),
        )
    };
    let first = matching("a", 1.0);
    let second = matching("b", 2.0);
    let third = matching("c", 3.0);

    let mut wrong_provider = forward_log("acct", "success", 9.0);
    wrong_provider.model = "legacy-provider".into();
    wrong_provider.provider_id = Some("goat".into());
    wrong_provider.client_key_id = Some("key-a".into());
    wrong_provider.timestamp = inside;
    insert_identity_log(
        &db,
        wrong_provider,
        Some("needle"),
        Some("alias-other"),
        Some("up-other"),
    );

    let mut wrong_key = forward_log("acct", "success", 8.0);
    wrong_key.model = "legacy-key".into();
    wrong_key.provider_id = Some("opencode".into());
    wrong_key.client_key_id = Some("key-b".into());
    wrong_key.timestamp = inside;
    insert_identity_log(&db, wrong_key, None, None, Some("needle"));

    let mut wrong_status = forward_log("acct", "error", 7.0);
    wrong_status.model = "needle".into();
    wrong_status.provider_id = Some("opencode".into());
    wrong_status.client_key_id = Some("key-a".into());
    wrong_status.timestamp = inside;
    let wrong_status_id = db.log_forward(&wrong_status).unwrap();
    clear_v23_identity(&db, wrong_status_id);

    let mut wrong_time = forward_log("acct", "success", 6.0);
    wrong_time.model = "legacy-time".into();
    wrong_time.provider_id = Some("opencode".into());
    wrong_time.client_key_id = Some("key-a".into());
    wrong_time.timestamp = outside;
    insert_identity_log(
        &db,
        wrong_time,
        Some("needle"),
        Some("needle"),
        Some("needle"),
    );

    for index in 0..5 {
        let mut decoy = forward_log("busy", "success", 100.0);
        decoy.model = format!("decoy-{index}");
        decoy.provider_id = Some("opencode".into());
        decoy.client_key_id = Some("key-a".into());
        decoy.timestamp = inside;
        db.log_forward(&decoy).unwrap();
    }

    let filtered = |limit, offset| ForwardLogQueryOptions {
        limit,
        offset,
        status: Some("success"),
        provider_id: Some("opencode"),
        model: Some("needle"),
        key_id: Some("key-a"),
        start_time: Some("2026-07-17T12:00:00+08:00"),
        end_time: Some("2026-07-17T12:30:00+08:00"),
        sort_by: Some("cost"),
        sort_order: Some("asc"),
        ..empty_forward_query()
    };
    let first_page = db.query_forward_logs(filtered(1, 0)).unwrap();
    assert_eq!(first_page.items.len(), 1);
    assert_eq!(first_page.items[0].id, first);
    assert_eq!(first_page.summary.total_requests, 3);
    assert_eq!(first_page.summary.prompt_tokens, 6);
    assert!((first_page.summary.cost - 6.0).abs() < f64::EPSILON);

    let second_page = db.query_forward_logs(filtered(1, 1)).unwrap();
    assert_eq!(second_page.items.len(), 1);
    assert_eq!(second_page.items[0].id, second);
    assert_eq!(second_page.summary.total_requests, 3);

    let rest = db.query_forward_logs(filtered(50, 2)).unwrap();
    assert_eq!(
        rest.items.iter().map(|log| log.id).collect::<Vec<_>>(),
        [third]
    );

    drop(db);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
pub(super) fn backfill_attributes_null_rows_in_chunks_with_resume_and_completion() {
    let dir = temp_data_dir("backfill-chunks");
    let db = Database::open(dir.clone()).unwrap();
    for index in 0..7 {
        let mut log = forward_log("acct", "success", index as f64);
        log.client_key_id = (index % 2 == 0).then(|| "already-set".to_string());
        db.log_forward(&log).unwrap();
    }

    // Chunk size 3 covers rowids 1..=7 in three steps; already-attributed
    // rows must never be overwritten by the range update.
    assert!(
        db.backfill_forward_logs_client_key_step("primary", "Primary", 3)
            .unwrap()
    );
    assert!(
        db.backfill_forward_logs_client_key_step("primary", "Primary", 3)
            .unwrap()
    );
    // The final chunk exactly reaches max rowid and records completion
    // in the same call; a further step is a no-op.
    assert!(
        !db.backfill_forward_logs_client_key_step("primary", "Primary", 3)
            .unwrap()
    );
    assert!(
        !db.backfill_forward_logs_client_key_step("primary", "Primary", 3)
            .unwrap()
    );
    assert_eq!(
        db.forward_log_backfill_marker().unwrap().as_deref(),
        Some(BACKFILL_DONE)
    );

    let rows = db.list_forward_logs(100).unwrap();
    assert_eq!(rows.len(), 7);
    for (index, row) in rows.iter().rev().enumerate() {
        if index % 2 == 0 {
            assert_eq!(row.client_key_id.as_deref(), Some("already-set"));
        } else {
            assert_eq!(row.client_key_id.as_deref(), Some("primary"));
            assert_eq!(row.client_key_name.as_deref(), Some("Primary"));
        }
    }

    // New NULL rows written by an older binary (a downgrade window)
    // restart the scan instead of staying "unattributed" forever.
    for cost in [9.0, 11.0] {
        db.log_forward(&forward_log("acct", "success", cost))
            .unwrap();
    }
    assert!(
        db.backfill_forward_logs_client_key_step("primary", "Primary", 3)
            .unwrap()
    );
    while db
        .backfill_forward_logs_client_key_step("primary", "Primary", 3)
        .unwrap()
    {}
    assert_eq!(
        db.forward_log_backfill_marker().unwrap().as_deref(),
        Some(BACKFILL_DONE)
    );
    let late_rows: Vec<_> = db
        .list_forward_logs(100)
        .unwrap()
        .into_iter()
        .filter(|row| row.client_key_name.as_deref() == Some("Primary"))
        .collect();
    assert_eq!(late_rows.len(), 5);
    for cost in [9.0, 11.0] {
        assert!(late_rows.iter().any(|row| row.cost == Some(cost)));
    }

    // A late row already carrying an attribution must not restart the scan.
    let mut attributed = forward_log("acct", "success", 13.0);
    attributed.client_key_id = Some("primary".into());
    attributed.client_key_name = Some("Primary".into());
    db.log_forward(&attributed).unwrap();
    assert!(
        !db.backfill_forward_logs_client_key_step("primary", "Primary", 50)
            .unwrap()
    );

    drop(db);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
pub(super) fn backfill_resumes_from_persisted_watermark_after_interruption() {
    let dir = temp_data_dir("backfill-resume");
    let db = Database::open(dir.clone()).unwrap();
    for index in 0..5 {
        db.log_forward(&forward_log("acct", "success", index as f64))
            .unwrap();
    }

    // Simulate a crash after the first chunk: the watermark persists but
    // the remaining rows are still NULL.
    assert!(
        db.backfill_forward_logs_client_key_step("primary", "Primary", 2)
            .unwrap()
    );
    assert_eq!(
        db.forward_log_backfill_marker().unwrap().as_deref(),
        Some("2")
    );
    let partial = db.list_forward_logs(100).unwrap();
    assert_eq!(
        partial
            .iter()
            .filter(|row| row.client_key_id.is_some())
            .count(),
        2
    );

    drop(db);
    let db = Database::open(dir.clone()).unwrap();
    assert_eq!(
        db.forward_log_backfill_marker().unwrap().as_deref(),
        Some("2")
    );

    // A restarted run continues from the watermark instead of
    // rescanning; the last chunk completes the table and records done.
    assert!(
        db.backfill_forward_logs_client_key_step("primary", "Primary", 2)
            .unwrap()
    );
    assert!(
        !db.backfill_forward_logs_client_key_step("primary", "Primary", 2)
            .unwrap()
    );
    assert_eq!(
        db.forward_log_backfill_marker().unwrap().as_deref(),
        Some(BACKFILL_DONE)
    );
    let rows = db.list_forward_logs(100).unwrap();
    assert!(
        rows.iter()
            .all(|row| row.client_key_id.as_deref() == Some("primary"))
    );
    // No row was attributed twice: costs and row counts are unchanged.
    assert_eq!(
        rows.iter().map(|row| row.cost.unwrap_or(0.0)).sum::<f64>() as i64,
        10
    );

    drop(db);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
pub(super) fn backfill_completes_inline_for_empty_tables() {
    let dir = temp_data_dir("backfill-empty");
    let db = Database::open(dir.clone()).unwrap();
    assert!(
        !db.backfill_forward_logs_client_key_step("primary", "Primary", 50_000)
            .unwrap()
    );
    assert_eq!(
        db.forward_log_backfill_marker().unwrap().as_deref(),
        Some(BACKFILL_DONE)
    );

    drop(db);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
pub(super) fn forward_log_keys_resolve_the_latest_name_per_id() {
    let dir = temp_data_dir("log-keys-latest-name");
    let db = Database::open(dir.clone()).unwrap();
    let base = ForwardLog {
        model: "m".into(),
        client_key_id: Some("sub-1".into()),
        client_key_name: Some("Laptop".into()),
        cost: None,
        raw_cost_usd: None,
        quota_debit: None,
        effective_paid_cost_usd: None,
        cost_state: "not_applicable".into(),
        ..forward_log("a", "success", 0.0)
    };
    // A lexicographically "larger" historical name must not win: it was
    // written first, the current name last.
    let mut zzz = base.clone();
    zzz.client_key_name = Some("zzz-old".into());
    db.log_forward(&zzz).unwrap();
    db.log_forward(&base).unwrap();
    let mut renamed = base.clone();
    renamed.client_key_name = Some("Deck".into());
    db.log_forward(&renamed).unwrap();

    let keys = db.list_forward_log_keys().unwrap();
    assert_eq!(keys.len(), 1, "one entry per distinct key id");
    assert_eq!(keys[0].id, "sub-1");
    assert_eq!(keys[0].name, "Deck", "the latest snapshot wins");

    // NULL-name rows fall back to the id label.
    let mut unnamed = base.clone();
    unnamed.client_key_id = Some("ghost".into());
    unnamed.client_key_name = None;
    db.log_forward(&unnamed).unwrap();
    let keys = db.list_forward_log_keys().unwrap();
    assert_eq!(keys.len(), 2);
    assert_eq!(
        keys.iter().find(|key| key.id == "ghost").unwrap().name,
        "ghost"
    );

    // The list stays purely log-driven: an id with no rows (e.g. the
    // primary key before its first attributed request) never appears.
    assert!(
        !keys
            .iter()
            .any(|key| key.id == "00000000-0000-0000-0000-000000000001")
    );

    // A primary-attributed row with a NULL name resolves to the fixed
    // display name, never the raw id constant.
    let mut unnamed_primary = base.clone();
    unnamed_primary.client_key_id = Some("00000000-0000-0000-0000-000000000001".into());
    unnamed_primary.client_key_name = None;
    db.log_forward(&unnamed_primary).unwrap();
    let keys = db.list_forward_log_keys().unwrap();
    let primary = keys
        .iter()
        .find(|key| key.id == "00000000-0000-0000-0000-000000000001")
        .unwrap();
    assert_eq!(primary.name, "Primary");

    drop(db);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
pub(super) fn forward_logs_dual_write_native_usd_attribution() {
    let dir = temp_data_dir("v23-native-logs");
    let db = Database::open(dir.clone()).unwrap();
    db.create_account(&account("priced")).unwrap();
    let mut log = forward_log("priced", "success", 1.25);
    log.raw_cost_usd = Some(1.25);
    log.cost_state = "priced".into();
    let id = db.log_forward(&log).unwrap();
    let attribution = db.forward_log_native_attribution(id).unwrap().unwrap();
    assert_eq!(attribution.native_cost_value, Some(1.25));
    assert_eq!(attribution.native_cost_unit.as_deref(), Some("usd"));
    assert_eq!(attribution.native_cost_currency.as_deref(), Some("USD"));
    assert_eq!(attribution.upstream_model.as_deref(), Some("test"));

    db.set_forward_log_native_attribution(
        id,
        &ForwardLogNativeAttribution {
            requested_model: Some("deepseek-v4-flash".into()),
            resolved_alias: Some("deepseek-v4-flash".into()),
            upstream_model: Some("deepseek/deepseek-v4-flash".into()),
            native_cost_value: Some(12.0),
            native_cost_unit: Some("credits".into()),
            native_cost_currency: None,
        },
    )
    .unwrap();
    let updated = db.forward_log_native_attribution(id).unwrap().unwrap();
    assert_eq!(
        updated.upstream_model.as_deref(),
        Some("deepseek/deepseek-v4-flash")
    );
    assert_eq!(updated.native_cost_unit.as_deref(), Some("credits"));

    drop(db);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
pub(super) fn update_forward_log_finalizes_native_usd_with_cost_fields() {
    let dir = temp_data_dir("v23-native-finalize");
    let db = Database::open(dir.clone()).unwrap();
    db.create_account(&account("stream")).unwrap();

    let mut streaming = forward_log("stream", "streaming", 0.0);
    streaming.cost = None;
    streaming.raw_cost_usd = None;
    streaming.cost_state = "not_applicable".into();
    streaming.provider_id = Some(OPENCODE_PROVIDER_ID.to_string());
    let streaming_id = db.log_forward(&streaming).unwrap();
    let preliminary = db
        .forward_log_native_attribution(streaming_id)
        .unwrap()
        .unwrap();
    assert_eq!(preliminary.native_cost_value, None);
    assert_eq!(preliminary.native_cost_unit, None);

    db.update_forward_log(
        streaming_id,
        "success",
        Some(200),
        ForwardMetrics {
            cost: 1.25,
            raw_cost_usd: Some(1.25),
            pricing_provider_id: Some(OPENCODE_PROVIDER_ID.to_string()),
            cost_state: "priced",
            ..ForwardMetrics::default()
        },
        None,
        None,
    )
    .unwrap();
    let finalized = db
        .forward_log_native_attribution(streaming_id)
        .unwrap()
        .unwrap();
    assert_eq!(finalized.native_cost_value, Some(1.25));
    assert_eq!(finalized.native_cost_unit.as_deref(), Some("usd"));
    assert_eq!(finalized.native_cost_currency.as_deref(), Some("USD"));
    let stored_cost: f64 = db
        .conn
        .query_row(
            "SELECT cost FROM forward_logs WHERE id = ?1",
            [streaming_id],
            |row| row.get(0),
        )
        .unwrap();
    assert!((stored_cost - 1.25).abs() < 1e-9);

    let zero_id = db
        .log_forward(&forward_log("stream", "streaming", 0.0))
        .unwrap();
    assert_eq!(
        db.forward_log_native_attribution(zero_id)
            .unwrap()
            .unwrap()
            .native_cost_value,
        Some(0.0)
    );
    db.update_forward_log(
        zero_id,
        "success",
        None,
        ForwardMetrics {
            cost: 2.5,
            raw_cost_usd: Some(2.5),
            cost_state: "priced",
            ..ForwardMetrics::default()
        },
        None,
        None,
    )
    .unwrap();
    assert_eq!(
        db.forward_log_native_attribution(zero_id)
            .unwrap()
            .unwrap()
            .native_cost_value,
        Some(2.5)
    );

    let mut zen = forward_log("stream", "streaming", 0.0);
    zen.cost = None;
    zen.raw_cost_usd = None;
    zen.cost_state = "not_applicable".into();
    zen.provider_id = Some(OPENCODE_ZEN_FREE_PROVIDER_ID.to_string());
    let zen_id = db.log_forward(&zen).unwrap();
    db.update_forward_log(
        zen_id,
        "success",
        Some(200),
        ForwardMetrics {
            cost: 1.0,
            raw_cost_usd: Some(1.0),
            cost_state: "priced",
            ..ForwardMetrics::default()
        },
        None,
        None,
    )
    .unwrap();
    let zen_native = db.forward_log_native_attribution(zen_id).unwrap().unwrap();
    assert_eq!(zen_native.native_cost_value, Some(0.0));
    assert_eq!(zen_native.native_cost_unit.as_deref(), Some("usd"));
    let zen_cost: (f64, String) = db
        .conn
        .query_row(
            "SELECT cost, cost_state FROM forward_logs WHERE id = ?1",
            [zen_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(zen_cost.0, 0.0);
    assert_eq!(zen_cost.1, "free");

    drop(db);
    fs::remove_dir_all(dir).unwrap();
}
