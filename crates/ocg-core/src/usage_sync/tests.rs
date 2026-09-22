use super::*;
use crate::crypto::{KeyCipher, StaticKeyCipher};
use crate::db::{AccountUsageCalibrationSnapshot, Database};
use crate::models::{
    Account, AccountSetupStep, AccountType, AppConfig, CreditBalance, QuotaWindow,
};
use crate::provider::{
    COMMAND_CODE_PROVIDER_ID, QUOTA_WINDOW_FIVE_HOURS, QUOTA_WINDOW_MONTH, QUOTA_WINDOW_WEEK,
    ZEN_FREE_ACCOUNT_ID,
};
use crate::state::{CoreState, CoreStateInner};
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering as AtomicOrdering};

fn fixed(ts: &str) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(ts)
        .unwrap()
        .with_timezone(&Utc)
}

fn usage_loop_started(state: &CoreState) -> bool {
    state.usage_sync.loop_started.load(AtomicOrdering::Acquire)
}

fn loopback_ephemeral() -> std::net::SocketAddr {
    std::net::SocketAddr::from(([127, 0, 0, 1], 0))
}

#[tokio::test]
async fn bind_does_not_start_usage_loop() {
    let (dir, state) = test_state("bind-no-loop");
    assert!(!usage_loop_started(&state));

    let first =
        crate::gateway::listener::GatewayLifecycle::bind(state.clone(), loopback_ephemeral())
            .await
            .unwrap();
    assert!(first.port != 0, "listener bind must occupy a TCP port");
    assert!(
        state.dashboard_local_mode(),
        "loopback bind must keep dashboard local mode"
    );
    assert!(
        !usage_loop_started(&state),
        "GatewayLifecycle::bind must not start the process-level usage worker"
    );

    crate::gateway::stop_gateway(first);
    assert!(
        !usage_loop_started(&state),
        "listener stop must not start the usage worker"
    );
    let _ = state.config();

    let second =
        crate::gateway::listener::GatewayLifecycle::bind(state.clone(), loopback_ephemeral())
            .await
            .unwrap();
    assert!(
        !usage_loop_started(&state),
        "a later bind still must not start the usage worker"
    );
    crate::gateway::stop_gateway(second);

    drop(state);
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn spawn_usage_sync_loop_starts_once_per_core_state() {
    let (dir, state) = test_state("once-loop");
    assert!(!usage_loop_started(&state));
    spawn_usage_sync_loop(state.clone());
    assert!(usage_loop_started(&state));
    spawn_usage_sync_loop(state.clone());
    spawn_usage_sync_loop(state.clone());
    ControlPlaneWorkers::ensure_started(state.clone());
    ControlPlaneWorkers::ensure_started(state.clone());
    assert!(
        usage_loop_started(&state),
        "repeat spawn_usage_sync_loop / ensure_started calls must keep the once-per-CoreState flag"
    );

    let first =
        crate::gateway::listener::GatewayLifecycle::bind(state.clone(), loopback_ephemeral())
            .await
            .unwrap();
    let second =
        crate::gateway::listener::GatewayLifecycle::bind(state.clone(), loopback_ephemeral())
            .await
            .unwrap();
    assert!(
        usage_loop_started(&state),
        "extra listener bind on the same CoreState must not reset the usage loop"
    );
    crate::gateway::stop_gateway(first);
    crate::gateway::stop_gateway(second);
    assert!(
        usage_loop_started(&state),
        "listener stop must not clear the process-level usage worker"
    );
    let _ = state.config();

    let rebound =
        crate::gateway::listener::GatewayLifecycle::bind(state.clone(), loopback_ephemeral())
            .await
            .unwrap();
    assert!(
        usage_loop_started(&state),
        "CoreState must remain usable for another bind after stop"
    );
    crate::gateway::stop_gateway(rebound);

    drop(state);
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn start_gateway_on_starts_usage_loop_and_stop_does_not_clear_it() {
    let (dir, state) = test_state("compat-start-loop");
    assert!(!usage_loop_started(&state));
    let handle = crate::gateway::start_gateway_on(state.clone(), loopback_ephemeral())
        .await
        .unwrap();
    assert!(
        usage_loop_started(&state),
        "public start_gateway_on must still start the process-level usage worker"
    );
    crate::gateway::stop_gateway(handle);
    assert!(
        usage_loop_started(&state),
        "stop_gateway is listener-only and must not clear the usage worker"
    );
    let _ = state.config();

    let restarted = crate::gateway::start_gateway_on(state.clone(), loopback_ephemeral())
        .await
        .unwrap();
    assert!(
        usage_loop_started(&state),
        "CoreState must remain usable for another public start after stop"
    );
    crate::gateway::stop_gateway(restarted);

    drop(state);
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn rebind_does_not_start_usage_loop() {
    let (dir, state) = test_state("rebind-no-loop");
    assert!(!usage_loop_started(&state));

    let first =
        crate::gateway::listener::GatewayLifecycle::bind(state.clone(), loopback_ephemeral())
            .await
            .unwrap();
    let first_port = first.port;
    *state.gateway.lock() = Some(first);
    assert!(
        !usage_loop_started(&state),
        "storing a bound listener must not start the usage worker"
    );

    let same = crate::gateway::listener::GatewayLifecycle::rebind(
        state.clone(),
        std::net::SocketAddr::from(([127, 0, 0, 1], first_port)),
    )
    .await
    .unwrap();
    assert_eq!(same, first_port);
    assert!(
        !usage_loop_started(&state),
        "same-port rebind must not start the process-level usage worker"
    );

    let moved =
        crate::gateway::listener::GatewayLifecycle::rebind(state.clone(), loopback_ephemeral())
            .await
            .unwrap();
    assert_ne!(moved, first_port);
    assert!(
        !usage_loop_started(&state),
        "new-port rebind must not start the process-level usage worker"
    );

    let handle = state.gateway.lock().take();
    if let Some(handle) = handle {
        crate::gateway::listener::GatewayLifecycle::stop_and_wait(handle).await;
    }
    assert!(
        !usage_loop_started(&state),
        "stop_and_wait must not start the usage worker"
    );

    drop(state);
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn rebind_keeps_started_usage_loop_without_duplicate() {
    let (dir, state) = test_state("rebind-keep-loop");
    assert!(!usage_loop_started(&state));
    let handle = crate::gateway::start_gateway_on(state.clone(), loopback_ephemeral())
        .await
        .unwrap();
    let first_port = handle.port;
    *state.gateway.lock() = Some(handle);
    assert!(
        usage_loop_started(&state),
        "public start_gateway_on must start the process-level usage worker"
    );
    ControlPlaneWorkers::ensure_started(state.clone());
    ControlPlaneWorkers::ensure_started(state.clone());
    assert!(
        usage_loop_started(&state),
        "ensure_started after start must stay once-per-CoreState"
    );

    let same = crate::gateway::listener::GatewayLifecycle::rebind(
        state.clone(),
        std::net::SocketAddr::from(([127, 0, 0, 1], first_port)),
    )
    .await
    .unwrap();
    assert_eq!(same, first_port);
    assert!(
        usage_loop_started(&state),
        "same-port rebind must not clear the process-level usage worker"
    );
    ControlPlaneWorkers::ensure_started(state.clone());
    assert!(
        usage_loop_started(&state),
        "ensure_started after same-port rebind must not spawn a second worker"
    );

    let moved =
        crate::gateway::listener::GatewayLifecycle::rebind(state.clone(), loopback_ephemeral())
            .await
            .unwrap();
    assert_ne!(moved, first_port);
    assert!(
        usage_loop_started(&state),
        "new-port rebind must not clear or restart the process-level usage worker"
    );
    ControlPlaneWorkers::ensure_started(state.clone());
    crate::gateway::stop_gateway(state.gateway.lock().take().unwrap());
    assert!(
        usage_loop_started(&state),
        "listener stop after rebind must not clear the usage worker"
    );

    drop(state);
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn settings_port_rebind_keeps_started_usage_loop() {
    let (dir, state) = test_state("settings-rebind-loop");
    let handle = crate::gateway::start_gateway_on(state.clone(), loopback_ephemeral())
        .await
        .unwrap();
    let first_port = handle.port;
    *state.gateway.lock() = Some(handle);
    assert!(
        usage_loop_started(&state),
        "public start_gateway_on must start the process-level usage worker"
    );

    state
        .rebind_gateway_listener_if_port_changed(first_port, 0, true)
        .await
        .expect("settings-shaped rebind should bind an ephemeral port");
    assert_ne!(state.active_gateway_port(), first_port);
    assert!(
        usage_loop_started(&state),
        "HTTP settings Gateway rebind must not terminate the process-level usage worker"
    );
    ControlPlaneWorkers::ensure_started(state.clone());
    assert!(
        usage_loop_started(&state),
        "ensure_started after settings rebind must not spawn a second worker"
    );

    crate::gateway::stop_gateway(state.gateway.lock().take().unwrap());
    assert!(
        usage_loop_started(&state),
        "listener stop after settings rebind must not clear the usage worker"
    );

    drop(state);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn failure_backoff_ladder_caps_at_six_hours() {
    assert_eq!(failure_backoff(1), Duration::minutes(5));
    assert_eq!(failure_backoff(2), Duration::minutes(15));
    assert_eq!(failure_backoff(3), Duration::hours(1));
    assert_eq!(failure_backoff(4), Duration::hours(6));
    assert_eq!(failure_backoff(99), Duration::hours(6));
}

#[test]
fn active_and_inactive_cadence() {
    assert_eq!(cadence_for(true), ACTIVE_CADENCE);
    assert_eq!(cadence_for(false), INACTIVE_CADENCE);
}

#[test]
fn manual_throttle_exposes_next_allowed() {
    let now = fixed("2026-08-18T12:00:00Z");
    assert_eq!(manual_next_allowed_at(None, now), None);
    assert_eq!(
        manual_next_allowed_at(Some(now - Duration::seconds(16)), now),
        None
    );
    assert_eq!(
        manual_next_allowed_at(Some(now - Duration::seconds(10)), now),
        Some(now + Duration::seconds(5))
    );
}

#[test]
fn success_next_respects_cadence_and_earliest_reset() {
    let now = fixed("2026-08-18T12:00:00Z");
    let next = compute_next_after_success(now, true, 10, 0.0);
    assert_eq!(next, now + Duration::minutes(10));
    // Reset farther than hourly cadence → cadence wins for active accounts.
    let next = compute_next_after_success(now, true, 500, 0.0);
    assert_eq!(next, now + ACTIVE_CADENCE);
    // Reset farther than daily cadence → cadence wins for inactive accounts.
    let next = compute_next_after_success(now, false, 60 * 30, 0.0);
    assert_eq!(next, now + INACTIVE_CADENCE);
    // Reset sooner than daily cadence still schedules around the reset.
    let next = compute_next_after_success(now, false, 500, 0.0);
    assert_eq!(next, now + Duration::minutes(500));
    // A stale official reset must not create a near-immediate polling loop.
    let next = compute_next_after_success(now, true, 0, 1.0);
    assert_eq!(next, now + ACTIVE_CADENCE);
    let next = compute_next_after_success(now, false, -1, 1.0);
    assert_eq!(next, now + INACTIVE_CADENCE);
}

#[test]
fn expedite_guard_is_fifteen_minutes() {
    let now = fixed("2026-08-18T12:00:00Z");
    assert!(!should_run_expedited(79.9, None, None, None, now));
    assert!(should_run_expedited(80.0, None, None, None, now));
    assert!(should_run_expedited(95.0, None, None, None, now));
    assert!(!should_run_expedited(
        95.0,
        Some(now - Duration::minutes(14)),
        None,
        None,
        now
    ));
    assert!(should_run_expedited(
        95.0,
        Some(now - Duration::minutes(15)),
        None,
        None,
        now
    ));
    // Any recent official attempt/success also anchors the 15m guard.
    assert!(!should_run_expedited(
        95.0,
        None,
        Some(now - Duration::minutes(5)),
        None,
        now
    ));
    assert!(!should_run_expedited(
        95.0,
        None,
        None,
        Some(now - Duration::minutes(1)),
        now
    ));
}

#[test]
fn auto_sync_excludes_disabled_non_ready_empty_key() {
    let provider = crate::provider::OPENCODE_PROVIDER_ID;
    assert!(!provider_account_is_auto_sync_candidate(
        provider, false, true, true
    ));
    assert!(!provider_account_is_auto_sync_candidate(
        provider, true, false, true
    ));
    assert!(!provider_account_is_auto_sync_candidate(
        provider, true, true, false
    ));
    assert!(provider_account_is_auto_sync_candidate(
        provider, true, true, true
    ));
    assert!(!provider_account_is_auto_sync_candidate(
        crate::provider::COMMAND_CODE_PROVIDER_ID,
        true,
        true,
        true,
    ));
    assert!(!provider_account_is_auto_sync_candidate(
        crate::provider::OPENCODE_ZEN_FREE_PROVIDER_ID,
        true,
        true,
        false,
    ));
}

fn temp_dir(label: &str) -> PathBuf {
    let dir =
        std::env::temp_dir().join(format!("ocg-usage-sync-{}-{}", label, uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn test_state(label: &str) -> (PathBuf, CoreState) {
    let dir = temp_dir(label);
    let cipher: Arc<dyn KeyCipher + Send + Sync> =
        Arc::new(StaticKeyCipher::new("usage-sync-test"));
    let db = Database::open(dir.clone()).unwrap();
    let state = Arc::new(CoreStateInner::new(db, dir.clone(), cipher).unwrap());
    (dir, state)
}

fn ready_account(state: &CoreState, id: &str, key: &str) -> Account {
    Account {
        id: id.to_string(),
        provider_id: crate::provider::default_provider_id(),

        credential_kind: crate::provider::default_credential_kind(),
        quota_scope: crate::provider::default_quota_scope(),
        name: id.to_string(),
        username: None,
        password_cipher: None,
        key_cipher: state.encrypt_key(key).unwrap(),
        enabled: true,
        account_type: AccountType::Key,
        setup_step: AccountSetupStep::Ready,
        referral_code: None,
        purchase_date: "2026-08-01".to_string(),
        expires_on: "2026-09-01".to_string(),
        cooldown_until: None,
        cooldown_generic_until: None,
        cooldown_5h_until: None,
        cooldown_week_until: None,
        cooldown_month_until: None,
        cooldown_free_until: None,
        last_error: None,
        auth_error: None,
        notes: None,
        created_at: Utc::now(),
        updated_at: Utc::now(),
    }
}

fn sample_snapshot() -> GoUsageSnapshot {
    GoUsageSnapshot {
        rolling_status: crate::go_usage::GoUsageWindowStatus::RateLimited,
        weekly_status: crate::go_usage::GoUsageWindowStatus::Ok,
        monthly_status: crate::go_usage::GoUsageWindowStatus::Ok,
        rolling_percent: 50.0,
        weekly_percent: 20.0,
        monthly_percent: 10.0,
        rolling_resets_in_minutes: 180,
        weekly_resets_in_minutes: 1_440,
        monthly_resets_in_minutes: 43200,
        earliest_resets_in_minutes: 180,
    }
}

#[tokio::test]
async fn manual_throttle_and_dedupe_share_one_upstream_call() {
    let (dir, state) = test_state("throttle-dedupe");
    let account = ready_account(&state, "acc-1", "sk-acc-1");
    state.db.lock().create_account(&account).unwrap();

    let now = fixed("2026-08-18T12:00:00Z");
    state.usage_sync.set_clock_for_test(move || now);
    state.usage_sync.set_jitter_for_test(|| 0.0);
    let calls = Arc::new(AtomicUsize::new(0));
    let calls_fetch = calls.clone();
    let release = Arc::new(tokio::sync::Notify::new());
    let entered = Arc::new(tokio::sync::Notify::new());
    let release_fetch = release.clone();
    let entered_fetch = entered.clone();
    state.usage_sync.set_fetch_for_test(move |_cfg, _key| {
        calls_fetch.fetch_add(1, AtomicOrdering::SeqCst);
        let release_fetch = release_fetch.clone();
        let entered_fetch = entered_fetch.clone();
        let snapshot = sample_snapshot();
        Box::pin(async move {
            entered_fetch.notify_one();
            release_fetch.notified().await;
            Ok(snapshot)
        })
    });

    let state_a = state.clone();
    let state_b = state.clone();
    let a = tokio::spawn(async move {
        refresh_official_usage(&state_a, "acc-1", UsageSyncTrigger::Manual).await
    });
    entered.notified().await;
    let b = tokio::spawn(async move {
        refresh_official_usage(&state_b, "acc-1", UsageSyncTrigger::Manual).await
    });
    // Let the second caller attach to the in-flight future before release.
    tokio::time::sleep(StdDuration::from_millis(20)).await;
    release.notify_waiters();
    let ra = a.await.unwrap().unwrap();
    let rb = b.await.unwrap().unwrap();
    assert_eq!(ra.last_success_at, rb.last_success_at);
    assert_eq!(calls.load(AtomicOrdering::SeqCst), 1);
    let success_events: Vec<_> = state
        .db
        .lock()
        .list_gateway_logs(20)
        .unwrap()
        .into_iter()
        .filter(|log| {
            log.message
                .starts_with("event=official_usage_refresh_succeeded account_id=acc-1")
        })
        .collect();
    assert_eq!(success_events.len(), 1, "deduped refresh logs once");
    assert!(!success_events[0].message.contains("sk-acc-1"));

    let throttled = refresh_official_usage(&state, "acc-1", UsageSyncTrigger::Manual).await;
    match throttled {
        Err(OfficialUsageRefreshError::Throttled {
            retry_after_secs, ..
        }) => assert!(retry_after_secs <= 60),
        other => panic!("expected throttle, got {other:?}"),
    }

    state.usage_sync.clear_test_seams();
    drop(state);
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn stale_guarded_leader_blocks_success_for_all_deduped_waiters_without_mutation() {
    let state = FakeUsageHost::new();
    state.insert_ready_go("guarded", "sk-guarded");
    let before = state.inner.sync.lock().get("guarded").cloned().unwrap();
    let authorization = state.guarded_authorization();
    let calls = Arc::new(AtomicUsize::new(0));
    let entered_fetch = Arc::new(Notify::new());
    let release_fetch = Arc::new(Notify::new());
    let calls_hook = calls.clone();
    let entered_fetch_hook = entered_fetch.clone();
    let release_fetch_hook = release_fetch.clone();
    state.inner.runtime.set_fetch_for_test(move |_cfg, _key| {
        calls_hook.fetch_add(1, AtomicOrdering::SeqCst);
        let entered_fetch = entered_fetch_hook.clone();
        let release_fetch = release_fetch_hook.clone();
        Box::pin(async move {
            entered_fetch.notify_one();
            release_fetch.notified().await;
            Ok(sample_snapshot())
        })
    });

    let cleanup_arrived = Arc::new(Notify::new());
    let cleanup_release = Arc::new(Notify::new());
    let cleanup_arrived_hook = cleanup_arrived.clone();
    let cleanup_release_hook = cleanup_release.clone();
    state
        .inner
        .runtime
        .set_before_inflight_cleanup_for_test(move || {
            let cleanup_arrived = cleanup_arrived_hook.clone();
            let cleanup_release = cleanup_release_hook.clone();
            Box::pin(async move {
                cleanup_arrived.notify_one();
                cleanup_release.notified().await;
            })
        });

    let guarded_state = state.clone();
    let guarded = tokio::spawn(async move {
        refresh_official_usage_with_authorization(
            &guarded_state,
            "guarded",
            UsageSyncTrigger::Manual,
            authorization,
        )
        .await
        .result
    });
    entered_fetch.notified().await;
    state.bump_settings_revision();
    release_fetch.notify_one();
    cleanup_arrived.notified().await;

    // While the stale V3-owned result remains in the in-flight map, an
    // unconditional V2 waiter must observe the same rejected result rather
    // than converting that stale leader into an authorized writer.
    let v2_state = state.clone();
    let v2_follower = tokio::spawn(async move {
        refresh_official_usage(&v2_state, "guarded", UsageSyncTrigger::Manual).await
    });
    cleanup_arrived.notified().await;
    cleanup_release.notify_waiters();

    for result in [guarded.await.unwrap(), v2_follower.await.unwrap()] {
        assert!(
            matches!(
                result,
                Err(OfficialUsageRefreshError::CommitAuthorizationRejected)
            ),
            "stale guarded leader must reject every waiter: {result:?}"
        );
    }
    assert_eq!(calls.load(AtomicOrdering::SeqCst), 1);
    assert_eq!(
        state.inner.sync.lock().get("guarded").cloned().unwrap(),
        before,
        "stale success must not write calibration sync metadata"
    );
}

#[tokio::test]
async fn stale_guarded_failure_does_not_write_attempt_or_backoff_metadata() {
    let state = FakeUsageHost::new();
    state.insert_ready_go("failure", "sk-failure");
    let before = state.inner.sync.lock().get("failure").cloned().unwrap();
    let authorization = state.guarded_authorization();
    let entered = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let entered_hook = entered.clone();
    let release_hook = release.clone();
    state.inner.runtime.set_fetch_for_test(move |_cfg, _key| {
        let entered = entered_hook.clone();
        let release = release_hook.clone();
        Box::pin(async move {
            entered.notify_one();
            release.notified().await;
            Err(GoUsageError::Timeout)
        })
    });

    let refresh_state = state.clone();
    let refresh = tokio::spawn(async move {
        refresh_official_usage_with_authorization(
            &refresh_state,
            "failure",
            UsageSyncTrigger::Manual,
            authorization,
        )
        .await
        .result
    });
    entered.notified().await;
    state.bump_settings_revision();
    release.notify_one();

    let result = refresh.await.unwrap();
    assert!(
        matches!(
            result,
            Err(OfficialUsageRefreshError::CommitAuthorizationRejected)
        ),
        "stale upstream failure must surface authorization conflict: {result:?}"
    );
    assert_eq!(
        state.inner.sync.lock().get("failure").cloned().unwrap(),
        before,
        "stale failure must not write last_attempt/failure_streak/backoff"
    );
}

#[tokio::test]
async fn stale_guarded_follower_cannot_veto_background_owned_inflight_result() {
    let state = FakeUsageHost::new();
    state.insert_ready_go("background", "sk-background");
    let stale_follower_authorization = state.guarded_authorization();
    state
        .inner
        .runtime
        .set_fetch_for_test(move |_cfg, _key| Box::pin(async { Ok(sample_snapshot()) }));

    let cleanup_arrived = Arc::new(Notify::new());
    let cleanup_release = Arc::new(Notify::new());
    let cleanup_arrived_hook = cleanup_arrived.clone();
    let cleanup_release_hook = cleanup_release.clone();
    state
        .inner
        .runtime
        .set_before_inflight_cleanup_for_test(move || {
            let cleanup_arrived = cleanup_arrived_hook.clone();
            let cleanup_release = cleanup_release_hook.clone();
            Box::pin(async move {
                cleanup_arrived.notify_one();
                cleanup_release.notified().await;
            })
        });

    let background_state = state.clone();
    let background = tokio::spawn(async move {
        refresh_official_usage(&background_state, "background", UsageSyncTrigger::Scheduled).await
    });
    cleanup_arrived.notified().await;
    state.bump_settings_revision();

    // The background leader has already committed under unconditional V2
    // semantics but remains in-flight until cleanup. A stale guarded
    // follower may observe that result; its unrelated token cannot veto it.
    let follower_state = state.clone();
    let follower = tokio::spawn(async move {
        refresh_official_usage_with_authorization(
            &follower_state,
            "background",
            UsageSyncTrigger::Scheduled,
            stale_follower_authorization,
        )
        .await
        .result
    });
    cleanup_arrived.notified().await;
    cleanup_release.notify_waiters();

    background.await.unwrap().unwrap();
    follower.await.unwrap().unwrap();
    let sync = state.inner.sync.lock().get("background").cloned().unwrap();
    assert!(sync.last_success_at.is_some());
    assert_eq!(sync.failure_streak, 0);
}

#[tokio::test]
async fn failure_preserves_last_success_and_calibration() {
    let (dir, state) = test_state("failure-preserve");
    let account = ready_account(&state, "acc-2", "sk-acc-2");
    state.db.lock().create_account(&account).unwrap();
    let now = fixed("2026-08-18T12:00:00Z");
    state.usage_sync.set_clock_for_test(move || now);
    state.usage_sync.set_jitter_for_test(|| 0.0);
    state.usage_sync.set_fetch_for_test(move |_cfg, _key| {
        let snapshot = sample_snapshot();
        Box::pin(async move { Ok(snapshot) })
    });
    refresh_official_usage(&state, "acc-2", UsageSyncTrigger::Manual)
        .await
        .unwrap();
    let before = state
        .db
        .lock()
        .account_usage_with_limits("acc-2", &state.pricing_snapshot().limits)
        .unwrap();
    let success_at = state
        .db
        .lock()
        .account_usage_sync_state("acc-2")
        .unwrap()
        .unwrap()
        .last_success_at;

    // Advance clock beyond manual throttle.
    let later = now + Duration::minutes(2);
    state.usage_sync.set_clock_for_test(move || later);
    state
        .usage_sync
        .set_fetch_for_test(move |_cfg, _key| Box::pin(async move { Err(GoUsageError::Timeout) }));
    let err = refresh_official_usage(&state, "acc-2", UsageSyncTrigger::Manual)
        .await
        .unwrap_err();
    assert!(matches!(
        err,
        OfficialUsageRefreshError::Upstream(GoUsageError::Timeout)
    ));

    let after = state
        .db
        .lock()
        .account_usage_with_limits("acc-2", &state.pricing_snapshot().limits)
        .unwrap();
    assert_eq!(after.window_5h, before.window_5h);
    assert_eq!(after.window_week, before.window_week);
    assert_eq!(after.window_month, before.window_month);
    let sync = state
        .db
        .lock()
        .account_usage_sync_state("acc-2")
        .unwrap()
        .unwrap();
    assert_eq!(sync.last_success_at, success_at);
    assert_eq!(sync.failure_streak, 1);
    assert_eq!(sync.next_eligible_at, Some(later + failure_backoff(1)));
    let failure_events: Vec<_> = state
            .db
            .lock()
            .list_gateway_logs(20)
            .unwrap()
            .into_iter()
            .filter(|log| {
                log.message
                    == "event=official_usage_refresh_failed account_id=acc-2 trigger=manual reason=upstream_timeout"
            })
            .collect();
    assert_eq!(failure_events.len(), 1);
    assert!(!failure_events[0].message.contains("sk-acc-2"));

    drop(state);
    std::fs::remove_dir_all(dir).unwrap();
}

#[tokio::test]
async fn key_cas_leaves_windows_unchanged_when_account_changes() {
    let (dir, state) = test_state("cas");
    let account = ready_account(&state, "acc-3", "sk-acc-3");
    state.db.lock().create_account(&account).unwrap();
    let limits = state.pricing_snapshot().limits.clone();
    {
        let db = state.db.lock();
        db.calibrate_account_usage_snapshot(
            "acc-3",
            &AccountUsageCalibrationSnapshot {
                rolling_percent: 11.0,
                weekly_percent: 22.0,
                monthly_percent: 33.0,
                rolling_resets_in_minutes: 100,
                weekly_resets_in_minutes: 200,
            },
            &limits,
        )
        .unwrap();
    }
    let before = state
        .db
        .lock()
        .account_usage_with_limits("acc-3", &limits)
        .unwrap();

    let sync_before = state
        .db
        .lock()
        .account_usage_sync_state("acc-3")
        .unwrap()
        .unwrap();
    let now = fixed("2026-08-18T12:00:00Z");
    state.usage_sync.set_clock_for_test(move || now);
    state.usage_sync.set_jitter_for_test(|| 0.0);
    let state_for_fetch = state.clone();
    state.usage_sync.set_fetch_for_test(move |_cfg, _key| {
        // Swap the key while the network call is "in flight".
        let rotated = state_for_fetch.encrypt_key("sk-rotated").unwrap();
        state_for_fetch
            .db
            .lock()
            .update_account(
                "acc-3",
                &crate::models::AccountUpdate {
                    name: None,
                    username: None,
                    password: None,
                    key: Some("sk-rotated".to_string()),
                    enabled: None,
                    referral_code: None,
                    purchase_date: None,
                    notes: None,
                },
                Some(&rotated),
                None,
            )
            .unwrap();
        let snapshot = sample_snapshot();
        Box::pin(async move { Ok(snapshot) })
    });

    let err = refresh_official_usage(&state, "acc-3", UsageSyncTrigger::Manual)
        .await
        .unwrap_err();
    assert!(matches!(err, OfficialUsageRefreshError::Conflict(_)));
    let after = state
        .db
        .lock()
        .account_usage_with_limits("acc-3", &limits)
        .unwrap();
    assert_eq!(after.window_5h, before.window_5h);
    assert_eq!(after.window_week, before.window_week);
    assert_eq!(after.window_month, before.window_month);
    let sync = state
        .db
        .lock()
        .account_usage_sync_state("acc-3")
        .unwrap()
        .unwrap();
    assert_eq!(
        sync, sync_before,
        "old-key results must not throttle the replacement"
    );

    state.usage_sync.clear_test_seams();
    drop(state);
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn official_rate_limited_status_does_not_write_cooldown() {
    let (dir, state) = test_state("official-rate-limited");
    let account = ready_account(&state, "acc-4", "sk-acc-4");
    state.db.lock().create_account(&account).unwrap();
    let now = fixed("2026-08-18T12:00:00Z");
    state.usage_sync.set_clock_for_test(move || now);
    state.usage_sync.set_jitter_for_test(|| 0.0);
    state.usage_sync.set_fetch_for_test(move |_cfg, _key| {
        let snapshot = sample_snapshot();
        Box::pin(async move { Ok(snapshot) })
    });
    refresh_official_usage(&state, "acc-4", UsageSyncTrigger::Scheduled)
        .await
        .unwrap();
    let stored = state.db.lock().get_account("acc-4").unwrap().unwrap();
    assert!(stored.cooldown_until.is_none());
    assert!(stored.cooldown_5h_until.is_none());
    assert!(stored.cooldown_week_until.is_none());
    assert!(stored.cooldown_month_until.is_none());
    assert!(stored.cooldown_generic_until.is_none());
    assert!(stored.cooldown_free_until.is_none());

    drop(state);
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn sync_metadata_survives_reopen() {
    let dir = temp_dir("persist");
    let cipher: Arc<dyn KeyCipher + Send + Sync> =
        Arc::new(StaticKeyCipher::new("usage-sync-persist"));
    let now = fixed("2026-08-18T12:00:00Z");
    {
        let db = Database::open(dir.clone()).unwrap();
        let state = Arc::new(CoreStateInner::new(db, dir.clone(), cipher.clone()).unwrap());
        let account = ready_account(&state, "persist", "sk-persist");
        state.db.lock().create_account(&account).unwrap();
        state
            .db
            .lock()
            .record_account_usage_sync_success("persist", now, now + ACTIVE_CADENCE, true)
            .unwrap();
        drop(state);
    }
    {
        let db = Database::open(dir.clone()).unwrap();
        let sync = db.account_usage_sync_state("persist").unwrap().unwrap();
        assert_eq!(sync.last_success_at, Some(now));
        assert_eq!(sync.next_eligible_at, Some(now + ACTIVE_CADENCE));
        assert_eq!(sync.failure_streak, 0);
        assert_eq!(sync.last_expedited_at, Some(now));
    }
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn eligibility_lists_active_hourly_vs_inactive_daily_and_exclusions() {
    let dir = temp_dir("eligibility");
    let cipher: Arc<dyn KeyCipher + Send + Sync> =
        Arc::new(StaticKeyCipher::new("usage-sync-elig"));
    let db = Database::open(dir.clone()).unwrap();
    let state = Arc::new(CoreStateInner::new(db, dir.clone(), cipher).unwrap());
    let now = fixed("2026-08-18T12:00:00Z");

    let active = ready_account(&state, "active", "sk-active");
    let inactive = ready_account(&state, "inactive", "sk-inactive");
    let mut disabled = ready_account(&state, "disabled", "sk-disabled");
    disabled.enabled = false;
    let mut pending = ready_account(&state, "pending", "sk-pending");
    pending.setup_step = AccountSetupStep::Payment;
    pending.enabled = false;
    let mut empty = ready_account(&state, "empty", "sk-empty");
    empty.key_cipher.clear();

    {
        let db = state.db.lock();
        db.create_account(&active).unwrap();
        db.create_account(&inactive).unwrap();
        db.create_account(&disabled).unwrap();
        db.create_account(&pending).unwrap();
        db.create_account(&empty).unwrap();
        // Seed due times in the past so both ready accounts are refreshable.
        db.record_account_usage_sync_success(
            "active",
            now - Duration::hours(2),
            now - Duration::minutes(1),
            false,
        )
        .unwrap();
        db.record_account_usage_sync_success(
            "inactive",
            now - Duration::hours(30),
            now - Duration::minutes(1),
            false,
        )
        .unwrap();
        db.log_forward(&crate::models::ForwardLog {
            id: 0,
            timestamp: now - Duration::hours(1),
            model: "mimo-v2.5".into(),
            account_id: "active".into(),
            account_name: "active".into(),
            route_account_id: None,
            provider_id: None,

            credential_account_id: None,
            client_key_id: None,
            client_key_name: None,
            status: "success".into(),
            http_status: Some(200),
            route: String::new(),
            prompt_tokens: 0,
            completion_tokens: 0,
            cached_tokens: 0,
            cache_creation_tokens: 0,
            cost: Some(1.0),
            raw_cost_usd: None,
            quota_debit: None,
            effective_paid_cost_usd: None,
            pricing_revision_id: None,
            quota_multiplier: None,
            local_adjustment_multiplier: None,
            service_tier: None,
            cost_state: "priced".into(),
            error_message: None,
            request_id: None,
            attempt: None,
            error_source: None,
            error_stage: None,
            duration_ms: None,
            diagnostic: None,
        })
        .unwrap();
    }

    let limits = state.pricing_snapshot().limits.clone();
    let candidates = list_auto_candidates(&state, now, &limits).unwrap();
    let ids: Vec<_> = candidates.iter().map(|c| c.account_id.as_str()).collect();
    assert!(ids.contains(&"active"));
    assert!(ids.contains(&"inactive"));
    assert!(!ids.contains(&"disabled"));
    assert!(!ids.contains(&"pending"));
    assert!(!ids.contains(&"empty"));

    let active_next = compute_next_after_success(now, true, 10_000, 0.0);
    let inactive_next = compute_next_after_success(now, false, 10_000, 0.0);
    assert_eq!(active_next, now + ACTIVE_CADENCE);
    assert_eq!(inactive_next, now + INACTIVE_CADENCE);

    drop(state);
    std::fs::remove_dir_all(dir).unwrap();
}

fn seed_high_usage(state: &CoreState, account_id: &str) {
    let limits = state.pricing_snapshot().limits.clone();
    state
        .db
        .lock()
        .calibrate_account_usage_snapshot(
            account_id,
            &AccountUsageCalibrationSnapshot {
                rolling_percent: 90.0,
                weekly_percent: 20.0,
                monthly_percent: 10.0,
                rolling_resets_in_minutes: 100,
                weekly_resets_in_minutes: 1_000,
            },
            &limits,
        )
        .unwrap();
}

#[tokio::test]
async fn failed_expedited_sync_stays_in_backoff_across_scheduler_scans() {
    let (dir, state) = test_state("expedite-backoff");
    let account = ready_account(&state, "hi", "sk-hi");
    state.db.lock().create_account(&account).unwrap();
    let now = fixed("2026-08-18T12:00:00Z");
    state.usage_sync.set_clock_for_test(move || now);
    state.usage_sync.set_jitter_for_test(|| 0.0);
    seed_high_usage(&state, "hi");
    state
        .db
        .lock()
        .record_account_usage_sync_success(
            "hi",
            now - Duration::hours(2),
            now + Duration::hours(20),
            false,
        )
        .unwrap();

    state
        .usage_sync
        .set_fetch_for_test(move |_cfg, _key| Box::pin(async move { Err(GoUsageError::Timeout) }));
    // First scan pulls expedite and fails into 5m backoff.
    let limits = state.pricing_snapshot().limits.clone();
    let candidates = list_auto_candidates(&state, now, &limits).unwrap();
    assert!(candidates.iter().any(|c| {
        c.account_id == "hi"
            && matches!(
                c.action,
                CandidateAction::Refresh {
                    trigger: UsageSyncTrigger::Expedited
                }
            )
    }));
    let _ = refresh_official_usage(&state, "hi", UsageSyncTrigger::Expedited).await;
    let sync = state
        .db
        .lock()
        .account_usage_sync_state("hi")
        .unwrap()
        .unwrap();
    assert_eq!(sync.failure_streak, 1);
    assert_eq!(sync.next_eligible_at, Some(now + failure_backoff(1)));

    // ~30s later the scheduler must not re-select despite still-high usage.
    let soon = now + Duration::seconds(30);
    let candidates = list_auto_candidates(&state, soon, &limits).unwrap();
    assert!(!candidates.iter().any(|c| c.account_id == "hi"));

    // Repeated failure advances the ladder.
    let after_backoff = now + failure_backoff(1);
    state.usage_sync.set_clock_for_test(move || after_backoff);
    let _ = refresh_official_usage(&state, "hi", UsageSyncTrigger::Scheduled).await;
    let sync = state
        .db
        .lock()
        .account_usage_sync_state("hi")
        .unwrap()
        .unwrap();
    assert_eq!(sync.failure_streak, 2);
    assert_eq!(
        sync.next_eligible_at,
        Some(after_backoff + failure_backoff(2))
    );

    state.usage_sync.clear_test_seams();
    drop(state);
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn successful_high_usage_retry_is_not_immediately_re_expedited() {
    let (dir, state) = test_state("no-reexpedite");
    let account = ready_account(&state, "hi2", "sk-hi2");
    state.db.lock().create_account(&account).unwrap();
    // Usage-window reads use the production clock, so keep this integration
    // test's injected sync clock aligned instead of crossing a real reset.
    let now = Utc::now();
    state.usage_sync.set_clock_for_test(move || now);
    state.usage_sync.set_jitter_for_test(|| 0.0);
    seed_high_usage(&state, "hi2");
    // Prior success is old; a recent failure left the account due.
    state
        .db
        .lock()
        .record_account_usage_sync_success(
            "hi2",
            now - Duration::hours(2),
            now + Duration::hours(20),
            false,
        )
        .unwrap();
    state
        .db
        .lock()
        .record_account_usage_sync_failure(
            "hi2",
            now - Duration::minutes(6),
            1,
            now - Duration::minutes(1),
        )
        .unwrap();

    let calls = Arc::new(AtomicUsize::new(0));
    let calls_fetch = calls.clone();
    state.usage_sync.set_fetch_for_test(move |_cfg, _key| {
        calls_fetch.fetch_add(1, AtomicOrdering::SeqCst);
        let mut snapshot = sample_snapshot();
        snapshot.rolling_percent = 90.0;
        Box::pin(async move { Ok(snapshot) })
    });
    refresh_official_usage(&state, "hi2", UsageSyncTrigger::Scheduled)
        .await
        .unwrap();
    assert_eq!(calls.load(AtomicOrdering::SeqCst), 1);

    let limits = state.pricing_snapshot().limits.clone();
    let soon = now + Duration::minutes(1);
    let candidates = list_auto_candidates(&state, soon, &limits).unwrap();
    assert!(
        !candidates.iter().any(|c| c.account_id == "hi2"),
        "successful retry at high usage must not be re-expedited inside 15m"
    );

    // After the 15m guard, high usage may expedite even though cadence/reset
    // next_eligible is still in the future (sample earliest reset is 180m).
    let later = now + EXPEDITE_GUARD;
    let candidates = list_auto_candidates(&state, later, &limits).unwrap();
    assert!(candidates.iter().any(|c| {
        c.account_id == "hi2"
            && matches!(
                c.action,
                CandidateAction::Refresh {
                    trigger: UsageSyncTrigger::Expedited
                }
            )
    }));

    state.usage_sync.clear_test_seams();
    drop(state);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn take_inflight_if_generation_ignores_stale_waiters() {
    let mut map = HashMap::new();
    let success = OfficialUsageRefreshSuccess {
        usage: UsageWindow {
            account_id: "a".into(),
            window_5h: 0.0,
            window_week: 0.0,
            window_month: 0.0,
            resets_in_5h: None,
            resets_in_week: None,
            resets_in_month: None,
        },
        source: "official_go_usage",
        last_success_at: fixed("2026-08-18T12:00:00Z").to_rfc3339(),
        next_allowed_at: fixed("2026-08-18T12:01:00Z").to_rfc3339(),
    };
    let finished = async move { Arc::new(Ok(success) as RefreshResult) }
        .boxed()
        .shared();
    map.insert(
        "a".into(),
        InflightEntry {
            generation: 1,
            future: finished.clone(),
            authorization: UsageSyncCommitAuthorization::Unconditional,
        },
    );
    assert!(take_inflight_if_generation(&mut map, "a", 1));
    map.insert(
        "a".into(),
        InflightEntry {
            generation: 2,
            future: finished,
            authorization: UsageSyncCommitAuthorization::Unconditional,
        },
    );
    assert!(!take_inflight_if_generation(&mut map, "a", 1));
    assert_eq!(map.get("a").map(|e| e.generation), Some(2));
    assert!(take_inflight_if_generation(&mut map, "a", 2));
    assert!(map.is_empty());
}

#[tokio::test]
async fn stale_waiter_does_not_drop_newer_inflight_generation() {
    let (dir, state) = test_state("inflight-gen");
    let account = ready_account(&state, "gen", "sk-gen");
    state.db.lock().create_account(&account).unwrap();
    let now = fixed("2026-08-18T12:00:00Z");
    state.usage_sync.set_clock_for_test(move || now);
    state.usage_sync.set_jitter_for_test(|| 0.0);

    let calls = Arc::new(AtomicUsize::new(0));
    let calls_fetch = calls.clone();
    let release_f1 = Arc::new(Notify::new());
    let entered_f1 = Arc::new(Notify::new());
    let release_f2 = Arc::new(Notify::new());
    let entered_f2 = Arc::new(Notify::new());
    let release_f1_fetch = release_f1.clone();
    let entered_f1_fetch = entered_f1.clone();
    let release_f2_fetch = release_f2.clone();
    let entered_f2_fetch = entered_f2.clone();
    state.usage_sync.set_fetch_for_test(move |_cfg, _key| {
        let n = calls_fetch.fetch_add(1, AtomicOrdering::SeqCst);
        let snapshot = sample_snapshot();
        if n == 0 {
            let release = release_f1_fetch.clone();
            let entered = entered_f1_fetch.clone();
            Box::pin(async move {
                entered.notify_waiters();
                release.notified().await;
                Ok(snapshot)
            })
        } else {
            let release = release_f2_fetch.clone();
            let entered = entered_f2_fetch.clone();
            Box::pin(async move {
                entered.notify_waiters();
                release.notified().await;
                Ok(snapshot)
            })
        }
    });

    let ticket = Arc::new(AtomicUsize::new(0));
    let hold_first = Arc::new(Notify::new());
    let hold_first_hook = hold_first.clone();
    let second_entered = Arc::new(Notify::new());
    let second_entered_hook = second_entered.clone();
    state
        .usage_sync
        .set_before_inflight_cleanup_for_test(move || {
            let ticket = ticket.clone();
            let hold_first = hold_first_hook.clone();
            let second_entered = second_entered_hook.clone();
            Box::pin(async move {
                let which = ticket.fetch_add(1, AtomicOrdering::SeqCst);
                if which == 0 {
                    hold_first.notified().await;
                } else {
                    second_entered.notify_one();
                }
            })
        });

    let state_w1 = state.clone();
    let state_w2 = state.clone();
    let w1 = tokio::spawn(async move {
        refresh_official_usage(&state_w1, "gen", UsageSyncTrigger::Manual).await
    });
    entered_f1.notified().await;
    let w2 = tokio::spawn(async move {
        refresh_official_usage(&state_w2, "gen", UsageSyncTrigger::Manual).await
    });
    tokio::time::sleep(StdDuration::from_millis(20)).await;
    release_f1.notify_waiters();
    // W2 reaches cleanup first (ticket 1) while W1 is held.
    second_entered.notified().await;

    // Start F2 while W1 still holds the stale cleanup.
    let later = now + Duration::minutes(2);
    state.usage_sync.set_clock_for_test(move || later);
    let state_w3 = state.clone();
    let w3 = tokio::spawn(async move {
        refresh_official_usage(&state_w3, "gen", UsageSyncTrigger::Manual).await
    });
    entered_f2.notified().await;
    assert_eq!(calls.load(AtomicOrdering::SeqCst), 2);

    // Stale W1 cleanup must not delete F2.
    hold_first.notify_one();
    let state_w4 = state.clone();
    let w4 = tokio::spawn(async move {
        refresh_official_usage(&state_w4, "gen", UsageSyncTrigger::Manual).await
    });
    tokio::time::sleep(StdDuration::from_millis(20)).await;
    assert_eq!(
        calls.load(AtomicOrdering::SeqCst),
        2,
        "F2 must remain deduped after stale waiter cleanup"
    );
    release_f2.notify_waiters();
    w1.await.unwrap().unwrap();
    w2.await.unwrap().unwrap();
    w3.await.unwrap().unwrap();
    w4.await.unwrap().unwrap();
    assert_eq!(calls.load(AtomicOrdering::SeqCst), 2);

    state.usage_sync.clear_test_seams();
    drop(state);
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn decrypt_internal_failure_records_backoff_not_busy_loop() {
    let (dir, state) = test_state("decrypt-fail");
    let mut account = ready_account(&state, "bad", "sk-bad");
    // Store ciphertext the StaticKeyCipher cannot decrypt.
    account.key_cipher = "not-a-valid-cipher".into();
    state.db.lock().create_account(&account).unwrap();
    let now = fixed("2026-08-18T12:00:00Z");
    state.usage_sync.set_clock_for_test(move || now);
    state.usage_sync.set_jitter_for_test(|| 0.0);
    state
        .db
        .lock()
        .record_account_usage_sync_success(
            "bad",
            now - Duration::hours(2),
            now - Duration::minutes(1),
            false,
        )
        .unwrap();

    let err = refresh_official_usage(&state, "bad", UsageSyncTrigger::Scheduled)
        .await
        .unwrap_err();
    assert!(matches!(err, OfficialUsageRefreshError::Internal(_)));
    let sync = state
        .db
        .lock()
        .account_usage_sync_state("bad")
        .unwrap()
        .unwrap();
    assert_eq!(sync.failure_streak, 1);
    assert_eq!(sync.next_eligible_at, Some(now + failure_backoff(1)));
    assert!(sync.last_success_at.is_some());

    let limits = state.pricing_snapshot().limits.clone();
    let soon = now + Duration::seconds(30);
    let candidates = list_auto_candidates(&state, soon, &limits).unwrap();
    assert!(!candidates.iter().any(|c| c.account_id == "bad"));

    state.usage_sync.clear_test_seams();
    drop(state);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn inactive_to_active_pulls_hourly_without_overriding_failure_backoff() {
    let (dir, state) = test_state("inactive-active");
    let account = ready_account(&state, "wake", "sk-wake");
    state.db.lock().create_account(&account).unwrap();
    let now = fixed("2026-08-18T12:00:00Z");
    let last_success = now - Duration::hours(2);
    state
        .db
        .lock()
        .record_account_usage_sync_success("wake", last_success, now + Duration::hours(20), false)
        .unwrap();
    state
        .db
        .lock()
        .log_forward(&crate::models::ForwardLog {
            id: 0,
            timestamp: now - Duration::minutes(10),
            model: "mimo-v2.5".into(),
            account_id: "wake".into(),
            account_name: "wake".into(),
            route_account_id: None,
            provider_id: None,

            credential_account_id: None,
            client_key_id: None,
            client_key_name: None,
            status: "success".into(),
            http_status: Some(200),
            route: String::new(),
            prompt_tokens: 0,
            completion_tokens: 0,
            cached_tokens: 0,
            cache_creation_tokens: 0,
            cost: Some(1.0),
            raw_cost_usd: None,
            quota_debit: None,
            effective_paid_cost_usd: None,
            pricing_revision_id: None,
            quota_multiplier: None,
            local_adjustment_multiplier: None,
            service_tier: None,
            cost_state: "priced".into(),
            error_message: None,
            request_id: None,
            attempt: None,
            error_source: None,
            error_stage: None,
            duration_ms: None,
            diagnostic: None,
        })
        .unwrap();

    let limits = state.pricing_snapshot().limits.clone();
    let candidates = list_auto_candidates(&state, now, &limits).unwrap();
    assert!(candidates.iter().any(|c| {
        c.account_id == "wake"
            && matches!(
                c.action,
                CandidateAction::Refresh {
                    trigger: UsageSyncTrigger::Scheduled
                }
            )
    }));

    // Same transition must not override an active failure backoff floor.
    state
        .db
        .lock()
        .record_account_usage_sync_failure("wake", now, 1, now + failure_backoff(1))
        .unwrap();
    let candidates = list_auto_candidates(&state, now + Duration::seconds(30), &limits).unwrap();
    assert!(!candidates.iter().any(|c| c.account_id == "wake"));
    let sync = state
        .db
        .lock()
        .account_usage_sync_state("wake")
        .unwrap()
        .unwrap();
    assert_eq!(sync.next_eligible_at, Some(now + failure_backoff(1)));

    drop(state);
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn goat_key_windows_are_independent_and_usage_failure_is_fail_soft() {
    let (dir, state) = test_state("goat-independent");
    let mut first = ready_account(&state, "goat-a", "goat-key-a");
    first.provider_id = COMMAND_CODE_PROVIDER_ID.to_string();
    first.enabled = false;
    let mut second = ready_account(&state, "goat-b", "goat-key-b");
    second.provider_id = COMMAND_CODE_PROVIDER_ID.to_string();
    second.enabled = false;
    {
        let db = state.db.lock();
        db.create_account(&first).unwrap();
        db.create_account(&second).unwrap();
    }
    let now = fixed("2026-08-18T12:00:00Z");
    for (account_id, used) in [("goat-a", 1.0), ("goat-b", 9.0)] {
        for kind in [
            QUOTA_WINDOW_FIVE_HOURS,
            QUOTA_WINDOW_WEEK,
            QUOTA_WINDOW_MONTH,
        ] {
            state
                .db
                .lock()
                .upsert_quota_window(&QuotaWindow {
                    account_id: account_id.to_string(),
                    window_kind: kind.to_string(),
                    used,
                    // The official GOAT contract is not verified, so the
                    // test exercises key scoping without inventing limits,
                    // units, or reset semantics.
                    limit_value: None,
                    started_at: None,
                    resets_at: None,
                    calibration_offset: 0.0,
                    unit: "unknown".to_string(),
                    source: "test-fixture".to_string(),
                    observed_at: Some(now),
                    updated_at: now,
                })
                .unwrap();
        }
        for (balance_kind, amount) in [("purchased", used * 10.0), ("free", used)] {
            state
                .db
                .lock()
                .upsert_credit_balance(&CreditBalance {
                    account_id: account_id.to_string(),
                    balance_kind: balance_kind.to_string(),
                    amount,
                    unit: "unknown".to_string(),
                    source: "test-fixture".to_string(),
                    observed_at: Some(now),
                    updated_at: now,
                })
                .unwrap();
        }
    }

    let first_windows = state.db.lock().list_quota_windows("goat-a").unwrap();
    let second_windows = state.db.lock().list_quota_windows("goat-b").unwrap();
    assert_eq!(first_windows.len(), 3);
    assert_eq!(second_windows.len(), 3);
    assert!(first_windows.iter().all(|window| window.used == 1.0));
    assert!(second_windows.iter().all(|window| window.used == 9.0));
    let first_balances = state.db.lock().list_credit_balances("goat-a").unwrap();
    let second_balances = state.db.lock().list_credit_balances("goat-b").unwrap();
    assert_eq!(first_balances[0].amount + first_balances[1].amount, 11.0);
    assert_eq!(second_balances[0].amount + second_balances[1].amount, 99.0);

    let fetches = Arc::new(AtomicUsize::new(0));
    let fetches_seen = fetches.clone();
    state.usage_sync.set_fetch_for_test(move |_cfg, _key| {
        fetches_seen.fetch_add(1, AtomicOrdering::SeqCst);
        Box::pin(async { Err(GoUsageError::Network) })
    });
    let error = refresh_official_usage(&state, "goat-a", UsageSyncTrigger::Manual)
        .await
        .unwrap_err();
    assert!(matches!(error, OfficialUsageRefreshError::NotEligible(_)));
    assert_eq!(fetches.load(AtomicOrdering::SeqCst), 0);
    let sync = state
        .db
        .lock()
        .account_usage_sync_state("goat-a")
        .unwrap()
        .unwrap();
    assert_eq!(sync.failure_streak, 0);
    assert_eq!(sync.last_attempt_at, None);
    assert_eq!(sync.next_eligible_at, None);

    assert_eq!(
        state
            .db
            .lock()
            .account_usage_sync_state("goat-a")
            .unwrap()
            .unwrap()
            .next_eligible_at,
        None,
        "experimental usage must stay independent from inference 429 handling"
    );
    let limits = state.pricing_snapshot().limits.clone();
    let candidates = list_auto_candidates(&state, now, &limits).unwrap();
    assert!(
        !candidates
            .iter()
            .any(|candidate| candidate.account_id.starts_with("goat-"))
    );

    let zen_windows = state
        .db
        .lock()
        .list_quota_windows(ZEN_FREE_ACCOUNT_ID)
        .unwrap();
    assert_eq!(zen_windows.len(), 1);
    assert_eq!(
        zen_windows[0].window_kind,
        crate::provider::QUOTA_WINDOW_FREE
    );
    assert_eq!(zen_windows[0].limit_value, None);

    state.usage_sync.clear_test_seams();
    drop(state);
    let _ = std::fs::remove_dir_all(dir);
}

struct FakeUsageInner {
    runtime: UsageSyncRuntime,
    settings_update: ParkingMutex<()>,
    settings_revision: AtomicU64,
    process_generation: u64,
    accounts: ParkingMutex<HashMap<String, Account>>,
    sync: ParkingMutex<HashMap<String, ProviderUsageSyncState>>,
    decrypts: ParkingMutex<HashMap<String, String>>,
}

#[derive(Clone)]
struct FakeUsageHost {
    inner: Arc<FakeUsageInner>,
}

impl FakeUsageHost {
    fn new() -> Self {
        Self {
            inner: Arc::new(FakeUsageInner {
                runtime: UsageSyncRuntime::new(),
                settings_update: ParkingMutex::new(()),
                settings_revision: AtomicU64::new(1),
                process_generation: 7,
                accounts: ParkingMutex::new(HashMap::new()),
                sync: ParkingMutex::new(HashMap::new()),
                decrypts: ParkingMutex::new(HashMap::new()),
            }),
        }
    }

    fn insert_ready_go(&self, id: &str, key: &str) {
        let account = Account {
            id: id.to_string(),
            provider_id: crate::provider::default_provider_id(),

            credential_kind: crate::provider::default_credential_kind(),
            quota_scope: crate::provider::default_quota_scope(),
            name: id.to_string(),
            username: None,
            password_cipher: None,
            key_cipher: format!("cipher:{key}"),
            enabled: true,
            account_type: AccountType::Key,
            setup_step: AccountSetupStep::Ready,
            referral_code: None,
            purchase_date: "2026-08-01".to_string(),
            expires_on: "2026-09-01".to_string(),
            cooldown_until: None,
            cooldown_generic_until: None,
            cooldown_5h_until: None,
            cooldown_week_until: None,
            cooldown_month_until: None,
            cooldown_free_until: None,
            last_error: None,
            auth_error: None,
            notes: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        };
        self.inner
            .decrypts
            .lock()
            .insert(account.key_cipher.clone(), key.to_string());
        self.inner.accounts.lock().insert(id.to_string(), account);
        self.inner.sync.lock().insert(
            id.to_string(),
            ProviderUsageSyncState {
                account_id: id.to_string(),
                last_success_at: None,
                last_attempt_at: None,
                next_eligible_at: None,
                failure_streak: 0,
                last_expedited_at: None,
            },
        );
    }

    fn settings_revision(&self) -> u64 {
        self.inner.settings_revision.load(AtomicOrdering::Acquire)
    }

    fn bump_settings_revision(&self) -> u64 {
        self.inner
            .settings_revision
            .fetch_add(1, AtomicOrdering::AcqRel)
            + 1
    }

    fn guarded_authorization(&self) -> UsageSyncCommitAuthorization {
        UsageSyncCommitAuthorization::control_revision(
            self.settings_revision(),
            self.inner.process_generation,
        )
    }
}

impl UsageSyncStore for FakeUsageInner {
    fn list_accounts(&self) -> anyhow::Result<Vec<Account>> {
        Ok(self.accounts.lock().values().cloned().collect())
    }
    fn get_account(&self, account_id: &str) -> anyhow::Result<Option<Account>> {
        Ok(self.accounts.lock().get(account_id).cloned())
    }
    fn account_usage_sync_state(
        &self,
        account_id: &str,
    ) -> anyhow::Result<Option<ProviderUsageSyncState>> {
        Ok(self.sync.lock().get(account_id).cloned())
    }
    fn pull_account_usage_sync_next_eligible(
        &self,
        account_id: &str,
        proposal: DateTime<Utc>,
        respect_failure_backoff: bool,
    ) -> anyhow::Result<()> {
        let mut sync = self.sync.lock();
        let Some(current) = sync.get_mut(account_id) else {
            return Ok(());
        };
        if respect_failure_backoff && current.failure_streak > 0 {
            return Ok(());
        }
        current.next_eligible_at = Some(match current.next_eligible_at {
            Some(existing) => existing.min(proposal),
            None => proposal,
        });
        Ok(())
    }
    fn account_has_local_activity_since(
        &self,
        _account_id: &str,
        _since: DateTime<Utc>,
    ) -> anyhow::Result<bool> {
        Ok(false)
    }
    fn account_usage_with_limits(
        &self,
        account_id: &str,
        _limits: &PricingLimits,
    ) -> anyhow::Result<UsageWindow> {
        Ok(UsageWindow {
            account_id: account_id.to_string(),
            window_5h: 0.0,
            window_week: 0.0,
            window_month: 0.0,
            resets_in_5h: None,
            resets_in_week: None,
            resets_in_month: None,
        })
    }
    fn commit_official_usage_sync_success(
        &self,
        account_id: &str,
        _expected_key_cipher: &str,
        _snapshot: &GoUsageSnapshot,
        _limits: &PricingLimits,
        metadata: OfficialUsageSyncSuccessMetadata,
    ) -> anyhow::Result<Option<UsageWindow>> {
        if let Some(current) = self.sync.lock().get_mut(account_id) {
            current.last_success_at = Some(metadata.now);
            current.last_attempt_at = Some(metadata.now);
            current.next_eligible_at = Some(metadata.next_eligible_at);
            current.failure_streak = 0;
            if metadata.mark_expedited {
                current.last_expedited_at = Some(metadata.now);
            }
        }
        Ok(Some(UsageWindow {
            account_id: account_id.to_string(),
            window_5h: 0.0,
            window_week: 0.0,
            window_month: 0.0,
            resets_in_5h: None,
            resets_in_week: None,
            resets_in_month: None,
        }))
    }
    fn record_account_usage_sync_failure(
        &self,
        account_id: &str,
        now: DateTime<Utc>,
        failure_streak: i64,
        next_eligible_at: DateTime<Utc>,
    ) -> anyhow::Result<()> {
        if let Some(current) = self.sync.lock().get_mut(account_id) {
            current.last_attempt_at = Some(now);
            current.failure_streak = failure_streak;
            current.next_eligible_at = Some(next_eligible_at);
        }
        Ok(())
    }
    fn log_gateway(&self, _level: &str, _category: &str, _message: &str) -> anyhow::Result<()> {
        Ok(())
    }
}

impl UsageSyncHost for FakeUsageHost {
    type Weak = std::sync::Weak<FakeUsageInner>;
    type Store = FakeUsageInner;

    fn downgrade(&self) -> Self::Weak {
        Arc::downgrade(&self.inner)
    }
    fn upgrade(weak: &Self::Weak) -> Option<Self> {
        weak.upgrade().map(|inner| Self { inner })
    }
    fn usage_runtime(&self) -> &UsageSyncRuntime {
        &self.inner.runtime
    }
    fn pricing_limits(&self) -> PricingLimits {
        crate::kernel::pricing::SEED_LIMITS
    }
    fn config(&self) -> AppConfig {
        AppConfig::default()
    }
    fn decrypt_account_key(&self, ciphertext: &str) -> anyhow::Result<String> {
        self.inner
            .decrypts
            .lock()
            .get(ciphertext)
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("missing test key"))
    }
    fn with_sync_store<F, R>(&self, f: F) -> R
    where
        F: FnOnce(&Self::Store) -> R,
    {
        f(&self.inner)
    }

    fn with_authorized_sync_store<F, R>(
        &self,
        authorization: &UsageSyncCommitAuthorization,
        f: F,
    ) -> Result<R, UsageSyncCommitAuthorizationRejected>
    where
        F: FnOnce(&Self::Store) -> R,
    {
        match authorization {
            UsageSyncCommitAuthorization::Unconditional => Ok(self.with_sync_store(f)),
            UsageSyncCommitAuthorization::ControlRevision {
                expected_revision,
                process_generation,
            } => {
                let _settings_update = self.inner.settings_update.lock();
                if *expected_revision != self.settings_revision()
                    || *process_generation != self.inner.process_generation
                {
                    return Err(UsageSyncCommitAuthorizationRejected);
                }
                Ok(f(&self.inner))
            }
        }
    }
}

#[tokio::test]
async fn usage_sync_host_loop_exits_when_the_host_is_dropped() {
    let host = FakeUsageHost::new();
    spawn_usage_sync_loop(host.clone());
    assert!(
        host.usage_runtime()
            .loop_started
            .load(AtomicOrdering::Acquire)
    );
    spawn_usage_sync_loop(host.clone());
    let weak = host.downgrade();
    drop(host);
    tokio::time::sleep(StdDuration::from_millis(20)).await;
    assert!(
        FakeUsageHost::upgrade(&weak).is_none(),
        "dropping the last host handle must end the scheduler lifetime"
    );
}

#[tokio::test]
async fn authoritative_usage_replaces_exhaustion_but_preserves_temporary_cooldown() {
    let (dir, state) = test_state("official-quota-evidence");
    let mut account = ready_account(&state, "official-quota", "test-key");
    let now = Utc::now();
    account.cooldown_generic_until = Some(now + Duration::seconds(120));
    account.cooldown_until = account.cooldown_generic_until;
    state.db.lock().create_account(&account).unwrap();
    state
        .usage_sync
        .set_fetch_for_test(|_, _| Box::pin(async { Ok(sample_snapshot()) }));
    refresh_official_usage(&state, &account.id, UsageSyncTrigger::Scheduled)
        .await
        .unwrap();
    let before = crate::db::quota_recovery::load_for_legacy_on(&state.db.lock().conn, &account.id)
        .unwrap()
        .unwrap()
        .3
        .unwrap();
    assert_eq!(before.windows.len(), 1);
    state
        .usage_sync
        .set_fetch_for_test(|_, _| Box::pin(async { Err(GoUsageError::Timeout) }));
    assert!(
        refresh_official_usage(&state, &account.id, UsageSyncTrigger::Scheduled)
            .await
            .is_err()
    );
    let after_failure =
        crate::db::quota_recovery::load_for_legacy_on(&state.db.lock().conn, &account.id)
            .unwrap()
            .unwrap()
            .3
            .unwrap();
    assert_eq!(
        serde_json::to_value(&before).unwrap(),
        serde_json::to_value(&after_failure).unwrap()
    );
    state.usage_sync.set_fetch_for_test(|_, _| {
        Box::pin(async {
            let mut snapshot = sample_snapshot();
            snapshot.rolling_status = crate::go_usage::GoUsageWindowStatus::Ok;
            Ok(snapshot)
        })
    });
    refresh_official_usage(&state, &account.id, UsageSyncTrigger::Scheduled)
        .await
        .unwrap();
    assert!(
        crate::db::quota_recovery::load_for_legacy_on(&state.db.lock().conn, &account.id)
            .unwrap()
            .unwrap()
            .3
            .is_none()
    );
    assert_eq!(
        state
            .db
            .lock()
            .get_account(&account.id)
            .unwrap()
            .unwrap()
            .cooldown_generic_until,
        account.cooldown_generic_until
    );
    state.usage_sync.clear_test_seams();
    drop(state);
    std::fs::remove_dir_all(dir).unwrap();
}

#[tokio::test]
async fn changed_credential_version_with_same_cipher_rejects_usage_write() {
    let (dir, state) = test_state("official-version-guard");
    let account = ready_account(&state, "version-guard", "same-key");
    state.db.lock().create_account(&account).unwrap();
    let before = state
        .db
        .lock()
        .account_usage_sync_state(&account.id)
        .unwrap();
    let mutate = state.clone();
    state.usage_sync.set_fetch_for_test(move |_, _| {
        let mutate = mutate.clone();
        Box::pin(async move {
            mutate.db.lock().conn.execute("UPDATE credentials SET credential_version = credential_version + 1 WHERE legacy_account_id = 'version-guard'", []).unwrap();
            Ok(sample_snapshot())
        })
    });
    assert!(matches!(
        refresh_official_usage(&state, &account.id, UsageSyncTrigger::Inference429).await,
        Err(OfficialUsageRefreshError::Conflict(_))
    ));
    assert!(
        crate::db::quota_recovery::load_for_legacy_on(&state.db.lock().conn, &account.id)
            .unwrap()
            .unwrap()
            .3
            .is_none()
    );
    assert_eq!(
        state
            .db
            .lock()
            .account_usage_sync_state(&account.id)
            .unwrap(),
        before
    );
    state.usage_sync.clear_test_seams();
    drop(state);
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn first_reactive_attempt_creates_throttle_metadata_without_a_success() {
    let (dir, state) = test_state("reactive-attempt-row");
    let account = ready_account(&state, "first-reactive", "test-key");
    state.db.lock().create_account(&account).unwrap();
    let now = Utc::now();
    {
        let db = state.db.lock();
        db.conn
            .execute(
                "DELETE FROM provider_usage_sync_state WHERE account_id = ?1",
                [&account.id],
            )
            .unwrap();
        db.touch_account_usage_sync_attempt(&account.id, now)
            .unwrap();
        let row = db.account_usage_sync_state(&account.id).unwrap().unwrap();
        assert_eq!(
            manual_next_allowed_at(row.last_attempt_at, now),
            Some(now + MANUAL_THROTTLE)
        );
        assert!(row.last_success_at.is_none());
    }
    drop(state);
    std::fs::remove_dir_all(dir).unwrap();
}
