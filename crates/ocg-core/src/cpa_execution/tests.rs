//! Isolated CPA execution checks. They use a synthetic data directory. Apply,
//! including skip-spawn, verifies the documented runtime-build artifact.
//! Cargo is not run from this leaf.

use super::{
    CorrelationAuthorization, CorrelationIntent, ExecutionError, artifact, callback,
    cancel_correlation, correlation_authorization_kind, correlation_cancelled,
    correlation_client_trace, correlation_decision, execution_report, install, io,
    management_password, owned_inference_connection, policy_callback, preview_attempt, project,
    ready, register_correlation, register_correlation_intent, register_correlation_trace, remove,
    rollback, schedule_owned_apply, set_artifact_dir, set_before_apply_commit, set_fail_persist,
    set_fail_ready, set_skip_spawn, set_test_password, start, stop, store, test_secrets,
    verified_owned_origin,
};
use crate::account_control::{self, MutationCas};
use crate::backup::{CipherWitness, create_snapshot, restore_snapshot};
use crate::cpa_policy::{CurrentFacts, PolicyFault, Reason};
use crate::cpa_runtime::host::register_owned_host;
use crate::cpa_runtime::{
    CpaRuntimeError, CpaRuntimeLogTail, CpaRuntimeProcessHost, CpaRuntimeProcessSpec,
    CpaRuntimeSecret,
};
use crate::crypto::{KeyCipher, StaticKeyCipher};
use crate::db::Database;
use crate::gateway_runtime::GatewayHandle;
use crate::state::{CoreState, CoreStateInner};
use axum::extract::{ConnectInfo, State};
use axum::http::HeaderMap;
use bytes::Bytes;
use chrono::Utc;
use std::net::{SocketAddr, TcpListener};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use uuid::Uuid;

fn pinned_dir() -> PathBuf {
    super::documented_runtime_dir()
}

fn platform_executable_name(platform: artifact::Platform) -> &'static str {
    match (platform.os, platform.arch) {
        ("windows", "x86_64") => "ocg-cpa-host.exe",
        ("linux", "x86_64") => "ocg-cpa-host-linux-amd64",
        ("macos", "aarch64") => "ocg-cpa-host-darwin-arm64",
        ("linux", "aarch64") => "ocg-cpa-host-linux-arm64",
        ("macos", "x86_64") => "ocg-cpa-host-darwin-amd64",
        ("windows", "aarch64") => "ocg-cpa-host-windows-arm64.exe",
        _ => "ocg-cpa-host",
    }
}

fn require_pinned() -> PathBuf {
    let dir = pinned_dir();
    let platform = artifact::current_platform().expect("compile platform");
    let exe = dir.join(platform_executable_name(platform));
    assert!(
        exe.is_file(),
        "pinned CPA host missing at {}",
        exe.display()
    );
    assert!(
        dir.join("manifest.json").is_file(),
        "documented CPA manifest missing at {}",
        dir.join("manifest.json").display()
    );
    dir
}

fn use_documented_runtime(state: &CoreState) {
    set_artifact_dir(state, require_pinned());
}

fn temp_dir(label: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("ocg-cpa-execution-{label}-{}", Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn open_state(dir: &Path) -> CoreState {
    let db = Database::open(dir.to_path_buf()).unwrap();
    let cipher: Arc<dyn KeyCipher + Send + Sync> =
        Arc::new(StaticKeyCipher::new("synthetic-cpa-execution"));
    Arc::new(CoreStateInner::new(db, dir.to_path_buf(), cipher).unwrap())
}

fn install_listener(state: &CoreState) {
    let (shutdown, _receiver) = tokio::sync::oneshot::channel();
    let task = tokio::spawn(async {});
    *state.gateway.lock() = Some(GatewayHandle {
        port: 9,
        listen_addr: "127.0.0.1:9".parse().unwrap(),
        dashboard_is_local: true,
        shutdown,
        task,
    });
    use_documented_runtime(state);
}

struct HookGuard;

impl HookGuard {
    fn synthetic() -> Self {
        set_skip_spawn(true);
        set_fail_ready(false);
        set_fail_persist(false);
        set_test_password(Some("synthetic-management".to_string()));
        set_before_apply_commit(None);
        Self
    }
}

impl Drop for HookGuard {
    fn drop(&mut self) {
        set_skip_spawn(false);
        set_fail_ready(false);
        set_fail_persist(false);
        set_test_password(None);
        set_before_apply_commit(None);
    }
}

fn lower_hex_64(value: Option<&str>) -> bool {
    value.is_some_and(|text| {
        text.len() == 64
            && text
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
    })
}

fn documented_manifest() -> serde_json::Value {
    let path = pinned_dir().join("manifest.json");
    let bytes = std::fs::read(&path)
        .unwrap_or_else(|_| panic!("documented CPA manifest missing at {}", path.display()));
    let value: serde_json::Value = serde_json::from_slice(&bytes)
        .unwrap_or_else(|_| panic!("documented CPA manifest is not JSON at {}", path.display()));
    assert_eq!(
        value.get("sourceCommit").and_then(|item| item.as_str()),
        Some(artifact::PINNED_COMMIT),
        "documented manifest sourceCommit"
    );
    assert_eq!(
        value.get("sourceVersion").and_then(|item| item.as_str()),
        Some(artifact::PINNED_VERSION),
        "documented manifest sourceVersion"
    );
    assert_eq!(
        value.get("protocolVersion").and_then(|item| item.as_u64()),
        Some(1),
        "documented manifest protocolVersion"
    );
    assert!(
        lower_hex_64(value.get("buildIdentity").and_then(|item| item.as_str())),
        "documented manifest buildIdentity"
    );
    assert!(
        lower_hex_64(value.get("hostSHA256").and_then(|item| item.as_str())),
        "documented manifest hostSHA256"
    );
    assert!(
        lower_hex_64(value.get("overlaySHA256").and_then(|item| item.as_str())),
        "documented manifest overlaySHA256"
    );
    assert!(
        value
            .get("buildTags")
            .and_then(|item| item.as_array())
            .is_some()
    );
    let names = value
        .get("capabilities")
        .and_then(|item| item.as_array())
        .unwrap_or_else(|| panic!("documented manifest capabilities"));
    let present = names
        .iter()
        .filter_map(|item| item.as_str())
        .collect::<Vec<_>>();
    for required in artifact::REQUIRED_CAPABILITIES {
        assert!(
            present.contains(&required),
            "documented manifest missing {required}"
        );
    }
    value
}

fn align_selected_variant(manifest: &mut serde_json::Value) {
    match artifact::selected_variant() {
        artifact::Variant::Production => {
            manifest["variant"] = serde_json::json!("production");
            manifest["buildTags"] = serde_json::json!([]);
        }
        artifact::Variant::NativeLoopbackFixture => {
            manifest["variant"] = serde_json::json!("native-loopback-fixture");
            manifest["buildTags"] = serde_json::json!(["ocg_native_loopback_fixture"]);
        }
    }
}

fn stage_documented_fixture(dir: &Path, os: &str, arch: &str) -> serde_json::Value {
    std::fs::create_dir_all(dir).unwrap();
    let platform = artifact::current_platform().expect("compile platform");
    let name = platform_executable_name(platform);
    let source = pinned_dir().join(name);
    let bytes = std::fs::read(&source)
        .unwrap_or_else(|_| panic!("documented executable missing at {}", source.display()));
    assert!(!bytes.is_empty(), "documented executable is empty");
    std::fs::write(dir.join(name), &bytes).unwrap();
    let mut manifest = documented_manifest();
    align_selected_variant(&mut manifest);
    manifest["os"] = serde_json::json!(os);
    manifest["arch"] = serde_json::json!(arch);
    manifest["executable"] = serde_json::json!(name);
    manifest
}

fn write_manifest(dir: &Path, manifest: &serde_json::Value) {
    std::fs::write(dir.join("manifest.json"), manifest.to_string()).unwrap();
}

async fn callback(
    state: &CoreState,
    token: &str,
    origin: Option<&str>,
    body: Vec<u8>,
    peer: SocketAddr,
) -> (u16, String) {
    let mut headers = HeaderMap::new();
    if !token.is_empty() {
        headers.insert(
            axum::http::header::AUTHORIZATION,
            format!("Bearer {token}").parse().unwrap(),
        );
    }
    if let Some(origin) = origin {
        headers.insert(axum::http::header::ORIGIN, origin.parse().unwrap());
    }
    let _response = policy_callback(
        ConnectInfo(peer),
        State(Arc::clone(state)),
        headers,
        Bytes::from(body),
    )
    .await;
    callback::take_callback().unwrap_or((0, String::new()))
}

fn token_of(state: &CoreState) -> String {
    std::fs::read_to_string(io::private_dir(&state.data_dir()).join("policy.token"))
        .unwrap()
        .trim()
        .to_string()
}

#[test]
fn empty_profile_does_not_spawn_or_write_secrets() {
    let dir = temp_dir("empty");
    let state = open_state(&dir);
    let report = execution_report(&state);
    assert_ne!(report.child_generation, state.process_generation());
    assert_ne!(report.child_generation, 0);
    assert!(!report.installed);
    assert!(!report.running);
    assert!(!report.listener_bound);
    assert!(!report.inference_ready);
    assert_eq!(report.applied_revision, 0);
    assert!(!io::private_dir(&dir).join("hop.key").exists());
    assert!(!io::private_dir(&dir).join("management.key").exists());
    assert!(!io::config_path(&dir).exists());
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn stock_health_and_foreign_platform_are_rejected() {
    let secrets = test_secrets("hop-secret", "policy-secret", "ready-secret");
    let error = ready::accept_ready(r#"{"status":"ok"}"#, &store::Record::empty(), &secrets)
        .unwrap_err()
        .to_string();
    assert!(error.contains("stock health"));

    let current = artifact::current_platform().expect("compile platform");
    let (os, arch) = if current.os == "linux" && current.arch == "x86_64" {
        ("windows", "x86_64")
    } else {
        ("linux", "x86_64")
    };
    assert_ne!((os, arch), (current.os, current.arch));
    let dir = temp_dir("platform");
    let manifest = stage_documented_fixture(&dir, os, arch);
    assert_eq!(manifest["sourceCommit"], artifact::PINNED_COMMIT);
    assert_eq!(manifest["sourceVersion"], artifact::PINNED_VERSION);
    write_manifest(&dir, &manifest);
    let error = artifact::verify(&dir).unwrap_err().to_string();
    assert!(
        error.contains("pinned CPA manifest os/arch does not match the selected record"),
        "{error}"
    );
    assert!(artifact::installed_executable(&dir, artifact::PINNED_SHA256).is_none());
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn tampered_manifest_and_hash_do_not_install() {
    let platform = artifact::current_platform().expect("compile platform");
    let dir = temp_dir("hash");
    let manifest = stage_documented_fixture(&dir, platform.os, platform.arch);
    let name = manifest["executable"]
        .as_str()
        .expect("staged executable name");
    let path = dir.join(name);
    let mut bytes = std::fs::read(&path).unwrap();
    assert!(!bytes.is_empty(), "documented executable is empty");
    bytes[0] ^= 0xff;
    std::fs::write(&path, &bytes).unwrap();
    write_manifest(&dir, &manifest);
    let error = artifact::verify(&dir).unwrap_err().to_string();
    assert!(
        error.contains("pinned CPA executable hash does not match the selected record"),
        "{error}"
    );
    assert!(artifact::installed_executable(&dir, artifact::PINNED_SHA256).is_none());
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn manifest_without_native_endpoint_pin_capability_does_not_install() {
    assert_eq!(artifact::REQUIRED_CAPABILITIES.len(), 16);
    assert_eq!(
        artifact::REQUIRED_CAPABILITIES[15],
        "native-final-endpoint-pin-v1"
    );
    let platform = artifact::current_platform().expect("compile platform");
    let dir = temp_dir("cap16");
    let mut manifest = stage_documented_fixture(&dir, platform.os, platform.arch);
    let caps = manifest["capabilities"]
        .as_array_mut()
        .expect("documented capabilities");
    assert_eq!(caps.len(), 16);
    caps.retain(|item| item.as_str() != Some("native-final-endpoint-pin-v1"));
    assert_eq!(caps.len(), 15);
    write_manifest(&dir, &manifest);
    let error = artifact::verify(&dir).unwrap_err().to_string();
    assert!(
        error.contains("pinned CPA manifest is missing a required capability"),
        "{error}"
    );
    assert!(!error.contains("hash"), "{error}");
    assert!(artifact::installed_executable(&dir, artifact::PINNED_SHA256).is_none());
    assert!(artifact::installed_executable(&dir, &"ab".repeat(32)).is_none());
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn pinned_artifact_matches_the_selected_record() {
    let dir = require_pinned();
    let verified = artifact::verify(&dir).expect("documented runtime must match the selected lock");
    let trusted = artifact::selected_trusted_sha().expect("selected digest");
    assert_eq!(verified.sha256, trusted);
    assert_ne!(trusted, artifact::PINNED_SHA256);
    assert_eq!(verified.source_commit, artifact::PINNED_COMMIT);
    assert_eq!(verified.source_version, artifact::PINNED_VERSION);
    assert!(artifact::version_accepted(Some("v8.0.10")).is_ok());
    assert!(artifact::version_accepted(Some("9.0.0")).is_err());
    let data = temp_dir("selected-install");
    let installed = artifact::install(&data, &verified).expect("install selected bytes");
    assert_eq!(
        artifact::installed_executable(&data, &trusted).as_deref(),
        Some(installed.as_path())
    );
    assert!(artifact::installed_executable(&data, artifact::PINNED_SHA256).is_none());
    let _ = std::fs::remove_dir_all(data);
}

fn install_remote_cpa(state: &CoreState, base_url: &str) {
    let plan =
        crate::provider::builtin_provider(crate::provider::CPA_PROVIDER_ID).expect("cpa provider");
    let now = Utc::now();
    let account = crate::models::Account {
        id: ocg_domain::ids::CPA_ACCOUNT_ID.to_string(),
        provider_id: plan.provider_id.to_string(),
        credential_kind: plan.credential_kind,
        quota_scope: plan.quota_scope,
        name: "synthetic-remote-cpa".into(),
        username: None,
        password_cipher: None,
        key_cipher: state
            .cipher
            .encrypt("synthetic-remote-inference")
            .expect("inference cipher"),
        enabled: true,
        account_type: crate::models::AccountType::Key,
        setup_step: crate::models::AccountSetupStep::Ready,
        referral_code: None,
        purchase_date: String::new(),
        expires_on: String::new(),
        cooldown_until: None,
        cooldown_generic_until: None,
        cooldown_5h_until: None,
        cooldown_week_until: None,
        cooldown_month_until: None,
        cooldown_free_until: None,
        last_error: None,
        auth_error: None,
        notes: None,
        created_at: now,
        updated_at: now,
    };
    let management = state
        .cipher
        .encrypt("synthetic-remote-management")
        .expect("management cipher");
    let db = state.db.lock();
    db.upsert_cpa_integration(&account, base_url, &management)
        .expect("remote cpa destination");
    let snapshot = crate::routing_snapshot::RoutingSnapshot::load(&db).expect("routing snapshot");
    let visible = snapshot.projection.destinations.iter().any(|destination| {
        destination.adapter == ocg_domain::destination::AdapterKind::Cpa
            && destination.base_url.as_deref() == Some(base_url)
    });
    assert!(
        visible,
        "v4 routing snapshot did not keep the remote CPA destination"
    );
}

/// A fresh profile has no CPA destination. The product writer creates the remote
/// row; the V4 snapshot must show that same base URL before origin checks.
#[test]
fn another_loopback_stays_remote_and_self_loop_is_rejected() {
    let dir = temp_dir("origin");
    let state = open_state(&dir);
    install_remote_cpa(&state, "https://remote.example");
    let db = state.db.lock();
    let changed = db
        .conn
        .execute(
            "UPDATE destinations SET base_url = ?1 WHERE adapter = 'cpa'",
            ["http://127.0.0.1:8317"],
        )
        .unwrap();
    assert!(changed >= 1, "CPA destination row is missing");
    let snapshot = crate::routing_snapshot::RoutingSnapshot::load(&db).unwrap();
    let remote = project::owned_destination_ids(&snapshot, "http://127.0.0.1:18080", "");
    assert!(
        remote.is_empty(),
        "a different loopback origin is not the owned child"
    );
    db.conn
        .execute(
            "UPDATE destinations SET base_url = ?1 WHERE adapter = 'cpa'",
            ["http://127.0.0.1:18080"],
        )
        .unwrap();
    let owned = crate::routing_snapshot::RoutingSnapshot::load(&db).unwrap();
    let ids = project::owned_destination_ids(&owned, "http://127.0.0.1:18080", "");
    assert!(!ids.is_empty());
    let error = project::reject_self_loop(&owned, "http://127.0.0.1:18080")
        .unwrap_err()
        .to_string();
    assert_eq!(error, "cpa_self_loop");
    drop(db);
    drop(state);
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn synthetic_apply_rejects_failure_conflict_and_rolls_back() {
    let _hooks = HookGuard::synthetic();
    let dir = temp_dir("apply");
    let state = open_state(&dir);
    install_listener(&state);
    let report = start(
        &state,
        state.settings_revision(),
        state.process_generation(),
    )
    .await
    .expect("synthetic apply");
    assert_eq!(report.apply_status, "applied");
    assert_eq!(report.applied_revision, 1);
    assert_ne!(report.child_generation, state.process_generation());
    assert!(io::config_path(&dir).is_file());
    let yaml = std::fs::read_to_string(io::config_path(&dir)).unwrap();
    let parsed: serde_yaml_ng::Value = serde_yaml_ng::from_str(&yaml).unwrap();
    let keys = parsed.get("api-keys").and_then(|value| value.as_sequence());
    assert_eq!(keys.map(|items| items.len()), Some(1));
    let secret = parsed
        .get("remote-management")
        .and_then(|value| value.get("secret-key"))
        .and_then(|value| value.as_str());
    assert_eq!(secret, Some(""));
    let auth_dir = parsed
        .get("auth-dir")
        .and_then(|value| value.as_str())
        .unwrap_or("");
    assert!(auth_dir.ends_with("/cpa/auth"), "{auth_dir}");
    assert!(!auth_dir.contains('\\'));
    let debug = format!("{:?}", state.cpa_execution);
    let hop = std::fs::read_to_string(io::private_dir(&dir).join("hop.key")).unwrap();
    assert!(!debug.contains(hop.trim()));
    assert!(!yaml.contains("ocg-keyless"));
    let management = std::fs::read_to_string(io::private_dir(&dir).join("management.key")).unwrap();
    assert_ne!(management.trim(), hop.trim());
    assert!(!management.trim().is_empty());
    assert!(!debug.contains(management.trim()));

    let applied_bytes = std::fs::read(io::config_path(&dir)).unwrap();
    let missing = rollback(
        &state,
        state.settings_revision(),
        state.process_generation(),
    )
    .await;
    assert!(matches!(missing, Err(ExecutionError::RollbackUnavailable)));

    set_fail_ready(true);
    let failed = start(
        &state,
        state.settings_revision(),
        state.process_generation(),
    )
    .await;
    assert!(failed.unwrap_err().to_string().contains("cpa_apply_failed"));
    let failed_report = execution_report(&state);
    assert_eq!(failed_report.apply_status, "applied");
    assert_eq!(failed_report.applied_revision, 1);
    assert!(failed_report.desired_revision > failed_report.applied_revision);
    assert_eq!(std::fs::read(io::config_path(&dir)).unwrap(), applied_bytes);
    set_fail_ready(false);

    let applied = std::fs::read(io::config_path(&dir)).unwrap();
    let hooked = Arc::clone(&state);
    set_before_apply_commit(Some(Arc::new(move || {
        hooked.bump_settings_revision();
    })));
    let conflict = start(
        &state,
        state.settings_revision(),
        state.process_generation(),
    )
    .await
    .unwrap_err();
    assert_eq!(conflict.to_string(), "cpa_apply_conflict");
    assert_eq!(std::fs::read(io::config_path(&dir)).unwrap(), applied);
    assert_eq!(execution_report(&state).apply_status, "applied");
    assert_eq!(execution_report(&state).applied_revision, 1);
    set_before_apply_commit(None);

    let restored = start(
        &state,
        state.settings_revision(),
        state.process_generation(),
    )
    .await
    .expect("apply after conflict");
    assert!(restored.applied_revision > 1);
    assert!(io::previous_config_path(&dir).is_file());
    let rolled = rollback(
        &state,
        state.settings_revision(),
        state.process_generation(),
    )
    .await
    .expect("rollback");
    assert_eq!(rolled.apply_status, "applied");
    assert!(rolled.applied_revision < restored.applied_revision);

    let stopped = stop(
        &state,
        state.settings_revision(),
        state.process_generation(),
    )
    .unwrap();
    assert!(!stopped.desired_running);
    assert_eq!(stopped.apply_status, "stopped");
    let restarted = start(
        &state,
        state.settings_revision(),
        state.process_generation(),
    )
    .await
    .unwrap();
    assert!(restarted.desired_running);
    state.stop_owned_cpa_runtime();
    assert!(execution_report(&state).desired_running);
    drop(state);
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn callback_fences_unknown_requests_and_rotated_credentials() {
    let _hooks = HookGuard::synthetic();
    let dir = temp_dir("callback");
    let state = open_state(&dir);
    install_listener(&state);
    let report = start(
        &state,
        state.settings_revision(),
        state.process_generation(),
    )
    .await
    .unwrap();
    let token = token_of(&state);
    let origin = "http://127.0.0.1:9";
    let loopback: SocketAddr = "127.0.0.1:1".parse().unwrap();
    let remote: SocketAddr = "8.8.8.8:1".parse().unwrap();

    let (status, _) = callback(
        &state,
        &token,
        Some(origin),
        br#"{"protocolVersion":1}"#.to_vec(),
        remote,
    )
    .await;
    assert_eq!(status, 403);
    let (status, body) = callback(
        &state,
        "wrong-token",
        Some(origin),
        br#"{"protocolVersion":1}"#.to_vec(),
        loopback,
    )
    .await;
    assert_eq!(status, 401);
    assert!(body.contains("unauthorized"));
    let (status, _) = callback(
        &state,
        &token,
        None,
        br#"{"protocolVersion":1}"#.to_vec(),
        loopback,
    )
    .await;
    assert_eq!(status, 403);
    let huge = vec![b'x'; 128 * 1024 + 1];
    let (status, body) = callback(&state, &token, Some(origin), huge, loopback).await;
    assert_eq!(status, 413);
    assert!(body.contains("oversize"));

    let unknown = serde_json::json!({
        "protocolVersion": 1,
        "operation": "admit",
        "processGeneration": report.child_generation.to_string(),
        "projectionRevision": report.desired_revision.to_string(),
        "projectionDigest": report.desired_digest,
        "requestId": Uuid::new_v4().to_string(),
        "attemptId": Uuid::new_v4().to_string(),
        "authId": "a",
        "credentialId": "c",
        "credentialVersion": "1",
        "providerId": "opencode",
        "publicModel": "public",
        "upstreamModel": "upstream",
        "registrationEpoch": "0",
        "materialRevision": "material",
        "kind": "accepted"
    });
    let (status, body) = callback(
        &state,
        &token,
        Some(origin),
        serde_json::to_vec(&unknown).unwrap(),
        loopback,
    )
    .await;
    assert_eq!(status, 200);
    assert!(body.contains("malformed"));
    assert!(!execution_report(&state).unavailable);

    let ready = serde_json::json!({
        "protocolVersion": 1,
        "operation": "ready",
        "processGeneration": report.child_generation.to_string(),
        "projectionRevision": report.desired_revision.to_string(),
        "projectionDigest": report.desired_digest
    });
    let (status, body) = callback(
        &state,
        &token,
        Some(origin),
        serde_json::to_vec(&ready).unwrap(),
        loopback,
    )
    .await;
    assert_eq!(status, 200, "{body}");
    assert!(body.contains("\"policyReady\":true"), "{body}");

    let created = account_control::create_go_api_key(
        &state,
        "fence".into(),
        "sk-metadata".into(),
        None,
        None,
    )
    .unwrap();
    publish_enabled_go_model(&state);
    let applied = start(
        &state,
        state.settings_revision(),
        state.process_generation(),
    )
    .await
    .expect("apply credential");
    let stamp = preview_attempt(&state, &created.id).unwrap();
    let yaml = std::fs::read_to_string(io::config_path(&dir)).unwrap();
    assert!(
        yaml.contains(&stamp.credential_id),
        "applied projection omitted the credential"
    );
    let request_id = Uuid::new_v4();
    client_intent(&state, request_id);
    let admit = serde_json::json!({
        "protocolVersion": 1,
        "operation": "admit",
        "processGeneration": applied.child_generation.to_string(),
        "projectionRevision": applied.desired_revision.to_string(),
        "projectionDigest": applied.desired_digest,
        "requestId": request_id.to_string(),
        "attemptId": Uuid::new_v4().to_string(),
        "authId": stamp.auth_id,
        "credentialId": stamp.credential_id,
        "credentialVersion": stamp.credential_version.to_string(),
        "providerId": stamp.provider_id,
        "publicModel": "public",
        "upstreamModel": "upstream",
        "registrationEpoch": stamp.registration_epoch.to_string(),
        "materialRevision": stamp.material_revision,
        "kind": "accepted",
        "callableProtocol": "chat_completions",
        "generationKind": "execute"
    });
    let _ = callback(
        &state,
        &token,
        Some(origin),
        serde_json::to_vec(&admit).unwrap(),
        loopback,
    )
    .await;
    account_control::rotate_upstream_credential(
        &state,
        &stamp.credential_id,
        "sk-metadata-rotated",
        MutationCas {
            expected_revision: state.settings_revision(),
            process_generation: state.process_generation(),
        },
    )
    .unwrap();
    let stale_request = Uuid::new_v4();
    client_intent(&state, stale_request);
    let mut stale = admit;
    stale["requestId"] = serde_json::json!(stale_request.to_string());
    stale["attemptId"] = serde_json::json!(Uuid::new_v4().to_string());
    let (status, body) = callback(
        &state,
        &token,
        Some(origin),
        serde_json::to_vec(&stale).unwrap(),
        loopback,
    )
    .await;
    assert_eq!(status, 200, "{body}");
    assert!(body.contains("stale_credential"), "{body}");
    drop(state);
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn backup_restores_applied_projection_without_printing_secrets() {
    let _hooks = HookGuard::synthetic();
    let root = temp_dir("backup");
    let source = root.join("source");
    std::fs::create_dir_all(&source).unwrap();
    let applied = {
        let state = open_state(&source);
        install_listener(&state);
        let report = start(
            &state,
            state.settings_revision(),
            state.process_generation(),
        )
        .await
        .unwrap();
        assert!(report.inference_ready || report.policy_ready);
        let management = std::fs::read(io::private_dir(&source).join("management.key")).unwrap();
        let hop = std::fs::read(io::private_dir(&source).join("hop.key")).unwrap();
        assert_ne!(management, hop);
        report.applied_revision
    };
    let output = root.join("snapshot.ocg");
    create_snapshot(
        &source,
        &output,
        CipherWitness::explicit("synthetic-cpa-execution"),
    )
    .unwrap();
    let target = root.join("restored");
    restore_snapshot(
        &target,
        &output,
        CipherWitness::explicit("synthetic-cpa-execution"),
    )
    .unwrap();
    let state = open_state(&target);
    let report = execution_report(&state);
    assert_eq!(report.applied_revision, applied);
    assert!(!report.installed);
    assert_ne!(report.child_generation, 0);
    let token = token_of(&state);
    let management =
        std::fs::read_to_string(io::private_dir(&target).join("management.key")).unwrap();
    let hop = std::fs::read_to_string(io::private_dir(&target).join("hop.key")).unwrap();
    let source_management =
        std::fs::read_to_string(io::private_dir(&source).join("management.key")).unwrap();
    assert_eq!(management.trim(), source_management.trim());
    assert_ne!(management.trim(), hop.trim());
    let debug = format!("{:?}", state.cpa_execution);
    assert!(!debug.contains(&token));
    assert!(!debug.contains(management.trim()));
    let managed = crate::cpa_runtime::load_managed(&target)
        .unwrap()
        .expect("managed runtime");
    assert_eq!(managed.current_version, "8.0.10");
    drop(state);
    let _ = std::fs::remove_dir_all(root);
}

#[cfg(all(windows, target_arch = "x86_64"))]
#[tokio::test]
#[ignore = "actual host lifecycle waits for the primary trusted artifact lock"]
async fn pinned_host_ready_identity_stop_and_restart() {
    let _hooks = HookGuard::synthetic();
    set_skip_spawn(false);
    let dir = temp_dir("host");
    let state = open_state(&dir);
    set_artifact_dir(&state, require_pinned());
    set_test_password(Some("synthetic-management".to_string()));
    register_owned_host(&state);
    struct Cleanup<'a>(&'a CoreState);
    impl Drop for Cleanup<'_> {
        fn drop(&mut self) {
            if let Ok(host) = self.0.cpa_runtime.installed_host() {
                let _ = host.stop_owned();
            }
            if let Some(handle) = self.0.gateway.lock().take() {
                crate::gateway::stop_gateway(handle);
            }
        }
    }
    let _cleanup = Cleanup(&state);
    let handle =
        crate::gateway::start_gateway_on(Arc::clone(&state), "127.0.0.1:0".parse().unwrap())
            .await
            .expect("loopback listener");
    *state.gateway.lock() = Some(handle);
    let installed = install(
        &state,
        state.settings_revision(),
        state.process_generation(),
        Some("v8.0.10"),
    )
    .unwrap();
    assert!(installed.installed);
    assert!(!installed.desired_running);
    let started = start(
        &state,
        state.settings_revision(),
        state.process_generation(),
    )
    .await
    .expect("pinned host ready");
    assert!(
        started.inference_ready,
        "{}",
        started.error.unwrap_or_default()
    );
    assert_ne!(started.child_generation, state.process_generation());
    assert_eq!(started.desired_digest.len(), 64);
    let stopped = stop(
        &state,
        state.settings_revision(),
        state.process_generation(),
    )
    .unwrap();
    assert!(!stopped.running);
    assert!(!stopped.desired_running);
    let restarted = start(
        &state,
        state.settings_revision(),
        state.process_generation(),
    )
    .await
    .expect("restart");
    assert!(restarted.inference_ready);
    let _ = stop(
        &state,
        state.settings_revision(),
        state.process_generation(),
    );
    drop(_cleanup);
    drop(state);
    let _ = std::fs::remove_dir_all(&dir);
}

/// Local V4 catalog adds force every protocol off and are not sendable.
/// This writes one enabled destination model, then requires the V4 runtime
/// projection and the routing snapshot to agree before CPA apply.
fn publish_enabled_go_model(state: &CoreState) {
    let destination_id =
        ocg_domain::destination::destination_id_for_builtin(crate::provider::OPENCODE_PROVIDER_ID);
    let model = ocg_domain::destination::CatalogModel {
        public_model: "public".into(),
        upstream_model: "upstream".into(),
        protocols: Vec::new(),
        preferred: None,
        enabled: true,
        upstream_override: None,
    };
    let db = state.db.lock();
    let projection =
        crate::destination_projection::load_runtime(&db).expect("v4 destination projection");
    assert!(
        projection
            .destinations
            .iter()
            .any(|destination| destination.id == destination_id),
        "v4 projection has no OpenCode Go destination"
    );
    crate::db::destination_store::replace_destination_catalog(&db.conn, &destination_id, &[model])
        .expect("enabled go catalog");
    let snapshot = crate::routing_snapshot::RoutingSnapshot::load(&db).expect("routing snapshot");
    let destination = snapshot
        .projection
        .destinations
        .iter()
        .find(|destination| destination.id == destination_id)
        .expect("routing snapshot dropped the OpenCode Go destination");
    assert!(
        destination
            .catalog
            .iter()
            .any(|model| model.enabled && model.public_model == "public"),
        "routing snapshot catalog does not contain the enabled model"
    );
    let routable = snapshot.credentials.iter().any(|credential| {
        credential.destination_id == destination_id
            && credential.enabled
            && credential.ready
            && credential.credential_version > 0
            && !credential.binding_id.is_empty()
            && !credential.grants.allowed_endpoint_ids.is_empty()
    });
    assert!(
        routable,
        "v4 routing snapshot has no routable go credential"
    );
}

fn set_credential(state: &CoreState, legacy_id: &str, binding: &str, rank: i64, enabled: i64) {
    state
        .db
        .lock()
        .conn
        .execute(
            "UPDATE credentials
             SET binding_id = ?2, routing_rank = ?3, enabled = ?4
             WHERE legacy_account_id = ?1",
            rusqlite::params![legacy_id, binding, rank, enabled],
        )
        .unwrap();
}

fn client_intent(state: &CoreState, request_id: Uuid) {
    let (key_id, key): (String, String) = state
        .db
        .lock()
        .conn
        .query_row(
            "SELECT id, key FROM access_keys
             WHERE is_primary = 1 AND enabled = 1 AND deleted_at IS NULL",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .expect("primary access key");
    register_correlation_intent(
        state,
        CorrelationIntent {
            request_id,
            deadline: Utc::now() + chrono::Duration::seconds(30),
            requested_model: "public".into(),
            authorization: CorrelationAuthorization::Client {
                key_id,
                captured_key_fingerprint: crate::cpa_runtime::fingerprint_key(&key),
            },
        },
    )
    .expect("client intent");
}

fn register_client(state: &CoreState, request_id: Uuid, key_id: &str, secret: &str) {
    register_correlation_intent(
        state,
        CorrelationIntent {
            request_id,
            deadline: Utc::now() + chrono::Duration::seconds(30),
            requested_model: "public".into(),
            authorization: CorrelationAuthorization::Client {
                key_id: key_id.to_string(),
                captured_key_fingerprint: crate::cpa_runtime::fingerprint_key(secret),
            },
        },
    )
    .expect("client intent");
}

fn insert_sub_key(state: &CoreState, id: &str, secret: &str) {
    state
        .db
        .lock()
        .conn
        .execute(
            "INSERT INTO access_keys (id, name, key, is_primary, enabled, deleted_at, created_at)
             VALUES (?1, 'intent-client', ?2, 0, 1, NULL, ?3)",
            rusqlite::params![id, secret, Utc::now().to_rfc3339()],
        )
        .expect("sub access key");
}

fn validated_intent(
    state: &CoreState,
    request_id: Uuid,
    credential_id: &str,
    credential_version: u64,
    requested_protocol: &str,
) {
    register_correlation_intent(
        state,
        CorrelationIntent {
            request_id,
            deadline: Utc::now() + chrono::Duration::seconds(30),
            requested_model: "public".into(),
            authorization: CorrelationAuthorization::Validated {
                credential_id: credential_id.to_string(),
                credential_version,
                requested_protocol: requested_protocol.to_string(),
            },
        },
    )
    .expect("validated intent");
}

fn allow_grant(state: &CoreState, credential_id: &str) {
    let _ = state.db.lock().conn.execute(
        "INSERT INTO credential_grants (credential_id, kind, value)
         VALUES (?1, 'endpoint_id', 'synthetic-cpa')",
        [credential_id],
    );
}

fn admit_body(
    report: &super::ExecutionReport,
    request_id: Uuid,
    attempt_id: Uuid,
    stamp: &store::AuthStamp,
) -> serde_json::Value {
    serde_json::json!({
        "protocolVersion": 1,
        "operation": "admit",
        "processGeneration": report.child_generation.to_string(),
        "projectionRevision": report.applied_revision.to_string(),
        "projectionDigest": report.applied_digest,
        "requestId": request_id.to_string(),
        "attemptId": attempt_id.to_string(),
        "authId": stamp.auth_id,
        "credentialId": stamp.credential_id,
        "credentialVersion": stamp.credential_version.to_string(),
        "providerId": stamp.provider_id,
        "publicModel": "public",
        "upstreamModel": "upstream",
        "registrationEpoch": stamp.registration_epoch.to_string(),
        "materialRevision": stamp.material_revision,
        "kind": "accepted",
        "callableProtocol": "chat_completions",
        "generationKind": "execute"
    })
}

fn rejection_body(admit: &serde_json::Value, response_body: &str) -> serde_json::Value {
    let mut result = admit.clone();
    result["operation"] = serde_json::json!("result");
    if let Some(object) = result.as_object_mut() {
        object.remove("callableProtocol");
        object.remove("generationKind");
    }
    result["endpointPin"] = serde_json::Value::Null;
    result["sent"] = serde_json::json!(true);
    result["status"] = serde_json::json!(429);
    result["bodyComplete"] = serde_json::json!(true);
    result["streamStarted"] = serde_json::json!(false);
    result["outcome"] = serde_json::json!("explicit_rejection");
    result["errorCode"] = serde_json::json!("provider_rejected");
    result["responseBody"] = serde_json::json!(response_body);
    result
}

#[test]
fn correlation_uses_deadline_grace_and_bounded_capacity() {
    let dir = temp_dir("correlation-bound");
    let state = open_state(&dir);
    let expired = register_correlation(
        &state,
        Uuid::new_v4(),
        Utc::now() - chrono::Duration::seconds(120),
    );
    assert!(
        expired
            .unwrap_err()
            .to_string()
            .contains("correlation_deadline")
    );
    let request_id = Uuid::new_v4();
    let open_deadline = Utc::now() - chrono::Duration::seconds(30);
    register_correlation(&state, request_id, open_deadline)
        .expect("deadline plus grace is still open");
    for _ in 1..1024 {
        register_correlation(
            &state,
            Uuid::new_v4(),
            Utc::now() + chrono::Duration::seconds(30),
        )
        .expect("under capacity");
    }
    let full = register_correlation(
        &state,
        Uuid::new_v4(),
        Utc::now() + chrono::Duration::seconds(30),
    );
    assert!(
        full.unwrap_err()
            .to_string()
            .contains("correlation_capacity")
    );
    register_correlation(&state, request_id, open_deadline)
        .expect("identical re-register keeps the existing slot");
    let conflict = register_correlation(
        &state,
        request_id,
        Utc::now() + chrono::Duration::seconds(30),
    );
    assert!(
        conflict
            .unwrap_err()
            .to_string()
            .contains("correlation_conflict")
    );
    let still_full = register_correlation(
        &state,
        Uuid::new_v4(),
        Utc::now() + chrono::Duration::seconds(30),
    );
    assert!(
        still_full
            .unwrap_err()
            .to_string()
            .contains("correlation_capacity")
    );
    drop(state);
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn typed_client_and_validated_intent_stop_or_keep_evidence() {
    let _hooks = HookGuard::synthetic();
    let dir = temp_dir("intent");
    let state = open_state(&dir);
    let empty = register_correlation_intent(
        &state,
        CorrelationIntent {
            request_id: Uuid::new_v4(),
            deadline: Utc::now() + chrono::Duration::seconds(30),
            requested_model: "public".into(),
            authorization: CorrelationAuthorization::Client {
                key_id: String::new(),
                captured_key_fingerprint: "ab".repeat(32),
            },
        },
    );
    assert!(
        empty
            .unwrap_err()
            .to_string()
            .contains("correlation_intent")
    );
    install_listener(&state);
    let created =
        account_control::create_go_api_key(&state, "intent".into(), "sk-intent".into(), None, None)
            .unwrap();
    publish_enabled_go_model(&state);
    let applied = start(
        &state,
        state.settings_revision(),
        state.process_generation(),
    )
    .await
    .expect("apply credential");
    let stamp = preview_attempt(&state, &created.id).unwrap();
    let token = token_of(&state);
    let origin = "http://127.0.0.1:9";
    let peer: SocketAddr = "127.0.0.1:1".parse().unwrap();
    let admit = |request_id, stamp: &store::AuthStamp| {
        admit_body(&applied, request_id, Uuid::new_v4(), stamp)
    };

    let untyped = Uuid::new_v4();
    register_correlation(&state, untyped, Utc::now() + chrono::Duration::seconds(30)).unwrap();
    let (_status, body) = callback(
        &state,
        &token,
        Some(origin),
        serde_json::to_vec(&admit(untyped, &stamp)).unwrap(),
        peer,
    )
    .await;
    assert!(body.contains("\"action\":\"stop\""), "{body}");
    assert!(body.contains("\"reason\":\"request_fence\""), "{body}");
    assert!(!body.contains("\"action\":\"skip\""), "{body}");

    let (key_id, key): (String, String) = state
        .db
        .lock()
        .conn
        .query_row(
            "SELECT id, key FROM access_keys WHERE is_primary = 1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    let wrong_model = Uuid::new_v4();
    register_correlation_intent(
        &state,
        CorrelationIntent {
            request_id: wrong_model,
            deadline: Utc::now() + chrono::Duration::seconds(30),
            requested_model: "other-model".into(),
            authorization: CorrelationAuthorization::Client {
                key_id: key_id.clone(),
                captured_key_fingerprint: crate::cpa_runtime::fingerprint_key(&key),
            },
        },
    )
    .unwrap();
    let (_status, body) = callback(
        &state,
        &token,
        Some(origin),
        serde_json::to_vec(&admit(wrong_model, &stamp)).unwrap(),
        peer,
    )
    .await;
    assert!(body.contains("\"reason\":\"model_fence\""), "{body}");
    assert!(!body.contains("\"action\":\"skip\""), "{body}");

    let wrong_key = Uuid::new_v4();
    register_correlation_intent(
        &state,
        CorrelationIntent {
            request_id: wrong_key,
            deadline: Utc::now() + chrono::Duration::seconds(30),
            requested_model: "public".into(),
            authorization: CorrelationAuthorization::Client {
                key_id,
                captured_key_fingerprint: "ab".repeat(32),
            },
        },
    )
    .unwrap();
    let (_status, body) = callback(
        &state,
        &token,
        Some(origin),
        serde_json::to_vec(&admit(wrong_key, &stamp)).unwrap(),
        peer,
    )
    .await;
    assert!(body.contains("\"reason\":\"unauthorized\""), "{body}");
    assert!(!body.contains("\"action\":\"skip\""), "{body}");

    let disabled_id = Uuid::new_v4().to_string();
    insert_sub_key(&state, &disabled_id, "ocg-intent-disabled");
    let disabled_key = Uuid::new_v4();
    register_client(&state, disabled_key, &disabled_id, "ocg-intent-disabled");
    state
        .db
        .lock()
        .conn
        .execute(
            "UPDATE access_keys SET enabled = 0 WHERE id = ?1",
            [&disabled_id],
        )
        .unwrap();
    let (_status, body) = callback(
        &state,
        &token,
        Some(origin),
        serde_json::to_vec(&admit(disabled_key, &stamp)).unwrap(),
        peer,
    )
    .await;
    assert!(body.contains("\"reason\":\"unauthorized\""), "{body}");

    let kept_id = Uuid::new_v4().to_string();
    insert_sub_key(&state, &kept_id, "ocg-intent-kept");
    let kept = Uuid::new_v4();
    register_client(&state, kept, &kept_id, "ocg-intent-kept");
    let kept_admit = admit_body(&applied, kept, Uuid::new_v4(), &stamp);
    let (_status, body) = callback(
        &state,
        &token,
        Some(origin),
        serde_json::to_vec(&kept_admit).unwrap(),
        peer,
    )
    .await;
    assert!(body.contains("eligible"), "{body}");
    state
        .db
        .lock()
        .conn
        .execute(
            "UPDATE access_keys SET enabled = 0 WHERE id = ?1",
            [&kept_id],
        )
        .unwrap();
    let (_status, body) = callback(
        &state,
        &token,
        Some(origin),
        serde_json::to_vec(&rejection_body(
            &kept_admit,
            "provider rejected this attempt",
        ))
        .unwrap(),
        peer,
    )
    .await;
    assert!(body.contains("provider_rejected"), "{body}");
    assert!(!body.contains("unauthorized"), "{body}");

    let cancelled = Uuid::new_v4();
    client_intent(&state, cancelled);
    let cancelled_admit = admit_body(&applied, cancelled, Uuid::new_v4(), &stamp);
    let (_status, body) = callback(
        &state,
        &token,
        Some(origin),
        serde_json::to_vec(&cancelled_admit).unwrap(),
        peer,
    )
    .await;
    assert!(body.contains("eligible"), "{body}");
    cancel_correlation(&state, cancelled);
    let (_status, body) = callback(
        &state,
        &token,
        Some(origin),
        serde_json::to_vec(&rejection_body(
            &cancelled_admit,
            "provider rejected this attempt",
        ))
        .unwrap(),
        peer,
    )
    .await;
    assert!(body.contains("provider_rejected"), "{body}");
    assert!(!body.contains("cancelled"), "{body}");

    let refreshed = Uuid::new_v4();
    client_intent(&state, refreshed);
    let conflict = register_correlation(
        &state,
        refreshed,
        Utc::now() + chrono::Duration::seconds(40),
    );
    assert!(
        conflict
            .unwrap_err()
            .to_string()
            .contains("correlation_conflict")
    );
    let (_status, body) = callback(
        &state,
        &token,
        Some(origin),
        serde_json::to_vec(&admit(refreshed, &stamp)).unwrap(),
        peer,
    )
    .await;
    assert!(body.contains("eligible"), "{body}");

    state
        .db
        .lock()
        .conn
        .execute(
            "UPDATE credentials SET setup_step = 'key_verification' WHERE id = ?1",
            [&stamp.credential_id],
        )
        .unwrap();
    let pending_client = Uuid::new_v4();
    client_intent(&state, pending_client);
    let (_status, body) = callback(
        &state,
        &token,
        Some(origin),
        serde_json::to_vec(&admit(pending_client, &stamp)).unwrap(),
        peer,
    )
    .await;
    assert!(body.contains("\"reason\":\"unauthorized\""), "{body}");
    assert!(!body.contains("disabled"), "{body}");
    let pending_validated = Uuid::new_v4();
    validated_intent(
        &state,
        pending_validated,
        &stamp.credential_id,
        stamp.credential_version,
        "chat_completions",
    );
    let mut pending_body = admit(pending_validated, &stamp);
    pending_body["kind"] = serde_json::json!("validated");
    let (_status, body) = callback(
        &state,
        &token,
        Some(origin),
        serde_json::to_vec(&pending_body).unwrap(),
        peer,
    )
    .await;
    assert!(body.contains("\"action\":\"stop\""), "{body}");
    assert!(body.contains("\"reason\":\"disabled\""), "{body}");
    assert!(!body.contains("\"action\":\"skip\""), "{body}");
    state
        .db
        .lock()
        .conn
        .execute(
            "UPDATE credentials SET setup_step = 'ready' WHERE id = ?1",
            [&stamp.credential_id],
        )
        .unwrap();

    let fallback = Uuid::new_v4();
    validated_intent(
        &state,
        fallback,
        &stamp.credential_id,
        stamp.credential_version,
        "chat_completions",
    );
    let mut fallback_body = admit(fallback, &stamp);
    fallback_body["kind"] = serde_json::json!("validated");
    let (_status, body) = callback(
        &state,
        &token,
        Some(origin),
        serde_json::to_vec(&fallback_body).unwrap(),
        peer,
    )
    .await;
    assert!(body.contains("eligible"), "{body}");

    state
        .db
        .lock()
        .conn
        .execute(
            "UPDATE destination_models SET protocols_json = ?1 WHERE public_model = 'public'",
            [r#"["responses"]"#],
        )
        .unwrap();
    let denied_protocol = Uuid::new_v4();
    validated_intent(
        &state,
        denied_protocol,
        &stamp.credential_id,
        stamp.credential_version,
        "chat_completions",
    );
    let mut denied_body = admit(denied_protocol, &stamp);
    denied_body["kind"] = serde_json::json!("validated");
    let (_status, body) = callback(
        &state,
        &token,
        Some(origin),
        serde_json::to_vec(&denied_body).unwrap(),
        peer,
    )
    .await;
    assert!(body.contains("\"reason\":\"model_fence\""), "{body}");
    assert!(!body.contains("\"action\":\"skip\""), "{body}");
    let responses = Uuid::new_v4();
    validated_intent(
        &state,
        responses,
        &stamp.credential_id,
        stamp.credential_version,
        "responses",
    );
    let mut responses_body = admit(responses, &stamp);
    responses_body["kind"] = serde_json::json!("validated");
    let (_status, body) = callback(
        &state,
        &token,
        Some(origin),
        serde_json::to_vec(&responses_body).unwrap(),
        peer,
    )
    .await;
    assert!(body.contains("\"reason\":\"model_fence\""), "{body}");
    assert!(!body.contains("eligible"), "{body}");

    state
        .db
        .lock()
        .conn
        .execute(
            "UPDATE destination_models SET upstream_override = ?1 WHERE public_model = 'public'",
            [r#"{"protocol":"messages","endpoint_url":"https://opencode.ai/zen/go/v1/messages"}"#],
        )
        .unwrap();
    let overridden = Uuid::new_v4();
    validated_intent(
        &state,
        overridden,
        &stamp.credential_id,
        stamp.credential_version,
        "responses",
    );
    let mut overridden_body = admit(overridden, &stamp);
    overridden_body["kind"] = serde_json::json!("validated");
    let (_status, body) = callback(
        &state,
        &token,
        Some(origin),
        serde_json::to_vec(&overridden_body).unwrap(),
        peer,
    )
    .await;
    assert!(body.contains("\"reason\":\"model_fence\""), "{body}");
    let messages = Uuid::new_v4();
    validated_intent(
        &state,
        messages,
        &stamp.credential_id,
        stamp.credential_version,
        "messages",
    );
    let mut messages_body = admit(messages, &stamp);
    messages_body["kind"] = serde_json::json!("validated");
    let (_status, body) = callback(
        &state,
        &token,
        Some(origin),
        serde_json::to_vec(&messages_body).unwrap(),
        peer,
    )
    .await;
    assert!(body.contains("\"reason\":\"model_fence\""), "{body}");
    assert!(!body.contains("eligible"), "{body}");

    let wrong_version = Uuid::new_v4();
    validated_intent(
        &state,
        wrong_version,
        &stamp.credential_id,
        stamp.credential_version + 1,
        "messages",
    );
    let mut wrong_body = admit(wrong_version, &stamp);
    wrong_body["kind"] = serde_json::json!("validated");
    wrong_body["credentialVersion"] = serde_json::json!((stamp.credential_version + 1).to_string());
    let (_status, body) = callback(
        &state,
        &token,
        Some(origin),
        serde_json::to_vec(&wrong_body).unwrap(),
        peer,
    )
    .await;
    assert!(body.contains("\"reason\":\"identity_fence\""), "{body}");
    assert!(!body.contains("\"action\":\"skip\""), "{body}");

    state
        .db
        .lock()
        .conn
        .execute(
            "UPDATE destination_models
             SET protocols_json = ?1, upstream_override = NULL
             WHERE public_model = 'public'",
            [r#"["chat_completions"]"#],
        )
        .unwrap();
    let pending_account = account_control::create_go_api_key(
        &state,
        "pending-intent".into(),
        "sk-pending-intent".into(),
        None,
        None,
    )
    .unwrap();
    state
        .db
        .lock()
        .conn
        .execute(
            "UPDATE credentials SET setup_step = 'key_verification' WHERE legacy_account_id = ?1",
            [&pending_account.id],
        )
        .unwrap();
    let pending_stamp = preview_attempt(&state, &pending_account.id).unwrap();
    let unstamped = Uuid::new_v4();
    validated_intent(
        &state,
        unstamped,
        &pending_stamp.credential_id,
        pending_stamp.credential_version,
        "chat_completions",
    );
    let mut unstamped_body = admit(unstamped, &pending_stamp);
    unstamped_body["kind"] = serde_json::json!("validated");
    let (_status, body) = callback(
        &state,
        &token,
        Some(origin),
        serde_json::to_vec(&unstamped_body).unwrap(),
        peer,
    )
    .await;
    assert!(body.contains("\"action\":\"stop\""), "{body}");
    assert!(body.contains("\"reason\":\"identity_fence\""), "{body}");
    assert!(!body.contains("eligible"), "{body}");
    let pending_client_new = Uuid::new_v4();
    client_intent(&state, pending_client_new);
    let (_status, body) = callback(
        &state,
        &token,
        Some(origin),
        serde_json::to_vec(&admit(pending_client_new, &pending_stamp)).unwrap(),
        peer,
    )
    .await;
    assert!(body.contains("\"reason\":\"unauthorized\""), "{body}");
    assert!(!body.contains("\"action\":\"skip\""), "{body}");

    drop(state);
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn per_attempt_identity_survives_reload_and_fences_the_next_admit() {
    let _hooks = HookGuard::synthetic();
    let dir = temp_dir("attempt-identity");
    let state = open_state(&dir);
    install_listener(&state);
    start(
        &state,
        state.settings_revision(),
        state.process_generation(),
    )
    .await
    .expect("synthetic plane");
    let left =
        account_control::create_go_api_key(&state, "left".into(), "sk-left".into(), None, None)
            .unwrap();
    let right =
        account_control::create_go_api_key(&state, "right".into(), "sk-right".into(), None, None)
            .unwrap();
    set_credential(&state, &left.id, "bind-left", 0, 1);
    set_credential(&state, &right.id, "bind-right", 5, 1);
    let left_stamp = preview_attempt(&state, &left.id).unwrap();
    let right_stamp = preview_attempt(&state, &right.id).unwrap();
    assert_ne!(left_stamp.binding_id, right_stamp.binding_id);
    {
        let db = state.db.lock();
        assert_eq!(project::oauth_routing_rank(&db, "missing-credential"), None);
        assert_eq!(
            project::oauth_routing_rank(&db, &left_stamp.credential_id),
            Some(0)
        );
        assert_eq!(
            project::oauth_routing_rank(&db, &right_stamp.credential_id),
            Some(5)
        );
    }
    set_credential(&state, &right.id, "bind-right", 5, 0);
    assert_eq!(
        project::oauth_routing_rank(&state.db.lock(), &right_stamp.credential_id),
        None
    );
    set_credential(&state, &right.id, "bind-right", 5, 1);
    allow_grant(&state, &left_stamp.credential_id);
    allow_grant(&state, &right_stamp.credential_id);
    publish_enabled_go_model(&state);
    let applied = start(
        &state,
        state.settings_revision(),
        state.process_generation(),
    )
    .await
    .expect("project both credentials");
    let token = token_of(&state);
    let origin = "http://127.0.0.1:9";
    let peer: SocketAddr = "127.0.0.1:1".parse().unwrap();

    let cancelled_request = Uuid::new_v4();
    let cancelled_deadline = Utc::now() + chrono::Duration::seconds(30);
    register_correlation(&state, cancelled_request, cancelled_deadline).unwrap();
    cancel_correlation(&state, cancelled_request);
    register_correlation(&state, cancelled_request, cancelled_deadline).unwrap();
    let (status, body) = callback(
        &state,
        &token,
        Some(origin),
        serde_json::to_vec(&admit_body(
            &applied,
            cancelled_request,
            Uuid::new_v4(),
            &left_stamp,
        ))
        .unwrap(),
        peer,
    )
    .await;
    assert_eq!(status, 200, "{body}");
    assert!(body.contains("cancelled"), "{body}");

    let failover = Uuid::new_v4();
    client_intent(&state, failover);
    let left_attempt = Uuid::new_v4();
    let left_admit = admit_body(&applied, failover, left_attempt, &left_stamp);
    let (status, body) = callback(
        &state,
        &token,
        Some(origin),
        serde_json::to_vec(&left_admit).unwrap(),
        peer,
    )
    .await;
    assert_eq!(status, 200, "{body}");
    assert!(body.contains("eligible"), "{body}");
    let (status, body) = callback(
        &state,
        &token,
        Some(origin),
        serde_json::to_vec(&rejection_body(
            &left_admit,
            "provider rejected this attempt",
        ))
        .unwrap(),
        peer,
    )
    .await;
    assert!(body.contains("provider_rejected"), "{status} {body}");
    let right_admit = admit_body(&applied, failover, Uuid::new_v4(), &right_stamp);
    let (status, body) = callback(
        &state,
        &token,
        Some(origin),
        serde_json::to_vec(&right_admit).unwrap(),
        peer,
    )
    .await;
    assert!(body.contains("eligible"), "{status} {body}");

    let preserved = Uuid::new_v4();
    client_intent(&state, preserved);
    let preserved_attempt = Uuid::new_v4();
    let preserved_admit = admit_body(&applied, preserved, preserved_attempt, &left_stamp);
    let (status, body) = callback(
        &state,
        &token,
        Some(origin),
        serde_json::to_vec(&preserved_admit).unwrap(),
        peer,
    )
    .await;
    assert!(body.contains("eligible"), "{status} {body}");
    set_credential(&state, &left.id, "bind-rebound", 0, 1);
    let reloaded = start(
        &state,
        state.settings_revision(),
        state.process_generation(),
    )
    .await
    .expect("reload after rebind");
    let old_result = rejection_body(&preserved_admit, "provider rejected this attempt");
    let (status, body) = callback(
        &state,
        &token,
        Some(origin),
        serde_json::to_vec(&old_result).unwrap(),
        peer,
    )
    .await;
    assert!(
        !body.contains("uncorrelated"),
        "admitted result kept its attempt identity: {status} {body}"
    );
    assert!(body.contains("provider_rejected"), "{body}");
    let stale_projection = admit_body(
        &applied,
        Uuid::new_v4(),
        Uuid::new_v4(),
        &preview_attempt(&state, &left.id).unwrap(),
    );
    let fresh_request = Uuid::new_v4();
    client_intent(&state, fresh_request);
    let mut stale_projection = stale_projection;
    stale_projection["requestId"] = serde_json::json!(fresh_request.to_string());
    let (status, body) = callback(
        &state,
        &token,
        Some(origin),
        serde_json::to_vec(&stale_projection).unwrap(),
        peer,
    )
    .await;
    assert!(body.contains("projection_fence"), "{status} {body}");
    let rebound = preview_attempt(&state, &left.id).unwrap();
    let rebound_request = Uuid::new_v4();
    client_intent(&state, rebound_request);
    let (status, body) = callback(
        &state,
        &token,
        Some(origin),
        serde_json::to_vec(&admit_body(
            &reloaded,
            rebound_request,
            Uuid::new_v4(),
            &rebound,
        ))
        .unwrap(),
        peer,
    )
    .await;
    assert!(body.contains("eligible"), "{status} {body}");

    let deleted_request = Uuid::new_v4();
    client_intent(&state, deleted_request);
    let deleted_admit = admit_body(&reloaded, deleted_request, Uuid::new_v4(), &right_stamp);
    let (status, body) = callback(
        &state,
        &token,
        Some(origin),
        serde_json::to_vec(&deleted_admit).unwrap(),
        peer,
    )
    .await;
    assert!(body.contains("eligible"), "{status} {body}");
    state
        .db
        .lock()
        .conn
        .execute(
            "DELETE FROM credentials WHERE id = ?1",
            [&right_stamp.credential_id],
        )
        .unwrap();
    let (status, body) = callback(
        &state,
        &token,
        Some(origin),
        serde_json::to_vec(&rejection_body(
            &deleted_admit,
            "provider rejected this attempt",
        ))
        .unwrap(),
        peer,
    )
    .await;
    assert!(!body.contains("uncorrelated"), "{status} {body}");
    let after_delete = Uuid::new_v4();
    client_intent(&state, after_delete);
    let (status, body) = callback(
        &state,
        &token,
        Some(origin),
        serde_json::to_vec(&admit_body(
            &reloaded,
            after_delete,
            Uuid::new_v4(),
            &right_stamp,
        ))
        .unwrap(),
        peer,
    )
    .await;
    assert!(body.contains("deleted"), "{status} {body}");

    let observed = Uuid::new_v4();
    client_intent(&state, observed);
    let observed_stamp = preview_attempt(&state, &left.id).unwrap();
    let observed_admit = admit_body(&reloaded, observed, Uuid::new_v4(), &observed_stamp);
    let (status, body) = callback(
        &state,
        &token,
        Some(origin),
        serde_json::to_vec(&observed_admit).unwrap(),
        peer,
    )
    .await;
    assert!(body.contains("eligible"), "{status} {body}");
    cancel_correlation(&state, observed);
    let trusted = rejection_body(
        &observed_admit,
        "Weekly usage limit reached. Resets in 4 days.",
    );
    let (status, body) = callback(
        &state,
        &token,
        Some(origin),
        serde_json::to_vec(&trusted).unwrap(),
        peer,
    )
    .await;
    assert!(body.contains("explicit_rejection"), "{status} {body}");
    let (status, body) = callback(
        &state,
        &token,
        Some(origin),
        serde_json::to_vec(&trusted).unwrap(),
        peer,
    )
    .await;
    assert!(body.contains("uncorrelated"), "{status} {body}");
    assert_eq!(
        correlation_decision(&state, observed).unwrap().reason,
        Reason::ExplicitRejection
    );
    let blocked = Uuid::new_v4();
    client_intent(&state, blocked);
    let (status, body) = callback(
        &state,
        &token,
        Some(origin),
        serde_json::to_vec(&admit_body(
            &reloaded,
            blocked,
            Uuid::new_v4(),
            &observed_stamp,
        ))
        .unwrap(),
        peer,
    )
    .await;
    assert!(body.contains("restricted"), "{status} {body}");
    drop(state);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn contradictory_intent_and_trace_stay_immutable() {
    let dir = temp_dir("trace-conflict");
    let state = open_state(&dir);
    let request_id = Uuid::new_v4();
    let deadline = Utc::now() + chrono::Duration::seconds(30);
    let intent = CorrelationIntent {
        request_id,
        deadline,
        requested_model: "public".into(),
        authorization: CorrelationAuthorization::Client {
            key_id: "key-1".into(),
            captured_key_fingerprint: "ab".repeat(32),
        },
    };
    register_correlation_intent(&state, intent.clone()).expect("intent");
    register_correlation_intent(&state, intent.clone()).expect("identical intent");
    let mut other_model = intent.clone();
    other_model.requested_model = "other".into();
    assert!(
        register_correlation_intent(&state, other_model)
            .unwrap_err()
            .to_string()
            .contains("correlation_conflict")
    );
    let mut other_deadline = intent.clone();
    other_deadline.deadline = deadline + chrono::Duration::seconds(15);
    assert!(
        register_correlation_intent(&state, other_deadline)
            .unwrap_err()
            .to_string()
            .contains("correlation_conflict")
    );
    let mut other_key = intent.clone();
    other_key.authorization = CorrelationAuthorization::Client {
        key_id: "key-2".into(),
        captured_key_fingerprint: "ab".repeat(32),
    };
    assert!(
        register_correlation_intent(&state, other_key)
            .unwrap_err()
            .to_string()
            .contains("correlation_conflict")
    );
    {
        let inner = state.cpa_execution.inner.lock();
        let stored = inner.correlations.get(&request_id).expect("correlation");
        assert_eq!(stored.deadline, deadline);
        assert_eq!(stored.requested_model, "public");
        assert!(!stored.cancelled);
        assert!(stored.decision.is_none());
        assert!(stored.pins.lock().is_empty());
        assert!(stored.client_trace_id.is_none());
    }

    assert!(
        register_correlation_trace(&state, request_id, &request_id.to_string())
            .unwrap_err()
            .to_string()
            .contains("correlation_trace")
    );
    assert!(
        register_correlation_trace(&state, request_id, "ocg-nope")
            .unwrap_err()
            .to_string()
            .contains("correlation_trace")
    );
    let canonical = "ocg-aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";
    register_correlation_trace(
        &state,
        request_id,
        "ocg-AAAAAAAA-AAAA-4AAA-8AAA-AAAAAAAAAAAA",
    )
    .expect("canonical trace");
    register_correlation_trace(&state, request_id, canonical).expect("same trace");
    assert_eq!(
        correlation_client_trace(&state, request_id).as_deref(),
        Some(canonical)
    );
    assert!(
        register_correlation_trace(
            &state,
            request_id,
            "ocg-bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb"
        )
        .unwrap_err()
        .to_string()
        .contains("correlation_trace")
    );
    assert_eq!(
        correlation_client_trace(&state, request_id).as_deref(),
        Some(canonical)
    );
    assert!(
        register_correlation_trace(&state, Uuid::new_v4(), canonical)
            .unwrap_err()
            .to_string()
            .contains("correlation_missing")
    );
    register_correlation_intent(&state, intent.clone()).expect("trace does not refresh intent");
    {
        let inner = state.cpa_execution.inner.lock();
        let stored = inner.correlations.get(&request_id).expect("correlation");
        assert_eq!(stored.deadline, deadline);
        assert!(!stored.cancelled);
        assert!(stored.pins.lock().is_empty());
    }

    let failed = Uuid::new_v4();
    register_correlation_intent(
        &state,
        CorrelationIntent {
            request_id: failed,
            deadline,
            requested_model: "public".into(),
            authorization: CorrelationAuthorization::Validated {
                credential_id: "cred-1".into(),
                credential_version: 3,
                requested_protocol: "chat_completions".into(),
            },
        },
    )
    .expect("failed intent");
    assert!(
        register_correlation_trace(&state, failed, "not-a-public-trace")
            .unwrap_err()
            .to_string()
            .contains("correlation_trace")
    );
    assert!(correlation_client_trace(&state, failed).is_none());
    cancel_correlation(&state, failed);
    assert_eq!(correlation_cancelled(&state, failed), Some(true));
    assert_eq!(
        correlation_authorization_kind(&state, failed),
        Some("validated")
    );
    register_correlation_intent(
        &state,
        CorrelationIntent {
            request_id: failed,
            deadline,
            requested_model: "public".into(),
            authorization: CorrelationAuthorization::Validated {
                credential_id: "cred-1".into(),
                credential_version: 3,
                requested_protocol: "chat_completions".into(),
            },
        },
    )
    .expect("identical re-register keeps cancellation");
    assert_eq!(correlation_cancelled(&state, failed), Some(true));
    assert!(correlation_client_trace(&state, failed).is_none());
    drop(state);
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn admitted_pin_is_frozen_and_the_result_log_uses_the_allow() {
    let _hooks = HookGuard::synthetic();
    let dir = temp_dir("frozen-pin");
    let state = open_state(&dir);
    install_listener(&state);
    let created =
        account_control::create_go_api_key(&state, "frozen".into(), "sk-frozen".into(), None, None)
            .unwrap();
    publish_enabled_go_model(&state);
    let applied = start(
        &state,
        state.settings_revision(),
        state.process_generation(),
    )
    .await
    .expect("apply credential");
    let stamp = preview_attempt(&state, &created.id).unwrap();
    let token = token_of(&state);
    let origin = "http://127.0.0.1:9";
    let peer: SocketAddr = "127.0.0.1:1".parse().unwrap();
    let request_id = Uuid::new_v4();
    let attempt_id = Uuid::new_v4();
    client_intent(&state, request_id);
    let trace = "ocg-11111111-1111-4111-8111-111111111111";
    register_correlation_trace(&state, request_id, trace).expect("public trace");
    let admit = admit_body(&applied, request_id, attempt_id, &stamp);

    let early = rejection_body(&admit, "provider rejected this attempt");
    let (_status, body) = callback(
        &state,
        &token,
        Some(origin),
        serde_json::to_vec(&early).unwrap(),
        peer,
    )
    .await;
    assert!(body.contains("uncorrelated"), "{body}");
    assert!(frozen_pins(&state, request_id).is_empty());
    assert_eq!(forward_log_count(&state), 0);

    let mut forged_admit = admit.clone();
    forged_admit["providerId"] = serde_json::json!("forged-provider");
    let (_status, body) = callback(
        &state,
        &token,
        Some(origin),
        serde_json::to_vec(&forged_admit).unwrap(),
        peer,
    )
    .await;
    assert!(body.contains("identity_fence"), "{body}");
    assert!(frozen_pins(&state, request_id).is_empty());
    assert_eq!(forward_log_count(&state), 0);

    let (_status, body) = callback(
        &state,
        &token,
        Some(origin),
        serde_json::to_vec(&admit).unwrap(),
        peer,
    )
    .await;
    assert!(body.contains("eligible"), "{body}");
    let (key_id, key_name): (String, String) = state
        .db
        .lock()
        .conn
        .query_row(
            "SELECT id, name FROM access_keys WHERE is_primary = 1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    let pinned = frozen_pins(&state, request_id);
    assert_eq!(pinned.len(), 1);
    let pin = &pinned[0];
    assert_eq!(pin.ordinal, 1);
    assert_eq!(pin.request_id, request_id);
    assert_eq!(pin.attempt_id, attempt_id);
    assert_eq!(pin.client_trace_id.as_deref(), Some(trace));
    assert_eq!(pin.provider_id, stamp.provider_id);
    assert_eq!(pin.credential_id, stamp.credential_id);
    assert_eq!(pin.credential_version, stamp.credential_version);
    assert_eq!(pin.binding_id, stamp.binding_id);
    assert_eq!(pin.auth_id, stamp.auth_id);
    assert_eq!(pin.material_revision, stamp.material_revision);
    assert_eq!(pin.registration_epoch, stamp.registration_epoch);
    assert_eq!(pin.public_model, "public");
    assert_eq!(pin.upstream_model, "upstream");
    assert_eq!(pin.kind, crate::cpa_policy::SendKind::Accepted);
    let client_key = pin.client_key.as_ref().expect("allow-time client key");
    assert_eq!(client_key.id, key_id);
    assert_eq!(client_key.name.as_deref(), Some(key_name.as_str()));
    assert!(!key_name.is_empty());

    let conflict = register_correlation(
        &state,
        request_id,
        Utc::now() + chrono::Duration::seconds(90),
    );
    assert!(
        conflict
            .unwrap_err()
            .to_string()
            .contains("correlation_conflict")
    );
    assert_eq!(frozen_pins(&state, request_id), pinned);

    let mut forged_result = rejection_body(&admit, "provider rejected this attempt");
    forged_result["providerId"] = serde_json::json!("forged-provider");
    forged_result["reportedUsage"] = serde_json::json!({"inputTokens": 9, "outputTokens": 9});
    let (_status, body) = callback(
        &state,
        &token,
        Some(origin),
        serde_json::to_vec(&forged_result).unwrap(),
        peer,
    )
    .await;
    assert!(body.contains("identity_fence"), "{body}");
    assert_eq!(frozen_pins(&state, request_id), pinned);
    assert_eq!(forward_log_count(&state), 0);

    let mut matched = rejection_body(&admit, "provider rejected this attempt");
    matched["reportedUsage"] = serde_json::json!({"inputTokens": 3, "outputTokens": 5});
    let (_status, body) = callback(
        &state,
        &token,
        Some(origin),
        serde_json::to_vec(&matched).unwrap(),
        peer,
    )
    .await;
    assert!(!body.contains("unavailable"), "{body}");
    assert_eq!(frozen_pins(&state, request_id), pinned);
    let row = one_forward_log(&state);
    assert_eq!(row.request_id.as_deref(), Some(trace));
    assert_eq!(row.model, "public");
    assert_eq!(row.client_key_name.as_deref(), Some(key_name.as_str()));
    assert_eq!(row.prompt_tokens, 3);
    assert_eq!(row.completion_tokens, 5);
    assert_eq!(row.cost, 0.0);
    assert_eq!(row.cost_state, "unknown");
    let diagnostic: serde_json::Value =
        serde_json::from_str(row.diagnostic_json.as_deref().expect("diagnostic")).unwrap();
    assert_eq!(diagnostic["request_id"], request_id.to_string());
    assert_eq!(diagnostic["client_trace_id"], trace);
    assert_eq!(diagnostic["provider_id"], stamp.provider_id);
    assert_eq!(diagnostic["credential_id"], stamp.credential_id);
    assert_eq!(diagnostic["ordinal"], 1);
    assert_eq!(diagnostic["public_model"], "public");
    assert_eq!(diagnostic["kind"], "accepted");
    assert_eq!(diagnostic["admission"], false);
    assert_eq!(diagnostic["cost_state"], "unknown");
    assert_ne!(diagnostic["provider_id"], "forged-provider");
    let saved = row.diagnostic_json.clone();

    let mut replay = matched.clone();
    replay["reportedUsage"] = serde_json::json!({"inputTokens": 9, "outputTokens": 8});
    let (_status, body) = callback(
        &state,
        &token,
        Some(origin),
        serde_json::to_vec(&replay).unwrap(),
        peer,
    )
    .await;
    assert!(body.contains("uncorrelated"), "{body}");
    assert_eq!(forward_log_count(&state), 1);
    let again = one_forward_log(&state);
    assert_eq!(again.prompt_tokens, 3);
    assert_eq!(again.completion_tokens, 5);
    assert_eq!(again.cost, 0.0);
    assert_eq!(again.diagnostic_json, saved);
    assert_eq!(frozen_pins(&state, request_id), pinned);
    drop(state);
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn configured_http_public_echo_settles_the_captured_rate() {
    let _hooks = HookGuard::synthetic();
    let dir = temp_dir("configured-http-echo");
    let state = open_state(&dir);
    install_listener(&state);
    let legacy = "echo-http";
    let endpoint = "https://lab.example/v1/chat/completions";
    save_custom_http(&state, legacy, "synthetic-echo-key", endpoint);
    configure_echo_credits(&state, legacy, 1_000_000.0, 1_000_000.0, true);
    let (credential_id, stored_legacy) = credential_identity(&state, legacy);
    assert_ne!(credential_id, stored_legacy);
    let applied = start(
        &state,
        state.settings_revision(),
        state.process_generation(),
    )
    .await
    .expect("apply configured http credential");
    let stamp = preview_attempt(&state, legacy).expect("applied http stamp");
    assert_eq!(stamp.credential_id, credential_id);
    let token = token_of(&state);
    let origin = "http://127.0.0.1:9";
    let peer: SocketAddr = "127.0.0.1:1".parse().unwrap();

    let (request_id, attempt_id) = (Uuid::new_v4(), Uuid::new_v4());
    client_model(&state, request_id, "public");
    let admit = admit_models(
        &applied, request_id, attempt_id, &stamp, "public", "upstream",
    );
    let (_status, body) = callback(
        &state,
        &token,
        Some(origin),
        serde_json::to_vec(&admit).unwrap(),
        peer,
    )
    .await;
    assert!(body.contains("eligible"), "{body}");
    raise_echo_rate(&state, legacy);
    let echoed = success_result(&admit, "public");
    let (_status, body) = callback(
        &state,
        &token,
        Some(origin),
        serde_json::to_vec(&echoed).unwrap(),
        peer,
    )
    .await;
    assert!(body.contains("completed"), "{body}");
    assert!(!body.contains("model_fence"), "{body}");
    let (log_account, upstream, prompt, completion, status, receipt): (
        String,
        String,
        i64,
        i64,
        String,
        Option<String>,
    ) = state
        .db
        .lock()
        .conn
        .query_row(
            "SELECT account_id, upstream_model, prompt_tokens, completion_tokens, status,
                    credit_receipt_json
             FROM forward_logs WHERE error_source='cpa'",
            [],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                ))
            },
        )
        .expect("settled observation");
    assert_eq!(log_account, credential_id);
    assert_eq!(upstream, "upstream");
    assert_eq!((prompt, completion, status.as_str()), (2, 3, "success"));
    let receipt = receipt.expect("credit receipt");
    let receipt: serde_json::Value = serde_json::from_str(&receipt).unwrap();
    assert_eq!(receipt["attempt"]["accountId"], stored_legacy);
    assert_eq!(receipt["attempt"]["credentialId"], credential_id);
    assert_eq!(receipt["attempt"]["model"], "upstream");
    assert_eq!(receipt["phase"], "settled");
    assert_eq!(receipt["amount"], 5.0);
    let view = credit_view(&state, legacy);
    assert_eq!(view.remaining, 95.0);
    assert_eq!(view.pending_requests, 0);
    assert_eq!(view.unpriced_requests, 0);
    let (_status, body) = callback(
        &state,
        &token,
        Some(origin),
        serde_json::to_vec(&echoed).unwrap(),
        peer,
    )
    .await;
    assert!(body.contains("uncorrelated"), "{body}");
    assert_eq!(credit_view(&state, legacy).remaining, 95.0);
    assert_eq!(forward_log_count(&state), 1);

    let (other_request, other_attempt) = (Uuid::new_v4(), Uuid::new_v4());
    client_model(&state, other_request, "public");
    let other_admit = admit_models(
        &applied,
        other_request,
        other_attempt,
        &stamp,
        "public",
        "upstream",
    );
    let (_status, body) = callback(
        &state,
        &token,
        Some(origin),
        serde_json::to_vec(&other_admit).unwrap(),
        peer,
    )
    .await;
    assert!(body.contains("eligible"), "{body}");
    let mut foreign = echoed.clone();
    foreign["requestId"] = serde_json::json!(other_request.to_string());
    foreign["attemptId"] = serde_json::json!(other_attempt.to_string());
    foreign["upstreamModel"] = serde_json::json!("other-upstream");
    let (_status, body) = callback(
        &state,
        &token,
        Some(origin),
        serde_json::to_vec(&foreign).unwrap(),
        peer,
    )
    .await;
    assert!(body.contains("model_fence"), "{body}");
    assert_eq!(forward_log_count(&state), 1);
    assert_eq!(credit_view(&state, legacy).remaining, 95.0);

    crate::db::billing::disable_on(&state.db.lock().conn, legacy, Utc::now()).unwrap();
    configure_echo_credits(&state, legacy, 1_000_000.0, 1_000_000.0, true);
    let (replaced_request, replaced_attempt) = (Uuid::new_v4(), Uuid::new_v4());
    client_model(&state, replaced_request, "public");
    let replaced_admit = admit_models(
        &applied,
        replaced_request,
        replaced_attempt,
        &stamp,
        "public",
        "upstream",
    );
    let (_status, body) = callback(
        &state,
        &token,
        Some(origin),
        serde_json::to_vec(&replaced_admit).unwrap(),
        peer,
    )
    .await;
    assert!(body.contains("eligible"), "{body}");
    crate::db::billing::disable_on(&state.db.lock().conn, legacy, Utc::now()).unwrap();
    configure_echo_credits(&state, legacy, 1_000_000.0, 1_000_000.0, true);
    let replaced_result = success_result(&replaced_admit, "public");
    let (_status, body) = callback(
        &state,
        &token,
        Some(origin),
        serde_json::to_vec(&replaced_result).unwrap(),
        peer,
    )
    .await;
    assert!(body.contains("completed"), "{body}");
    let replaced = credit_view(&state, legacy);
    assert_eq!(replaced.remaining, 100.0);
    assert_eq!(replaced.pending_requests, 0);
    assert_eq!(replaced.unpriced_requests, 0);

    let (plain_request, plain_attempt) = (Uuid::new_v4(), Uuid::new_v4());
    client_model(&state, plain_request, "plain");
    let plain_admit = admit_models(
        &applied,
        plain_request,
        plain_attempt,
        &stamp,
        "plain",
        "plain-upstream",
    );
    let (_status, body) = callback(
        &state,
        &token,
        Some(origin),
        serde_json::to_vec(&plain_admit).unwrap(),
        peer,
    )
    .await;
    assert!(body.contains("eligible"), "{body}");
    let plain_result = success_result(&plain_admit, "plain");
    let (_status, body) = callback(
        &state,
        &token,
        Some(origin),
        serde_json::to_vec(&plain_result).unwrap(),
        peer,
    )
    .await;
    assert!(body.contains("completed"), "{body}");
    let unpriced = credit_view(&state, legacy);
    assert_eq!(unpriced.remaining, 100.0);
    assert_eq!(unpriced.pending_requests, 0);
    assert_eq!(unpriced.unpriced_requests, 1);

    crate::db::billing::disable_on(&state.db.lock().conn, legacy, Utc::now()).unwrap();
    let (open_request, open_attempt) = (Uuid::new_v4(), Uuid::new_v4());
    client_model(&state, open_request, "public");
    let open_admit = admit_models(
        &applied,
        open_request,
        open_attempt,
        &stamp,
        "public",
        "upstream",
    );
    let (_status, body) = callback(
        &state,
        &token,
        Some(origin),
        serde_json::to_vec(&open_admit).unwrap(),
        peer,
    )
    .await;
    assert!(body.contains("eligible"), "{body}");
    let open_result = success_result(&open_admit, "public");
    let (_status, body) = callback(
        &state,
        &token,
        Some(origin),
        serde_json::to_vec(&open_result).unwrap(),
        peer,
    )
    .await;
    assert!(body.contains("completed"), "{body}");
    assert!(
        crate::db::billing::read_view_on(&state.db.lock().conn, legacy, Utc::now())
            .unwrap()
            .is_none()
    );
    let open_receipt: Option<String> = state
        .db
        .lock()
        .conn
        .query_row(
            "SELECT credit_receipt_json FROM forward_logs WHERE error_source='cpa'
             ORDER BY id DESC LIMIT 1",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert!(open_receipt.is_none());
    drop(state);
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn configured_http_credit_failure_rolls_back_the_result() {
    let _hooks = HookGuard::synthetic();
    let dir = temp_dir("configured-http-credit-rollback");
    let state = open_state(&dir);
    install_listener(&state);
    let legacy = "echo-rollback";
    let endpoint = "https://lab.example/v1/chat/completions";
    save_custom_http(&state, legacy, "synthetic-rollback-key", endpoint);
    configure_echo_credits(&state, legacy, 1_000_000.0, 1_000_000.0, true);
    let applied = start(
        &state,
        state.settings_revision(),
        state.process_generation(),
    )
    .await
    .expect("apply configured http credential");
    let stamp = preview_attempt(&state, legacy).expect("applied http stamp");
    let token = token_of(&state);
    let origin = "http://127.0.0.1:9";
    let peer: SocketAddr = "127.0.0.1:1".parse().unwrap();
    let (request_id, attempt_id) = (Uuid::new_v4(), Uuid::new_v4());
    client_model(&state, request_id, "public");
    let admit = admit_models(
        &applied, request_id, attempt_id, &stamp, "public", "upstream",
    );
    let (_status, body) = callback(
        &state,
        &token,
        Some(origin),
        serde_json::to_vec(&admit).unwrap(),
        peer,
    )
    .await;
    assert!(body.contains("eligible"), "{body}");
    state
        .db
        .lock()
        .conn
        .execute_batch(
            "CREATE TRIGGER block_cpa_settlement BEFORE UPDATE ON forward_logs
             WHEN json_extract(NEW.credit_receipt_json,'$.phase')='settled'
             BEGIN SELECT RAISE(ABORT,'fixture write failure'); END;",
        )
        .unwrap();
    let echoed = success_result(&admit, "public");
    let (_status, body) = callback(
        &state,
        &token,
        Some(origin),
        serde_json::to_vec(&echoed).unwrap(),
        peer,
    )
    .await;
    assert!(body.contains("unavailable"), "{body}");
    assert_eq!(credit_view(&state, legacy).remaining, 100.0);
    assert_eq!(forward_log_count(&state), 0);
    drop(state);
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn keyless_http_adopts_the_accepted_epoch_and_refuses_a_change() {
    let _hooks = HookGuard::synthetic();
    let dir = temp_dir("keyless-http-epoch");
    let state = open_state(&dir);
    install_listener(&state);
    let legacy = "keyless-http";
    let endpoint = "https://lab.example/v1/chat/completions";
    save_custom_http(&state, legacy, "synthetic-replaced-material", endpoint);
    force_keyless_route(&state, legacy, endpoint);
    let applied = start(
        &state,
        state.settings_revision(),
        state.process_generation(),
    )
    .await
    .expect("apply keyless http credential");
    let (credential_id, _) = credential_identity(&state, legacy);
    let mut record = store::load(&state.db.lock().conn)
        .expect("record")
        .expect("applied record");
    let stamp = record
        .applied_auth
        .iter()
        .find(|stamp| stamp.credential_id == credential_id)
        .cloned()
        .expect("keyless applied stamp");
    assert_eq!(stamp.material_revision, "no-material");
    assert_eq!(stamp.registration_epoch, 0);
    let desired_epoch = record
        .desired_auth
        .iter()
        .find(|item| item.credential_id == credential_id)
        .map(|item| item.registration_epoch)
        .unwrap_or(0);
    let body = format!(
        r#"{{"authRefs":[{{"authId":"{}","credentialId":"{}","credentialVersion":"{}","materialRevision":"{}","providerId":"{}","registrationEpoch":"3"}}]}}"#,
        stamp.auth_id,
        stamp.credential_id,
        stamp.credential_version,
        stamp.material_revision,
        stamp.provider_id
    );
    ready::note_applied_keyed_epochs(&mut record, &body).expect("accepted keyed epoch");
    let applied_epoch = record
        .applied_auth
        .iter()
        .find(|item| item.credential_id == credential_id)
        .expect("applied stamp")
        .registration_epoch;
    let desired_after = record
        .desired_auth
        .iter()
        .find(|item| item.credential_id == credential_id)
        .map(|item| item.registration_epoch);
    assert_eq!(applied_epoch, 3);
    assert_eq!(desired_after, Some(desired_epoch));
    store::save(&state.db.lock().conn, &record).expect("save accepted epoch");
    state.cpa_execution.inner.lock().record = record;
    let mut stamp = stamp;
    stamp.registration_epoch = 3;
    let _ = applied;
    let token = token_of(&state);
    let origin = "http://127.0.0.1:9";
    let peer: SocketAddr = "127.0.0.1:1".parse().unwrap();
    let report = execution_report(&state);
    let (request_id, attempt_id) = (Uuid::new_v4(), Uuid::new_v4());
    client_model(&state, request_id, "public");
    let admit = admit_models(
        &report, request_id, attempt_id, &stamp, "public", "upstream",
    );
    let (_status, body) = callback(
        &state,
        &token,
        Some(origin),
        serde_json::to_vec(&admit).unwrap(),
        peer,
    )
    .await;
    assert!(body.contains("eligible"), "{body}");
    let (changed_request, changed_attempt) = (Uuid::new_v4(), Uuid::new_v4());
    client_model(&state, changed_request, "public");
    let mut changed = admit_models(
        &report,
        changed_request,
        changed_attempt,
        &stamp,
        "public",
        "upstream",
    );
    changed["registrationEpoch"] = serde_json::json!("4");
    let (_status, body) = callback(
        &state,
        &token,
        Some(origin),
        serde_json::to_vec(&changed).unwrap(),
        peer,
    )
    .await;
    assert!(body.contains("identity_fence"), "{body}");
    assert!(!body.contains("eligible"), "{body}");
    drop(state);
    let _ = std::fs::remove_dir_all(dir);
}

fn save_custom_http(state: &CoreState, id: &str, secret: &str, endpoint: &str) {
    let plan = crate::provider::builtin_provider(crate::provider::CUSTOM_PROVIDER_ID)
        .expect("custom provider");
    let now = Utc::now();
    let account = crate::models::Account {
        id: id.to_string(),
        provider_id: crate::provider::CUSTOM_PROVIDER_ID.to_string(),
        credential_kind: plan.credential_kind,
        quota_scope: plan.quota_scope,
        name: id.to_string(),
        username: None,
        password_cipher: None,
        key_cipher: state.encrypt_key(secret).expect("encrypt synthetic key"),
        enabled: true,
        account_type: crate::models::AccountType::Key,
        setup_step: crate::models::AccountSetupStep::Ready,
        referral_code: None,
        purchase_date: String::new(),
        expires_on: String::new(),
        cooldown_until: None,
        cooldown_generic_until: None,
        cooldown_5h_until: None,
        cooldown_week_until: None,
        cooldown_month_until: None,
        cooldown_free_until: None,
        last_error: None,
        auth_error: None,
        notes: None,
        created_at: now,
        updated_at: now,
    };
    let capability =
        |public_model: &str, upstream_model: &str| crate::models::AccountModelCapabilityInput {
            public_model: public_model.to_string(),
            upstream_model: upstream_model.to_string(),
            protocol: ocg_domain::catalog::UpstreamProtocolKind::ChatCompletions,
            source: None,
        };
    state
        .db
        .lock()
        .create_account_with_contract(
            &account,
            Some(&crate::models::AccountCustomConfigInput {
                endpoint_url: endpoint.to_string(),
                upstream_protocol: ocg_domain::catalog::UpstreamProtocolKind::ChatCompletions,
            }),
            &[
                capability("public", "upstream"),
                capability("plain", "plain-upstream"),
            ],
        )
        .expect("custom http account");
}

fn force_keyless_route(state: &CoreState, legacy: &str, endpoint: &str) {
    let destination_id = ocg_domain::destination::destination_id_for_custom_account(legacy);
    let routes = vec![ocg_domain::destination::HttpProtocolRoute {
        protocol: ocg_domain::catalog::UpstreamProtocolKind::ChatCompletions,
        endpoint_url: endpoint.to_string(),
        auth_scheme: ocg_domain::destination::AuthScheme::None,
    }];
    let protocols = vec![ocg_domain::catalog::UpstreamProtocolKind::ChatCompletions];
    state
        .db
        .lock()
        .conn
        .execute(
            "UPDATE destinations
             SET protocol_routes_json=?2, protocols_json=?3, base_url=?4, auth_scheme=?5
             WHERE id=?1",
            rusqlite::params![
                destination_id,
                serde_json::to_string(&routes).unwrap(),
                serde_json::to_string(&protocols).unwrap(),
                endpoint,
                ocg_domain::destination::AuthScheme::None.as_str(),
            ],
        )
        .unwrap();
    state
        .db
        .lock()
        .conn
        .execute(
            "UPDATE credentials SET key_cipher='' WHERE legacy_account_id=?1",
            [legacy],
        )
        .unwrap();
}

fn configure_echo_credits(
    state: &CoreState,
    legacy: &str,
    input_per_million: f64,
    output_per_million: f64,
    initial: bool,
) {
    let now = Utc::now();
    let configuration = crate::billing_types::CreditConfiguration {
        name: "echo".into(),
        currency: "USD".into(),
        credits_per_currency: 1.0,
        rates: vec![crate::billing_types::CreditRate {
            model: "upstream".into(),
            input_per_million,
            output_per_million,
            cache_read_per_million: None,
            cache_write_per_million: None,
        }],
        monthly: None,
        source_url: None,
    };
    let buckets = initial.then(|| {
        vec![crate::billing_types::CreditBucket {
            id: format!("echo-bucket-{}", Uuid::new_v4()),
            kind: crate::billing_types::CreditBucketKind::Manual,
            label: "echo".into(),
            granted: 100.0,
            remaining: 100.0,
            starts_at: now,
            expires_at: None,
        }]
    });
    crate::db::billing::configure_on(&state.db.lock().conn, legacy, configuration, buckets, now)
        .expect("configure credits");
}

fn raise_echo_rate(state: &CoreState, legacy: &str) {
    configure_echo_credits(state, legacy, 2_000_000.0, 2_000_000.0, false);
}

fn credential_identity(state: &CoreState, legacy: &str) -> (String, String) {
    state
        .db
        .lock()
        .conn
        .query_row(
            "SELECT id, legacy_account_id FROM credentials WHERE legacy_account_id=?1",
            [legacy],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .expect("credential identity")
}

fn credit_view(state: &CoreState, legacy: &str) -> crate::billing_types::CreditMeterView {
    crate::db::billing::read_view_on(&state.db.lock().conn, legacy, Utc::now())
        .expect("credit view")
        .expect("configured meter")
}

fn client_model(state: &CoreState, request_id: Uuid, model: &str) {
    let (key_id, key): (String, String) = state
        .db
        .lock()
        .conn
        .query_row(
            "SELECT id, key FROM access_keys
             WHERE is_primary = 1 AND enabled = 1 AND deleted_at IS NULL",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .expect("primary access key");
    register_correlation_intent(
        state,
        CorrelationIntent {
            request_id,
            deadline: Utc::now() + chrono::Duration::seconds(30),
            requested_model: model.to_string(),
            authorization: CorrelationAuthorization::Client {
                key_id,
                captured_key_fingerprint: crate::cpa_runtime::fingerprint_key(&key),
            },
        },
    )
    .expect("client intent");
}

fn admit_models(
    report: &super::ExecutionReport,
    request_id: Uuid,
    attempt_id: Uuid,
    stamp: &store::AuthStamp,
    public_model: &str,
    upstream_model: &str,
) -> serde_json::Value {
    let mut body = admit_body(report, request_id, attempt_id, stamp);
    body["publicModel"] = serde_json::json!(public_model);
    body["upstreamModel"] = serde_json::json!(upstream_model);
    body
}

fn success_result(admit: &serde_json::Value, upstream_model: &str) -> serde_json::Value {
    let mut result = rejection_body(admit, "ok");
    result["status"] = serde_json::json!(200);
    result["outcome"] = serde_json::json!("success");
    result["errorCode"] = serde_json::json!("none");
    result["upstreamModel"] = serde_json::json!(upstream_model);
    result["reportedUsage"] = serde_json::json!({"inputTokens": 2, "outputTokens": 3});
    result
}

fn frozen_pins(
    state: &CoreState,
    request_id: Uuid,
) -> Vec<crate::cpa_observation::AdmittedAttemptContext> {
    let inner = state.cpa_execution.inner.lock();
    let Some(correlation) = inner.correlations.get(&request_id) else {
        return Vec::new();
    };
    let mut pins: Vec<_> = correlation
        .pins
        .lock()
        .values()
        .map(|pinned| pinned.context.clone())
        .collect();
    pins.sort_by_key(|pin| pin.ordinal);
    pins
}

fn forward_log_count(state: &CoreState) -> i64 {
    state
        .db
        .lock()
        .conn
        .query_row("SELECT COUNT(*) FROM forward_logs", [], |row| row.get(0))
        .unwrap()
}

struct ForwardObservation {
    request_id: Option<String>,
    model: String,
    client_key_name: Option<String>,
    prompt_tokens: i64,
    completion_tokens: i64,
    cost: f64,
    cost_state: String,
    diagnostic_json: Option<String>,
}

fn one_forward_log(state: &CoreState) -> ForwardObservation {
    state
        .db
        .lock()
        .conn
        .query_row(
            "SELECT request_id, model, client_key_name, prompt_tokens, completion_tokens, cost, cost_state, diagnostic_json
             FROM forward_logs",
            [],
            |row| {
                Ok(ForwardObservation {
                    request_id: row.get(0)?,
                    model: row.get(1)?,
                    client_key_name: row.get(2)?,
                    prompt_tokens: row.get(3)?,
                    completion_tokens: row.get(4)?,
                    cost: row.get(5)?,
                    cost_state: row.get(6)?,
                    diagnostic_json: row.get(7)?,
                })
            },
        )
        .unwrap()
}

struct ToggleHost {
    running: AtomicBool,
}

impl CpaRuntimeProcessHost for ToggleHost {
    fn start_owned(&self, _spec: &CpaRuntimeProcessSpec) -> Result<(), CpaRuntimeError> {
        self.running.store(true, Ordering::SeqCst);
        Ok(())
    }

    fn stop_owned(&self) -> Result<(), CpaRuntimeError> {
        self.running.store(false, Ordering::SeqCst);
        Ok(())
    }

    fn owned_running(&self) -> bool {
        self.running.load(Ordering::SeqCst)
    }

    fn logs(&self) -> CpaRuntimeLogTail {
        CpaRuntimeLogTail {
            stdout: String::new(),
            stderr: String::new(),
        }
    }

    fn add_log_secret(&self, _secret: &CpaRuntimeSecret) {}
}

#[tokio::test]
async fn owned_inference_connection_requires_the_verified_child() {
    let rejected = verified_owned_origin("https://remote.example", 8317, "http://127.0.0.1:9");
    assert!(rejected.unwrap_err().to_string().contains("loopback"));
    let collided = verified_owned_origin("http://127.0.0.1:9", 9, "http://127.0.0.1:9");
    assert!(collided.unwrap_err().to_string().contains("public origin"));
    let shifted = verified_owned_origin("http://127.0.0.1:8317", 8400, "http://127.0.0.1:9");
    assert!(shifted.unwrap_err().to_string().contains("port"));

    let _hooks = HookGuard::synthetic();
    let dir = temp_dir("inference-hop");
    let state = open_state(&dir);
    install_listener(&state);
    let missing = owned_inference_connection(&state).unwrap_err().to_string();
    assert!(missing.contains("missing"), "{missing}");
    let report = start(
        &state,
        state.settings_revision(),
        state.process_generation(),
    )
    .await
    .expect("synthetic apply");
    let still_missing = owned_inference_connection(&state).unwrap_err().to_string();
    assert!(still_missing.contains("missing"), "{still_missing}");

    let host: Arc<dyn CpaRuntimeProcessHost> = Arc::new(ToggleHost {
        running: AtomicBool::new(true),
    });
    state.set_cpa_runtime_host(host.clone());
    let connection = owned_inference_connection(&state).expect("verified child");
    assert_eq!(connection.child_generation, report.child_generation);
    assert_eq!(connection.applied_revision, report.applied_revision);
    assert_eq!(connection.applied_digest, report.applied_digest);
    assert!(connection.base_url.starts_with("http://127.0.0.1:"));
    assert_ne!(connection.base_url, "http://127.0.0.1:9");
    let hop = std::fs::read_to_string(io::private_dir(&dir).join("hop.key")).unwrap();
    assert_eq!(connection.hop.expose(), hop.trim());
    let debug = format!("{connection:?}");
    assert!(!debug.contains(hop.trim()));
    assert!(debug.contains("[redacted]"));
    host.stop_owned().unwrap();
    let stopped = owned_inference_connection(&state).unwrap_err().to_string();
    assert!(stopped.contains("missing"), "{stopped}");
    drop(state);
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn management_secret_loads_without_an_operator_override() {
    set_skip_spawn(true);
    set_fail_ready(false);
    set_test_password(None);
    set_before_apply_commit(None);
    struct Reset;
    impl Drop for Reset {
        fn drop(&mut self) {
            set_skip_spawn(false);
            set_fail_ready(false);
            set_test_password(None);
            set_before_apply_commit(None);
        }
    }
    let _reset = Reset;
    let dir = temp_dir("management");
    let state = open_state(&dir);
    assert!(!io::private_dir(&dir).join("management.key").exists());
    install_listener(&state);
    start(
        &state,
        state.settings_revision(),
        state.process_generation(),
    )
    .await
    .expect("apply without an operator password");
    let path = io::private_dir(&dir).join("management.key");
    let file = std::fs::read_to_string(&path).unwrap();
    let hop = std::fs::read_to_string(io::private_dir(&dir).join("hop.key")).unwrap();
    assert_ne!(file.trim(), hop.trim());
    assert_ne!(file.trim(), "ocg-keyless");
    let resolved = management_password(&state).expect("stored management secret");
    match std::env::var("MANAGEMENT_PASSWORD") {
        Ok(value) if !value.trim().is_empty() => assert_eq!(resolved, value),
        _ => assert_eq!(resolved, file.trim()),
    }
    let again = std::fs::read(&path).unwrap();
    assert_eq!(again, file.into_bytes());
    let removed = remove(
        &state,
        state.settings_revision(),
        state.process_generation(),
    )
    .unwrap();
    assert_eq!(removed.apply_status, "not_prepared");
    assert!(path.is_file(), "remove keeps the private management secret");
    drop(state);
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn coherent_apply_fallback_serves_the_previous_child_until_credentials_move() {
    let _hooks = HookGuard::synthetic();
    let dir = temp_dir("coherent");
    let state = open_state(&dir);
    install_listener(&state);
    let created = account_control::create_go_api_key(
        &state,
        "coherent".into(),
        "sk-coherent".into(),
        None,
        None,
    )
    .unwrap();
    let kept = account_control::create_go_api_key(
        &state,
        "coherent-kept".into(),
        "sk-coherent-kept".into(),
        None,
        None,
    )
    .unwrap();
    publish_enabled_go_model(&state);
    let report = start(
        &state,
        state.settings_revision(),
        state.process_generation(),
    )
    .await
    .expect("apply credential");
    let stamp = preview_attempt(&state, &created.id).unwrap();
    let kept_stamp = preview_attempt(&state, &kept.id).unwrap();
    assert_ne!(stamp.credential_id, kept_stamp.credential_id);
    let host = Arc::new(ToggleHost {
        running: AtomicBool::new(true),
    });
    state.set_cpa_runtime_host(host.clone());
    let connection = owned_inference_connection(&state).expect("current applied child");
    assert_eq!(connection.applied_revision, report.applied_revision);

    set_fail_ready(true);
    let failed = start(
        &state,
        state.settings_revision(),
        state.process_generation(),
    )
    .await;
    assert!(failed.unwrap_err().to_string().contains("cpa_apply_failed"));
    set_fail_ready(false);
    let ahead = execution_report(&state);
    assert_eq!(ahead.apply_status, "applied");
    assert_eq!(ahead.applied_revision, report.applied_revision);
    assert!(ahead.desired_revision > ahead.applied_revision);
    assert_eq!(ahead.current_operation.as_deref(), Some("applied"));
    assert!(!ahead.current_operation.unwrap_or_default().contains(';'));
    host.start_owned(&CpaRuntimeProcessSpec {
        codex_device_login: false,
        executable: PathBuf::new(),
        config_path: PathBuf::new(),
        working_dir: PathBuf::new(),
        management_password: CpaRuntimeSecret::new(String::new()),
        log_secrets: Vec::new(),
    })
    .unwrap();
    let still = owned_inference_connection(&state).expect("previous coherent child");
    assert_eq!(still.applied_revision, report.applied_revision);
    assert_eq!(still.applied_digest, report.applied_digest);
    let applied_before = load_record(&state);
    let (before_version, before_auth): (i64, i64) = state
        .db
        .lock()
        .conn
        .query_row(
            "SELECT COALESCE(credential_version, 1), COALESCE(auth_state_version, 1)
             FROM credentials WHERE id = ?1",
            [&stamp.credential_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();

    let rotated = account_control::rotate_upstream_credential(
        &state,
        &stamp.credential_id,
        "sk-coherent-rotated",
        MutationCas {
            expected_revision: state.settings_revision(),
            process_generation: state.process_generation(),
        },
    )
    .unwrap();
    assert_eq!(rotated.credential_id, stamp.credential_id);
    assert!(rotated.version > stamp.credential_version);
    assert!(rotated.version > before_version as u64);
    assert!(rotated.auth_state_version > before_auth as u64);
    let applied_after = load_record(&state);
    assert_eq!(
        applied_after.applied_generation,
        applied_before.applied_generation
    );
    assert_eq!(
        applied_after.applied_revision,
        applied_before.applied_revision
    );
    assert_eq!(applied_after.applied_digest, applied_before.applied_digest);
    assert_eq!(applied_after.applied_auth, applied_before.applied_auth);
    let current = owned_inference_connection(&state).expect("transport remains the restored child");
    assert_eq!(current.applied_revision, report.applied_revision);
    assert_eq!(current.applied_digest, report.applied_digest);

    let stale_request = Uuid::new_v4();
    client_intent(&state, stale_request);
    let (_status, stale) = callback(
        &state,
        &token_of(&state),
        Some("http://127.0.0.1:9"),
        serde_json::to_vec(&admit_body(
            &execution_report(&state),
            stale_request,
            Uuid::new_v4(),
            &stamp,
        ))
        .unwrap(),
        "127.0.0.1:1".parse().unwrap(),
    )
    .await;
    assert!(stale.contains("\"reason\":\"identity_fence\""), "{stale}");
    assert!(!stale.contains("eligible"), "{stale}");
    assert!(!stale.contains("\"action\":\"allow\""), "{stale}");
    {
        let inner = state.cpa_execution.inner.lock();
        let stored = inner.correlations.get(&stale_request).expect("intent");
        assert!(stored.pins.lock().is_empty());
    }
    let kept_body = callback_stamp(&state, &kept_stamp).await;
    assert!(kept_body.contains("eligible"), "{kept_body}");
    drop(state);
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn failed_apply_restores_the_accepted_port_not_the_candidate() {
    let _hooks = HookGuard::synthetic();
    let dir = temp_dir("restore-port");
    let state = open_state(&dir);
    install_listener(&state);
    state.db.lock().set_zen_free_enabled(false).unwrap();
    let created_a = account_control::create_go_api_key(
        &state,
        "restore-a".into(),
        "sk-restore-a".into(),
        None,
        None,
    )
    .unwrap();
    publish_enabled_go_model(&state);
    let applied_a = start(
        &state,
        state.settings_revision(),
        state.process_generation(),
    )
    .await
    .expect("apply a");
    let stamp_a = preview_attempt(&state, &created_a.id).unwrap();
    let port_a = applied_a.port.expect("accepted A port");
    let occupied = TcpListener::bind(("127.0.0.1", port_a)).expect("occupy accepted A port");
    assert!(
        TcpListener::bind(("127.0.0.1", port_a)).is_err(),
        "candidate reservation must not reuse A's occupied port"
    );
    let created_b = account_control::create_go_api_key(
        &state,
        "restore-b".into(),
        "sk-restore-b".into(),
        None,
        None,
    )
    .unwrap();
    publish_enabled_go_model(&state);
    let stamp_b = preview_attempt(&state, &created_b.id).unwrap();
    set_fail_ready(true);
    let failed = start(
        &state,
        state.settings_revision(),
        state.process_generation(),
    )
    .await;
    assert!(failed.unwrap_err().to_string().contains("cpa_apply_failed"));
    set_fail_ready(false);
    let yaml = std::fs::read_to_string(io::config_path(&dir)).unwrap();
    let yaml_port = super::yaml_listen_port(&yaml).expect("restored YAML port");
    let restored = execution_report(&state);
    assert_eq!(yaml_port, port_a);
    assert_eq!(restored.port, Some(port_a));
    assert_eq!(restored.apply_status, "applied");
    assert!(restored.policy_ready);
    assert_eq!(restored.applied_revision, applied_a.applied_revision);
    assert_eq!(
        restored.base_url,
        Some(format!("http://127.0.0.1:{port_a}"))
    );
    let selected = load_record(&state);
    assert_eq!(selected.listen_port, port_a);
    assert_eq!(selected.owned_origin, format!("http://127.0.0.1:{port_a}"));
    assert!(has_credential(
        &selected.applied_auth,
        &stamp_a.credential_id
    ));
    assert!(!has_credential(
        &selected.applied_auth,
        &stamp_b.credential_id
    ));
    assert!(has_route(&selected.applied_routes, &stamp_a.credential_id));
    assert!(!has_route(&selected.applied_routes, &stamp_b.credential_id));
    drop(occupied);
    let host = Arc::new(ToggleHost {
        running: AtomicBool::new(true),
    });
    state.set_cpa_runtime_host(host.clone());
    host.start_owned(&CpaRuntimeProcessSpec {
        codex_device_login: false,
        executable: PathBuf::new(),
        config_path: PathBuf::new(),
        working_dir: PathBuf::new(),
        management_password: CpaRuntimeSecret::new(String::new()),
        log_secrets: Vec::new(),
    })
    .unwrap();
    let connection = owned_inference_connection(&state).expect("restored A connection");
    assert_eq!(connection.base_url, format!("http://127.0.0.1:{port_a}"));
    assert_eq!(connection.applied_revision, applied_a.applied_revision);
    let admitted = callback_stamp(&state, &stamp_a).await;
    assert!(admitted.contains("eligible"), "{admitted}");
    let denied = callback_stamp(&state, &stamp_b).await;
    assert!(!denied.contains("eligible"), "{denied}");
    drop(state);
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn failed_apply_persist_error_does_not_publish_ready() {
    let _hooks = HookGuard::synthetic();
    let dir = temp_dir("restore-persist");
    let state = open_state(&dir);
    install_listener(&state);
    account_control::create_go_api_key(
        &state,
        "persist-a".into(),
        "sk-persist-a".into(),
        None,
        None,
    )
    .unwrap();
    publish_enabled_go_model(&state);
    start(
        &state,
        state.settings_revision(),
        state.process_generation(),
    )
    .await
    .expect("apply a");
    let host = Arc::new(ToggleHost {
        running: AtomicBool::new(true),
    });
    state.set_cpa_runtime_host(host.clone());
    set_fail_persist(true);
    set_fail_ready(true);
    let failed = start(
        &state,
        state.settings_revision(),
        state.process_generation(),
    )
    .await;
    assert!(failed.unwrap_err().to_string().contains("cpa_apply_failed"));
    set_fail_ready(false);
    set_fail_persist(false);
    let memory = execution_report(&state);
    assert_ne!(memory.apply_status, "applied");
    assert!(!memory.inference_ready);
    assert!(!memory.policy_ready);
    assert!(memory.unavailable);
    let durable = load_record(&state);
    assert_eq!(durable.apply_status, "apply_pending");
    host.start_owned(&CpaRuntimeProcessSpec {
        codex_device_login: false,
        executable: PathBuf::new(),
        config_path: PathBuf::new(),
        working_dir: PathBuf::new(),
        management_password: CpaRuntimeSecret::new(String::new()),
        log_secrets: Vec::new(),
    })
    .unwrap();
    assert!(owned_inference_connection(&state).is_err());
    drop(state);
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn schedule_owned_apply_is_a_noop_until_the_child_is_desired() {
    let _hooks = HookGuard::synthetic();
    let dir = temp_dir("schedule");
    let state = open_state(&dir);
    let fresh = schedule_owned_apply(&state).await.expect("fresh schedule");
    assert_eq!(fresh.apply_status, "not_prepared");
    assert_eq!(fresh.desired_revision, 0);
    install_listener(&state);
    let started = start(
        &state,
        state.settings_revision(),
        state.process_generation(),
    )
    .await
    .unwrap();
    let scheduled = schedule_owned_apply(&state)
        .await
        .expect("desired schedule");
    assert!(scheduled.applied_revision > started.applied_revision);
    assert_eq!(scheduled.apply_status, "applied");
    drop(state);
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn owned_apply_keeps_a_configured_remote_integration() {
    let _hooks = HookGuard::synthetic();
    let dir = temp_dir("remote-isolation");
    let state = open_state(&dir);
    install_remote_cpa(&state, "https://remote.example");
    let before = {
        let db = state.db.lock();
        let record = db.cpa_integration().unwrap().unwrap();
        let account = db
            .get_account(ocg_domain::ids::CPA_ACCOUNT_ID)
            .unwrap()
            .unwrap();
        (
            record.base_url,
            record.management_key_cipher,
            account.key_cipher,
        )
    };
    install_listener(&state);
    start(
        &state,
        state.settings_revision(),
        state.process_generation(),
    )
    .await
    .expect("owned apply");
    let (base_url, management, inference) = {
        let db = state.db.lock();
        let record = db.cpa_integration().unwrap().unwrap();
        let account = db
            .get_account(ocg_domain::ids::CPA_ACCOUNT_ID)
            .unwrap()
            .unwrap();
        (
            record.base_url,
            record.management_key_cipher,
            account.key_cipher,
        )
    };
    assert_eq!(base_url, before.0);
    assert_eq!(base_url, "https://remote.example");
    assert_eq!(management, before.1);
    assert_eq!(inference, before.2);
    let yaml = std::fs::read_to_string(io::config_path(&dir)).unwrap();
    let hop = std::fs::read_to_string(io::private_dir(&dir).join("hop.key")).unwrap();
    assert!(yaml.contains(hop.trim()));
    assert!(!yaml.contains("synthetic-remote-inference"));
    assert!(!yaml.contains(&state.config().gateway_key) || state.config().gateway_key.is_empty());
    drop(state);
    let _ = std::fs::remove_dir_all(dir);
}

fn auth_stamp(credential_id: &str, binding_id: &str) -> store::AuthStamp {
    store::AuthStamp {
        auth_id: format!("auth-{credential_id}"),
        credential_id: credential_id.into(),
        credential_version: 1,
        binding_id: binding_id.into(),
        material_revision: "material".into(),
        provider_id: crate::provider::COMMAND_CODE_PROVIDER_ID.into(),
        registration_epoch: 0,
    }
}

fn ready_facts(state: &CoreState, binding_hint: &str) -> super::identity::LiveFacts {
    super::identity::LiveFacts {
        mode: super::identity::FenceMode::Ready,
        attempt: Some(super::identity::CapturedAttempt {
            request_id: Uuid::new_v4(),
            attempt_id: Uuid::new_v4(),
            auth_id: format!("auth-{binding_hint}"),
            credential_id: "cred-ready".into(),
            credential_version: 1,
            provider_id: crate::provider::COMMAND_CODE_PROVIDER_ID.into(),
            public_model: "public".into(),
            upstream_model: "upstream".into(),
            registration_epoch: 0,
            material_revision: "material".into(),
            kind: crate::cpa_policy::SendKind::Accepted,
            callable_protocol: "chat_completions".into(),
            generation_kind: "execute".into(),
        }),
        ready: Some(super::identity::ReadyIdentity {
            generation: 9,
            revision: 2,
            digest: "cd".repeat(32),
        }),
        deadline: Utc::now() + chrono::Duration::seconds(30),
        now: Utc::now(),
        cipher: state.cipher.clone(),
        pin: Arc::new(parking_lot::Mutex::new(std::collections::HashMap::<
            Uuid,
            super::identity::PinnedAttempt,
        >::new())),
        gate_intent: false,
        authorization: None,
        requested_model: "public".into(),
        client_trace_id: None,
        observation_fault: Arc::new(parking_lot::Mutex::new(None)),
        restriction_authority: true,
    }
}

#[test]
fn ready_fallback_picks_the_applied_auth_map() {
    let dir = temp_dir("ready-route-map");
    let state = open_state(&dir);
    let mut record = store::Record::empty();
    record.child_generation = 3;
    record.applied_generation = 9;
    record.desired_revision = 4;
    record.applied_revision = 2;
    record.desired_digest = "ab".repeat(32);
    record.applied_digest = "cd".repeat(32);
    record.apply_status = "applied".into();
    record.desired_auth = vec![auth_stamp("cred-ready", "desired-bind")];
    record.applied_auth = vec![auth_stamp("cred-ready", "applied-bind")];
    let db = state.db.lock();
    store::save(&db.conn, &record).expect("save ready record");
    let mut facts = ready_facts(&state, "ready");
    {
        let tx = db.conn.unchecked_transaction().expect("transaction");
        let policy = facts.revalidate(&tx).expect("ready fallback");
        assert_eq!(policy.applied.process_generation, 9);
        assert_eq!(policy.applied.revision, 2);
        assert_eq!(policy.attempt.expect("attempt").binding_id, "applied-bind");
    }
    record.child_generation = 9;
    record.desired_revision = record.applied_revision;
    record.desired_digest = record.applied_digest.clone();
    store::save(&db.conn, &record).expect("save desired match");
    {
        let tx = db.conn.unchecked_transaction().expect("transaction");
        let policy = facts.revalidate(&tx).expect("ready stays desired");
        assert_eq!(policy.applied.process_generation, 9);
        assert_eq!(policy.applied.revision, 2);
        assert_eq!(policy.attempt.expect("attempt").binding_id, "desired-bind");
    }
    drop(db);
    drop(state);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn admit_refresh_does_not_invent_a_binding() {
    let dir = temp_dir("refresh-unbound");
    let state = open_state(&dir);
    let db = state.db.lock();
    let tx = db.conn.unchecked_transaction().expect("transaction");
    let mut record = store::Record::empty();
    let attempt = super::identity::CapturedAttempt {
        request_id: Uuid::new_v4(),
        attempt_id: Uuid::new_v4(),
        auth_id: String::new(),
        credential_id: String::new(),
        credential_version: 0,
        provider_id: String::new(),
        public_model: String::new(),
        upstream_model: String::new(),
        registration_epoch: 0,
        material_revision: String::new(),
        kind: crate::cpa_policy::SendKind::Accepted,
        callable_protocol: String::new(),
        generation_kind: String::new(),
    };
    let _refresh = super::native::advance_refresh_on_tx(&tx, &mut record, &attempt);
    assert!(record.oauth.is_empty());
    assert!(record.applied_auth.is_empty());
    assert!(record.desired_auth.is_empty());
    assert!(record.applied_routes.is_empty());
    assert!(record.desired_routes.is_empty());
    let loaded = store::load_tx(&tx).expect("record still readable");
    assert!(loaded.oauth.is_empty());
    assert!(loaded.applied_auth.is_empty());
    assert!(loaded.desired_auth.is_empty());
    assert!(loaded.applied_routes.is_empty());
    assert!(loaded.desired_routes.is_empty());
    drop(tx);
    drop(db);
    drop(state);
    let _ = std::fs::remove_dir_all(dir);
}

fn command_code_account(
    state: &CoreState,
    legacy_id: &str,
    material: &str,
) -> crate::models::Account {
    let provider_id = crate::provider::COMMAND_CODE_PROVIDER_ID;
    let plan = crate::provider::builtin_provider(provider_id);
    crate::models::Account {
        id: legacy_id.into(),
        provider_id: provider_id.into(),
        credential_kind: plan
            .map(|plan| plan.credential_kind)
            .unwrap_or_else(crate::provider::default_credential_kind),
        quota_scope: plan
            .map(|plan| plan.quota_scope)
            .unwrap_or_else(crate::provider::default_quota_scope),
        name: legacy_id.into(),
        username: None,
        password_cipher: None,
        key_cipher: state
            .cipher
            .encrypt(material)
            .expect("encrypt synthetic key"),
        enabled: true,
        account_type: crate::models::AccountType::Key,
        setup_step: crate::models::AccountSetupStep::Ready,
        referral_code: None,
        purchase_date: String::new(),
        expires_on: String::new(),
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

fn project_command_code(state: &CoreState) -> crate::cpa_projection::ProductProjection {
    let snapshot = {
        let db = state.db.lock();
        crate::routing_snapshot::RoutingSnapshot::load(&db).expect("snapshot")
    };
    let mut config = crate::models::AppConfig::default();
    config.gateway_key = "synthetic-gateway-client-key".into();
    let presets = std::collections::BTreeMap::new();
    crate::cpa_projection::project(crate::cpa_projection::ProjectionInput {
        snapshot: &snapshot,
        config: &config,
        cipher: state.cipher.as_ref(),
        revisions: crate::cpa_projection::ProjectionRevisions {
            desired: 4,
            applied: 1,
        },
        owned_listener: "http://127.0.0.1:8317",
        owned_origins: &[],
        owned_destination_ids: &[],
        oauth_refs: &[],
        preset_ids: &presets,
        validation: crate::cpa_projection::ValidationSidecar::none(),
        runtime: crate::cpa_projection::RuntimeEnvelope {
            process_generation: 17,
            port: 8317,
            auth_dir: "owned-auth",
            policy_url: "http://127.0.0.1:9042/_internal/ocg/cpa-policy",
            policy_origin: "http://127.0.0.1:9042",
            ready_key: "synthetic-ready-key",
            hop_secret: "synthetic-private-hop",
            policy_token: "synthetic-policy-token",
        },
    })
    .expect("command code projection")
}

fn pin_applied(
    state: &CoreState,
    record: &store::Record,
    credential_id: &str,
    credential_version: u64,
    public_model: &str,
    protocol: &str,
) -> Result<(store::AuthStamp, crate::cpa_projection::NormalizedRoute), PolicyFault> {
    let db = state.db.lock();
    let tx = db.conn.unchecked_transaction().expect("pin transaction");
    super::identity::validated_route_pin_on(
        &tx,
        record,
        credential_id,
        credential_version,
        public_model,
        protocol,
        state.cipher.as_ref(),
    )
}

fn revalidate_client(
    state: &CoreState,
    credential_id: &str,
    credential_version: u64,
    auth_id: &str,
    material_revision: &str,
    public_model: &str,
    upstream_model: &str,
    key_id: &str,
    key_secret: &str,
) -> crate::cpa_policy::PolicyFacts {
    let mut facts = super::identity::LiveFacts {
        mode: super::identity::FenceMode::Applied,
        attempt: Some(super::identity::CapturedAttempt {
            request_id: Uuid::new_v4(),
            attempt_id: Uuid::new_v4(),
            auth_id: auth_id.into(),
            credential_id: credential_id.into(),
            credential_version,
            provider_id: crate::provider::COMMAND_CODE_PROVIDER_ID.into(),
            public_model: public_model.into(),
            upstream_model: upstream_model.into(),
            registration_epoch: 0,
            material_revision: material_revision.into(),
            kind: crate::cpa_policy::SendKind::Accepted,
            callable_protocol: "chat_completions".into(),
            generation_kind: "execute".into(),
        }),
        ready: None,
        deadline: Utc::now() + chrono::Duration::seconds(30),
        now: Utc::now(),
        cipher: state.cipher.clone(),
        pin: Arc::new(parking_lot::Mutex::new(std::collections::HashMap::<
            Uuid,
            super::identity::PinnedAttempt,
        >::new())),
        gate_intent: true,
        authorization: Some(CorrelationAuthorization::Client {
            key_id: key_id.into(),
            captured_key_fingerprint: crate::cpa_runtime::fingerprint_key(key_secret),
        }),
        requested_model: public_model.into(),
        client_trace_id: None,
        observation_fault: Arc::new(parking_lot::Mutex::new(None)),
        restriction_authority: true,
    };
    let db = state.db.lock();
    let tx = db.conn.unchecked_transaction().expect("client transaction");
    facts.revalidate(&tx).expect("client revalidate")
}

#[test]
fn validated_route_pin_uses_the_goat_catalog_alias() {
    let raw = ocg_domain::ids::COMMAND_CODE_GOAT_DEEPSEEK_V4_FLASH_UPSTREAM;
    let alias = ocg_domain::ids::COMMAND_CODE_GOAT_DEEPSEEK_V4_FLASH_ALIAS;
    let protocol = ocg_domain::catalog::UpstreamProtocolKind::ChatCompletions.as_str();
    let dir = temp_dir("goat-route-pin");
    let state = open_state(&dir);
    let account = command_code_account(&state, "goat-route", "synthetic-goat-route-key");
    let destination_id = ocg_domain::destination::destination_id_for_builtin(
        crate::provider::COMMAND_CODE_PROVIDER_ID,
    );
    {
        let db = state.db.lock();
        db.create_account(&account).expect("create goat account");
        crate::db::destination_store::refresh_builtin_catalog(
            &db,
            &crate::provider_contracts::ContractScope::provider(
                crate::provider::COMMAND_CODE_PROVIDER_ID,
            ),
        )
        .expect("refresh builtin catalog");
        db.conn
            .execute(
                "UPDATE credentials SET enabled = 0 WHERE legacy_account_id = ?1",
                [ocg_domain::ids::ZEN_FREE_ACCOUNT_ID],
            )
            .expect("disable zen");
        let catalog =
            crate::db::destination_store::load_destination_catalog(&db.conn, &destination_id)
                .expect("catalog");
        assert!(
            catalog.iter().any(|model| model.upstream_model == raw),
            "normal catalog is missing the raw GOAT upstream"
        );
        assert!(
            catalog.iter().all(|model| model.public_model != alias),
            "alias must come from the curated catalog, not a destination_models public name"
        );
    }
    let projection = project_command_code(&state);
    let (credential_id, credential_version): (String, u64) = {
        let db = state.db.lock();
        db.conn
            .query_row(
                "SELECT id, credential_version FROM credentials WHERE legacy_account_id = 'goat-route'",
                [],
                |row| {
                    let version: i64 = row.get(1)?;
                    Ok((row.get(0)?, u64::try_from(version).unwrap_or(0)))
                },
            )
            .expect("goat credential")
    };
    let set = projection
        .route_sets
        .iter()
        .find(|set| set.credential_id == credential_id)
        .expect("goat route set")
        .clone();
    let alias_route = set
        .routes
        .iter()
        .find(|route| {
            route.public_model == alias
                && route.upstream_model == raw
                && route.protocol == protocol
                && !route.validation_only
                && !route.endpoint_fingerprint.is_empty()
                && route.origin.contains("api.commandcode.ai")
        })
        .expect("curated alias route")
        .clone();
    let stamp = store::AuthStamp {
        auth_id: set.auth_id.clone(),
        credential_id: set.credential_id.clone(),
        credential_version: set.credential_version,
        binding_id: set.binding_id.clone(),
        material_revision: set.material_fingerprint.clone(),
        provider_id: crate::provider::COMMAND_CODE_PROVIDER_ID.into(),
        registration_epoch: 0,
    };
    assert_eq!(stamp.credential_version, credential_version);
    let mut record = store::Record::empty();
    record.apply_status = "applied".into();
    record.applied_generation = 9;
    record.applied_revision = 1;
    record.applied_digest = "cd".repeat(32);
    record.applied_auth = vec![stamp.clone()];
    record.applied_routes = vec![set];
    let (pinned_stamp, pinned_route) = pin_applied(
        &state,
        &record,
        &credential_id,
        credential_version,
        alias,
        protocol,
    )
    .expect("alias pin");
    assert_eq!(pinned_stamp.auth_id, stamp.auth_id);
    assert_eq!(pinned_stamp.binding_id, stamp.binding_id);
    assert_eq!(pinned_route.public_model, alias);
    assert_eq!(pinned_route.upstream_model, raw);
    assert_eq!(
        pinned_route.endpoint_fingerprint,
        alias_route.endpoint_fingerprint
    );
    assert!(pinned_route.origin.contains("api.commandcode.ai"));

    {
        let db = state.db.lock();
        let removed = db
            .conn
            .execute(
                "DELETE FROM credential_grants
                 WHERE credential_id = ?1 AND kind = 'endpoint_id' AND value = ?2",
                rusqlite::params![credential_id, alias_route.endpoint_id],
            )
            .expect("delete chat grant");
        assert!(removed >= 1, "chat endpoint grant was not stored");
    }
    assert!(matches!(
        pin_applied(
            &state,
            &record,
            &credential_id,
            credential_version,
            alias,
            protocol
        ),
        Err(PolicyFault::Unavailable)
    ));
    {
        let db = state.db.lock();
        db.conn
            .execute(
                "INSERT INTO credential_grants (credential_id, kind, value) VALUES (?1, 'endpoint_id', ?2)",
                rusqlite::params![credential_id, alias_route.endpoint_id],
            )
            .expect("restore chat grant");
    }
    pin_applied(
        &state,
        &record,
        &credential_id,
        credential_version,
        alias,
        protocol,
    )
    .expect("restored grant pins");

    let probed = format!(
        "{}{}?probe=1",
        crate::provider::COMMAND_CODE_GOAT_BASE_URL,
        crate::provider::COMMAND_CODE_GOAT_CHAT_COMPLETIONS_PATH
    );
    let override_json = serde_json::to_string(&ocg_domain::dynamic::DynamicModelUpstreamOverride {
        protocol: ocg_domain::catalog::UpstreamProtocolKind::ChatCompletions,
        endpoint_url: probed,
    })
    .expect("override json");
    {
        let db = state.db.lock();
        db.conn
            .execute(
                "UPDATE destination_models SET upstream_override = ?1
                 WHERE destination_id = ?2 AND upstream_model = ?3",
                rusqlite::params![override_json, destination_id, raw],
            )
            .expect("set query override");
    }
    assert!(matches!(
        pin_applied(
            &state,
            &record,
            &credential_id,
            credential_version,
            alias,
            protocol
        ),
        Err(PolicyFault::Unavailable)
    ));
    {
        let db = state.db.lock();
        db.conn
            .execute(
                "UPDATE destination_models SET upstream_override = NULL
                 WHERE destination_id = ?1 AND upstream_model = ?2",
                rusqlite::params![destination_id, raw],
            )
            .expect("clear query override");
    }
    assert!(matches!(
        pin_applied(
            &state,
            &record,
            "other-goat-credential",
            credential_version,
            alias,
            protocol
        ),
        Err(PolicyFault::Unavailable)
    ));
    pin_applied(
        &state,
        &record,
        &credential_id,
        credential_version,
        alias,
        protocol,
    )
    .expect("original credential still pins");

    // Client exclusion stays on the ready key. A later managed disable is not a client admit.
    let key_id = "goat-client-key";
    let key_secret = "synthetic-goat-client-secret";
    insert_sub_key(&state, key_id, key_secret);
    let mut client_record = record.clone();
    let mut chat = alias_route.clone();
    chat.validation_only = true;
    client_record.applied_routes[0].routes = vec![chat];
    {
        let db = state.db.lock();
        store::save(&db.conn, &client_record).expect("save validation-only client route");
    }
    let stopped = revalidate_client(
        &state,
        &credential_id,
        credential_version,
        &stamp.auth_id,
        &stamp.material_revision,
        alias,
        raw,
        key_id,
        key_secret,
    );
    assert_eq!(stopped.caller_stop, Some(Reason::ModelFence));
    client_record.applied_routes[0].routes[0].validation_only = false;
    {
        let db = state.db.lock();
        store::save(&db.conn, &client_record).expect("save client route");
    }
    let admitted = revalidate_client(
        &state,
        &credential_id,
        credential_version,
        &stamp.auth_id,
        &stamp.material_revision,
        alias,
        raw,
        key_id,
        key_secret,
    );
    assert_eq!(admitted.caller_stop, None);

    let version_before: i64 = state
        .db
        .lock()
        .conn
        .query_row(
            "SELECT credential_version FROM credentials WHERE id = ?1",
            [&credential_id],
            |row| row.get(0),
        )
        .expect("version");
    state
        .db
        .lock()
        .conn
        .execute(
            "UPDATE credentials
             SET enabled = 0, account_type = 'managed', setup_step = 'key_verification'
             WHERE id = ?1",
            [&credential_id],
        )
        .expect("managed candidate");
    let version_after: i64 = state
        .db
        .lock()
        .conn
        .query_row(
            "SELECT credential_version FROM credentials WHERE id = ?1",
            [&credential_id],
            |row| row.get(0),
        )
        .expect("version after");
    assert_eq!(version_before, version_after);
    let chat_index = record.applied_routes[0]
        .routes
        .iter()
        .position(|route| route.public_model == alias && route.protocol == protocol)
        .expect("chat alias route");
    record.applied_routes[0].routes[chat_index].validation_only = true;
    let (managed_stamp, managed_route) = pin_applied(
        &state,
        &record,
        &credential_id,
        credential_version,
        alias,
        protocol,
    )
    .expect("managed validation route");
    assert_eq!(managed_stamp.credential_id, credential_id);
    assert!(managed_route.validation_only);
    assert_eq!(managed_route.upstream_model, raw);

    crate::db::destination_store::replace_destination_catalog(
        &state.db.lock().conn,
        &destination_id,
        &[ocg_domain::destination::CatalogModel {
            public_model: alias.into(),
            upstream_model: alias.into(),
            protocols: vec![ocg_domain::catalog::UpstreamProtocolKind::ChatCompletions],
            preferred: None,
            enabled: true,
            upstream_override: None,
        }],
    )
    .expect("fake alias catalog");
    assert!(matches!(
        pin_applied(
            &state,
            &record,
            &credential_id,
            credential_version,
            alias,
            protocol
        ),
        Err(PolicyFault::Unavailable)
    ));
    drop(state);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn pending_oauth_cannot_admit_or_grant_restriction_authority() {
    let dir = temp_dir("pending-oauth");
    let state = open_state(&dir);
    let account = command_code_account(&state, "pending-oauth", "synthetic-pending-oauth-key");
    {
        let db = state.db.lock();
        db.create_account(&account).expect("create account");
    }
    let (credential_id, credential_version): (String, u64) = {
        let db = state.db.lock();
        db.conn
            .query_row(
                "SELECT id, credential_version FROM credentials WHERE legacy_account_id = 'pending-oauth'",
                [],
                |row| {
                    let version: i64 = row.get(1)?;
                    Ok((row.get(0)?, u64::try_from(version).unwrap_or(0)))
                },
            )
            .expect("credential")
    };
    let mut record = store::Record::empty();
    record.child_generation = 4;
    record.applied_generation = 4;
    record.desired_revision = 2;
    record.applied_revision = 2;
    record.desired_digest = "ab".repeat(32);
    record.applied_digest = "ab".repeat(32);
    record.apply_status = "applied".into();
    record.oauth.push(store::OAuthStamp {
        relative_path: "pending.json".into(),
        auth_id: "pending-auth".into(),
        credential_id: credential_id.clone(),
        credential_version,
        material_revision: "material".into(),
        provider_id: crate::provider::COMMAND_CODE_PROVIDER_ID.into(),
        native_provider: "codex".into(),
        registration_epoch: 1,
        models: Vec::new(),
        presence: store::OAuthPresence::Pending,
        recovery: String::new(),
        raw_provider_label: String::new(),
        native_mode: String::new(),
        reported_base: String::new(),
    });
    {
        let db = state.db.lock();
        store::save(&db.conn, &record).expect("save pending stamp");
    }
    let key_id = "pending-client";
    let key_secret = "synthetic-pending-client";
    insert_sub_key(&state, key_id, key_secret);
    let mut facts = super::identity::LiveFacts {
        mode: super::identity::FenceMode::Applied,
        attempt: Some(super::identity::CapturedAttempt {
            request_id: Uuid::new_v4(),
            attempt_id: Uuid::new_v4(),
            auth_id: "pending-auth".into(),
            credential_id: credential_id.clone(),
            credential_version,
            provider_id: crate::provider::COMMAND_CODE_PROVIDER_ID.into(),
            public_model: "public".into(),
            upstream_model: "upstream".into(),
            registration_epoch: 1,
            material_revision: "material".into(),
            kind: crate::cpa_policy::SendKind::Accepted,
            callable_protocol: "chat_completions".into(),
            generation_kind: "execute".into(),
        }),
        ready: None,
        deadline: Utc::now() + chrono::Duration::seconds(30),
        now: Utc::now(),
        cipher: state.cipher.clone(),
        pin: Arc::new(parking_lot::Mutex::new(std::collections::HashMap::new())),
        gate_intent: true,
        authorization: Some(CorrelationAuthorization::Client {
            key_id: key_id.into(),
            captured_key_fingerprint: crate::cpa_runtime::fingerprint_key(key_secret),
        }),
        requested_model: "public".into(),
        client_trace_id: None,
        observation_fault: Arc::new(parking_lot::Mutex::new(None)),
        restriction_authority: true,
    };
    let policy = {
        let db = state.db.lock();
        let tx = db.conn.unchecked_transaction().expect("transaction");
        facts.revalidate(&tx).expect("revalidate")
    };
    assert_eq!(policy.caller_stop, Some(Reason::IdentityFence));
    let current = policy.current.expect("current credential");
    assert!(!current.granted);
    assert!(!facts.restriction_authority);
    assert!(facts.pin.lock().is_empty());
    assert!(record.applied_routes.is_empty());
    assert!(record.desired_routes.is_empty());
    assert!(record.host_capabilities.is_empty());
    let saved = {
        let db = state.db.lock();
        store::load(&db.conn).expect("load").expect("record")
    };
    assert_eq!(saved.oauth.len(), 1);
    assert_eq!(saved.oauth[0].presence, store::OAuthPresence::Pending);
    assert!(saved.applied_routes.is_empty());
    drop(state);
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn keyed_host_registration_epoch_admits_and_a_newer_epoch_fences() {
    let _hooks = HookGuard::synthetic();
    let dir = temp_dir("keyed-host-epoch");
    let state = open_state(&dir);
    install_listener(&state);
    let created =
        account_control::create_go_api_key(&state, "epoch".into(), "sk-epoch".into(), None, None)
            .unwrap();
    publish_enabled_go_model(&state);
    let applied = start(
        &state,
        state.settings_revision(),
        state.process_generation(),
    )
    .await
    .expect("apply credential");
    let mut stamp = preview_attempt(&state, &created.id).unwrap();
    assert_eq!(stamp.registration_epoch, 0);
    {
        let db = state.db.lock();
        let mut record = store::load(&db.conn).unwrap().unwrap();
        for saved in record
            .applied_auth
            .iter_mut()
            .chain(record.desired_auth.iter_mut())
        {
            if saved.credential_id == stamp.credential_id {
                saved.registration_epoch = 1;
            }
        }
        store::save(&db.conn, &record).unwrap();
    }
    stamp.registration_epoch = 1;
    let token = token_of(&state);
    let origin = "http://127.0.0.1:9";
    let peer: SocketAddr = "127.0.0.1:1".parse().unwrap();
    let current_request = Uuid::new_v4();
    client_intent(&state, current_request);
    let (_status, body) = callback(
        &state,
        &token,
        Some(origin),
        serde_json::to_vec(&admit_body(
            &applied,
            current_request,
            Uuid::new_v4(),
            &stamp,
        ))
        .unwrap(),
        peer,
    )
    .await;
    assert!(body.contains("eligible"), "{body}");

    stamp.registration_epoch = 2;
    let newer = Uuid::new_v4();
    client_intent(&state, newer);
    let (_status, body) = callback(
        &state,
        &token,
        Some(origin),
        serde_json::to_vec(&admit_body(&applied, newer, Uuid::new_v4(), &stamp)).unwrap(),
        peer,
    )
    .await;
    assert!(body.contains("\"reason\":\"identity_fence\""), "{body}");
    assert!(!body.contains("eligible"), "{body}");
    drop(state);
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn client_translation_admits_responses_and_messages_on_the_chat_route() {
    let _hooks = HookGuard::synthetic();
    let dir = temp_dir("client-translation");
    let state = open_state(&dir);
    install_listener(&state);
    let created = account_control::create_go_api_key(
        &state,
        "translate".into(),
        "sk-translate".into(),
        None,
        None,
    )
    .unwrap();
    publish_enabled_go_model(&state);
    let applied = start(
        &state,
        state.settings_revision(),
        state.process_generation(),
    )
    .await
    .expect("apply credential");
    let stamp = preview_attempt(&state, &created.id).unwrap();
    {
        let db = state.db.lock();
        let mut record = store::load(&db.conn).unwrap().unwrap();
        for set in record
            .applied_routes
            .iter_mut()
            .chain(record.desired_routes.iter_mut())
        {
            set.routes
                .retain(|route| route.protocol == "chat_completions");
        }
        assert!(
            record.applied_routes.iter().any(|set| {
                set.routes.iter().any(|route| {
                    route.public_model == "public" && route.protocol == "chat_completions"
                })
            }),
            "chat route missing"
        );
        store::save(&db.conn, &record).unwrap();
    }
    let token = token_of(&state);
    let origin = "http://127.0.0.1:9";
    let peer: SocketAddr = "127.0.0.1:1".parse().unwrap();
    let admit = |protocol: &str, upstream: &str, kind: &str| {
        let request_id = Uuid::new_v4();
        client_intent(&state, request_id);
        let mut body = admit_body(&applied, request_id, Uuid::new_v4(), &stamp);
        body["callableProtocol"] = serde_json::json!(protocol);
        body["upstreamModel"] = serde_json::json!(upstream);
        body["kind"] = serde_json::json!(kind);
        body
    };
    for protocol in ["responses", "messages", "chat_completions"] {
        let (_status, body) = callback(
            &state,
            &token,
            Some(origin),
            serde_json::to_vec(&admit(protocol, "upstream", "accepted")).unwrap(),
            peer,
        )
        .await;
        assert!(body.contains("eligible"), "{protocol}: {body}");
        assert!(!body.contains("model_fence"), "{protocol}: {body}");
    }
    let (_status, body) = callback(
        &state,
        &token,
        Some(origin),
        serde_json::to_vec(&admit("responses", "other-upstream", "accepted")).unwrap(),
        peer,
    )
    .await;
    assert!(body.contains("\"reason\":\"model_fence\""), "{body}");
    assert!(!body.contains("eligible"), "{body}");
    let validated = Uuid::new_v4();
    validated_intent(
        &state,
        validated,
        &stamp.credential_id,
        stamp.credential_version,
        "responses",
    );
    let mut validated_body = admit_body(&applied, validated, Uuid::new_v4(), &stamp);
    validated_body["kind"] = serde_json::json!("validated");
    validated_body["callableProtocol"] = serde_json::json!("responses");
    let (_status, body) = callback(
        &state,
        &token,
        Some(origin),
        serde_json::to_vec(&validated_body).unwrap(),
        peer,
    )
    .await;
    assert!(body.contains("\"reason\":\"model_fence\""), "{body}");
    assert!(!body.contains("eligible"), "{body}");
    drop(state);
    let _ = std::fs::remove_dir_all(dir);
}

fn load_record(state: &CoreState) -> store::Record {
    store::load(&state.db.lock().conn)
        .expect("load")
        .expect("record")
}

fn has_credential(stamps: &[store::AuthStamp], credential_id: &str) -> bool {
    stamps
        .iter()
        .any(|stamp| stamp.credential_id == credential_id)
}

fn has_route(routes: &[crate::cpa_projection::CredentialRouteSet], credential_id: &str) -> bool {
    routes.iter().any(|set| set.credential_id == credential_id)
}

async fn callback_stamp(state: &CoreState, stamp: &store::AuthStamp) -> String {
    let report = execution_report(state);
    let request_id = Uuid::new_v4();
    client_intent(state, request_id);
    let (_status, body) = callback(
        state,
        &token_of(state),
        Some("http://127.0.0.1:9"),
        serde_json::to_vec(&admit_body(&report, request_id, Uuid::new_v4(), stamp)).unwrap(),
        "127.0.0.1:1".parse().unwrap(),
    )
    .await;
    body
}

#[tokio::test]
async fn accepted_history_rollback_restores_the_selected_map() {
    let _hooks = HookGuard::synthetic();
    let dir = temp_dir("history-map");
    let state = open_state(&dir);
    install_listener(&state);
    state.db.lock().set_zen_free_enabled(false).unwrap();
    let created_a = account_control::create_go_api_key(
        &state,
        "history-a".into(),
        "sk-history-a".into(),
        None,
        None,
    )
    .unwrap();
    publish_enabled_go_model(&state);
    start(
        &state,
        state.settings_revision(),
        state.process_generation(),
    )
    .await
    .expect("apply a");
    let stamp_a = preview_attempt(&state, &created_a.id).unwrap();
    let created_b = account_control::create_go_api_key(
        &state,
        "history-b".into(),
        "sk-history-b".into(),
        None,
        None,
    )
    .unwrap();
    publish_enabled_go_model(&state);
    let applied_b = start(
        &state,
        state.settings_revision(),
        state.process_generation(),
    )
    .await
    .expect("apply b");
    let stamp_b = preview_attempt(&state, &created_b.id).unwrap();
    assert_ne!(stamp_a.credential_id, stamp_b.credential_id);
    let record_b = load_record(&state);
    let snapshot = record_b.previous_accepted.clone().expect("accepted A");
    assert!(has_credential(&snapshot.auth, &stamp_a.credential_id));
    assert!(!has_credential(&snapshot.auth, &stamp_b.credential_id));
    assert!(has_route(&snapshot.routes, &stamp_a.credential_id));
    assert!(!has_route(&snapshot.routes, &stamp_b.credential_id));
    assert!(has_credential(
        &record_b.applied_auth,
        &stamp_b.credential_id
    ));
    assert!(has_route(&record_b.applied_routes, &stamp_b.credential_id));
    assert_ne!(snapshot.routes, record_b.applied_routes);
    let previous_text = std::fs::read_to_string(io::previous_config_path(&dir)).unwrap();
    assert_eq!(
        crate::cpa_projection::wire_digest(&previous_text),
        snapshot.wire_digest
    );
    let current_b = std::fs::read(io::config_path(&dir)).unwrap();

    set_fail_ready(true);
    let failed = rollback(
        &state,
        state.settings_revision(),
        state.process_generation(),
    )
    .await
    .unwrap_err();
    assert!(failed.to_string().contains("cpa_apply_failed"), "{failed}");
    set_fail_ready(false);
    let restored = load_record(&state);
    assert_eq!(restored.apply_status, "applied");
    assert!(restored.policy_ready);
    assert!(has_credential(
        &restored.applied_auth,
        &stamp_b.credential_id
    ));
    assert_eq!(std::fs::read(io::config_path(&dir)).unwrap(), current_b);

    let hooked = Arc::clone(&state);
    set_before_apply_commit(Some(Arc::new(move || {
        hooked.bump_settings_revision();
    })));
    let conflict = rollback(
        &state,
        state.settings_revision(),
        state.process_generation(),
    )
    .await
    .unwrap_err();
    assert_eq!(conflict.to_string(), "revisionConflict");
    set_before_apply_commit(None);
    let still_b = load_record(&state);
    assert_eq!(still_b.apply_status, "applied");
    assert!(still_b.policy_ready);
    assert!(has_credential(
        &still_b.applied_auth,
        &stamp_a.credential_id
    ));
    assert!(has_credential(
        &still_b.applied_auth,
        &stamp_b.credential_id
    ));
    assert_eq!(std::fs::read(io::config_path(&dir)).unwrap(), current_b);

    let previous_bytes = std::fs::read(io::previous_config_path(&dir)).unwrap();
    let mut tampered = previous_bytes.clone();
    tampered.push(b'\n');
    std::fs::write(io::previous_config_path(&dir), &tampered).unwrap();
    let before = load_record(&state);
    let refused = rollback(
        &state,
        state.settings_revision(),
        state.process_generation(),
    )
    .await
    .unwrap_err();
    assert!(matches!(refused, ExecutionError::RollbackUnavailable));
    assert_eq!(load_record(&state), before);
    assert_eq!(std::fs::read(io::config_path(&dir)).unwrap(), current_b);
    std::fs::write(io::previous_config_path(&dir), &previous_bytes).unwrap();

    let rolled = rollback(
        &state,
        state.settings_revision(),
        state.process_generation(),
    )
    .await
    .expect("rollback to A");
    assert_eq!(rolled.apply_status, "applied");
    assert!(rolled.policy_ready);
    assert!(rolled.applied_revision < applied_b.applied_revision);
    let selected = load_record(&state);
    assert!(has_credential(
        &selected.applied_auth,
        &stamp_a.credential_id
    ));
    assert!(!has_credential(
        &selected.applied_auth,
        &stamp_b.credential_id
    ));
    assert!(has_route(&selected.applied_routes, &stamp_a.credential_id));
    assert!(!has_route(&selected.applied_routes, &stamp_b.credential_id));
    let published_a = selected
        .applied_auth
        .iter()
        .find(|stamp| stamp.credential_id == stamp_a.credential_id)
        .cloned()
        .unwrap();
    let admitted = callback_stamp(&state, &published_a).await;
    assert!(admitted.contains("eligible"), "{admitted}");
    let denied = callback_stamp(&state, &stamp_b).await;
    assert!(!denied.contains("eligible"), "{denied}");

    account_control::rotate_upstream_credential(
        &state,
        &published_a.credential_id,
        "sk-history-a-rotated",
        MutationCas {
            expected_revision: state.settings_revision(),
            process_generation: state.process_generation(),
        },
    )
    .unwrap();
    let stale = callback_stamp(&state, &published_a).await;
    assert!(stale.contains("stale_credential"), "{stale}");
    assert!(!stale.contains("eligible"), "{stale}");
    drop(state);
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn accepted_history_rollback_refuses_unbound_bytes() {
    let _hooks = HookGuard::synthetic();
    let dir = temp_dir("history-unbound");
    let state = open_state(&dir);
    install_listener(&state);
    state.db.lock().set_zen_free_enabled(false).unwrap();
    let created = account_control::create_go_api_key(
        &state,
        "history-unbound".into(),
        "sk-history-unbound".into(),
        None,
        None,
    )
    .unwrap();
    publish_enabled_go_model(&state);
    start(
        &state,
        state.settings_revision(),
        state.process_generation(),
    )
    .await
    .expect("first plane");
    start(
        &state,
        state.settings_revision(),
        state.process_generation(),
    )
    .await
    .expect("second plane");
    let _ = created;
    std::fs::write(io::previous_config_path(&dir), [0xff, 0xfe]).unwrap();
    let before = load_record(&state);
    let current = std::fs::read(io::config_path(&dir)).unwrap();
    let invalid = rollback(
        &state,
        state.settings_revision(),
        state.process_generation(),
    )
    .await
    .unwrap_err();
    assert!(matches!(invalid, ExecutionError::Invalid(_)));
    assert_eq!(load_record(&state), before);
    assert_eq!(std::fs::read(io::config_path(&dir)).unwrap(), current);
    drop(state);
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn accepted_history_rollback_refuses_a_foreign_artifact_before_mutation() {
    let _hooks = HookGuard::synthetic();
    let dir = temp_dir("history-artifact");
    let state = open_state(&dir);
    install_listener(&state);
    state.db.lock().set_zen_free_enabled(false).unwrap();
    account_control::create_go_api_key(
        &state,
        "artifact-a".into(),
        "sk-artifact-a".into(),
        None,
        None,
    )
    .unwrap();
    publish_enabled_go_model(&state);
    start(
        &state,
        state.settings_revision(),
        state.process_generation(),
    )
    .await
    .expect("apply a");
    account_control::create_go_api_key(
        &state,
        "artifact-b".into(),
        "sk-artifact-b".into(),
        None,
        None,
    )
    .unwrap();
    publish_enabled_go_model(&state);
    start(
        &state,
        state.settings_revision(),
        state.process_generation(),
    )
    .await
    .expect("apply b");
    let current = std::fs::read(io::config_path(&dir)).unwrap();
    let previous = std::fs::read(io::previous_config_path(&dir)).unwrap();
    let mut record = load_record(&state);
    let snapshot = record.previous_accepted.as_mut().expect("accepted A");
    let foreign = if snapshot.artifact_sha256.starts_with('a') {
        "b".repeat(64)
    } else {
        "a".repeat(64)
    };
    assert_ne!(foreign, record.artifact_sha256);
    snapshot.artifact_sha256 = foreign;
    store::save(&state.db.lock().conn, &record).unwrap();
    let before = load_record(&state);
    let refused = rollback(
        &state,
        state.settings_revision(),
        state.process_generation(),
    )
    .await
    .unwrap_err();
    assert!(matches!(refused, ExecutionError::RollbackUnavailable));
    assert_eq!(load_record(&state), before);
    assert_eq!(std::fs::read(io::config_path(&dir)).unwrap(), current);
    assert_eq!(
        std::fs::read(io::previous_config_path(&dir)).unwrap(),
        previous
    );
    drop(state);
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn accepted_history_invalid_epoch_is_not_admitted() {
    let _hooks = HookGuard::synthetic();
    let dir = temp_dir("history-epoch");
    let state = open_state(&dir);
    install_listener(&state);
    state.db.lock().set_zen_free_enabled(false).unwrap();
    let created = account_control::create_go_api_key(
        &state,
        "history-epoch".into(),
        "sk-history-epoch".into(),
        None,
        None,
    )
    .unwrap();
    publish_enabled_go_model(&state);
    start(
        &state,
        state.settings_revision(),
        state.process_generation(),
    )
    .await
    .expect("apply epoch credential");
    let stamp = preview_attempt(&state, &created.id).unwrap();
    let mut record = load_record(&state);
    for saved in record
        .applied_auth
        .iter_mut()
        .chain(record.desired_auth.iter_mut())
    {
        if saved.credential_id == stamp.credential_id {
            saved.registration_epoch = 7;
        }
    }
    let body = format!(
        r#"{{"authRefs":[{{"relativePath":1,"authId":"{}","credentialId":"{}","credentialVersion":"{}","materialRevision":"{}","providerId":"{}","registrationEpoch":"7"}}]}}"#,
        stamp.auth_id,
        stamp.credential_id,
        stamp.credential_version,
        stamp.material_revision,
        stamp.provider_id
    );
    let error = ready::note_keyed_registration_epochs(&mut record.applied_auth, &body).unwrap_err();
    assert!(
        error
            .to_string()
            .contains("keyed registration evidence is invalid")
    );
    assert!(
        record
            .applied_auth
            .iter()
            .find(|saved| saved.credential_id == stamp.credential_id)
            .unwrap()
            .registration_epoch
            == 0
    );
    store::save(&state.db.lock().conn, &record).unwrap();
    state.cpa_execution.inner.lock().record = record;
    let mut stale = stamp;
    stale.registration_epoch = 7;
    let body = callback_stamp(&state, &stale).await;
    assert!(body.contains("\"reason\":\"identity_fence\""), "{body}");
    assert!(!body.contains("eligible"), "{body}");
    drop(state);
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn accepted_history_failed_candidate_is_not_granted() {
    let _hooks = HookGuard::synthetic();
    let dir = temp_dir("history-failed");
    let state = open_state(&dir);
    install_listener(&state);
    state.db.lock().set_zen_free_enabled(false).unwrap();
    account_control::create_go_api_key(&state, "failed-a".into(), "sk-failed-a".into(), None, None)
        .unwrap();
    publish_enabled_go_model(&state);
    start(
        &state,
        state.settings_revision(),
        state.process_generation(),
    )
    .await
    .expect("apply a");
    let created_b = account_control::create_go_api_key(
        &state,
        "failed-b".into(),
        "sk-failed-b".into(),
        None,
        None,
    )
    .unwrap();
    publish_enabled_go_model(&state);
    start(
        &state,
        state.settings_revision(),
        state.process_generation(),
    )
    .await
    .expect("apply b");
    let stamp_b = preview_attempt(&state, &created_b.id).unwrap();
    let created_c = account_control::create_go_api_key(
        &state,
        "failed-c".into(),
        "sk-failed-c".into(),
        None,
        None,
    )
    .unwrap();
    publish_enabled_go_model(&state);
    let stamp_c = preview_attempt(&state, &created_c.id).unwrap();
    set_fail_ready(true);
    let failed = start(
        &state,
        state.settings_revision(),
        state.process_generation(),
    )
    .await
    .unwrap_err();
    assert!(failed.to_string().contains("cpa_apply_failed"), "{failed}");
    set_fail_ready(false);
    let restored = load_record(&state);
    assert_eq!(restored.apply_status, "applied");
    assert!(has_credential(
        &restored.applied_auth,
        &stamp_b.credential_id
    ));
    assert!(!has_credential(
        &restored.applied_auth,
        &stamp_c.credential_id
    ));
    assert!(!has_route(&restored.applied_routes, &stamp_c.credential_id));
    let rolled = rollback(
        &state,
        state.settings_revision(),
        state.process_generation(),
    )
    .await
    .expect("rollback of the actual previous slot");
    assert_eq!(rolled.apply_status, "applied");
    let selected = load_record(&state);
    assert!(has_credential(
        &selected.applied_auth,
        &stamp_b.credential_id
    ));
    assert!(!has_credential(
        &selected.applied_auth,
        &stamp_c.credential_id
    ));
    assert!(!has_credential(
        &selected.desired_auth,
        &stamp_c.credential_id
    ));
    assert!(!has_route(&selected.applied_routes, &stamp_c.credential_id));
    drop(state);
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn accepted_history_reopen_rebinds_and_admits_the_selected_map() {
    let _hooks = HookGuard::synthetic();
    let dir = temp_dir("history-reopen");
    let (old_generation, stamp_a_id, stamp_b_id) = {
        let state = open_state(&dir);
        install_listener(&state);
        state.db.lock().set_zen_free_enabled(false).unwrap();
        let created_a = account_control::create_go_api_key(
            &state,
            "reopen-a".into(),
            "sk-reopen-a".into(),
            None,
            None,
        )
        .unwrap();
        publish_enabled_go_model(&state);
        let applied_a = start(
            &state,
            state.settings_revision(),
            state.process_generation(),
        )
        .await
        .expect("apply a");
        let stamp_a = preview_attempt(&state, &created_a.id).unwrap();
        let created_b = account_control::create_go_api_key(
            &state,
            "reopen-b".into(),
            "sk-reopen-b".into(),
            None,
            None,
        )
        .unwrap();
        publish_enabled_go_model(&state);
        start(
            &state,
            state.settings_revision(),
            state.process_generation(),
        )
        .await
        .expect("apply b");
        let stamp_b = preview_attempt(&state, &created_b.id).unwrap();
        drop(state);
        (
            applied_a.child_generation,
            stamp_a.credential_id,
            stamp_b.credential_id,
        )
    };
    let state = open_state(&dir);
    install_listener(&state);
    let rolled = rollback(
        &state,
        state.settings_revision(),
        state.process_generation(),
    )
    .await
    .expect("rebind rollback");
    assert_eq!(rolled.apply_status, "applied");
    assert!(rolled.policy_ready);
    assert_ne!(rolled.child_generation, old_generation);
    let selected = load_record(&state);
    assert_eq!(rolled.child_generation, selected.applied_generation);
    assert!(has_credential(&selected.applied_auth, &stamp_a_id));
    assert!(!has_credential(&selected.applied_auth, &stamp_b_id));
    let yaml = std::fs::read_to_string(io::config_path(&dir)).unwrap();
    assert_eq!(
        selected.applied_digest,
        crate::cpa_projection::wire_digest(&yaml)
    );
    let published = selected
        .applied_auth
        .iter()
        .find(|stamp| stamp.credential_id == stamp_a_id)
        .cloned()
        .unwrap();
    let admitted = callback_stamp(&state, &published).await;
    assert!(admitted.contains("eligible"), "{admitted}");
    drop(state);
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn zen_free_none_adopts_the_accepted_epoch_and_refuses_a_change() {
    let _hooks = HookGuard::synthetic();
    let dir = temp_dir("zen-free-epoch");
    let state = open_state(&dir);
    install_listener(&state);
    state.db.lock().set_zen_free_enabled(true).unwrap();
    start(
        &state,
        state.settings_revision(),
        state.process_generation(),
    )
    .await
    .expect("apply zen");
    let credential_id = ocg_domain::credential::credential_id_for_legacy_account(
        ocg_domain::ids::ZEN_FREE_ACCOUNT_ID,
    );
    let mut record = load_record(&state);
    let stamp = record
        .applied_auth
        .iter()
        .find(|stamp| stamp.credential_id == credential_id.as_str())
        .cloned()
        .expect("zen applied stamp");
    assert_eq!(stamp.material_revision, "no-material");
    assert_eq!(stamp.registration_epoch, 0);
    let route = record
        .applied_routes
        .iter()
        .find(|set| set.credential_id == credential_id.as_str())
        .and_then(|set| set.routes.first())
        .cloned()
        .expect("zen route");
    let body = format!(
        r#"{{"authRefs":[{{"authId":"{}","credentialId":"{}","credentialVersion":"{}","materialRevision":"{}","providerId":"{}","registrationEpoch":"5"}}]}}"#,
        stamp.auth_id,
        stamp.credential_id,
        stamp.credential_version,
        stamp.material_revision,
        stamp.provider_id
    );
    ready::note_applied_keyed_epochs(&mut record, &body).expect("zen epoch");
    store::save(&state.db.lock().conn, &record).unwrap();
    state.cpa_execution.inner.lock().record = record;
    let mut accepted = stamp.clone();
    accepted.registration_epoch = 5;
    let report = execution_report(&state);
    let request_id = Uuid::new_v4();
    client_model(&state, request_id, &route.public_model);
    let admit = admit_models(
        &report,
        request_id,
        Uuid::new_v4(),
        &accepted,
        &route.public_model,
        &route.upstream_model,
    );
    let token = token_of(&state);
    let origin = "http://127.0.0.1:9";
    let peer: SocketAddr = "127.0.0.1:1".parse().unwrap();
    let (_status, body) = callback(
        &state,
        &token,
        Some(origin),
        serde_json::to_vec(&admit).unwrap(),
        peer,
    )
    .await;
    assert!(body.contains("eligible"), "{body}");
    let changed_request = Uuid::new_v4();
    client_model(&state, changed_request, &route.public_model);
    let mut changed = admit.clone();
    changed["requestId"] = serde_json::json!(changed_request.to_string());
    changed["attemptId"] = serde_json::json!(Uuid::new_v4().to_string());
    changed["registrationEpoch"] = serde_json::json!("6");
    let (_status, body) = callback(
        &state,
        &token,
        Some(origin),
        serde_json::to_vec(&changed).unwrap(),
        peer,
    )
    .await;
    assert!(body.contains("\"reason\":\"identity_fence\""), "{body}");
    assert!(!body.contains("eligible"), "{body}");

    let mut blocked = load_record(&state);
    blocked.oauth.push(store::OAuthStamp {
        relative_path: "zen/token.json".into(),
        auth_id: stamp.auth_id.clone(),
        credential_id: stamp.credential_id.clone(),
        credential_version: stamp.credential_version,
        material_revision: stamp.material_revision.clone(),
        provider_id: "cpa".into(),
        native_provider: "codex".into(),
        registration_epoch: 5,
        models: Vec::new(),
        presence: store::OAuthPresence::Absent,
        recovery: String::new(),
        raw_provider_label: String::new(),
        native_mode: String::new(),
        reported_base: String::new(),
    });
    store::save(&state.db.lock().conn, &blocked).unwrap();
    state.cpa_execution.inner.lock().record = blocked;
    let absent_request = Uuid::new_v4();
    client_model(&state, absent_request, &route.public_model);
    let mut absent = admit.clone();
    absent["requestId"] = serde_json::json!(absent_request.to_string());
    absent["attemptId"] = serde_json::json!(Uuid::new_v4().to_string());
    let (_status, body) = callback(
        &state,
        &token,
        Some(origin),
        serde_json::to_vec(&absent).unwrap(),
        peer,
    )
    .await;
    assert!(!body.contains("eligible"), "{body}");
    drop(state);
    let _ = std::fs::remove_dir_all(dir);
}
