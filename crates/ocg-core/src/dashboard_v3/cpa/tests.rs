use super::*;

fn snapshot() -> cpa_runtime::CpaRuntimeSnapshot {
    cpa_runtime::CpaRuntimeSnapshot {
        supported: true,
        unavailable_reason: None,
        installed: true,
        running: false,
        desired_running: false,
        owned: true,
        current_version: Some("1.0.0".into()),
        previous_version: None,
        asset_sha256: None,
        port: Some(8317),
        base_url: Some("http://127.0.0.1:8317".into()),
        phase: cpa_runtime::CpaRuntimePhase::Idle,
        error: None,
        latest_version: None,
        update_available: false,
        current_operation: None,
    }
}

#[test]
fn runtime_actions_follow_host_ownership_lifecycle_and_versions() {
    let mut runtime = snapshot();
    let stopped = runtime_actions(&runtime, false);
    assert!(!stopped.install && stopped.start && !stopped.stop);
    assert!(stopped.check_update && stopped.remove);
    assert!(!stopped.update && !stopped.rollback);

    runtime.running = true;
    let running = runtime_actions(&runtime, false);
    assert!(!running.start && running.stop);
    runtime.running = false;
    runtime.desired_running = true;
    runtime.phase = cpa_runtime::CpaRuntimePhase::Failed;
    let failed_restore = runtime_actions(&runtime, false);
    assert!(failed_restore.start && failed_restore.stop);

    runtime.previous_version = Some("0.9.0".into());
    runtime.update_available = true;
    assert!(!runtime_actions(&runtime, false).update);
    runtime.latest_version = Some("1.1.0".into());
    let update = runtime_actions(&runtime, false);
    assert!(update.update && update.rollback);
    runtime.update_available = false;
    assert!(!runtime_actions(&runtime, false).update);

    for phase in [
        cpa_runtime::CpaRuntimePhase::Checking,
        cpa_runtime::CpaRuntimePhase::Downloading,
        cpa_runtime::CpaRuntimePhase::Installing,
        cpa_runtime::CpaRuntimePhase::Starting,
    ] {
        runtime.phase = phase;
        assert_eq!(
            runtime_actions(&runtime, false),
            CpaRuntimeActions::default()
        );
    }
    runtime.phase = cpa_runtime::CpaRuntimePhase::Idle;
    runtime.supported = false;
    assert_eq!(
        runtime_actions(&runtime, false),
        CpaRuntimeActions::default()
    );
    runtime.supported = true;
    runtime.owned = false;
    assert_eq!(
        runtime_actions(&runtime, false),
        CpaRuntimeActions::default()
    );
    runtime.installed = false;
    runtime.desired_running = false;
    let fresh = runtime_actions(&runtime, false);
    assert!(fresh.install && fresh.check_update);
    assert!(!fresh.start && !fresh.stop && !fresh.update && !fresh.rollback && !fresh.remove);
}

#[test]
fn external_endpoint_blocks_launch_and_replacement_but_preserves_owned_cleanup() {
    let mut runtime = snapshot();
    runtime.running = true;
    runtime.previous_version = Some("0.9.0".into());
    runtime.latest_version = Some("1.1.0".into());
    runtime.update_available = true;
    let actions = runtime_actions(&runtime, true);
    assert!(!actions.install && !actions.start && !actions.update && !actions.rollback);
    assert!(actions.stop && actions.remove && actions.check_update);
    runtime.installed = false;
    runtime.owned = false;
    assert!(!runtime_actions(&runtime, true).install);
}

#[tokio::test]
async fn runtime_read_publishes_capabilities_and_mutation_projection_uses_current_facts() {
    let (dir, state) = super::tests::test_state("runtime-projection");
    let Json(unavailable) = get_runtime(State(state.clone())).await.unwrap();
    assert_eq!(unavailable.actions, CpaRuntimeActions::default());
    assert!(!unavailable.client_keys_available && !unavailable.codex_device_login_available);
    assert!(!unavailable.startup_restore_pending);

    let mut current = snapshot();
    current.desired_running = true;
    current.running = true;
    let running = runtime_view(&state, current.clone());
    assert!(running.client_keys_available && running.startup_restore_pending);
    assert_eq!(
        running.codex_device_login_available,
        std::env::var_os(cpa::CPA_BASE_URL_ENV).is_none()
    );
    assert!(running.actions.stop);
    current.running = false;
    current.desired_running = false;
    let stopped = runtime_view(&state, current);
    assert!(!stopped.actions.stop && !stopped.codex_device_login_available);
    assert!(!stopped.startup_restore_pending);
    assert_eq!(stopped.revision, state.settings_revision());
    assert_eq!(stopped.process_generation, state.process_generation());
    for (supported, owned, installed) in [
        (false, true, true),
        (true, false, true),
        (true, true, false),
    ] {
        let mut unavailable = snapshot();
        unavailable.running = true;
        unavailable.supported = supported;
        unavailable.owned = owned;
        unavailable.installed = installed;
        let view = runtime_view(&state, unavailable);
        assert!(!view.client_keys_available && !view.codex_device_login_available);
    }
    drop(state);
    std::fs::remove_dir_all(dir).unwrap();
}
