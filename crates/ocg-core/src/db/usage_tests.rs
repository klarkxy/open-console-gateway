//! Behavioral tests split from the root db test module.

use super::tests::*;
use super::*;
use std::fs;

#[test]
pub(super) fn d02_shared_identity_pool_fans_out_cooldown_to_sibling_key() {
    use crate::models::{UpstreamChannel, local_today};
    use crate::provider::ConnectionVerificationStatus;
    use ocg_domain::credential::{identity_id_for_legacy_account, quota_pool_id_for_identity};

    let dir = temp_data_dir("d02-shared-pool");
    let db = open_with_host_cipher(dir.clone()).unwrap();
    let mut first = account("pool-a");
    first.name = "Pool A".into();
    first.key_cipher = fixture_account_key_cipher();
    db.create_account(&first).unwrap();
    let identity_id: String = db
        .conn
        .query_row(
            "SELECT identity_id FROM credentials WHERE legacy_account_id = 'pool-a'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        identity_id,
        identity_id_for_legacy_account("pool-a").as_str()
    );

    let mut second = account("pool-b");
    second.name = "Pool B".into();
    second.key_cipher = fixture_account_key_cipher();
    let created = db
        .create_account_for_identity(
            &identity_id,
            &second,
            &local_today(),
            ConnectionVerificationStatus::NotRequired,
            crate::db::identity::QuotaSharingJoin::Shared {
                source_credential_id: ocg_domain::credential::credential_id_for_legacy_account(
                    "pool-a",
                )
                .to_string(),
            },
            None,
        )
        .unwrap();
    assert_eq!(created.identity_id, identity_id);
    assert_ne!(created.account_id, "pool-a");

    let pool_id = quota_pool_id_for_identity(&identity_id);
    let members: Vec<String> = db
        .conn
        .prepare("SELECT account_id FROM quota_pool_members WHERE pool_id = ?1 ORDER BY account_id")
        .unwrap()
        .query_map([pool_id.as_str()], |row| row.get(0))
        .unwrap()
        .map(|row| row.unwrap())
        .collect();
    assert_eq!(members, vec!["pool-a".to_string(), "pool-b".to_string()]);
    let confidence: String = db
        .conn
        .query_row(
            "SELECT relation_confidence FROM quota_pools WHERE id = ?1",
            [pool_id.as_str()],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(confidence, "declared");

    let until = Utc::now() + chrono::Duration::hours(2);
    db.set_account_rate_limit(
        "pool-a",
        until,
        "429 exhausted",
        Some(UsageWindowKind::FiveHours),
    )
    .unwrap();
    let sibling = db.get_account("pool-b").unwrap().expect("sibling");
    assert_eq!(sibling.cooldown_5h_until, Some(until));
    assert!(sibling.is_cooling_for(UpstreamChannel::Go, Utc::now()));
    assert_eq!(sibling.last_error.as_deref(), Some("429 exhausted"));
    assert!(sibling.auth_error.is_none());

    db.set_account_auth_error("pool-a", Some("401 only A"))
        .unwrap();
    let sibling = db.get_account("pool-b").unwrap().expect("sibling");
    assert!(sibling.auth_error.is_none());
    drop(db);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
pub(super) fn clear_account_cooldown_clears_free_window() {
    let dir = temp_data_dir("clear-free-cooldown");
    let db = Database::open(dir.clone()).expect("db should open");
    db.create_account(&account("free-cd"))
        .expect("account should be created");

    let until = Utc::now() + Duration::minutes(30);
    db.set_account_rate_limit(
        "free-cd",
        until,
        r#"{"type":"FreeUsageLimitError","message":"Free usage exceeded"}"#,
        Some(UsageWindowKind::Free),
    )
    .expect("free rate limit should save");

    let cooled = db
        .get_account("free-cd")
        .expect("account should load")
        .expect("account should exist");
    assert!(cooled.cooldown_free_until.is_some());
    assert!(cooled.cooldown_until.is_some());

    db.clear_account_cooldown("free-cd")
        .expect("clear should succeed");
    let cleared = db
        .get_account("free-cd")
        .expect("account should load")
        .expect("account should exist");
    assert!(cleared.cooldown_free_until.is_none());
    assert!(cleared.cooldown_until.is_none());
    assert!(cleared.cooldown_generic_until.is_none());
    assert!(cleared.last_error.is_none());

    drop(db);
    fs::remove_dir_all(dir).expect("test data dir should be removed");
}

#[test]
pub(super) fn free_channel_cooldown_survives_account_deletion_restart_and_expires() {
    let dir = temp_data_dir("global-free-cooldown");
    let until = Utc::now() + Duration::minutes(30);
    {
        let mut db = Database::open(dir.clone()).expect("db should open");
        db.create_account(&account("free-source"))
            .expect("source account should be created");
        db.set_account_rate_limit(
            "free-source",
            until,
            "free quota exhausted",
            Some(UsageWindowKind::Free),
        )
        .expect("free rate limit should save");
        db.delete_account("free-source")
            .expect("source account should be deleted");
        db.create_account(&account("replacement"))
            .expect("replacement account should be created");

        assert!(db.free_channel_cooldown_until().unwrap().is_some());
    }

    let db = Database::open(dir.clone()).expect("db should reopen");
    assert!(
        db.free_channel_cooldown_until()
            .expect("global cooldown should load")
            .is_some(),
        "deleting every source row and reopening must not clear the IP-wide cooldown"
    );
    assert!(
        db.free_channel_cooldown_until_at(until + Duration::seconds(1))
            .expect("expiry should be evaluated")
            .is_none(),
        "the global gate must reopen after its deadline"
    );

    drop(db);
    fs::remove_dir_all(dir).expect("test data dir should be removed");
}

#[test]
pub(super) fn durable_free_cooldown_expires_at_exact_deadline() {
    let dir = temp_data_dir("free-cooldown-exact-boundary");
    let until = DateTime::from_naive_utc_and_offset(
        NaiveDate::from_ymd_opt(2024, 1, 2)
            .unwrap()
            .and_hms_opt(3, 4, 5)
            .unwrap(),
        Utc,
    );
    let db = Database::open(dir.clone()).expect("db should open");
    db.create_account(&account("free-source"))
        .expect("source account should be created");
    db.set_account_rate_limit(
        "free-source",
        until,
        "free quota exhausted",
        Some(UsageWindowKind::Free),
    )
    .expect("free rate limit should save");

    let stored = db
        .free_channel_cooldown_until_at(until - Duration::days(1))
        .expect("durable cooldown should load")
        .expect("durable cooldown should be active far before the deadline");
    assert!(
        db.free_channel_cooldown_until_at(stored - Duration::seconds(1))
            .expect("pre-deadline evaluation")
            .is_some(),
        "until > now must keep the durable Free gate closed"
    );
    assert_eq!(
        db.free_channel_cooldown_until_at(stored)
            .expect("exact-deadline evaluation"),
        None,
        "until == now must expire the durable Free gate"
    );

    drop(db);
    fs::remove_dir_all(dir).expect("test data dir should be removed");
}

#[test]
pub(super) fn account_stays_cooling_until_all_windows_expire() {
    let dir = temp_data_dir("multi-window-cooldown");
    let db = Database::open(dir.clone()).expect("db should open");
    db.create_account(&account("multi"))
        .expect("account should be created");

    let now = Utc::now();
    let past_5h = now - Duration::minutes(1);
    let future_week = now + Duration::days(2);
    db.set_account_rate_limit(
        "multi",
        past_5h,
        "5-hour usage limit reached. Resets in 13min.",
        Some(UsageWindowKind::FiveHours),
    )
    .expect("5h rate limit should save");
    db.set_account_rate_limit(
        "multi",
        future_week,
        "weekly usage limit reached. Resets in 4 days.",
        Some(UsageWindowKind::Week),
    )
    .expect("weekly rate limit should save");

    let account = db
        .get_account("multi")
        .expect("account should load")
        .expect("account should exist");
    assert!(account.cooldown_5h_until.is_some_and(|until| until <= now));
    assert!(account.cooldown_week_until.is_some_and(|until| until > now));
    assert!(
        account
            .cooldown_until
            .is_some_and(|until| (until - future_week).num_seconds().abs() < 2)
    );

    drop(db);
    fs::remove_dir_all(dir).expect("test data dir should be removed");
}

#[test]
pub(super) fn fixed_window_5h_anchors_at_the_first_unexpired_success() {
    let dir = temp_data_dir("fixed-5h-windows");
    let db = Database::open(dir.clone()).unwrap();
    for (id, history, expected_cost, expected_minutes) in [
        ("active", &[(4, 1.0), (3, 2.0)][..], 3.0, 60),
        ("after-expiry", &[(6, 10.0), (1, 5.0)][..], 5.0, 240),
        (
            "after-multiple",
            &[(19, 10.0), (13, 5.0), (7, 3.0), (1, 2.0)][..],
            2.0,
            240,
        ),
    ] {
        db.create_account(&account(id)).unwrap();
        let now = Utc::now();
        for &(hours_ago, cost) in history {
            finalize_success(&db, id, cost, now - Duration::hours(hours_ago));
        }
        let usage = db.opencode_go_account_usage(id).unwrap();
        assert_cost(usage.window_5h, expected_cost);
        let reset = usage.resets_in_5h.expect("active window must have a reset");
        let remaining = (reset - Utc::now()).num_minutes();
        assert!(
            (expected_minutes - 5..=expected_minutes + 5).contains(&remaining),
            "{id}: expected ~{expected_minutes}min remaining, got {remaining}"
        );
    }
    drop(db);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
pub(super) fn fixed_window_treats_exact_end_as_the_next_window_start() {
    let dir = temp_data_dir("fixed-boundary");
    let db = Database::open(dir.clone()).expect("db should open");
    db.create_account(&account("boundary"))
        .expect("account should be created");

    let first = Utc::now() - Duration::hours(5) - Duration::minutes(1);
    let exact_end = first + Duration::hours(5);
    finalize_success(&db, "boundary", 10.0, first);
    finalize_success(&db, "boundary", 2.0, exact_end);

    let usage = db
        .opencode_go_account_usage("boundary")
        .expect("usage should load");
    assert_cost(usage.window_5h, 2.0);
    assert!(usage.resets_in_5h.is_some());

    drop(db);
    fs::remove_dir_all(dir).expect("test data dir should be removed");
}

#[test]
pub(super) fn fixed_window_5h_advances_through_multiple_expired_windows_in_one_call() {
    // 复现用户报告的"刷新递减"循环 bug：
    //   4 条间隔 6h 的计费日志（全部已过期）。
    // 旧实现每次刷新只前进一个窗口，前端可见 60→30→13→5.8→0→60+ 循环；
    // 修复后一次调用内连过 4 个过期窗口，next=None 时清空并返回 0，
    // 第二次刷新仍为 0，不再回到最旧日志。
    let dir = temp_data_dir("fixed-5h-multi-expired");
    let db = Database::open(dir.clone()).expect("db should open");
    db.create_account(&account("cycle"))
        .expect("account should be created");

    // ts1 = -24h, ts2 = -18h, ts3 = -12h, ts4 = -6h：每条间隔 6h（> 5h 窗口长度）。
    let ts1 = Utc::now() - Duration::hours(24);
    let ts2 = ts1 + Duration::hours(6);
    let ts3 = ts2 + Duration::hours(6);
    let ts4 = ts3 + Duration::hours(6);
    finalize_success(&db, "cycle", 10.0, ts1);
    finalize_success(&db, "cycle", 5.0, ts2);
    finalize_success(&db, "cycle", 3.0, ts3);
    finalize_success(&db, "cycle", 2.0, ts4);

    // 第一次刷新：应直接走完所有过期窗口，返回 0（无新请求）。
    let usage = db
        .opencode_go_account_usage("cycle")
        .expect("usage should load");
    assert_cost(usage.window_5h, 0.0);
    assert!(
        usage.resets_in_5h.is_none(),
        "no active window after all expired; resets_in_5h should be None"
    );

    // 第二次刷新：不应回到最旧日志循环重放，仍稳定为 0。
    let usage2 = db
        .opencode_go_account_usage("cycle")
        .expect("usage should load again");
    assert_cost(usage2.window_5h, 0.0);
    assert!(usage2.resets_in_5h.is_none());

    drop(db);
    fs::remove_dir_all(dir).expect("test data dir should be removed");
}

#[test]
pub(super) fn fixed_window_5h_with_no_usage_returns_zero_and_full_window_remaining() {
    let dir = temp_data_dir("fixed-5h-empty");
    let db = Database::open(dir.clone()).expect("db should open");
    db.create_account(&account("empty"))
        .expect("account should be created");

    let usage = db
        .opencode_go_account_usage("empty")
        .expect("usage should load");
    assert_cost(usage.window_5h, 0.0);
    // 没用过：倒计时为 None（前端显示"5h0min"由默认值决定）
    assert!(usage.resets_in_5h.is_none());

    drop(db);
    fs::remove_dir_all(dir).expect("test data dir should be removed");
}

#[test]
pub(super) fn month_window_accumulates_from_purchase_date_to_expires_on() {
    let dir = temp_data_dir("month-window");
    let db = Database::open(dir.clone()).expect("db should open");
    let mut acct = account("monthly");
    acct.purchase_date = "2026-07-01".into();
    db.create_account(&acct).expect("account should be created");

    // 模拟一条历史成功请求（任何时间都算，月窗口从 purchase_date 累计）
    finalize_success(&db, "monthly", 5.0, Utc::now());

    let usage = db
        .opencode_go_account_usage("monthly")
        .expect("usage should load");
    assert_cost(usage.window_month, 5.0);
    let reset = usage
        .resets_in_month
        .expect("month window reset should be purchase_date + 1 month");
    // 2026-07-01 + 1 自然月 = 2026-08-01 00:00
    let expected = DateTime::parse_from_rfc3339("2026-08-01T00:00:00+00:00")
        .unwrap()
        .with_timezone(&Utc);
    assert!(
        (reset - expected).num_seconds().abs() < 86400,
        "expected ~2026-08-01, got {reset}"
    );

    drop(db);
    fs::remove_dir_all(dir).expect("test data dir should be removed");
}

#[test]
pub(super) fn manual_calibrate_5h_window_sets_started_at_and_cost_offset() {
    let dir = temp_data_dir("calibrate-5h");
    let db = Database::open(dir.clone()).expect("db should open");
    db.create_account(&account("calib"))
        .expect("account should be created");

    // 用户在别处已用 50%，距上游重置还剩 3 小时
    db.calibrate_account_usage("calib", UsageWindowKind::FiveHours, 50.0, Some(180), 12.0)
        .expect("calibrate should save");

    let usage = db
        .opencode_go_account_usage("calib")
        .expect("usage should load");
    // 5h 限额 12.0，50% = 6.0
    assert_cost(usage.window_5h, 6.0);
    let reset = usage
        .resets_in_5h
        .expect("5h window reset should be set after manual calibrate");
    let remaining_min = (reset - Utc::now()).num_minutes();
    assert!(
        (175..=185).contains(&remaining_min),
        "expected ~180min remaining, got {remaining_min}"
    );

    // 后续网关内的请求累加到偏移之上
    finalize_success(&db, "calib", 1.0, Utc::now());
    let usage = db
        .opencode_go_account_usage("calib")
        .expect("usage should reload");
    assert_cost(usage.window_5h, 7.0);

    drop(db);
    fs::remove_dir_all(dir).expect("test data dir should be removed");
}

#[test]
pub(super) fn calibrate_subtracts_existing_window_usage_from_offset() {
    // 回归测试：活跃账号（窗口内已有 forward_logs）校准时，
    // offset 必须 = target_cost - actual_cost，否则 compute_fixed_window
    // 返回 offset + actual_cost，显示百分比会高于用户输入。
    let dir = temp_data_dir("calibrate-with-usage");
    let db = Database::open(dir.clone()).expect("db should open");
    db.create_account(&account("active"))
        .expect("account should be created");

    // 1 小时前已用 $3（落在 5h 窗口内）
    let ts = Utc::now() - Duration::hours(1);
    finalize_success(&db, "active", 3.0, ts);

    // 用户说"我在别处用到了 50%"（5h 限额 12.0 → target_cost = 6.0）
    // 期望：offset = 6.0 - 3.0 = 3.0，compute_fixed_window 返回 3.0 + 3.0 = 6.0 = 50%
    // 修复前 bug：offset = 6.0，compute_fixed_window 返回 6.0 + 3.0 = 9.0 = 75%
    // 用 resets_in_minutes=180 让新窗口的 started_at = now + 3h - 5h = now - 2h，
    // 把 1 小时前的 log 稳稳包含进窗口（避开 finalize 与 calibrate 之间的微秒级时序差）。
    db.calibrate_account_usage("active", UsageWindowKind::FiveHours, 50.0, Some(180), 12.0)
        .expect("calibrate should save with existing usage");
    let usage = db
        .opencode_go_account_usage("active")
        .expect("usage should load");
    assert_cost(usage.window_5h, 6.0);

    // 后续请求继续累加：offset=3.0 + actual=3.0 + new=2.0 = 8.0
    finalize_success(&db, "active", 2.0, Utc::now());
    let usage = db
        .opencode_go_account_usage("active")
        .expect("usage should reload");
    assert_cost(usage.window_5h, 8.0);

    drop(db);
    fs::remove_dir_all(dir).expect("test data dir should be removed");
}

#[test]
pub(super) fn calibrate_below_actual_usage_allows_negative_offset() {
    // 回归测试（Bug 1.5）：用户校准的百分比低于窗口内实际 cost 时，offset 允许为负数，
    // 让 compute_fixed_window 返回 offset + actual = target_cost，与用户输入一致。
    // 之前 max(0, target - actual) 钳制 + schema CHECK (offset >= 0) 约束让向左拉
    // 滑块时被锁死在实际 cost 对应的百分比（9.0 / 12.0 * 100 = 75%，对应用户看到的 40.2%）。
    let dir = temp_data_dir("calibrate-below-usage");
    let db = Database::open(dir.clone()).expect("db should open");
    db.create_account(&account("clamp"))
        .expect("account should be created");

    // 已用 $9
    let ts = Utc::now() - Duration::hours(1);
    finalize_success(&db, "clamp", 9.0, ts);

    // 用户校准到 20%（target_cost = 2.4，但实际已用 9.0）
    // offset = 2.4 - 9.0 = -6.6；compute_fixed_window 返回 -6.6 + 9.0 = 2.4 = 20%。
    // 用 resets_in_minutes=180 让新窗口的 started_at = now - 2h，把 1 小时前的
    // $9 log 稳稳包含进窗口（避开 finalize 与 calibrate 之间的微秒级时序差）。
    db.calibrate_account_usage("clamp", UsageWindowKind::FiveHours, 20.0, Some(180), 12.0)
        .expect("calibrate below actual usage should allow negative offset");
    let usage = db
        .opencode_go_account_usage("clamp")
        .expect("usage should load");
    // 显示的 cost = offset(-6.6) + actual(9.0) = 2.4（用户输入的 20%）
    assert_cost(usage.window_5h, 2.4);

    drop(db);
    fs::remove_dir_all(dir).expect("test data dir should be removed");
}

#[test]
pub(super) fn calibrate_month_window_writes_offset_without_started_at() {
    // 回归测试（Bug 2）：月窗口必须支持手动校准。
    // 月窗口不写 started_at 列（起点固定为 purchase_date），只更新 cost_offset。
    // resets_in_minutes 被忽略——窗口由 purchase_date/expires_on 决定。
    let dir = temp_data_dir("calibrate-month");
    let db = Database::open(dir.clone()).expect("db should open");
    let mut acct = account("monthly-calib");
    acct.purchase_date = "2026-07-01".into();
    db.create_account(&acct).expect("account should be created");

    // 已用 $5（落在月窗口内：purchase_date 00:00 起）
    finalize_success(&db, "monthly-calib", 5.0, Utc::now());

    // 用户校准到 50%（月限额 100.0 → target_cost = 50.0）
    // 期望：offset = 50.0 - 5.0 = 45.0；compute_month_window 返回 45.0 + 5.0 = 50.0 = 50%。
    db.calibrate_account_usage("monthly-calib", UsageWindowKind::Month, 50.0, None, 100.0)
        .expect("month window calibrate should save");
    let usage = db
        .opencode_go_account_usage("monthly-calib")
        .expect("usage should load");
    assert_cost(usage.window_month, 50.0);
    // resets_in_month 仍是 purchase_date + 1 自然月（不受 resets_in_minutes 影响）
    let reset = usage
        .resets_in_month
        .expect("month window reset should be purchase_date + 1 month");
    let expected = DateTime::parse_from_rfc3339("2026-08-01T00:00:00+00:00")
        .unwrap()
        .with_timezone(&Utc);
    assert!(
        (reset - expected).num_seconds().abs() < 86400,
        "expected ~2026-08-01, got {reset}"
    );

    drop(db);
    fs::remove_dir_all(dir).expect("test data dir should be removed");
}

#[test]
pub(super) fn changing_purchase_date_resets_month_calibration_offset() {
    let dir = temp_data_dir("month-renewal-reset");
    let db = Database::open(dir.clone()).expect("db should open");
    let new_purchase_date = local_today();
    let old_purchase_date = (Local::now().date_naive() - Duration::days(10))
        .format("%Y-%m-%d")
        .to_string();
    let mut acct = account("monthly-renewal");
    acct.purchase_date = old_purchase_date;
    db.create_account(&acct).expect("account should be created");

    finalize_success(&db, "monthly-renewal", 5.0, Utc::now() - Duration::days(2));
    db.calibrate_account_usage("monthly-renewal", UsageWindowKind::Month, 0.0, None, 100.0)
        .expect("month calibration should save a negative offset");
    assert_cost(
        db.opencode_go_account_usage("monthly-renewal")
            .expect("usage should load")
            .window_month,
        0.0,
    );

    db.update_account(
        "monthly-renewal",
        &AccountUpdate {
            name: None,
            username: None,
            password: None,
            key: None,
            enabled: None,
            referral_code: None,
            purchase_date: Some(new_purchase_date),
            notes: None,
        },
        None,
        None,
    )
    .expect("purchase date should update");
    let offset: f64 = db
        .conn
        .query_row(
            "SELECT usage_month_window_cost_offset FROM credentials WHERE legacy_account_id = ?1",
            ["monthly-renewal"],
            |row| row.get(0),
        )
        .expect("month offset should load");
    assert_cost(offset, 0.0);
    assert_cost(
        db.opencode_go_account_usage("monthly-renewal")
            .expect("renewed usage should load")
            .window_month,
        0.0,
    );

    finalize_success(&db, "monthly-renewal", 2.0, Utc::now());
    assert_cost(
        db.opencode_go_account_usage("monthly-renewal")
            .expect("new cycle usage should load")
            .window_month,
        2.0,
    );

    drop(db);
    fs::remove_dir_all(dir).expect("test data dir should be removed");
}

#[test]
pub(super) fn calibrate_rejects_reset_outside_fixed_window_without_panicking() {
    let dir = temp_data_dir("calibrate-reset-bounds");
    let db = Database::open(dir.clone()).expect("db should open");
    db.create_account(&account("reset-bounds"))
        .expect("account should be created");

    for (window, minutes) in [
        (UsageWindowKind::FiveHours, -1),
        (UsageWindowKind::FiveHours, 301),
        (UsageWindowKind::Week, 10_081),
        (UsageWindowKind::FiveHours, i64::MAX),
    ] {
        assert!(
            db.calibrate_account_usage("reset-bounds", window, 50.0, Some(minutes), 100.0,)
                .is_err(),
            "{window:?} should reject {minutes} minutes"
        );
    }

    drop(db);
    fs::remove_dir_all(dir).expect("test data dir should be removed");
}

#[test]
pub(super) fn calibrate_account_usage_snapshot_updates_all_three_windows() {
    let dir = temp_data_dir("calibrate-snapshot-ok");
    let db = Database::open(dir.clone()).expect("db should open");
    let mut acct = account("snap-ok");
    acct.purchase_date = "2026-07-01".into();
    db.create_account(&acct).expect("account should be created");
    finalize_success(&db, "snap-ok", 3.0, Utc::now() - Duration::hours(1));

    let limits = snapshot_limits();
    let usage = db
        .calibrate_account_usage_snapshot(
            "snap-ok",
            &usage_calibration(50.0, 20.0, 10.0, 180, 1_440),
            &limits,
        )
        .expect("snapshot calibrate should save");
    assert_cost(usage.window_5h, 6.0);
    assert_cost(usage.window_week, 6.0);
    assert_cost(usage.window_month, 10.0);
    let remaining_5h =
        (usage.resets_in_5h.expect("5h reset should be set") - Utc::now()).num_minutes();
    assert!(
        (175..=185).contains(&remaining_5h),
        "expected ~180min remaining, got {remaining_5h}"
    );
    let remaining_week =
        (usage.resets_in_week.expect("week reset should be set") - Utc::now()).num_minutes();
    assert!(
        (1_435..=1_445).contains(&remaining_week),
        "expected ~1440min remaining, got {remaining_week}"
    );

    drop(db);
    fs::remove_dir_all(dir).expect("test data dir should be removed");
}

#[test]
pub(super) fn official_usage_sync_rolls_back_baseline_when_success_metadata_fails() {
    let dir = temp_data_dir("official-sync-atomic-failure");
    let db = Database::open(dir.clone()).expect("db should open");
    let mut acct = account("atomic-sync");
    acct.purchase_date = "2026-07-01".into();
    db.create_account(&acct).expect("account should be created");
    let limits = snapshot_limits();
    db.calibrate_account_usage_snapshot(
        "atomic-sync",
        &usage_calibration(10.0, 20.0, 30.0, 120, 1_200),
        &limits,
    )
    .expect("initial baseline should save");
    let previous_success = Utc::now() - Duration::hours(2);
    db.record_account_usage_sync_success(
        "atomic-sync",
        previous_success,
        previous_success + Duration::hours(24),
        false,
    )
    .expect("initial sync metadata should save");
    let before = db
        .account_usage_with_limits("atomic-sync", &limits)
        .expect("initial usage should load");
    let sync_before = db
        .account_usage_sync_state("atomic-sync")
        .expect("initial sync state should load")
        .expect("sync state should exist");

    db.conn
        .execute_batch(
            "CREATE TRIGGER fail_official_sync_metadata
                 BEFORE UPDATE OF last_success_at ON provider_usage_sync_state
                 WHEN NEW.account_id = 'atomic-sync'
                 BEGIN
                    SELECT RAISE(ABORT, 'forced usage sync metadata failure');
                 END;",
        )
        .expect("failure trigger should install");

    let now = Utc::now();
    let result = db.commit_official_usage_sync_success(
        "atomic-sync",
        "cipher",
        &usage_calibration(80.0, 70.0, 60.0, 180, 1_440),
        &limits,
        AccountUsageSyncSuccessMetadata {
            now,
            next_eligible_at: now + Duration::hours(1),
            mark_expedited: true,
        },
    );
    assert!(
        result.is_err(),
        "forced metadata failure must abort the sync"
    );

    let after = db
        .account_usage_with_limits("atomic-sync", &limits)
        .expect("usage should remain readable");
    assert_cost(after.window_5h, before.window_5h);
    assert_cost(after.window_week, before.window_week);
    assert_cost(after.window_month, before.window_month);
    let sync_after = db
        .account_usage_sync_state("atomic-sync")
        .expect("sync state should load")
        .expect("sync state should exist");
    assert_eq!(sync_after.last_success_at, sync_before.last_success_at);
    assert_eq!(sync_after.next_eligible_at, sync_before.next_eligible_at);
    assert_eq!(sync_after.failure_streak, sync_before.failure_streak);
    assert_eq!(sync_after.last_expedited_at, sync_before.last_expedited_at);

    drop(db);
    fs::remove_dir_all(dir).expect("test data dir should be removed");
}

#[test]
pub(super) fn calibrate_account_usage_snapshot_rolls_back_when_second_window_fails() {
    let dir = temp_data_dir("calibrate-snapshot-week-fail");
    let db = Database::open(dir.clone()).expect("db should open");
    let mut acct = account("snap-week");
    acct.purchase_date = "2026-07-01".into();
    db.create_account(&acct).expect("account should be created");
    let limits = snapshot_limits();
    db.calibrate_account_usage_snapshot(
        "snap-week",
        &usage_calibration(10.0, 20.0, 30.0, 100, 200),
        &limits,
    )
    .expect("initial snapshot should save");
    let before = usage_offset_row(&db, "snap-week");
    let before_usage = db
        .account_usage_with_limits("snap-week", &limits)
        .expect("usage should load");

    assert!(
        db.calibrate_account_usage_snapshot(
            "snap-week",
            &usage_calibration(80.0, 90.0, 40.0, 180, 10_081),
            &limits
        )
        .is_err(),
        "weekly minutes outside the 7-day window should fail"
    );

    assert_eq!(usage_offset_row(&db, "snap-week"), before);
    let after = db
        .account_usage_with_limits("snap-week", &limits)
        .expect("usage should reload");
    assert_cost(after.window_5h, before_usage.window_5h);
    assert_cost(after.window_week, before_usage.window_week);
    assert_cost(after.window_month, before_usage.window_month);

    drop(db);
    fs::remove_dir_all(dir).expect("test data dir should be removed");
}

#[test]
pub(super) fn calibrate_account_usage_snapshot_rolls_back_when_third_window_fails() {
    let dir = temp_data_dir("calibrate-snapshot-month-fail");
    let db = Database::open(dir.clone()).expect("db should open");
    let mut acct = account("snap-month");
    acct.purchase_date = "2026-07-01".into();
    db.create_account(&acct).expect("account should be created");
    let limits = snapshot_limits();
    db.calibrate_account_usage_snapshot(
        "snap-month",
        &usage_calibration(10.0, 20.0, 30.0, 100, 200),
        &limits,
    )
    .expect("initial snapshot should save");
    let before = usage_offset_row(&db, "snap-month");
    let before_usage = db
        .account_usage_with_limits("snap-month", &limits)
        .expect("usage should load");

    db.conn
        .execute_batch(
            "CREATE TRIGGER reject_month_calibrate
                 BEFORE UPDATE OF usage_month_window_cost_offset ON credentials
                 WHEN NEW.legacy_account_id = 'snap-month'
                 BEGIN
                     SELECT RAISE(ABORT, 'forced month calibrate failure');
                 END;",
        )
        .expect("failure trigger should be installed");

    assert!(
        db.calibrate_account_usage_snapshot(
            "snap-month",
            &usage_calibration(80.0, 90.0, 40.0, 180, 1_440),
            &limits
        )
        .is_err(),
        "month window trigger should fail the transaction"
    );

    assert_eq!(usage_offset_row(&db, "snap-month"), before);
    let after = db
        .account_usage_with_limits("snap-month", &limits)
        .expect("usage should reload");
    assert_cost(after.window_5h, before_usage.window_5h);
    assert_cost(after.window_week, before_usage.window_week);
    assert_cost(after.window_month, before_usage.window_month);

    drop(db);
    fs::remove_dir_all(dir).expect("test data dir should be removed");
}

#[test]
pub(super) fn soonest_reset_is_minimum_of_each_accounts_latest_active_cooldown() {
    let dir = temp_data_dir("soonest-account-reset");
    let db = Database::open(dir.clone()).expect("db should open");
    for id in ["first", "second"] {
        db.create_account(&account(id))
            .expect("account should be created");
    }

    let now = Utc::now();
    let first_early = now + Duration::hours(1);
    let first_latest = now + Duration::hours(4);
    let second_latest = now + Duration::hours(2);
    db.set_account_rate_limit(
        "first",
        first_early,
        "5-hour usage limit reached",
        Some(UsageWindowKind::FiveHours),
    )
    .expect("first short cooldown should save");
    db.set_account_rate_limit(
        "first",
        first_latest,
        "weekly usage limit reached",
        Some(UsageWindowKind::Week),
    )
    .expect("first long cooldown should save");
    db.set_account_rate_limit("second", second_latest, "unknown rate limit", None)
        .expect("second cooldown should save");

    let reset = db
        .soonest_cooldown_reset()
        .expect("reset query should work")
        .expect("a reset should exist");
    assert!((reset - second_latest).num_seconds().abs() < 2);

    db.set_account_auth_error("second", Some("upstream auth error 401"))
        .expect("auth breaker should save");
    let reset = db
        .soonest_cooldown_reset()
        .expect("reset query should work")
        .expect("an eligible reset should exist");
    assert!((reset - first_latest).num_seconds().abs() < 2);

    drop(db);
    fs::remove_dir_all(dir).expect("test data dir should be removed");
}

#[test]
pub(super) fn new_accounts_have_safe_usage_sync_defaults() {
    let dir = temp_data_dir("v21-usage-sync");
    let db = Database::open(dir.clone()).unwrap();
    let account = account("sync-defaults");
    db.create_account(&account).unwrap();
    let sync = db
        .account_usage_sync_state("sync-defaults")
        .unwrap()
        .unwrap();
    assert!(sync.last_success_at.is_none());
    assert!(sync.last_attempt_at.is_none());
    assert!(sync.next_eligible_at.is_none());
    assert_eq!(sync.failure_streak, 0);
    assert!(sync.last_expedited_at.is_none());

    drop(db);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
pub(super) fn go_and_ollama_accounts_persist_enablement_changes() {
    let dir = temp_data_dir("enablement-gate");
    let db = Database::open(dir.clone()).unwrap();
    for (id, provider_id) in [
        ("go-enabled", OPENCODE_PROVIDER_ID),
        ("ollama-enabled", OLLAMA_PROVIDER_ID),
    ] {
        let mut candidate = account(id);
        candidate.provider_id = provider_id.to_string();
        candidate.enabled = true;
        db.create_account(&candidate).unwrap();
        assert!(db.get_account(id).unwrap().unwrap().enabled, "{id}");
        for enabled in [false, true] {
            db.update_account(
                id,
                &AccountUpdate {
                    enabled: Some(enabled),
                    ..AccountUpdate::default()
                },
                None,
                None,
            )
            .unwrap();
            assert_eq!(
                db.get_account(id).unwrap().unwrap().enabled,
                enabled,
                "{id}"
            );
        }
    }
    drop(db);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
pub(super) fn ollama_billing_create_failure_rolls_back_the_account_row() {
    let dir = temp_data_dir("ollama-billing-atomic");
    let db = open_with_host_cipher(dir.clone()).unwrap();
    let mut ollama = account("ollama-atomic");
    ollama.provider_id = OLLAMA_PROVIDER_ID.to_string();
    ollama.key_cipher = fixture_account_key_cipher();
    ollama.purchase_date = "2026-08-01".into();
    db.conn
        .execute_batch(
            "CREATE TRIGGER fail_ollama_billing
                 BEFORE INSERT ON ollama_cloud_billing
                 BEGIN
                     SELECT RAISE(ABORT, 'forced ollama billing failure');
                 END;",
        )
        .unwrap();
    let error = db
        .create_account_with_contract_and_billing(&ollama, None, &[], Some(OllamaBillingTier::Pro))
        .expect_err("billing failure should abort the create");
    assert!(
        error.to_string().contains("forced ollama billing failure"),
        "{error}"
    );
    assert!(db.get_account("ollama-atomic").unwrap().is_none());
    assert_eq!(db.ollama_cloud_billing_tier("ollama-atomic").unwrap(), None);
    drop(db);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
pub(super) fn ollama_billing_update_failure_preserves_account_fields_and_key() {
    let dir = temp_data_dir("ollama-billing-update-atomic");
    let db = open_with_host_cipher(dir.clone()).unwrap();
    let original_key = fixture_account_key_cipher();
    let replacement_key = test_host_cipher()
        .encrypt("sk-replacement")
        .expect("replacement key should encrypt");
    let mut ollama = account("ollama-update-atomic");
    ollama.provider_id = OLLAMA_PROVIDER_ID.to_string();
    ollama.name = "original-name".into();
    ollama.key_cipher = original_key.clone();
    ollama.purchase_date = "2026-08-01".into();
    db.create_account_with_contract_and_billing(&ollama, None, &[], Some(OllamaBillingTier::Pro))
        .unwrap();

    db.conn
        .execute_batch(
            "CREATE TRIGGER fail_ollama_billing_update
                 BEFORE INSERT ON ollama_cloud_billing
                 BEGIN
                     SELECT RAISE(ABORT, 'forced ollama billing update failure');
                 END;",
        )
        .unwrap();
    let rename = AccountUpdate {
        name: Some("renamed".into()),
        ..AccountUpdate::default()
    };
    let error = db
        .update_account_with_billing(
            "ollama-update-atomic",
            &rename,
            Some(&replacement_key),
            None,
            Some(Some(OllamaBillingTier::Max)),
        )
        .expect_err("billing failure should abort the account update");
    assert!(
        error
            .to_string()
            .contains("forced ollama billing update failure"),
        "{error}"
    );
    let rolled_back = db.get_account("ollama-update-atomic").unwrap().unwrap();
    assert_eq!(rolled_back.name, "original-name");
    assert_eq!(rolled_back.key_cipher, original_key);
    assert_eq!(
        db.ollama_cloud_billing_tier("ollama-update-atomic")
            .unwrap(),
        Some(OllamaBillingTier::Pro)
    );

    db.conn
        .execute_batch("DROP TRIGGER fail_ollama_billing_update;")
        .unwrap();
    db.update_account_with_billing(
        "ollama-update-atomic",
        &rename,
        Some(&replacement_key),
        None,
        Some(Some(OllamaBillingTier::Max)),
    )
    .unwrap();
    let updated = db.get_account("ollama-update-atomic").unwrap().unwrap();
    assert_eq!(updated.name, "renamed");
    assert_eq!(updated.key_cipher, replacement_key);
    assert_eq!(
        db.ollama_cloud_billing_tier("ollama-update-atomic")
            .unwrap(),
        Some(OllamaBillingTier::Max)
    );

    db.update_account_with_billing(
        "ollama-update-atomic",
        &AccountUpdate {
            name: Some("name-only".into()),
            ..AccountUpdate::default()
        },
        None,
        None,
        None,
    )
    .unwrap();
    let name_only = db.get_account("ollama-update-atomic").unwrap().unwrap();
    assert_eq!(name_only.name, "name-only");
    assert_eq!(name_only.key_cipher, replacement_key);
    assert_eq!(
        db.ollama_cloud_billing_tier("ollama-update-atomic")
            .unwrap(),
        Some(OllamaBillingTier::Max)
    );

    db.update_account_with_billing(
        "ollama-update-atomic",
        &AccountUpdate {
            name: Some("cleared".into()),
            ..AccountUpdate::default()
        },
        None,
        None,
        Some(None),
    )
    .unwrap();
    let cleared = db.get_account("ollama-update-atomic").unwrap().unwrap();
    assert_eq!(cleared.name, "cleared");
    assert_eq!(cleared.key_cipher, replacement_key);
    assert_eq!(
        db.ollama_cloud_billing_tier("ollama-update-atomic")
            .unwrap(),
        None
    );

    drop(db);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
pub(super) fn ollama_billing_tier_round_trip_and_cascade() {
    let dir = temp_data_dir("ollama-billing-roundtrip");
    let mut db = Database::open(dir.clone()).unwrap();
    let mut ollama = account("ollama-bill");
    ollama.provider_id = OLLAMA_PROVIDER_ID.to_string();
    ollama.purchase_date = "2026-08-01".into();
    db.create_account(&ollama).unwrap();
    db.set_ollama_cloud_billing_tier("ollama-bill", Some(OllamaBillingTier::Pro))
        .unwrap();
    assert_eq!(
        db.ollama_cloud_billing_tier("ollama-bill").unwrap(),
        Some(OllamaBillingTier::Pro)
    );
    db.set_ollama_cloud_billing_tier("ollama-bill", None)
        .unwrap();
    assert_eq!(db.ollama_cloud_billing_tier("ollama-bill").unwrap(), None);
    db.set_ollama_cloud_billing_tier("ollama-bill", Some(OllamaBillingTier::Team))
        .unwrap();
    db.delete_account("ollama-bill").unwrap();
    assert_eq!(db.ollama_cloud_billing_tier("ollama-bill").unwrap(), None);
    drop(db);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
pub(super) fn ollama_tier_change_clears_month_offset_same_tier_preserves() {
    let dir = temp_data_dir("ollama-tier-offset");
    let db = Database::open(dir.clone()).unwrap();
    let mut ollama = account("ollama-offset");
    ollama.provider_id = OLLAMA_PROVIDER_ID.to_string();
    ollama.purchase_date = "2026-08-01".into();
    db.create_account(&ollama).unwrap();
    db.set_ollama_cloud_billing_tier("ollama-offset", Some(OllamaBillingTier::Pro))
        .unwrap();
    db.conn
        .execute(
            "UPDATE credentials SET usage_month_window_cost_offset = 12.5 WHERE legacy_account_id = ?1",
            ["ollama-offset"],
        )
        .unwrap();
    db.set_ollama_cloud_billing_tier("ollama-offset", Some(OllamaBillingTier::Pro))
        .unwrap();
    let offset: f64 = db
        .conn
        .query_row(
            "SELECT usage_month_window_cost_offset FROM credentials WHERE legacy_account_id = ?1",
            ["ollama-offset"],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(offset, 12.5);
    db.set_ollama_cloud_billing_tier("ollama-offset", Some(OllamaBillingTier::Max))
        .unwrap();
    let offset: f64 = db
        .conn
        .query_row(
            "SELECT usage_month_window_cost_offset FROM credentials WHERE legacy_account_id = ?1",
            ["ollama-offset"],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(offset, 0.0);
    assert_eq!(db.list_forward_logs(10).unwrap().len(), 0);
    drop(db);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
pub(super) fn ollama_month_window_is_half_open_and_exposes_overage() {
    use chrono::TimeZone;
    let dir = temp_data_dir("ollama-month-bounds");
    let db = Database::open(dir.clone()).unwrap();
    let mut ollama = account("ollama-month");
    ollama.provider_id = OLLAMA_PROVIDER_ID.to_string();
    ollama.purchase_date = "2026-08-01".into();
    db.create_account(&ollama).unwrap();
    db.set_ollama_cloud_billing_tier("ollama-month", Some(OllamaBillingTier::Pro))
        .unwrap();

    let start_naive = chrono::NaiveDate::from_ymd_opt(2026, 8, 1)
        .unwrap()
        .and_hms_opt(0, 0, 0)
        .unwrap();
    let start = chrono::Local
        .from_local_datetime(&start_naive)
        .single()
        .unwrap()
        .with_timezone(&Utc);
    let expires = purchase_expires_on("2026-08-01").unwrap();
    let end_naive = chrono::NaiveDate::parse_from_str(&expires, "%Y-%m-%d")
        .unwrap()
        .and_hms_opt(0, 0, 0)
        .unwrap();
    let end = chrono::Local
        .from_local_datetime(&end_naive)
        .single()
        .unwrap()
        .with_timezone(&Utc);

    let mut before = forward_log("ollama-month", "success", 5.0);
    before.cost_state = "priced".into();
    before.timestamp = start - chrono::Duration::hours(1);
    db.log_forward(&before).unwrap();

    let mut inside = forward_log("ollama-month", "success", 80.0);
    inside.cost_state = "priced".into();
    inside.timestamp = start + chrono::Duration::days(1);
    db.log_forward(&inside).unwrap();

    let mut at_end = forward_log("ollama-month", "success", 9.0);
    at_end.cost_state = "priced".into();
    at_end.timestamp = end;
    db.log_forward(&at_end).unwrap();

    let mut after = forward_log("ollama-month", "success", 11.0);
    after.cost_state = "priced".into();
    after.timestamp = end + chrono::Duration::hours(1);
    db.log_forward(&after).unwrap();

    let windows = db
        .live_ollama_month_quota_window("ollama-month", 60.0)
        .unwrap();
    assert_eq!(windows.len(), 1);
    assert_eq!(windows[0].used, 80.0);
    assert_eq!(windows[0].limit_value, Some(60.0));
    let (used, reset) = db.ollama_month_usage("ollama-month").unwrap();
    assert_eq!(used, 80.0);
    assert_eq!(reset, Some(end));
    drop(db);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
pub(super) fn utc_token_day_bounds_include_the_whole_earliest_calendar_day() {
    let now = chrono::DateTime::parse_from_rfc3339("2026-09-22T14:00:00Z")
        .unwrap()
        .with_timezone(&chrono::Utc);
    let (start, end) = utc_token_day_bounds(now, 1).unwrap();
    assert_eq!(
        start,
        chrono::DateTime::parse_from_rfc3339("2026-09-22T00:00:00Z")
            .unwrap()
            .with_timezone(&chrono::Utc)
    );
    assert_eq!(
        end,
        chrono::DateTime::parse_from_rfc3339("2026-09-23T00:00:00Z")
            .unwrap()
            .with_timezone(&chrono::Utc)
    );
    let morning = chrono::DateTime::parse_from_rfc3339("2026-09-22T10:00:00Z")
        .unwrap()
        .with_timezone(&chrono::Utc);
    assert!(morning >= start && morning < end);
    let (week_start, week_end) = utc_token_day_bounds(now, 7).unwrap();
    let early_morning = chrono::DateTime::parse_from_rfc3339("2026-09-16T10:00:00Z")
        .unwrap()
        .with_timezone(&chrono::Utc);
    let early_evening = chrono::DateTime::parse_from_rfc3339("2026-09-16T20:00:00Z")
        .unwrap()
        .with_timezone(&chrono::Utc);
    assert!(early_morning >= week_start && early_morning < week_end);
    assert!(early_evening >= week_start && early_evening < week_end);
    assert!(utc_token_day_bounds(now, 0).is_err());
}

#[test]
pub(super) fn daily_tokens_by_model_includes_midnight_of_the_earliest_utc_day() {
    let dir = temp_data_dir("daily-token-calendar");
    let db = Database::open(dir.clone()).unwrap();
    let now = chrono::Utc::now();
    let mut today = forward_log("acct", "success", 0.0);
    today.timestamp = now.date_naive().and_hms_opt(0, 0, 0).unwrap().and_utc();
    today.model = "today-model".into();
    today.prompt_tokens = 100;
    db.log_forward(&today).unwrap();

    let earliest = (now.date_naive() - chrono::Duration::days(6))
        .and_hms_opt(0, 0, 0)
        .unwrap()
        .and_utc();
    let mut early = forward_log("acct", "success", 0.0);
    early.timestamp = earliest;
    early.model = "early-model".into();
    early.prompt_tokens = 40;
    early.completion_tokens = 10;
    db.log_forward(&early).unwrap();
    let mut early_evening = forward_log("acct", "success", 0.0);
    early_evening.timestamp = earliest + chrono::Duration::hours(20);
    early_evening.model = "early-model".into();
    early_evening.prompt_tokens = 5;
    early_evening.completion_tokens = 5;
    db.log_forward(&early_evening).unwrap();

    let one_day = db.daily_tokens_by_model(1).unwrap();
    assert_eq!(one_day.iter().map(|row| row.tokens).sum::<i64>(), 100);
    let week = db.daily_tokens_by_model(7).unwrap();
    let early_tokens: i64 = week
        .iter()
        .filter(|row| row.model == "early-model")
        .map(|row| row.tokens)
        .sum();
    assert_eq!(early_tokens, 60);
    assert!(db.daily_tokens_by_model(0).is_err());
    drop(db);
    fs::remove_dir_all(dir).unwrap();
}
