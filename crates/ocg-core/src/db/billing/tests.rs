use super::*;
use crate::billing_types::{CreditBucketKind, CreditRate};

const URL: &str = "https://example.test/v1/chat/completions";

fn at() -> DateTime<Utc> {
    "2026-01-15T00:00:00Z".parse().unwrap()
}

fn fixture() -> Connection {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch("CREATE TABLE credentials(id TEXT PRIMARY KEY, destination_id TEXT, legacy_account_id TEXT, credential_purpose TEXT, credit_meter_json TEXT, key_cipher TEXT, quota_pool_id TEXT);
        CREATE TABLE destinations(id TEXT PRIMARY KEY, adapter TEXT, legacy_kind TEXT, base_url TEXT);
        CREATE TABLE forward_logs(id INTEGER PRIMARY KEY, account_id TEXT, credit_receipt_json TEXT, native_cost_value REAL, native_cost_unit TEXT, native_cost_currency TEXT);
        INSERT INTO destinations VALUES('supplier-a','http','custom_account','https://example.test/v1/chat/completions');
        INSERT INTO credentials VALUES('credential-a','supplier-a','a','inference',NULL,'key-a','same-old-pool');
        INSERT INTO credentials VALUES('credential-b','supplier-a','b','inference',NULL,'key-b','same-old-pool');").unwrap();
    for account in ["a", "b"] {
        configure_on(&conn, account, config(), Some(vec![bucket()]), at()).unwrap();
    }
    conn
}

fn config() -> CreditConfiguration {
    CreditConfiguration {
        name: "Personal".into(),
        currency: "CNY".into(),
        credits_per_currency: 1.0,
        rates: vec![CreditRate {
            model: "model".into(),
            input_per_million: 10.0,
            output_per_million: 20.0,
            cache_read_per_million: Some(2.0),
            cache_write_per_million: Some(10.0),
        }],
        monthly: None,
        source_url: None,
    }
}

fn bucket() -> CreditBucket {
    CreditBucket {
        id: "initial".into(),
        kind: CreditBucketKind::Manual,
        label: "Current".into(),
        granted: 100.0,
        remaining: 75.0,
        starts_at: at(),
        expires_at: None,
    }
}

fn capture(conn: &Connection, id: i64, model: &str) -> CreditAttempt {
    let attempt = capture_on(conn, "a", URL, model, at()).unwrap().unwrap();
    conn.execute(
        "INSERT INTO forward_logs(id,account_id) VALUES(?1,'a')",
        [id],
    )
    .unwrap();
    attach_attempt_on(conn, id, &attempt).unwrap();
    attempt
}

fn settle(conn: &Connection, id: i64, attempt: &CreditAttempt, status: &str) {
    let tx = conn.unchecked_transaction().unwrap();
    settle_on(
        &tx,
        id,
        attempt,
        BillingTokens::new(1_000_000, 0, 0, 0),
        status,
        at(),
    )
    .unwrap();
    tx.commit().unwrap();
}

fn remaining(conn: &Connection, account: &str) -> f64 {
    read_view_on(conn, account, at())
        .unwrap()
        .unwrap()
        .remaining
}

#[test]
fn credits_freeze_exact_model_rate_and_never_share_accounts() {
    let conn = fixture();
    let attempt = capture(&conn, 1, "model");
    let mut changed = config();
    changed.rates[0].input_per_million = 80.0;
    configure_on(&conn, "a", changed, None, at()).unwrap();
    settle(&conn, 1, &attempt, "success_unpriced");
    assert_eq!(remaining(&conn, "a"), 65.0);
    assert_eq!(remaining(&conn, "b"), 75.0);
    settle(&conn, 1, &attempt, "success_unpriced");
    assert_eq!(remaining(&conn, "a"), 65.0);
    let unknown = capture(&conn, 2, "MODEL");
    settle(&conn, 2, &unknown, "success_unpriced");
    assert_eq!(remaining(&conn, "a"), 65.0);
    assert_eq!(load_on(&conn, "a").unwrap().unwrap().unpriced_requests, 1);
    conn.execute("DELETE FROM forward_logs", []).unwrap();
    assert_eq!(remaining(&conn, "a"), 65.0);
}

#[test]
fn credit_calibration_waits_for_pending_and_rejects_foreign_receipts() {
    let conn = fixture();
    let attempt = capture(&conn, 1, "model");
    assert!(
        calibrate_on(
            &conn,
            "a",
            &[CreditBalanceCorrection {
                bucket_id: "initial".into(),
                remaining: 30.0
            }],
            at()
        )
        .is_err()
    );
    let view = read_view_on(&conn, "a", at()).unwrap().unwrap();
    assert_eq!(view.pending_requests, 1);
    assert_eq!(view.remaining, 75.0);
    let mut foreign = attempt.clone();
    foreign.account_id = "b".into();
    settle(&conn, 1, &foreign, "success");
    assert_eq!(remaining(&conn, "a"), 75.0);
    settle(&conn, 1, &attempt, "success");
    assert_eq!(remaining(&conn, "a"), 65.0);
    assert_eq!(
        read_view_on(&conn, "a", at())
            .unwrap()
            .unwrap()
            .pending_requests,
        0
    );
    calibrate_on(
        &conn,
        "a",
        &[CreditBalanceCorrection {
            bucket_id: "initial".into(),
            remaining: 30.0,
        }],
        at(),
    )
    .unwrap();
    settle(&conn, 1, &attempt, "success");
    assert_eq!(remaining(&conn, "a"), 30.0);
}

#[test]
fn credit_client_rejections_are_zero_cost_while_unknown_outcomes_remain_uncertain() {
    for (status, uncertain, amount) in [
        ("client_error", false, Some(0.0)),
        ("error", false, Some(0.0)),
        ("outcome_unknown", true, None),
    ] {
        let conn = fixture();
        let attempt = capture(&conn, 1, "model");
        // Repeated completion must neither debit nor count uncertainty twice.
        for _ in 0..2 {
            settle(&conn, 1, &attempt, status);
            let view = read_view_on(&conn, "a", at()).unwrap().unwrap();
            assert_eq!(view.remaining, 75.0, "{status}");
            assert_eq!(view.pending_requests, 0, "{status}");
            assert_eq!(view.unpriced_requests, u64::from(uncertain), "{status}");
            let (_, receipt) = receipt_on(&conn, 1).unwrap().unwrap();
            assert_eq!(receipt.phase, "settled", "{status}");
            assert_eq!(receipt.uncertain, uncertain, "{status}");
            assert_eq!(receipt.amount, amount, "{status}");
            let native_cost: Option<f64> = conn
                .query_row(
                    "SELECT native_cost_value FROM forward_logs WHERE id=1",
                    [],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(native_cost, amount, "{status}");
        }
    }
}

#[test]
fn credit_rejections_unknowns_and_startup_recovery_are_distinct() {
    let conn = fixture();
    let rejected = capture(&conn, 1, "model");
    settle(&conn, 1, &rejected, "error");
    let missing_usage = capture(&conn, 2, "model");
    settle(&conn, 2, &missing_usage, "success_no_usage");
    capture(&conn, 3, "model");
    let tx = conn.unchecked_transaction().unwrap();
    recover_pending_on(&tx, at()).unwrap();
    tx.commit().unwrap();
    recover_pending_on(&conn, at()).unwrap();
    assert_eq!(remaining(&conn, "a"), 75.0);
    assert_eq!(load_on(&conn, "a").unwrap().unwrap().unpriced_requests, 2);
    assert_eq!(
        read_view_on(&conn, "a", at())
            .unwrap()
            .unwrap()
            .pending_requests,
        0
    );
}

#[test]
fn credit_deleted_or_replaced_meter_never_receives_an_old_debit() {
    let conn = fixture();
    let attempt = capture(&conn, 1, "model");
    disable_on(&conn, "a", at()).unwrap();
    configure_on(&conn, "a", config(), Some(vec![bucket()]), at()).unwrap();
    settle(&conn, 1, &attempt, "success");
    assert_eq!(remaining(&conn, "a"), 75.0);
    let pruned = capture(&conn, 2, "model");
    conn.execute("DELETE FROM forward_logs WHERE id=2", [])
        .unwrap();
    settle(&conn, 2, &pruned, "success");
    assert_eq!(remaining(&conn, "a"), 75.0);
    conn.execute(
        "UPDATE credentials SET key_cipher='rotated' WHERE legacy_account_id='a'",
        [],
    )
    .unwrap();
    assert_eq!(remaining(&conn, "a"), 75.0);
    conn.execute(
        "UPDATE destinations SET base_url='https://different.test/v1'",
        [],
    )
    .unwrap();
    assert!(load_on(&conn, "a").unwrap().is_none());
    settle(&conn, 1, &attempt, "success");
}

#[test]
fn credit_receipt_failure_rolls_back_the_balance_in_caller_transaction() {
    let conn = fixture();
    let attempt = capture(&conn, 1, "model");
    conn.execute_batch("CREATE TRIGGER block_settlement BEFORE UPDATE ON forward_logs BEGIN SELECT RAISE(ABORT,'fixture write failure'); END;").unwrap();
    {
        let tx = conn.unchecked_transaction().unwrap();
        assert!(
            settle_on(
                &tx,
                1,
                &attempt,
                BillingTokens::new(1_000_000, 0, 0, 0),
                "success",
                at()
            )
            .is_err()
        );
    }
    assert_eq!(remaining(&conn, "a"), 75.0);
    assert!(!settlement_finished_on(&conn, 1).unwrap());
}

#[test]
fn credit_export_is_readonly_and_merge_preserves_target_baseline() {
    let conn = fixture();
    capture(&conn, 1, "model");
    let before = load_on(&conn, "a").unwrap().unwrap();
    let portable = export_on(&conn, "a", at()).unwrap().unwrap();
    assert_eq!(portable.unpriced_requests, 1);
    assert_eq!(load_on(&conn, "a").unwrap().unwrap(), before);
    calibrate_on(
        &conn,
        "b",
        &[CreditBalanceCorrection {
            bucket_id: "initial".into(),
            remaining: 5.0,
        }],
        at(),
    )
    .unwrap();
    import_on(&conn, "b", &portable, at()).unwrap();
    assert_eq!(remaining(&conn, "b"), 5.0);
    disable_on(&conn, "b", at()).unwrap();
    import_on(&conn, "b", &portable, at()).unwrap();
    let imported = load_on(&conn, "b").unwrap().unwrap();
    assert_ne!(imported.meter_id, before.meter_id);
    assert_ne!(imported.credential_id, before.credential_id);
    assert_eq!(imported.unpriced_requests, 1);
    assert_eq!(remaining(&conn, "b"), 75.0);
}

#[test]
fn missing_accounts_never_create_a_meter_or_receipt() {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch("CREATE TABLE credentials(id TEXT, destination_id TEXT, legacy_account_id TEXT, credential_purpose TEXT, credit_meter_json TEXT);
        CREATE TABLE destinations(id TEXT, adapter TEXT, legacy_kind TEXT, base_url TEXT);").unwrap();
    assert!(load_on(&conn, "missing").unwrap().is_none());
    assert!(
        read_view_on(&conn, "missing", Utc::now())
            .unwrap()
            .is_none()
    );
}
