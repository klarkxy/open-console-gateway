use super::*;
use std::sync::atomic::{AtomicBool, Ordering};
struct Mock {
    extension_root: PathBuf,
    installed: AtomicBool,
    fail: AtomicBool,
    partial_failure: AtomicBool,
    corrupt_restore: AtomicBool,
    calls: Mutex<Vec<Vec<String>>>,
}
impl Runner for Mock {
    fn run(&self, _: &Installation, args: &[String]) -> ByokResult<String> {
        self.calls.lock().unwrap().push(args.to_vec());
        if args.iter().any(|s| s == "--version") {
            return Ok("1.141.0\nfixture\nx64".into());
        }
        if args.iter().any(|s| s == "--list-extensions") {
            return Ok(if self.installed.load(Ordering::SeqCst) {
                format!("{EXTENSION_ID}@{EXTENSION_VERSION}")
            } else {
                String::new()
            });
        }
        if self.fail.load(Ordering::SeqCst) {
            return Err(ByokError::precondition("fixture CLI failed"));
        }
        if args.iter().any(|s| s == "--install-extension") {
            let path = self
                .extension_root
                .join(format!("{EXTENSION_ID}-{EXTENSION_VERSION}"));
            safe::ensure_safe_directory_chain(&path.join("dist"))?;
            if self.partial_failure.swap(false, Ordering::SeqCst) {
                safe::write_private_atomic(
                    &path.join("dist/extension.cjs"),
                    b"partial-cli-runtime",
                )?;
                return Err(ByokError::precondition("fixture partial CLI update"));
            }
            safe::write_private_atomic(
                &path.join("dist/extension.cjs"),
                if self.corrupt_restore.load(Ordering::SeqCst) {
                    b"wrong-restored-runtime"
                } else {
                    package::RUNTIME
                },
            )?;
            safe::write_private_atomic(&path.join("package.json"),&serde_json::to_vec(&json!({"publisher":"open-console-gateway","name":"copilot","version":EXTENSION_VERSION})).unwrap())?;
            safe::write_private_atomic(
                &self.extension_root.join("extensions.json"),
                &serde_json::to_vec(
                    &json!([{"identifier":{"id":EXTENSION_ID},"version":EXTENSION_VERSION}]),
                )
                .unwrap(),
            )?;
            self.installed.store(true, Ordering::SeqCst);
        }
        if args.iter().any(|s| s == "--uninstall-extension") {
            safe::write_private_atomic(&self.extension_root.join("extensions.json"), b"[]")?;
            self.installed.store(false, Ordering::SeqCst);
        }
        Ok(String::new())
    }
}
struct Fixture {
    root: PathBuf,
    host: Host,
    runner: Arc<Mock>,
}
impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!("ocg-copilot-test-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&root).unwrap();
        let extensions = root.join("extensions");
        let runner = Arc::new(Mock {
            extension_root: extensions.clone(),
            installed: AtomicBool::new(false),
            fail: AtomicBool::new(false),
            partial_failure: AtomicBool::new(false),
            corrupt_restore: AtomicBool::new(false),
            calls: Mutex::new(vec![]),
        });
        let host = Host {
            data_dir: root.join("ocg"),
            installations: vec![Installation {
                info: CopilotInstallation {
                    id: "fixture".into(),
                    label: "Fixture".into(),
                    executable: "fixture-code".into(),
                    version: Some("1.141.0".into()),
                    user_data_dir: root.join("user").to_string_lossy().into(),
                    extensions_dir: extensions.to_string_lossy().into(),
                },
                cli: None,
                scheme: "vscode".into(),
                portable: None,
            }],
            runner: runner.clone(),
        };
        Self { root, host, runner }
    }
    fn inspect(&self) -> CopilotInspection {
        self.host.inspect_input(CopilotTarget::default()).unwrap()
    }
    fn install(&self) -> CopilotInspection {
        let before = self.inspect();
        self.host
            .execute(CopilotApplicationHostRequest::Install {
                target: CopilotTarget::default(),
                expected_fingerprint: before.fingerprint.unwrap(),
                gateway_v1_url: "http://127.0.0.1:9042/v1".into(),
                secret: crate::dsh_application::DshGatewaySecret::new(
                    "synthetic-test-secret".into(),
                ),
            })
            .unwrap()
    }
    fn acknowledge(&self, status: &str) {
        let target = self.host.required_target(CopilotTarget::default()).unwrap();
        let handoff = read_json(&target.storage.join(HANDOFF)).unwrap().unwrap();
        let ack = json!({"schemaVersion":1,"connectionId":handoff["connectionId"],"runtimeDigest":handoff["runtimeDigest"],"status":status,"modelCount":2,"metadataMissing":[]});
        safe::remove_regular_file(&target.storage.join(HANDOFF)).unwrap();
        safe::write_private_atomic(
            &target.storage.join(ACK),
            &serde_json::to_vec(&ack).unwrap(),
        )
        .unwrap();
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}
#[test]
fn inspect_creates_no_package_handoff_or_key() {
    let f = Fixture::new();
    let view = f.inspect();
    assert_eq!(view.status, CopilotStatus::Ready);
    assert!(!f.root.join("ocg").exists());
    assert!(!f.root.join("user").exists());
    assert!(f.runner.calls.lock().unwrap().is_empty());
}
#[test]
fn installation_is_pending_until_matching_secret_import_ack() {
    let f = Fixture::new();
    let installed = f.install();
    assert_eq!(installed.status, CopilotStatus::InstalledPending);
    assert!(installed.activation_required);
    let encoded = serde_json::to_string(&installed).unwrap();
    assert!(!encoded.contains("synthetic-test-secret"));
    f.acknowledge("connected");
    assert_eq!(f.inspect().status, CopilotStatus::Connected);
    assert_eq!(f.inspect().model_count, Some(2));
}
#[test]
fn missing_handoff_alone_is_not_connected() {
    let f = Fixture::new();
    f.install();
    let target = f.host.required_target(CopilotTarget::default()).unwrap();
    safe::remove_regular_file(&target.storage.join(HANDOFF)).unwrap();
    assert_eq!(f.inspect().status, CopilotStatus::InstalledPending);
}
#[test]
fn stale_fingerprint_precedes_install_effects() {
    let f = Fixture::new();
    let err = f
        .host
        .execute(CopilotApplicationHostRequest::Install {
            target: CopilotTarget::default(),
            expected_fingerprint: "stale".into(),
            gateway_v1_url: "http://127.0.0.1:9042/v1".into(),
            secret: crate::dsh_application::DshGatewaySecret::new("secret".into()),
        })
        .unwrap_err();
    assert_eq!(err.kind, crate::byok_application::ByokErrorKind::Conflict);
    assert!(!f.root.join("ocg").exists());
}
#[test]
fn foreign_same_identity_is_not_adopted_or_removed() {
    let f = Fixture::new();
    f.runner.installed.store(true, Ordering::SeqCst);
    safe::ensure_safe_directory_chain(&f.runner.extension_root).unwrap();
    safe::write_private_atomic(
        &f.runner.extension_root.join("extensions.json"),
        &serde_json::to_vec(
            &json!([{"identifier":{"id":EXTENSION_ID},"version":EXTENSION_VERSION}]),
        )
        .unwrap(),
    )
    .unwrap();
    let view = f.inspect();
    assert_eq!(view.status, CopilotStatus::Conflict);
    assert!(!view.install_supported);
    assert!(!view.uninstall_supported);
}
#[test]
fn failed_cli_update_preserves_existing_connection() {
    let f = Fixture::new();
    f.install();
    f.acknowledge("connected");
    let target = f.host.required_target(CopilotTarget::default()).unwrap();
    let receipt = safe::read_regular_file(&target.receipt).unwrap();
    let before = f.inspect();
    f.runner.fail.store(true, Ordering::SeqCst);
    assert!(
        f.host
            .execute(CopilotApplicationHostRequest::Install {
                target: before.target,
                expected_fingerprint: before.fingerprint.unwrap(),
                gateway_v1_url: "http://127.0.0.1:9042/v1".into(),
                secret: crate::dsh_application::DshGatewaySecret::new("replacement".into())
            })
            .is_err()
    );
    assert_eq!(safe::read_regular_file(&target.receipt).unwrap(), receipt);
    assert_eq!(f.inspect().status, CopilotStatus::Connected);
}
#[test]
fn uninstall_waits_for_extension_owned_secret_deletion() {
    let f = Fixture::new();
    f.install();
    f.acknowledge("connected");
    let before = f.inspect();
    let pending = f
        .host
        .execute(CopilotApplicationHostRequest::Uninstall {
            target: before.target,
            expected_fingerprint: before.fingerprint.unwrap(),
        })
        .unwrap();
    assert!(pending.installed);
    assert_eq!(pending.status, CopilotStatus::InstalledPending);
    assert!(
        !f.runner
            .calls
            .lock()
            .unwrap()
            .iter()
            .any(|a| a.contains(&"--uninstall-extension".into()))
    );
    f.acknowledge("disconnected");
    let before = f.inspect();
    let removed = f
        .host
        .execute(CopilotApplicationHostRequest::Uninstall {
            target: before.target,
            expected_fingerprint: before.fingerprint.unwrap(),
        })
        .unwrap();
    assert!(!removed.installed);
}
#[test]
fn named_profile_uses_its_storage_and_cli_flags() {
    let f = Fixture::new();
    let user = f.root.join("user");
    safe::ensure_safe_directory_chain(&user.join("User/globalStorage")).unwrap();
    safe::write_private_atomic(
        &user.join("User/globalStorage/storage.json"),
        br#"{"userDataProfiles":[{"name":"Writer","location":"abc123"}]}"#,
    )
    .unwrap();
    let target = f
        .host
        .required_target(CopilotTarget {
            profile: Some("Writer".into()),
            ..Default::default()
        })
        .unwrap();
    assert_eq!(
        target.storage,
        user.join("User/profiles/abc123/globalStorage")
            .join(EXTENSION_ID)
    );
    assert!(
        target
            .args()
            .windows(2)
            .any(|p| p == ["--profile", "Writer"])
    );
}
#[test]
fn shared_global_state_profile_requires_explicit_default_target() {
    let f = Fixture::new();
    let user = f.root.join("user");
    safe::ensure_safe_directory_chain(&user.join("User/globalStorage")).unwrap();
    safe::write_private_atomic(&user.join("User/globalStorage/storage.json"),br#"{"userDataProfiles":[{"name":"Shared","location":"abc","useDefaultFlags":{"globalState":true}}]}"#).unwrap();
    assert!(
        f.host
            .required_target(CopilotTarget {
                profile: Some("Shared".into()),
                ..Default::default()
            })
            .is_err()
    );
}

#[test]
fn foreign_runtime_cannot_receive_disconnect_handoff() {
    let f = Fixture::new();
    f.install();
    f.acknowledge("connected");
    let target = f.host.required_target(CopilotTarget::default()).unwrap();
    let bundle = f.runner.extension_root.join(format!(
        "{EXTENSION_ID}-{EXTENSION_VERSION}/dist/extension.cjs"
    ));
    safe::write_private_atomic(&bundle, b"foreign-runtime").unwrap();
    let before = f.inspect();
    assert_eq!(before.status, CopilotStatus::Conflict);
    assert!(
        f.host
            .execute(CopilotApplicationHostRequest::Disconnect {
                target: before.target,
                expected_fingerprint: before.fingerprint.unwrap(),
            })
            .is_err()
    );
    assert!(!target.storage.join(HANDOFF).exists());
}
#[test]
fn portable_target_rejects_directory_overrides() {
    let mut f = Fixture::new();
    f.host.installations[0].portable = Some(f.root.join("portable"));
    assert!(
        f.host
            .required_target(CopilotTarget {
                user_data_dir: Some(f.root.join("other").to_string_lossy().into()),
                ..Default::default()
            })
            .is_err()
    );
    assert!(f.host.required_target(CopilotTarget::default()).is_ok());
}

#[test]
fn malformed_receipt_without_runtime_digest_is_not_owned() {
    let f = Fixture::new();
    f.install();
    f.acknowledge("connected");
    let target = f.host.required_target(CopilotTarget::default()).unwrap();
    let mut receipt = read_json(&target.receipt).unwrap().unwrap();
    receipt.as_object_mut().unwrap().remove("runtimeDigest");
    safe::write_private_atomic(&target.receipt, &serde_json::to_vec(&receipt).unwrap()).unwrap();
    let view = f.inspect();
    assert_eq!(view.status, CopilotStatus::Conflict);
    assert!(!view.uninstall_supported);
}

#[test]
fn partial_cli_update_restores_verified_runtime_and_existing_connection() {
    let f = Fixture::new();
    f.install();
    f.acknowledge("connected");
    let before = f.inspect();
    f.runner.partial_failure.store(true, Ordering::SeqCst);
    assert!(
        f.host
            .execute(CopilotApplicationHostRequest::Install {
                target: before.target,
                expected_fingerprint: before.fingerprint.unwrap(),
                gateway_v1_url: "http://127.0.0.1:9042/v1".into(),
                secret: crate::dsh_application::DshGatewaySecret::new("replacement".into()),
            })
            .is_err()
    );
    assert_eq!(f.inspect().status, CopilotStatus::Connected);
    let target = f.host.required_target(CopilotTarget::default()).unwrap();
    assert_eq!(
        runtime_at(&target, EXTENSION_VERSION).unwrap().as_deref(),
        Some(package::runtime_digest().as_str())
    );
    assert!(!target.storage.join(HANDOFF).exists());
}
#[test]
fn successful_cli_exit_with_wrong_restored_runtime_reports_partial_failure() {
    let f = Fixture::new();
    f.install();
    f.acknowledge("connected");
    let before = f.inspect();
    f.runner.partial_failure.store(true, Ordering::SeqCst);
    f.runner.corrupt_restore.store(true, Ordering::SeqCst);
    let err = f
        .host
        .execute(CopilotApplicationHostRequest::Install {
            target: before.target,
            expected_fingerprint: before.fingerprint.unwrap(),
            gateway_v1_url: "http://127.0.0.1:9042/v1".into(),
            secret: crate::dsh_application::DshGatewaySecret::new("replacement".into()),
        })
        .unwrap_err();
    assert!(err.message.contains("partially changed"));
    assert_eq!(f.inspect().status, CopilotStatus::Conflict);
}
