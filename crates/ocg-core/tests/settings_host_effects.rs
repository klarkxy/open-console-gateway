//! Host settings-effects extraction: shared CoreState path, hook rollback,
//! V2/V3 adapter mapping, and unchanged production host SCC.

use ocg_core::crypto::{KeyCipher, StaticKeyCipher};
use ocg_core::dashboard_v3::{
    ERROR_INTERNAL, ERROR_INVALID_REQUEST, ERROR_MISSING_EXPECTED_REVISION, ERROR_REVISION_CONFLICT,
};
use ocg_core::db::Database;
use ocg_core::state::{CoreStateInner, DockVisibilitySync, HostSettingsError};
use reqwest::StatusCode;
use serde_json::{Value, json};
use std::cell::{Cell, RefCell};
use std::fs;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

#[path = "fixtures/dashboard_v3/harness.rs"]
mod harness;

use harness::{V3Harness, start_loopback};

fn production_prefix(source: &str) -> &str {
    source.split("#[cfg(test)]").next().unwrap_or(source)
}

fn temp_data_dir(label: &str) -> PathBuf {
    let mut dir = std::env::temp_dir();
    dir.push(format!(
        "ocg-host-effects-{}-{}",
        label,
        uuid::Uuid::new_v4()
    ));
    fs::create_dir_all(&dir).unwrap();
    dir
}

fn new_state(label: &str) -> (Arc<CoreStateInner>, PathBuf) {
    let dir = temp_data_dir(label);
    let db = Database::open(dir.clone()).unwrap();
    let cipher: Arc<dyn KeyCipher + Send + Sync> = Arc::new(StaticKeyCipher::new("host-effects"));
    (
        Arc::new(CoreStateInner::new(db, dir.clone(), cipher).unwrap()),
        dir,
    )
}

fn persisted_auto_start(state: &CoreStateInner) -> bool {
    let stored = state.db.lock().get_setting("config").unwrap().unwrap();
    serde_json::from_str::<Value>(&stored).unwrap()["auto_start"]
        .as_bool()
        .unwrap()
}

fn persisted_show_dock_icon(state: &CoreStateInner) -> bool {
    let stored = state.db.lock().get_setting("config").unwrap().unwrap();
    serde_json::from_str::<Value>(&stored).unwrap()["show_dock_icon"]
        .as_bool()
        .unwrap()
}

thread_local! {
    static AUTO_START_CALLS: RefCell<Vec<bool>> = const { RefCell::new(Vec::new()) };
    static AUTO_START_FAIL_AT: Cell<Option<usize>> = const { Cell::new(None) };
}

fn reset_auto_start_hook() {
    AUTO_START_CALLS.with(|calls| calls.borrow_mut().clear());
    AUTO_START_FAIL_AT.with(|fail| fail.set(None));
}

fn auto_start_calls() -> Vec<bool> {
    AUTO_START_CALLS.with(|calls| calls.borrow().clone())
}

fn recording_auto_start(enabled: bool) -> ocg_core::Result<()> {
    let index = AUTO_START_CALLS.with(|calls| {
        let mut calls = calls.borrow_mut();
        calls.push(enabled);
        calls.len() - 1
    });
    if AUTO_START_FAIL_AT.with(|fail| fail.get() == Some(index)) {
        anyhow::bail!("auto-start hook failed");
    }
    Ok(())
}

fn always_fail_auto_start(_enabled: bool) -> ocg_core::Result<()> {
    anyhow::bail!("auto-start hook failed")
}

fn fail_enable_auto_start(enabled: bool) -> ocg_core::Result<()> {
    if enabled {
        anyhow::bail!("auto-start hook failed");
    }
    Ok(())
}

fn ok_auto_start(_enabled: bool) -> ocg_core::Result<()> {
    Ok(())
}

struct DockHook {
    calls: Mutex<Vec<bool>>,
    fail_at: Mutex<Option<usize>>,
}

impl DockHook {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            calls: Mutex::new(Vec::new()),
            fail_at: Mutex::new(None),
        })
    }

    fn fail_at(self: &Arc<Self>, index: usize) {
        *self.fail_at.lock().unwrap() = Some(index);
    }

    fn calls(&self) -> Vec<bool> {
        self.calls.lock().unwrap().clone()
    }

    fn sync(self: &Arc<Self>) -> DockVisibilitySync {
        let hook = Arc::clone(self);
        Arc::new(move |visible| {
            let index = {
                let mut calls = hook.calls.lock().unwrap();
                calls.push(visible);
                calls.len() - 1
            };
            if *hook.fail_at.lock().unwrap() == Some(index) {
                anyhow::bail!("dock hook failed");
            }
            Ok(())
        })
    }
}

fn cas_patch(harness: &V3Harness, patch: Value) -> Value {
    let mut body = match patch {
        Value::Object(map) => map,
        _ => serde_json::Map::new(),
    };
    body.insert(
        "expectedRevision".into(),
        json!(harness.state.settings_revision()),
    );
    body.insert(
        "processGeneration".into(),
        json!(harness.state.process_generation()),
    );
    Value::Object(body)
}

fn v2_payload(config: &ocg_core::models::AppConfig, expected_revision: Option<u64>) -> Value {
    let mut payload = serde_json::to_value(config).unwrap();
    if let Some(revision) = expected_revision {
        payload["expected_revision"] = json!(revision);
    }
    payload
}

#[test]
fn v2_and_v3_settings_updates_share_the_core_state_host_path() {
    let state_src = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/state.rs"));
    let v2_src = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/dashboard.rs"));
    let v3_src = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/src/dashboard_v3/settings.rs"
    ));
    let lib_src = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/lib.rs"));
    let state_production = production_prefix(state_src);
    // dashboard.rs has #[cfg(test)] helpers before update_settings; inspect
    // the full file so the shared call site is not skipped.
    let v2_production = v2_src;
    let v3_production = production_prefix(v3_src);
    let lib_production = production_prefix(lib_src);

    assert!(state_production.contains("pub fn apply_host_settings("));
    assert!(state_production.contains("pub enum HostSettingsError"));
    assert!(state_production.contains("failed to synchronize desktop settings:"));
    assert_eq!(
        state_production.matches("self.set_config(next)").count(),
        1,
        "success path must persist next exactly once"
    );
    assert!(
        state_production.contains(
            "if auto_start_supported {\n                self.sync_auto_start(next_auto_start)?;"
        ),
        "supported auto-start must be reasserted after every successful save"
    );
    assert!(
        state_production.contains("if dock_visibility_supported {\n                self.sync_dock_visibility(next_show_dock_icon)?;"),
        "supported Dock visibility must be reasserted after every successful save"
    );
    assert!(
        !state_production.contains("auto_start_changed"),
        "supported auto-start invoke must not be gated on a changed flag"
    );
    assert!(
        !state_production.contains("dock_changed"),
        "supported Dock invoke must not be gated on a changed flag"
    );

    assert!(v2_production.contains("apply_host_settings"));
    assert!(v3_production.contains("apply_host_settings"));
    assert!(v2_production.contains("map_host_settings_error"));
    assert!(v3_production.contains("map_host_settings_error"));

    for adapter in [v2_production, v3_production] {
        assert!(
            !adapter.contains("failed to synchronize desktop settings"),
            "adapters must not keep the host rollback policy"
        );
        assert!(
            !adapter.contains("failed to restore auto-start state"),
            "adapters must not keep auto-start restore policy"
        );
        assert!(
            !adapter.contains("failed to restore Dock visibility"),
            "adapters must not keep Dock restore policy"
        );
        assert!(
            !adapter.contains("sync_auto_start("),
            "adapters must not call auto-start hooks directly"
        );
        assert!(
            !adapter.contains("sync_dock_visibility("),
            "adapters must not call Dock hooks directly"
        );
        assert!(
            !adapter.contains("auto-start is unavailable in this runtime"),
            "unsupported messages must live on HostSettingsError"
        );
        assert!(
            !adapter.contains("Dock visibility is unavailable in this runtime"),
            "unsupported messages must live on HostSettingsError"
        );
    }

    assert!(!lib_production.contains("mod host_settings"));
    assert!(!lib_production.contains("mod settings_host"));
    assert!(!lib_production.contains("mod settings_effects"));
}

#[test]
fn production_host_scc_membership_is_unchanged() {
    let kernel = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/kernel/mod.rs"));
    let start = kernel
        .find("const EXPECTED_HOST_SCC:")
        .expect("host SCC whitelist should remain in kernel/mod.rs");
    let block = &kernel[start..];
    let end = block.find(';').expect("EXPECTED_HOST_SCC should end");
    let members: Vec<&str> = block[..end]
        .lines()
        .filter_map(|line| {
            let trimmed = line.trim().trim_end_matches(',');
            trimmed.strip_prefix('"')?.strip_suffix('"')
        })
        .collect();
    assert_eq!(
        members,
        [
            "dashboard",
            "dashboard_v3",
            "gateway",
            "protocol_probe",
            "state"
        ]
    );
}

#[test]
fn successful_host_saves_reassert_every_supported_hook() {
    reset_auto_start_hook();
    let (state, dir) = new_state("host-success");
    state.set_auto_start_sync(recording_auto_start);
    let dock = DockHook::new();
    state.set_dock_visibility_sync(dock.sync());
    let previous = state.config();
    let before = state.settings_revision();

    let mut timeout_only = previous.clone();
    timeout_only.connect_timeout_secs = 12;
    state
        .apply_host_settings(&previous, timeout_only)
        .expect("unrelated settings must persist and reassert host hooks");
    assert_eq!(state.settings_revision(), before + 1);
    assert_eq!(state.config().connect_timeout_secs, 12);
    assert!(!state.config().auto_start);
    assert!(state.config().show_dock_icon);
    assert_eq!(auto_start_calls(), vec![false]);
    assert_eq!(dock.calls(), vec![true]);

    let after_timeout = state.config();
    let mut auto_only = after_timeout.clone();
    auto_only.auto_start = true;
    state
        .apply_host_settings(&after_timeout, auto_only)
        .expect("auto-start change should sync and reassert Dock");
    assert!(state.config().auto_start);
    assert_eq!(auto_start_calls(), vec![false, true]);
    assert_eq!(dock.calls(), vec![true, true]);

    let after_auto = state.config();
    let mut dock_only = after_auto.clone();
    dock_only.show_dock_icon = false;
    state
        .apply_host_settings(&after_auto, dock_only)
        .expect("Dock change should sync and reassert auto-start");
    assert!(!state.config().show_dock_icon);
    assert_eq!(auto_start_calls(), vec![false, true, true]);
    assert_eq!(dock.calls(), vec![true, true, false]);
    assert_eq!(state.settings_revision(), before + 3);

    drop(state);
    let _ = fs::remove_dir_all(dir);
}

#[test]
fn unchanged_supported_hook_failure_rolls_back_config_and_restores_both_hooks() {
    reset_auto_start_hook();
    AUTO_START_FAIL_AT.with(|fail| fail.set(Some(0)));
    let (state, dir) = new_state("host-unchanged-fail");
    state.set_auto_start_sync(recording_auto_start);
    let dock = DockHook::new();
    state.set_dock_visibility_sync(dock.sync());
    let previous = state.config();
    let before = state.settings_revision();
    let mut timeout_only = previous.clone();
    timeout_only.connect_timeout_secs = 12;

    let error = state
        .apply_host_settings(&previous, timeout_only)
        .unwrap_err();
    match error {
        HostSettingsError::Sync(message) => {
            assert_eq!(
                message,
                "failed to synchronize desktop settings: auto-start hook failed"
            );
        }
        other => panic!("expected Sync, got {other:?}"),
    }
    assert_eq!(
        state.config().connect_timeout_secs,
        previous.connect_timeout_secs
    );
    assert!(!state.config().auto_start);
    assert!(state.config().show_dock_icon);
    assert!(!persisted_auto_start(&state));
    assert!(persisted_show_dock_icon(&state));
    assert_eq!(state.settings_revision(), before + 2);
    assert_eq!(auto_start_calls(), vec![false, false]);
    assert_eq!(
        dock.calls(),
        vec![true],
        "unchanged auto-start failure must skip the forward Dock sync and still restore Dock"
    );

    drop(state);
    let _ = fs::remove_dir_all(dir);
}

#[test]
fn unsupported_host_deltas_fail_before_persistence_or_revision_change() {
    let (state, dir) = new_state("host-unsupported");
    let previous = state.config();
    let before = state.settings_revision();
    let primary = previous.gateway_key.clone();

    let mut auto = previous.clone();
    auto.auto_start = true;
    let error = state.apply_host_settings(&previous, auto).unwrap_err();
    assert!(matches!(error, HostSettingsError::AutoStartUnsupported));
    assert_eq!(error.to_string(), HostSettingsError::AUTO_START_UNAVAILABLE);
    assert_eq!(state.settings_revision(), before);
    assert!(!state.config().auto_start);
    assert!(!persisted_auto_start(&state));
    assert_eq!(state.config().gateway_key, primary);

    let mut dock = previous.clone();
    dock.show_dock_icon = false;
    let error = state.apply_host_settings(&previous, dock).unwrap_err();
    assert!(matches!(
        error,
        HostSettingsError::DockVisibilityUnsupported
    ));
    assert_eq!(
        error.to_string(),
        HostSettingsError::DOCK_VISIBILITY_UNAVAILABLE
    );
    assert_eq!(state.settings_revision(), before);
    assert!(state.config().show_dock_icon);
    assert!(persisted_show_dock_icon(&state));

    let mut timeout_only = previous.clone();
    timeout_only.connect_timeout_secs = 12;
    state
        .apply_host_settings(&previous, timeout_only)
        .expect("unsupported runtimes must still persist unchanged host fields");
    assert_eq!(state.settings_revision(), before + 1);
    assert_eq!(state.config().connect_timeout_secs, 12);
    assert!(!state.config().auto_start);
    assert!(state.config().show_dock_icon);

    drop(state);
    let _ = fs::remove_dir_all(dir);
}

#[test]
fn persist_failure_does_not_call_hooks_or_change_revision() {
    reset_auto_start_hook();
    let (state, dir) = new_state("host-persist-fail");
    state.set_auto_start_sync(recording_auto_start);
    let dock = DockHook::new();
    state.set_dock_visibility_sync(dock.sync());
    let previous = state.config();
    let before = state.settings_revision();
    let mut invalid = previous.clone();
    invalid.connect_timeout_secs = 0;
    invalid.auto_start = true;
    invalid.show_dock_icon = false;

    let error = state.apply_host_settings(&previous, invalid).unwrap_err();
    assert!(matches!(error, HostSettingsError::Persist(_)));
    assert_eq!(state.settings_revision(), before);
    assert!(!state.config().auto_start);
    assert!(state.config().show_dock_icon);
    assert!(auto_start_calls().is_empty());
    assert!(dock.calls().is_empty());

    drop(state);
    let _ = fs::remove_dir_all(dir);
}

#[test]
fn first_hook_failure_rolls_back_config_and_restores_both_hooks() {
    reset_auto_start_hook();
    AUTO_START_FAIL_AT.with(|fail| fail.set(Some(0)));
    let (state, dir) = new_state("host-first-hook");
    state.set_auto_start_sync(recording_auto_start);
    let dock = DockHook::new();
    state.set_dock_visibility_sync(dock.sync());
    let previous = state.config();
    let before = state.settings_revision();
    let mut next = previous.clone();
    next.auto_start = true;
    next.show_dock_icon = false;

    let error = state.apply_host_settings(&previous, next).unwrap_err();
    match error {
        HostSettingsError::Sync(message) => {
            assert_eq!(
                message,
                "failed to synchronize desktop settings: auto-start hook failed"
            );
        }
        other => panic!("expected Sync, got {other:?}"),
    }
    assert!(!state.config().auto_start);
    assert!(state.config().show_dock_icon);
    assert!(!persisted_auto_start(&state));
    assert!(persisted_show_dock_icon(&state));
    assert_eq!(state.settings_revision(), before + 2);
    assert_eq!(auto_start_calls(), vec![true, false]);
    assert_eq!(
        dock.calls(),
        vec![true],
        "first-hook failure must skip the forward Dock sync and still restore Dock"
    );

    drop(state);
    let _ = fs::remove_dir_all(dir);
}

#[test]
fn second_hook_failure_rolls_back_config_and_restores_both_hooks() {
    reset_auto_start_hook();
    let (state, dir) = new_state("host-second-hook");
    state.set_auto_start_sync(recording_auto_start);
    let dock = DockHook::new();
    dock.fail_at(0);
    state.set_dock_visibility_sync(dock.sync());
    let previous = state.config();
    let before = state.settings_revision();
    let mut next = previous.clone();
    next.auto_start = true;
    next.show_dock_icon = false;

    let error = state.apply_host_settings(&previous, next).unwrap_err();
    match error {
        HostSettingsError::Sync(message) => {
            assert_eq!(
                message,
                "failed to synchronize desktop settings: dock hook failed"
            );
        }
        other => panic!("expected Sync, got {other:?}"),
    }
    assert!(!state.config().auto_start);
    assert!(state.config().show_dock_icon);
    assert_eq!(state.settings_revision(), before + 2);
    assert_eq!(auto_start_calls(), vec![true, false]);
    assert_eq!(dock.calls(), vec![false, true]);

    drop(state);
    let _ = fs::remove_dir_all(dir);
}

#[test]
fn hook_restore_failures_append_to_the_sync_error() {
    let (state, dir) = new_state("host-restore-fail");
    state.set_auto_start_sync(always_fail_auto_start);
    let dock = DockHook::new();
    dock.fail_at(0);
    state.set_dock_visibility_sync(dock.sync());
    let previous = state.config();
    let mut next = previous.clone();
    next.auto_start = true;

    let error = state.apply_host_settings(&previous, next).unwrap_err();
    match error {
        HostSettingsError::Sync(message) => {
            assert_eq!(
                message,
                "failed to synchronize desktop settings: auto-start hook failed; failed to restore auto-start state: auto-start hook failed; failed to restore Dock visibility: dock hook failed"
            );
        }
        other => panic!("expected Sync, got {other:?}"),
    }
    assert!(!state.config().auto_start);

    drop(state);
    let _ = fs::remove_dir_all(dir);
}

#[tokio::test]
async fn v2_and_v3_map_unsupported_and_sync_errors_without_shape_drift() {
    let harness = start_loopback("host-http-errors").await;
    let before = harness.state.settings_revision();
    let generation = harness.state.process_generation();
    let primary = harness.state.config().gateway_key.clone();

    let mut unsupported = harness.state.config();
    unsupported.auto_start = true;
    let v2 = harness
        .client
        .post(format!("{}/settings", harness.v2_base))
        .json(&v2_payload(
            &unsupported,
            Some(harness.state.settings_revision()),
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(v2.status(), StatusCode::BAD_REQUEST);
    let v2_body: Value = v2.json().await.unwrap();
    assert_eq!(
        v2_body["error"].as_str(),
        Some(HostSettingsError::AUTO_START_UNAVAILABLE)
    );
    assert!(v2_body.get("code").is_none());
    assert!(v2_body.get("currentRevision").is_none());
    assert_eq!(harness.state.settings_revision(), before);
    assert!(!harness.state.config().auto_start);

    let (status, body) =
        put_json(&harness, &cas_patch(&harness, json!({ "autoStart": true }))).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["code"], ERROR_INVALID_REQUEST);
    assert_eq!(
        body["message"].as_str(),
        Some(HostSettingsError::AUTO_START_UNAVAILABLE)
    );
    assert_eq!(body["currentRevision"], before);
    assert_eq!(body["processGeneration"], generation);
    assert!(body.get("current_revision").is_none());
    assert_eq!(harness.state.settings_revision(), before);

    let mut unsupported_dock = harness.state.config();
    unsupported_dock.show_dock_icon = false;
    let v2 = harness
        .client
        .post(format!("{}/settings", harness.v2_base))
        .json(&v2_payload(&unsupported_dock, None))
        .send()
        .await
        .unwrap();
    assert_eq!(v2.status(), StatusCode::BAD_REQUEST);
    let v2_body: Value = v2.json().await.unwrap();
    assert_eq!(
        v2_body["error"].as_str(),
        Some(HostSettingsError::DOCK_VISIBILITY_UNAVAILABLE)
    );
    assert_eq!(harness.state.settings_revision(), before);
    assert!(harness.state.config().show_dock_icon);

    let (status, body) = put_json(
        &harness,
        &cas_patch(&harness, json!({ "showDockIcon": false })),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["code"], ERROR_INVALID_REQUEST);
    assert_eq!(
        body["message"].as_str(),
        Some(HostSettingsError::DOCK_VISIBILITY_UNAVAILABLE)
    );
    assert_eq!(harness.state.settings_revision(), before);

    harness.state.set_auto_start_sync(fail_enable_auto_start);
    let mut failing = harness.state.config();
    failing.auto_start = true;
    let v2 = harness
        .client
        .post(format!("{}/settings", harness.v2_base))
        .json(&v2_payload(
            &failing,
            Some(harness.state.settings_revision()),
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(v2.status(), StatusCode::INTERNAL_SERVER_ERROR);
    let v2_body: Value = v2.json().await.unwrap();
    assert_eq!(
        v2_body["error"],
        "failed to synchronize desktop settings: auto-start hook failed"
    );
    assert!(!harness.state.config().auto_start);
    assert_ne!(harness.state.settings_revision(), before);
    assert_eq!(harness.state.config().gateway_key, primary);

    let after_v2_fail = harness.state.settings_revision();
    let (status, body) =
        put_json(&harness, &cas_patch(&harness, json!({ "autoStart": true }))).await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(body["code"], ERROR_INTERNAL);
    assert_eq!(
        body["message"],
        "failed to synchronize desktop settings: auto-start hook failed"
    );
    assert_eq!(body["currentRevision"], Value::Null);
    assert_eq!(body["processGeneration"], Value::Null);
    assert!(!harness.state.config().auto_start);
    assert_ne!(harness.state.settings_revision(), after_v2_fail);
    assert_eq!(harness.state.process_generation(), generation);
    assert_eq!(harness.state.config().gateway_key, primary);

    harness.stop();
}

#[tokio::test]
async fn v2_and_v3_successful_host_writes_preserve_cas_and_primary_key() {
    let harness = start_loopback("host-http-success").await;
    harness.state.set_auto_start_sync(ok_auto_start);
    let dock = DockHook::new();
    harness.state.set_dock_visibility_sync(dock.sync());
    let before = harness.state.settings_revision();
    let generation = harness.state.process_generation();
    let primary = harness.state.config().gateway_key.clone();

    let mut v2_config = harness.state.config();
    v2_config.auto_start = true;
    let v2 = harness
        .client
        .post(format!("{}/settings", harness.v2_base))
        .json(&v2_payload(&v2_config, None))
        .send()
        .await
        .unwrap();
    assert_eq!(v2.status(), StatusCode::OK);
    let v2_body: Value = v2.json().await.unwrap();
    assert_eq!(v2_body["revision"], before + 1);
    assert!(v2_body.get("processGeneration").is_none());
    assert!(harness.state.config().auto_start);
    assert_eq!(harness.state.config().gateway_key, primary);
    assert_eq!(
        dock.calls(),
        vec![true],
        "V2 auto-start save must reassert the unchanged Dock hook"
    );

    let stale = harness
        .client
        .post(format!("{}/settings", harness.v2_base))
        .json(&v2_payload(&v2_config, Some(before)))
        .send()
        .await
        .unwrap();
    assert_eq!(stale.status(), StatusCode::CONFLICT);
    let stale_body: Value = stale.json().await.unwrap();
    assert_eq!(
        stale_body["error"],
        "settings changed since they were loaded; reload and try again"
    );
    assert_eq!(harness.state.settings_revision(), before + 1);

    let (status, body) = put_json(
        &harness,
        &cas_patch(&harness, json!({ "showDockIcon": false })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["revision"], before + 2);
    assert_eq!(body["processGeneration"], generation);
    assert!(!harness.state.config().show_dock_icon);
    assert!(harness.state.config().auto_start);
    assert_eq!(harness.state.config().gateway_key, primary);
    assert_eq!(dock.calls(), vec![true, false]);

    let (status, body) = put_json(
        &harness,
        &json!({
            "processGeneration": generation,
            "autoStart": false
        }),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["code"], ERROR_MISSING_EXPECTED_REVISION);
    assert!(harness.state.config().auto_start);

    let (status, body) = put_json(
        &harness,
        &json!({
            "expectedRevision": harness.state.settings_revision(),
            "processGeneration": generation ^ 1,
            "autoStart": false
        }),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(body["code"], ERROR_REVISION_CONFLICT);
    assert_eq!(body["currentRevision"], before + 2);
    assert_eq!(body["processGeneration"], generation);
    assert!(harness.state.config().auto_start);

    harness.stop();
}

#[tokio::test(flavor = "current_thread")]
async fn v2_and_v3_reassert_unchanged_supported_hooks_and_rollback_on_drift() {
    reset_auto_start_hook();
    let harness = start_loopback("host-http-reassert").await;
    harness.state.set_auto_start_sync(recording_auto_start);
    let dock = DockHook::new();
    harness.state.set_dock_visibility_sync(dock.sync());
    let before = harness.state.settings_revision();
    let generation = harness.state.process_generation();
    let primary = harness.state.config().gateway_key.clone();

    let mut v2_config = harness.state.config();
    v2_config.connect_timeout_secs = 12;
    let v2 = harness
        .client
        .post(format!("{}/settings", harness.v2_base))
        .json(&v2_payload(&v2_config, None))
        .send()
        .await
        .unwrap();
    assert_eq!(v2.status(), StatusCode::OK);
    let v2_body: Value = v2.json().await.unwrap();
    assert_eq!(v2_body["revision"], before + 1);
    assert_eq!(harness.state.config().connect_timeout_secs, 12);
    assert!(!harness.state.config().auto_start);
    assert!(harness.state.config().show_dock_icon);
    assert_eq!(harness.state.config().gateway_key, primary);
    assert_eq!(auto_start_calls(), vec![false]);
    assert_eq!(dock.calls(), vec![true]);

    let (status, body) = put_json(
        &harness,
        &cas_patch(&harness, json!({ "connectTimeoutSecs": 13 })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["revision"], before + 2);
    assert_eq!(body["processGeneration"], generation);
    assert_eq!(harness.state.config().connect_timeout_secs, 13);
    assert_eq!(auto_start_calls(), vec![false, false]);
    assert_eq!(dock.calls(), vec![true, true]);

    AUTO_START_FAIL_AT.with(|fail| fail.set(Some(auto_start_calls().len())));
    let after_success = harness.state.settings_revision();
    let mut failing = harness.state.config();
    failing.connect_timeout_secs = 14;
    let v2 = harness
        .client
        .post(format!("{}/settings", harness.v2_base))
        .json(&v2_payload(
            &failing,
            Some(harness.state.settings_revision()),
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(v2.status(), StatusCode::INTERNAL_SERVER_ERROR);
    let v2_body: Value = v2.json().await.unwrap();
    assert_eq!(
        v2_body["error"],
        "failed to synchronize desktop settings: auto-start hook failed"
    );
    assert_eq!(harness.state.config().connect_timeout_secs, 13);
    assert!(!harness.state.config().auto_start);
    assert_eq!(harness.state.config().gateway_key, primary);
    assert_ne!(harness.state.settings_revision(), after_success);
    assert_eq!(auto_start_calls(), vec![false, false, false, false]);
    assert_eq!(
        dock.calls(),
        vec![true, true, true],
        "unchanged auto-start drift must skip forward Dock sync and still restore Dock"
    );

    AUTO_START_FAIL_AT.with(|fail| fail.set(None));
    dock.fail_at(dock.calls().len());
    let after_v2_fail = harness.state.settings_revision();
    let (status, body) = put_json(
        &harness,
        &cas_patch(&harness, json!({ "connectTimeoutSecs": 14 })),
    )
    .await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(body["code"], ERROR_INTERNAL);
    assert_eq!(
        body["message"],
        "failed to synchronize desktop settings: dock hook failed"
    );
    assert_eq!(body["currentRevision"], Value::Null);
    assert_eq!(body["processGeneration"], Value::Null);
    assert_eq!(harness.state.config().connect_timeout_secs, 13);
    assert_ne!(harness.state.settings_revision(), after_v2_fail);
    assert_eq!(harness.state.process_generation(), generation);
    assert_eq!(harness.state.config().gateway_key, primary);
    assert_eq!(
        auto_start_calls(),
        vec![false, false, false, false, false, false]
    );
    assert_eq!(dock.calls(), vec![true, true, true, true, true]);

    harness.stop();
}

async fn put_json(harness: &V3Harness, body: &Value) -> (StatusCode, Value) {
    let response = harness
        .client
        .put(format!("{}/settings", harness.v3_base))
        .json(body)
        .send()
        .await
        .unwrap();
    let status = response.status();
    let body = response.json().await.unwrap_or(Value::Null);
    (status, body)
}
