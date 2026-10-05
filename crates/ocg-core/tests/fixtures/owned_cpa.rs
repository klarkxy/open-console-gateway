//! Test-only composition of the public owned-CPA plane.
//!
//! The compile cfg picks the documented host directory. Product verify and
//! install decide trust. Callers do not pass a digest, a manifest hash, or a
//! stand-in executable. This module does not spawn a fake listener, skip the
//! child, or call the retired executor.

use ocg_core::cpa_execution::ExecutionReport;
use ocg_core::cpa_runtime::host::register_owned_host;
use ocg_core::gateway;
use ocg_core::state::{CoreState, GatewayHandle};
use std::path::PathBuf;

/// Pin names the product requires before `inference_ready`. The ready
/// acceptance stores this set only after the child document contains every
/// required name, so a true `inference_ready` is the public proof.
pub(crate) const ACCEPTED_PIN_CAPABILITIES: [&str; 3] = [
    "validated-protocol-pin-v1",
    "validation-only-routes-v1",
    "absolute-request-deadline-v1",
];

/// Documented runtime-build directory for this compile. Feature
/// `ollama-cloud-loopback-test` selects `native-loopback-fixture`; feature off
/// stays on the production base. A joined `..` is not used: the artifact check
/// rejects a parent component as an unsafe pin path.
pub(crate) fn documented_host_dir() -> PathBuf {
    let mut path = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    path.pop();
    path.pop();
    path.push("tmp");
    path.push("ocg3-cli-delivery");
    path.push("orchestration-20261004");
    path.push("runtime-build");
    if cfg!(feature = "ollama-cloud-loopback-test") {
        path.push("native-loopback-fixture");
    }
    path
}

/// Move the bound listener into the only owner slot and return its port.
pub(crate) fn store_listener(state: &CoreState, handle: GatewayHandle) -> u16 {
    let port = handle.port;
    let mut slot = state.gateway.lock();
    assert!(slot.is_none(), "public listener slot already has an owner");
    *slot = Some(handle);
    port
}

pub(crate) fn release_listener(state: &CoreState) {
    if let Some(handle) = state.gateway.lock().take() {
        gateway::stop_gateway(handle);
    }
}

/// Explicit operator Stop, then release the listener.
/// Product Stop clears persisted desired-run intent and bumps CAS.
/// Destructive profile cleanup and setup failure use this path.
/// A state that never registered a host only releases the listener.
pub(crate) fn shutdown_owned(state: &CoreState) {
    if state.cpa_runtime_supported() {
        let _ = ocg_core::cpa_execution::stop(
            state,
            state.settings_revision(),
            state.process_generation(),
        );
        state.stop_owned_cpa_runtime();
    }
    release_listener(state);
}

/// Host exit for a directory that will be reopened.
/// The child and listener stop. Persisted desired/applied facts, the managed
/// run intent, and CAS stay as the product left them.
pub(crate) fn exit_host_preserve_intent(state: &CoreState) {
    if state.cpa_runtime_supported() {
        state.stop_owned_cpa_runtime();
    }
    release_listener(state);
}

pub(crate) fn fail_setup(state: &CoreState, message: &str) -> ! {
    if state.cpa_runtime_supported() {
        let _ = ocg_core::cpa_execution::stop(
            state,
            state.settings_revision(),
            state.process_generation(),
        );
        state.stop_owned_cpa_runtime();
    }
    panic!("owned CPA setup failed: {message}");
}

fn plane_accepted(report: &ExecutionReport) -> bool {
    report.running
        && report.desired_running
        && report.listener_bound
        && report.policy_ready
        && report.inference_ready
        && !report.unavailable
        && report.error.is_none()
        && report.apply_status == "applied"
        && report.applied_revision != 0
        && report.applied_revision == report.desired_revision
        && report.applied_digest.len() == 64
        && report
            .applied_digest
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
}

/// Register the owned host, point at the compile-selected directory, and
/// install then start with the current CAS. The public listener must already
/// sit in `state.gateway`. Setup failure stops the child and panics; the
/// harness still owns listener and directory cleanup.
pub(crate) async fn adopt_owned_plane(state: &CoreState) -> ExecutionReport {
    if state.gateway.lock().is_none() {
        fail_setup(state, "public listener is not bound");
    }
    if !state.cpa_runtime_supported() {
        register_owned_host(state);
    }
    if !state.cpa_runtime_supported() {
        fail_setup(state, "owned CPA host is not registered on this platform");
    }
    ocg_core::cpa_execution::set_artifact_dir(state, documented_host_dir());
    let installed = match ocg_core::cpa_execution::install(
        state,
        state.settings_revision(),
        state.process_generation(),
        Some("v8.0.10"),
    ) {
        Ok(report) => report,
        Err(error) => fail_setup(state, &error.to_string()),
    };
    if !installed.installed {
        fail_setup(
            state,
            "install did not report the compile-selected artifact",
        );
    }
    let started = match ocg_core::cpa_execution::start(
        state,
        state.settings_revision(),
        state.process_generation(),
    )
    .await
    {
        Ok(report) => report,
        Err(error) => fail_setup(state, &error.to_string()),
    };
    let report = ocg_core::cpa_execution::execution_report(state);
    if !plane_accepted(&report) || report.applied_revision != started.applied_revision {
        fail_setup(
            state,
            &format!(
                "owned plane is not running, applied, and inference-ready with {ACCEPTED_PIN_CAPABILITIES:?}: {report:?}"
            ),
        );
    }
    report
}

/// Install and start when the plane is not already accepted.
/// A second call keeps the running child. Tests that change a snapshotted
/// projection call `adopt_owned_plane` again; that replaces the child.
pub(crate) async fn ensure_owned_plane(state: &CoreState) -> ExecutionReport {
    let report = ocg_core::cpa_execution::execution_report(state);
    if plane_accepted(&report) {
        return report;
    }
    adopt_owned_plane(state).await
}

/// A second start with the wrong settings revision must conflict before the
/// applied plane changes.
pub(crate) async fn refuse_stale_revision(state: &CoreState) {
    let before = ocg_core::cpa_execution::execution_report(state);
    let revision = state.settings_revision();
    let generation = state.process_generation();
    let error = ocg_core::cpa_execution::start(state, revision.saturating_add(1), generation)
        .await
        .expect_err("stale settings revision must not apply");
    let rendered = error.to_string();
    assert!(
        rendered.contains("revisionConflict"),
        "stale CAS must be a conflict: {rendered}"
    );
    let after = ocg_core::cpa_execution::execution_report(state);
    assert_eq!(after.apply_status, before.apply_status);
    assert_eq!(after.applied_revision, before.applied_revision);
    assert_eq!(after.desired_revision, before.desired_revision);
    assert_eq!(after.applied_digest, before.applied_digest);
    assert_eq!(after.running, before.running);
    assert_eq!(after.inference_ready, before.inference_ready);
    assert_eq!(state.settings_revision(), revision);
    assert_eq!(state.process_generation(), generation);
}
