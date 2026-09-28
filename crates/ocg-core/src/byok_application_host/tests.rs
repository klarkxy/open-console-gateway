use super::lock::{
    CrossProcessLock, LockPolicy, capture_dir_id, lock_dir_for, ocg_sidecar_for,
    set_directory_mtime, sidecar_json,
};
use super::paths::{DiscoveredPaths, ResolvedTarget};
use super::receipt::{
    ApplyPlan, FileRole, Journal, ManagedSnapshot, PendingFile, PendingKind, PlannedFile,
    RECEIPT_VERSION, Store, new_receipt,
};
use super::*;
use crate::byok_application::{ByokClient, ByokHostRequest, ByokSecret, ByokStatus};
use crate::model_metadata::ModelMetadata;
use serde_json::json;
use sha2::{Digest, Sha256};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

struct Harness {
    root: PathBuf,
    host: ByokNativeHost,
}

impl Drop for Harness {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn harness(name: &str) -> Harness {
    let root =
        std::env::temp_dir().join(format!("ocg-byok-{name}-{}", uuid::Uuid::new_v4().simple()));
    let data_dir = root.join("data");
    let home = root.join("home");
    fs::create_dir_all(&data_dir).unwrap();
    fs::create_dir_all(&home).unwrap();
    let target = |client: ByokClient, parts: &[&str]| {
        let mut path = home.clone();
        for part in parts {
            path.push(part);
        }
        ResolvedTarget {
            client,
            path,
            discovery_source: "default".into(),
        }
    };
    let paths = DiscoveredPaths {
        user_home: home.clone(),
        codex: target(ByokClient::Codex, &[".codex", "config.toml"]),
        kimi: target(ByokClient::Kimi, &[".kimi-code", "config.toml"]),
        minimax: target(ByokClient::Minimax, &[".minimax", "config.yaml"]),
        zcode: target(ByokClient::Zcode, &[".zcode", "v2", "provider_config.json"]),
    };
    let mut policy = LockPolicy::default();
    policy.minimax_max_wait = Duration::from_millis(120);
    policy.minimax_retry = Duration::from_millis(10);
    policy.minimax_heartbeat = Duration::from_millis(40);
    policy.zcode_max_wait = Duration::from_millis(120);
    policy.zcode_retry_delays_ms = vec![10];
    Harness {
        root,
        host: ByokNativeHost::new(data_dir, paths, policy),
    }
}

fn model(id: &str, context: u64, output: Option<u64>) -> ByokModel {
    ByokModel {
        id: id.into(),
        metadata: ModelMetadata {
            name: Some(format!("Name {id}")),
            context_window: Some(context),
            max_output_tokens: output,
            tool_calling: Some(true),
            input_modalities: Some(vec!["text".into()]),
            ..ModelMetadata::default()
        },
    }
}

fn secret() -> ByokSecret {
    ByokSecret::new("sk-test-secret-do-not-leak-xyz".into())
}

const GATEWAY: &str = "http://127.0.0.1:9042/v1";

fn inspect(host: &ByokNativeHost, client: ByokClient) -> ByokInspection {
    host.execute(ByokHostRequest::Inspect {
        client,
        target_path: None,
    })
    .unwrap()
}

fn configure(
    host: &ByokNativeHost,
    client: ByokClient,
    fingerprint: &str,
    models: Vec<ByokModel>,
    default_model_id: Option<String>,
) -> ByokResult<ByokInspection> {
    host.execute(ByokHostRequest::Configure {
        client,
        target_path: None,
        expected_fingerprint: fingerprint.into(),
        gateway_v1_url: GATEWAY.into(),
        secret: secret(),
        models,
        default_model_id,
        client_closed: true,
    })
}

fn remove(
    host: &ByokNativeHost,
    client: ByokClient,
    fingerprint: &str,
) -> ByokResult<ByokInspection> {
    host.execute(ByokHostRequest::Remove {
        client,
        target_path: None,
        expected_fingerprint: fingerprint.into(),
        client_closed: true,
    })
}

fn recover(
    host: &ByokNativeHost,
    client: ByokClient,
    fingerprint: &str,
) -> ByokResult<ByokInspection> {
    host.execute(ByokHostRequest::Recover {
        client,
        target_path: None,
        expected_fingerprint: fingerprint.into(),
        client_closed: true,
    })
}

fn read_text(path: &Path) -> String {
    String::from_utf8(fs::read(path).unwrap()).unwrap()
}

fn target_file(host: &ByokNativeHost, client: ByokClient) -> PathBuf {
    host.paths.default_for(client).path.clone()
}

fn all_clients() -> [ByokClient; 4] {
    [
        ByokClient::Codex,
        ByokClient::Kimi,
        ByokClient::Minimax,
        ByokClient::Zcode,
    ]
}

fn output_for(client: ByokClient) -> Option<u64> {
    match client {
        ByokClient::Minimax | ByokClient::Zcode => Some(8192),
        _ => None,
    }
}

#[test]
fn inspect_missing_files_in_isolated_homes() {
    let h = harness("missing");
    for client in all_clients() {
        let view = inspect(&h.host, client);
        assert_eq!(view.status, ByokStatus::NotDetected);
        assert!(!view.detected);
        assert!(view.configure_supported);
        assert_eq!(view.discovery_source, "default");
        assert!(view.config_path.ends_with(match client {
            ByokClient::Codex | ByokClient::Kimi => "config.toml",
            ByokClient::Minimax => "config.yaml",
            ByokClient::Zcode => "provider_config.json",
        }));
        assert!(view.fingerprint.is_some());
    }
}

#[test]
fn configure_update_remove_all_four_formats() {
    let h = harness("roundtrip");
    for client in all_clients() {
        let first = inspect(&h.host, client);
        let models = vec![
            model("org/model.v1", 128000, output_for(client)),
            model("gpt-4.1", 200000, output_for(client)),
        ];
        let configured = configure(
            &h.host,
            client,
            first.fingerprint.as_deref().unwrap(),
            models,
            Some("org/model.v1".into()),
        )
        .unwrap_or_else(|error| {
            panic!(
                "{client:?} configure failed: {error:?} target={}",
                target_file(&h.host, client).display()
            )
        });
        assert_eq!(configured.status, ByokStatus::Configured);
        assert!(configured.activation_required);
        assert!(
            configured
                .configured_model_ids
                .contains(&"org/model.v1".into())
        );
        assert!(configured.configured_model_ids.contains(&"gpt-4.1".into()));
        assert_eq!(configured.default_model_id.as_deref(), Some("org/model.v1"));
        let text = read_text(&target_file(&h.host, client));
        let catalog = super::paths::catalog_path(h.host.paths.default_for(client))
            .and_then(|path| fs::read_to_string(path).ok())
            .unwrap_or_default();
        assert!(text.contains("org/model.v1") || catalog.contains("org/model.v1"));
        assert!(text.contains("gpt-4.1") || catalog.contains("gpt-4.1"));
        assert!(
            !configured
                .detail
                .as_deref()
                .unwrap_or("")
                .contains("sk-test-secret-do-not-leak-xyz")
        );

        let again = inspect(&h.host, client);
        let updated = configure(
            &h.host,
            client,
            again.fingerprint.as_deref().unwrap(),
            vec![model("org/model.v1", 128000, output_for(client))],
            None,
        )
        .unwrap();
        assert_eq!(updated.status, ByokStatus::Configured);
        assert_eq!(
            updated.configured_model_ids,
            vec!["org/model.v1".to_string()]
        );
        assert_eq!(updated.default_model_id.as_deref(), Some("org/model.v1"));

        let removed = remove(
            &h.host,
            client,
            inspect(&h.host, client).fingerprint.as_deref().unwrap(),
        )
        .unwrap();
        assert_ne!(removed.status, ByokStatus::Configured);
        let after = fs::read_to_string(target_file(&h.host, client)).unwrap_or_default();
        assert!(!after.contains("org/model.v1") || after.trim().is_empty());
    }
}

#[test]
fn toml_comments_and_unrelated_values_are_preserved() {
    let h = harness("comments");
    for client in [ByokClient::Codex, ByokClient::Kimi] {
        let path = target_file(&h.host, client);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(
            &path,
            concat!(
                "# keep-me-comment\n",
                "[other]\n",
                "unrelated = \"yes\"\n",
                "count = 7\n",
            ),
        )
        .unwrap();
        let view = inspect(&h.host, client);
        configure(
            &h.host,
            client,
            view.fingerprint.as_deref().unwrap(),
            vec![model("keep.model", 1000, None)],
            None,
        )
        .unwrap();
        let text = read_text(&path);
        assert!(text.contains("keep-me-comment"));
        assert!(text.contains("unrelated"));
        assert!(text.contains('7'));
    }
}

#[test]
fn yaml_and_json_unrelated_values_are_preserved() {
    let h = harness("unrelated");
    let mini = target_file(&h.host, ByokClient::Minimax);
    fs::create_dir_all(mini.parent().unwrap()).unwrap();
    fs::write(
        &mini,
        "logLevel: debug\nprovider:\n  minimax:\n    name: official\n",
    )
    .unwrap();
    configure(
        &h.host,
        ByokClient::Minimax,
        inspect(&h.host, ByokClient::Minimax)
            .fingerprint
            .as_deref()
            .unwrap(),
        vec![model("m1", 1000, Some(100))],
        None,
    )
    .unwrap();
    let yaml = read_text(&mini);
    assert!(yaml.contains("logLevel"));
    assert!(yaml.contains("minimax"));
    assert!(!yaml.contains("\nprovider:\n") || yaml.contains("minimax"));

    let zpath = target_file(&h.host, ByokClient::Zcode);
    fs::create_dir_all(zpath.parent().unwrap()).unwrap();
    fs::write(
        &zpath,
        r#"{"schemaVersion":1,"config":{"providerOrder":["keep"],"providerConfigRules":{"providerRules":[{"providerId":"keep","providerName":"Keep","enabled":true,"config":{"group":"standard-personal"}}]},"modelConfigRules":{"providerModelRules":[],"manualProviderModelRules":[]},"extraUser":true}}"#,
    )
    .unwrap();
    configure(
        &h.host,
        ByokClient::Zcode,
        inspect(&h.host, ByokClient::Zcode)
            .fingerprint
            .as_deref()
            .unwrap(),
        vec![model("z1", 1000, Some(100))],
        None,
    )
    .unwrap();
    let json = read_text(&zpath);
    assert!(json.contains("\"keep\""));
    assert!(json.contains("extraUser"));
}

#[test]
fn unowned_ocg_collision_is_rejected() {
    let h = harness("collision");
    let path = target_file(&h.host, ByokClient::Kimi);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(
        &path,
        "[providers.ocg]\ntype = \"openai\"\nbase_url = \"http://example.test\"\napi_key = \"foreign\"\n",
    )
    .unwrap();
    let view = inspect(&h.host, ByokClient::Kimi);
    assert_eq!(view.status, ByokStatus::Conflict);
    let err = configure(
        &h.host,
        ByokClient::Kimi,
        view.fingerprint.as_deref().unwrap(),
        vec![model("m", 1000, None)],
        None,
    )
    .unwrap_err();
    assert_eq!(err.kind, crate::byok_application::ByokErrorKind::Conflict);
}

#[test]
fn automatic_default_activation_restores_external_default_only_if_unchanged() {
    let h = harness("defaults");
    let path = target_file(&h.host, ByokClient::Kimi);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(&path, "default_model = \"moonshot\"\n").unwrap();
    let first = inspect(&h.host, ByokClient::Kimi);
    configure(
        &h.host,
        ByokClient::Kimi,
        first.fingerprint.as_deref().unwrap(),
        vec![model("a", 1000, None)],
        None,
    )
    .unwrap();
    assert!(read_text(&path).contains("ocg/a"));

    let second = inspect(&h.host, ByokClient::Kimi);
    configure(
        &h.host,
        ByokClient::Kimi,
        second.fingerprint.as_deref().unwrap(),
        vec![model("a", 1000, None)],
        Some("a".into()),
    )
    .unwrap();
    assert!(read_text(&path).contains("ocg/a"));

    remove(
        &h.host,
        ByokClient::Kimi,
        inspect(&h.host, ByokClient::Kimi)
            .fingerprint
            .as_deref()
            .unwrap(),
    )
    .unwrap();
    assert!(read_text(&path).contains("moonshot"));

    fs::write(&path, "default_model = \"moonshot\"\n").unwrap();
    let again = inspect(&h.host, ByokClient::Kimi);
    configure(
        &h.host,
        ByokClient::Kimi,
        again.fingerprint.as_deref().unwrap(),
        vec![model("b", 1000, None)],
        Some("b".into()),
    )
    .unwrap();
    let mut text = read_text(&path);
    text = text.replace("ocg/b", "user-changed");
    fs::write(&path, text).unwrap();
    let _ = remove(
        &h.host,
        ByokClient::Kimi,
        inspect(&h.host, ByokClient::Kimi)
            .fingerprint
            .as_deref()
            .unwrap(),
    );
    assert!(read_text(&path).contains("user-changed"));
}

#[test]
fn automatic_default_keeps_a_published_choice_and_replaces_a_removed_choice() {
    for client in ByokClient::ALL {
        let h = harness(&format!("auto-default-{}", client.id()));
        let first = inspect(&h.host, client);
        configure(
            &h.host,
            client,
            first.fingerprint.as_deref().unwrap(),
            vec![model("a", 4096, Some(1000)), model("b", 4096, Some(1000))],
            Some("b".into()),
        )
        .unwrap();
        let current = inspect(&h.host, client);
        let kept = configure(
            &h.host,
            client,
            current.fingerprint.as_deref().unwrap(),
            vec![model("a", 4096, Some(1000)), model("b", 4096, Some(1000))],
            None,
        )
        .unwrap();
        assert_eq!(kept.default_model_id.as_deref(), Some("b"));
        let updated = configure(
            &h.host,
            client,
            kept.fingerprint.as_deref().unwrap(),
            vec![model("a", 4096, Some(1000))],
            None,
        )
        .unwrap();
        assert_eq!(updated.default_model_id.as_deref(), Some("a"));
    }
}

#[test]
fn malformed_config_is_incompatible_and_not_overwritten() {
    let h = harness("malformed");
    let path = target_file(&h.host, ByokClient::Codex);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(&path, "[[[not toml").unwrap();
    let view = inspect(&h.host, ByokClient::Codex);
    assert_eq!(view.status, ByokStatus::Incompatible);
    assert!(!view.configure_supported);
    let err = configure(
        &h.host,
        ByokClient::Codex,
        view.fingerprint.as_deref().unwrap(),
        vec![model("m", 1000, None)],
        None,
    )
    .unwrap_err();
    assert_eq!(err.kind, crate::byok_application::ByokErrorKind::Invalid);
    assert_eq!(read_text(&path), "[[[not toml");
}

#[test]
fn zcode_legacy_schema_is_incompatible() {
    let h = harness("zlegacy");
    let path = target_file(&h.host, ByokClient::Zcode);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(&path, r#"{"options":{"apiKey":"x"}}"#).unwrap();
    let view = inspect(&h.host, ByokClient::Zcode);
    assert_eq!(view.status, ByokStatus::Incompatible);
}

#[test]
fn stale_fingerprint_is_rejected() {
    let h = harness("stale-fp");
    let first = inspect(&h.host, ByokClient::Minimax);
    fs::create_dir_all(target_file(&h.host, ByokClient::Minimax).parent().unwrap()).unwrap();
    fs::write(
        target_file(&h.host, ByokClient::Minimax),
        "logLevel: info\n",
    )
    .unwrap();
    let err = configure(
        &h.host,
        ByokClient::Minimax,
        first.fingerprint.as_deref().unwrap(),
        vec![model("m", 1000, Some(100))],
        None,
    )
    .unwrap_err();
    assert_eq!(err.kind, crate::byok_application::ByokErrorKind::Conflict);
}

#[test]
fn client_must_be_closed_for_codex_and_kimi() {
    let h = harness("closed");
    let view = inspect(&h.host, ByokClient::Codex);
    let err = h
        .host
        .execute(ByokHostRequest::Configure {
            client: ByokClient::Codex,
            target_path: None,
            expected_fingerprint: view.fingerprint.unwrap(),
            gateway_v1_url: GATEWAY.into(),
            secret: secret(),
            models: vec![model("m", 1000, None)],
            default_model_id: None,
            client_closed: false,
        })
        .unwrap_err();
    assert_eq!(
        err.kind,
        crate::byok_application::ByokErrorKind::Precondition
    );
}

#[test]
fn secret_is_redacted_from_debug_and_errors() {
    let secret = ByokSecret::new("sk-test-secret-do-not-leak-xyz".into());
    assert!(!format!("{secret:?}").contains("sk-test-secret-do-not-leak-xyz"));
    let h = harness("redact");
    let view = inspect(&h.host, ByokClient::Kimi);
    let configured = configure(
        &h.host,
        ByokClient::Kimi,
        view.fingerprint.as_deref().unwrap(),
        vec![model("m", 1000, None)],
        None,
    )
    .unwrap();
    assert!(!format!("{configured:?}").contains("sk-test-secret-do-not-leak-xyz"));
}

#[test]
fn symlink_targets_are_rejected() {
    let h = harness("symlink");
    let real = h.root.join("real.toml");
    fs::write(&real, "ok = 1\n").unwrap();
    let path = target_file(&h.host, ByokClient::Codex);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let linked = match make_symlink(&real, &path) {
        Ok(()) => true,
        Err(_) => false,
    };
    if !linked {
        return;
    }
    let result = h.host.execute(ByokHostRequest::Inspect {
        client: ByokClient::Codex,
        target_path: None,
    });
    assert!(result.is_err() || result.unwrap().status == ByokStatus::Conflict);
}

#[test]
fn minimax_lock_directory_blocks_writes() {
    let h = harness("mm-lock");
    let path = target_file(&h.host, ByokClient::Minimax);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(&path, "logLevel: info\n").unwrap();
    fs::create_dir(format!("{}.lock", path.display())).unwrap();
    let view = inspect(&h.host, ByokClient::Minimax);
    let err = configure(
        &h.host,
        ByokClient::Minimax,
        view.fingerprint.as_deref().unwrap(),
        vec![model("m", 1000, Some(100))],
        None,
    )
    .unwrap_err();
    assert_eq!(
        err.kind,
        crate::byok_application::ByokErrorKind::Precondition
    );
}

#[test]
fn crash_journal_recovers_matching_new_state_and_refuses_changed_files() {
    let h = harness("journal");
    let client = ByokClient::Kimi;
    let first = inspect(&h.host, client);
    configure(
        &h.host,
        client,
        first.fingerprint.as_deref().unwrap(),
        vec![model("one", 1000, None)],
        None,
    )
    .unwrap();
    let path = target_file(&h.host, client);
    let old_bytes = fs::read(&path).unwrap();
    let second = inspect(&h.host, client);
    configure(
        &h.host,
        client,
        second.fingerprint.as_deref().unwrap(),
        vec![model("two", 2000, None)],
        None,
    )
    .unwrap();
    let new_bytes = fs::read(&path).unwrap();
    let target = ResolvedTarget {
        client,
        path: PathBuf::from(inspect(&h.host, client).config_path),
        discovery_source: "default".into(),
    };
    let store = Store::open(&h.host.data_dir, &target).unwrap();
    let receipt = store.load().unwrap().expect("receipt after configure");
    let old_hash = hex::encode(Sha256::digest(&old_bytes));
    let new_hash = hex::encode(Sha256::digest(&new_bytes));
    fs::create_dir_all(store.backup_dir()).unwrap();
    fs::write(store.backup_dir().join("old-target.bin"), &old_bytes).unwrap();
    fs::write(store.backup_dir().join("new-target.bin"), &new_bytes).unwrap();
    let journal = Journal {
        prior_receipt: Some(receipt.clone()),
        kind: PendingKind::Configure,
        files: vec![PendingFile {
            role: FileRole::Target,
            old_hash: old_hash.clone(),
            new_hash: new_hash.clone(),
        }],
    };
    fs::write(
        store.journal_path(),
        serde_json::to_vec_pretty(&journal).unwrap(),
    )
    .unwrap();
    let recovering = inspect(&h.host, client);
    assert_eq!(recovering.status, ByokStatus::RecoveryRequired);
    assert!(recovering.recovery_supported);
    recover(&h.host, client, recovering.fingerprint.as_deref().unwrap()).unwrap();
    assert_eq!(fs::read(&path).unwrap(), old_bytes);

    fs::write(
        store.journal_path(),
        serde_json::to_vec_pretty(&journal).unwrap(),
    )
    .unwrap();
    fs::write(&path, b"user-changed-after-crash").unwrap();
    let refused = inspect(&h.host, client);
    let err = recover(&h.host, client, refused.fingerprint.as_deref().unwrap()).unwrap_err();
    assert_eq!(err.kind, crate::byok_application::ByokErrorKind::Conflict);
    assert_eq!(read_text(&path), "user-changed-after-crash");
}

#[test]
fn invalid_codex_catalog_parent_is_rejected_before_writing() {
    let h = harness("codex-catalog-parent");
    let client = ByokClient::Codex;
    let path = target_file(&h.host, client);
    let home = path.parent().unwrap().to_path_buf();
    fs::create_dir_all(&home).unwrap();
    let original = b"# original\nkeep = 1\n";
    fs::write(&path, original).unwrap();
    let view = inspect(&h.host, client);
    let fingerprint = view.fingerprint.as_deref().unwrap().to_string();
    let blocker = home.join(".ocg-byok");
    let blocker_bytes = b"not-a-directory";
    fs::write(&blocker, blocker_bytes).unwrap();
    let inspect_err = h
        .host
        .execute(ByokHostRequest::Inspect {
            client,
            target_path: None,
        })
        .unwrap_err();
    assert_eq!(
        inspect_err.kind,
        crate::byok_application::ByokErrorKind::Conflict
    );
    let configure_err = configure(
        &h.host,
        client,
        &fingerprint,
        vec![model("m", 1000, None)],
        None,
    )
    .unwrap_err();
    assert_eq!(
        configure_err.kind,
        crate::byok_application::ByokErrorKind::Conflict
    );
    assert_eq!(fs::read(&path).unwrap(), original);
    assert_eq!(fs::read(&blocker).unwrap(), blocker_bytes);
    assert!(blocker.is_file());
    let store = Store::open(
        &h.host.data_dir,
        &ResolvedTarget {
            client,
            path: path.clone(),
            discovery_source: "default".into(),
        },
    )
    .unwrap();
    assert!(store.load().unwrap().is_none());
    assert!(!store.receipt_path().is_file());
}

#[test]
fn explicit_target_must_match_client_filename() {
    let h = harness("explicit");
    let err = h
        .host
        .execute(ByokHostRequest::Inspect {
            client: ByokClient::Codex,
            target_path: Some(h.root.join("odd.json").to_string_lossy().into_owned()),
        })
        .unwrap_err();
    assert_eq!(err.kind, crate::byok_application::ByokErrorKind::Invalid);
}

#[test]
fn concurrent_configure_second_caller_sees_stale_fingerprint() {
    let h = harness("concurrent");
    let client = ByokClient::Kimi;
    let first = inspect(&h.host, client);
    let fingerprint = first.fingerprint.unwrap();
    let (ra, rb) = std::thread::scope(|scope| {
        let a = scope.spawn(|| {
            h.host.execute(ByokHostRequest::Configure {
                client,
                target_path: None,
                expected_fingerprint: fingerprint.clone(),
                gateway_v1_url: GATEWAY.into(),
                secret: secret(),
                models: vec![model("m", 1000, None)],
                default_model_id: None,
                client_closed: true,
            })
        });
        let b = scope.spawn(|| {
            h.host.execute(ByokHostRequest::Configure {
                client,
                target_path: None,
                expected_fingerprint: fingerprint.clone(),
                gateway_v1_url: GATEWAY.into(),
                secret: secret(),
                models: vec![model("n", 1000, None)],
                default_model_id: None,
                client_closed: true,
            })
        });
        (a.join().unwrap(), b.join().unwrap())
    });
    let ok = ra.is_ok() as u8 + rb.is_ok() as u8;
    assert_eq!(ok, 1);
    assert!(ra.is_err() || rb.is_err());
    let err = if ra.is_err() {
        ra.unwrap_err()
    } else {
        rb.unwrap_err()
    };
    assert_eq!(err.kind, crate::byok_application::ByokErrorKind::Conflict);
}

#[test]
fn tool_calling_false_is_exported_without_filtering() {
    let h = harness("tools");
    let view = inspect(&h.host, ByokClient::Kimi);
    let mut denied = model("m", 1000, None);
    denied.metadata.tool_calling = Some(false);
    let result = configure(
        &h.host,
        ByokClient::Kimi,
        view.fingerprint.as_deref().unwrap(),
        vec![denied],
        None,
    )
    .unwrap();
    assert_eq!(result.configured_model_ids, vec!["m"]);
}

#[test]
fn unicode_public_model_ids_are_exported_exactly_for_every_client() {
    let id = "模".repeat(100);
    for client in ByokClient::ALL {
        let h = harness("unicode-full-id");
        let before = inspect(&h.host, client);
        let result = configure(
            &h.host,
            client,
            before.fingerprint.as_deref().unwrap(),
            vec![model(&id, 1000, None)],
            None,
        )
        .unwrap();
        assert_eq!(result.configured_model_ids, vec![id.clone()]);
        assert_eq!(result.default_model_id.as_deref(), Some(id.as_str()));
    }
}

#[test]
fn failed_first_adoption_restores_absent_receipt() {
    let h = harness("absent-receipt");
    *h.host.fail_after_writes.lock().unwrap() = Some(0);
    let view = inspect(&h.host, ByokClient::Kimi);
    configure(
        &h.host,
        ByokClient::Kimi,
        view.fingerprint.as_deref().unwrap(),
        vec![model("m", 1000, None)],
        None,
    )
    .unwrap_err();
    let target = ResolvedTarget {
        client: ByokClient::Kimi,
        path: target_file(&h.host, ByokClient::Kimi),
        discovery_source: "default".into(),
    };
    let store = Store::open(&h.host.data_dir, &target).unwrap();
    assert!(store.load().unwrap().is_none());
    assert!(!target_file(&h.host, ByokClient::Kimi).is_file());
}

#[test]
fn mixed_codex_interruption_rolls_each_file_to_old() {
    let h = harness("mixed-codex");
    let client = ByokClient::Codex;
    let path = target_file(&h.host, client);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(&path, "model = \"gpt-4\"\nmodel_provider = \"openai\"\n").unwrap();
    *h.host.fail_after_writes.lock().unwrap() = Some(1);
    let view = inspect(&h.host, client);
    configure(
        &h.host,
        client,
        view.fingerprint.as_deref().unwrap(),
        vec![model("m", 1000, None)],
        Some("m".into()),
    )
    .unwrap_err();
    assert!(read_text(&path).contains("openai"));
    assert!(read_text(&path).contains("gpt-4"));
    let catalog = path
        .parent()
        .unwrap()
        .join(".ocg-byok")
        .join("model_catalog.json");
    assert!(!catalog.is_file());
    let store = Store::open(
        &h.host.data_dir,
        &ResolvedTarget {
            client,
            path: path.clone(),
            discovery_source: "default".into(),
        },
    )
    .unwrap();
    assert!(store.load().unwrap().is_none());
}

#[test]
fn codex_remove_restores_original_openai_selection() {
    let h = harness("codex-openai");
    let path = target_file(&h.host, ByokClient::Codex);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(&path, "model = \"gpt-4\"\nmodel_provider = \"openai\"\n").unwrap();
    let view = inspect(&h.host, ByokClient::Codex);
    configure(
        &h.host,
        ByokClient::Codex,
        view.fingerprint.as_deref().unwrap(),
        vec![model("ocg-model", 1000, None)],
        Some("ocg-model".into()),
    )
    .unwrap();
    assert!(read_text(&path).contains("ocg-model"));
    remove(
        &h.host,
        ByokClient::Codex,
        inspect(&h.host, ByokClient::Codex)
            .fingerprint
            .as_deref()
            .unwrap(),
    )
    .unwrap();
    let text = read_text(&path);
    assert!(text.contains("gpt-4"));
    assert!(text.contains("openai"));
}

#[test]
fn existing_codex_catalog_file_is_unowned_collision() {
    let h = harness("catalog-collision");
    let path = target_file(&h.host, ByokClient::Codex);
    let catalog = path
        .parent()
        .unwrap()
        .join(".ocg-byok")
        .join("model_catalog.json");
    fs::create_dir_all(catalog.parent().unwrap()).unwrap();
    fs::write(&catalog, "{\"models\":[]}").unwrap();
    let view = inspect(&h.host, ByokClient::Codex);
    let err = configure(
        &h.host,
        ByokClient::Codex,
        view.fingerprint.as_deref().unwrap(),
        vec![model("m", 1000, None)],
        None,
    )
    .unwrap_err();
    assert_eq!(err.kind, crate::byok_application::ByokErrorKind::Conflict);
}

#[test]
fn kimi_unowned_model_alias_is_rejected() {
    let h = harness("kimi-alias");
    let path = target_file(&h.host, ByokClient::Kimi);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(
        &path,
        "[models.\"ocg/taken\"]\nprovider = \"other\"\nmodel = \"taken\"\n",
    )
    .unwrap();
    let view = inspect(&h.host, ByokClient::Kimi);
    let err = configure(
        &h.host,
        ByokClient::Kimi,
        view.fingerprint.as_deref().unwrap(),
        vec![model("taken", 1000, None)],
        None,
    )
    .unwrap_err();
    assert_eq!(err.kind, crate::byok_application::ByokErrorKind::Conflict);
}

#[test]
fn minimax_user_model_metadata_edit_conflicts() {
    let h = harness("mm-edit");
    let first = inspect(&h.host, ByokClient::Minimax);
    configure(
        &h.host,
        ByokClient::Minimax,
        first.fingerprint.as_deref().unwrap(),
        vec![model("m1", 1000, Some(100))],
        None,
    )
    .unwrap();
    let path = target_file(&h.host, ByokClient::Minimax);
    let mut text = read_text(&path);
    text = text.replace("context: 1000", "context: 2000");
    fs::write(&path, text).unwrap();
    let view = inspect(&h.host, ByokClient::Minimax);
    assert_eq!(view.status, ByokStatus::Conflict);
    let err = configure(
        &h.host,
        ByokClient::Minimax,
        view.fingerprint.as_deref().unwrap(),
        vec![model("m1", 1000, Some(100))],
        None,
    )
    .unwrap_err();
    assert_eq!(err.kind, crate::byok_application::ByokErrorKind::Conflict);
}

#[test]
fn zcode_remove_keeps_unrelated_user_data() {
    let h = harness("zcode-keep");
    let path = target_file(&h.host, ByokClient::Zcode);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(
        &path,
        r#"{"schemaVersion":1,"config":{"providerOrder":[],"providerConfigRules":{"providerRules":[]},"modelConfigRules":{"providerModelRules":[],"manualProviderModelRules":[]},"extraUser":true}}"#,
    )
    .unwrap();
    let view = inspect(&h.host, ByokClient::Zcode);
    configure(
        &h.host,
        ByokClient::Zcode,
        view.fingerprint.as_deref().unwrap(),
        vec![model("z1", 1000, Some(100))],
        None,
    )
    .unwrap();
    remove(
        &h.host,
        ByokClient::Zcode,
        inspect(&h.host, ByokClient::Zcode)
            .fingerprint
            .as_deref()
            .unwrap(),
    )
    .unwrap();
    assert!(read_text(&path).contains("extraUser"));
    assert!(path.is_file());
}

#[test]
fn minimax_external_lock_is_not_deleted() {
    let h = harness("mm-lock-keep");
    let path = target_file(&h.host, ByokClient::Minimax);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(&path, "logLevel: info\n").unwrap();
    let lock = PathBuf::from(format!("{}.lock", path.display()));
    fs::create_dir(&lock).unwrap();
    fs::write(lock.join("foreign"), b"keep").unwrap();
    let view = inspect(&h.host, ByokClient::Minimax);
    let _ = configure(
        &h.host,
        ByokClient::Minimax,
        view.fingerprint.as_deref().unwrap(),
        vec![model("m", 1000, Some(100))],
        None,
    );
    assert!(lock.join("foreign").is_file());
}

#[test]
fn origin_backup_survives_update() {
    let h = harness("origin");
    let path = target_file(&h.host, ByokClient::Kimi);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(&path, "keep = \"first\"\n").unwrap();
    let first = inspect(&h.host, ByokClient::Kimi);
    configure(
        &h.host,
        ByokClient::Kimi,
        first.fingerprint.as_deref().unwrap(),
        vec![model("a", 1000, None)],
        None,
    )
    .unwrap();
    let store = Store::open(
        &h.host.data_dir,
        &ResolvedTarget {
            client: ByokClient::Kimi,
            path: path.clone(),
            discovery_source: "default".into(),
        },
    )
    .unwrap();
    let origin = fs::read(store.origin_dir().join("target.bin")).unwrap();
    let second = inspect(&h.host, ByokClient::Kimi);
    configure(
        &h.host,
        ByokClient::Kimi,
        second.fingerprint.as_deref().unwrap(),
        vec![model("b", 2000, None)],
        None,
    )
    .unwrap();
    assert_eq!(
        fs::read(store.origin_dir().join("target.bin")).unwrap(),
        origin
    );
    assert!(String::from_utf8(origin).unwrap().contains("first"));
}

#[test]
fn codex_catalog_omits_invented_reasoning_and_keeps_efforts() {
    let h = harness("catalog-shape");
    let mut with_effort = model("reasoner", 8000, None);
    with_effort.metadata.reasoning = Some(true);
    with_effort.metadata.reasoning_efforts = Some(
        [("high".into(), "high".into()), ("low".into(), "low".into())]
            .into_iter()
            .collect(),
    );
    let unknown = model("plain", 4000, None);
    let view = inspect(&h.host, ByokClient::Codex);
    configure(
        &h.host,
        ByokClient::Codex,
        view.fingerprint.as_deref().unwrap(),
        vec![with_effort, unknown],
        None,
    )
    .unwrap();
    let catalog = target_file(&h.host, ByokClient::Codex)
        .parent()
        .unwrap()
        .join(".ocg-byok")
        .join("model_catalog.json");
    let value: serde_json::Value = serde_json::from_slice(&fs::read(&catalog).unwrap()).unwrap();
    let models = value.get("models").and_then(|v| v.as_array()).unwrap();
    let reasoner = models.iter().find(|m| m["slug"] == "reasoner").unwrap();
    assert!(
        reasoner["supported_reasoning_levels"]
            .as_array()
            .is_some_and(|levels| levels.len() == 2)
    );
    let plain = models.iter().find(|m| m["slug"] == "plain").unwrap();
    assert!(plain["default_reasoning_level"].is_null());
    assert_eq!(
        plain["supported_reasoning_levels"]
            .as_array()
            .unwrap()
            .len(),
        0
    );
    assert_eq!(plain["shell_type"], "shell_command");
    assert_eq!(plain["truncation_policy"]["mode"], "bytes");
}

fn lock_policy_fast() -> LockPolicy {
    let mut policy = LockPolicy::default();
    policy.minimax_max_wait = Duration::from_millis(80);
    policy.minimax_retry = Duration::from_millis(5);
    policy.minimax_heartbeat = Duration::from_millis(20);
    policy.zcode_max_wait = Duration::from_millis(80);
    policy.zcode_retry_delays_ms = vec![5];
    policy
}

fn dead_pid() -> u32 {
    let mut child = if cfg!(windows) {
        Command::new("cmd")
            .args(["/C", "exit"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap()
    } else {
        Command::new("true")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap()
    };
    let pid = child.id();
    let _ = child.wait();
    pid
}

fn write_ocg_sidecar(sidecar: &Path, pid: u32, lock_dir: &Path) {
    let identity = capture_dir_id(lock_dir).unwrap();
    fs::write(sidecar, sidecar_json(pid, "dead", identity)).unwrap();
}

fn store_plan(
    path: &Path,
    bytes: Option<Vec<u8>>,
    first_owned: serde_json::Value,
    baseline: Option<&str>,
) -> ApplyPlan {
    ApplyPlan {
        files: vec![PlannedFile {
            role: FileRole::Target,
            path: path.to_path_buf(),
            new_bytes: bytes,
        }],
        created_target: true,
        created_catalog: false,
        baseline_default: baseline.map(str::to_string),
        last_applied_default: None,
        managed: ManagedSnapshot {
            provider_id: "ocg".into(),
            model_ids: Vec::new(),
            owned: json!({}),
            applied_default: None,
        },
        first_owned,
    }
}

#[test]
fn minimax_directory_mtime_advances_on_heartbeat() {
    let root =
        std::env::temp_dir().join(format!("ocg-byok-mtime-{}", uuid::Uuid::new_v4().simple()));
    fs::create_dir_all(&root).unwrap();
    let target = root.join("config.yaml");
    fs::write(&target, b"x").unwrap();
    let policy = lock_policy_fast();
    let held = CrossProcessLock::acquire(ByokClient::Minimax, &target, &policy).unwrap();
    held.assert_held().unwrap();
    let lock_dir = lock_dir_for(&target).unwrap();
    assert!(lock_dir.read_dir().unwrap().next().is_none());
    let before = fs::metadata(&lock_dir).unwrap().modified().unwrap();
    std::thread::sleep(Duration::from_millis(80));
    held.assert_held().unwrap();
    let after_heartbeat = fs::metadata(&lock_dir).unwrap().modified().unwrap();
    assert!(after_heartbeat > before);
    set_directory_mtime(&lock_dir).unwrap();
    let after_touch = fs::metadata(&lock_dir).unwrap().modified().unwrap();
    assert!(after_touch >= after_heartbeat);
    drop(held);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn dead_ocg_minimax_lock_is_reclaimed() {
    let root = std::env::temp_dir().join(format!(
        "ocg-byok-deadlock-{}",
        uuid::Uuid::new_v4().simple()
    ));
    fs::create_dir_all(&root).unwrap();
    let target = root.join("config.yaml");
    fs::write(&target, b"x").unwrap();
    let lock_dir = lock_dir_for(&target).unwrap();
    fs::create_dir(&lock_dir).unwrap();
    write_ocg_sidecar(&ocg_sidecar_for(&lock_dir), dead_pid(), &lock_dir);
    let held =
        CrossProcessLock::acquire(ByokClient::Minimax, &target, &lock_policy_fast()).unwrap();
    held.assert_held().unwrap();
    drop(held);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn live_ocg_and_external_minimax_locks_are_retained() {
    let root = std::env::temp_dir().join(format!(
        "ocg-byok-keeplock-{}",
        uuid::Uuid::new_v4().simple()
    ));
    fs::create_dir_all(&root).unwrap();
    let live = root.join("live.yaml");
    fs::write(&live, b"x").unwrap();
    let live_dir = lock_dir_for(&live).unwrap();
    fs::create_dir(&live_dir).unwrap();
    write_ocg_sidecar(&ocg_sidecar_for(&live_dir), std::process::id(), &live_dir);
    let err = CrossProcessLock::acquire(ByokClient::Minimax, &live, &lock_policy_fast())
        .err()
        .expect("lock acquisition must fail");
    assert_eq!(
        err.kind,
        crate::byok_application::ByokErrorKind::Precondition
    );
    assert!(live_dir.is_dir());
    assert!(ocg_sidecar_for(&live_dir).is_file());

    let foreign = root.join("foreign.yaml");
    fs::write(&foreign, b"x").unwrap();
    let foreign_dir = lock_dir_for(&foreign).unwrap();
    fs::create_dir(&foreign_dir).unwrap();
    fs::write(foreign_dir.join("foreign"), b"keep").unwrap();
    let err = CrossProcessLock::acquire(ByokClient::Minimax, &foreign, &lock_policy_fast())
        .err()
        .expect("lock acquisition must fail");
    assert_eq!(
        err.kind,
        crate::byok_application::ByokErrorKind::Precondition
    );
    assert_eq!(fs::read(foreign_dir.join("foreign")).unwrap(), b"keep");

    let zpath = root.join("provider_config.json");
    fs::write(&zpath, b"{}").unwrap();
    let zlock = lock_dir_for(&zpath).unwrap();
    fs::create_dir(&zlock).unwrap();
    fs::write(zlock.join("owner-123.json"), b"{\"pid\":1}").unwrap();
    let err = CrossProcessLock::acquire(ByokClient::Zcode, &zpath, &lock_policy_fast())
        .err()
        .expect("lock acquisition must fail");
    assert_eq!(
        err.kind,
        crate::byok_application::ByokErrorKind::Precondition
    );
    assert!(zlock.join("owner-123.json").is_file());
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn link_or_junction_ancestor_refuses_read_and_delete() {
    let root =
        std::env::temp_dir().join(format!("ocg-byok-junc-{}", uuid::Uuid::new_v4().simple()));
    let real = root.join("real");
    fs::create_dir_all(&real).unwrap();
    let secret = real.join("secret.bin");
    fs::write(&secret, b"hidden").unwrap();
    let link = root.join("link");
    let linked = create_dir_link(&real, &link);
    if !linked {
        let _ = fs::remove_dir_all(root);
        return;
    }
    let through = link.join("secret.bin");
    let read = super::fs::read_regular_file(&through);
    assert!(read.is_err());
    let missing = super::fs::read_regular_file(&link.join("absent.bin"));
    assert!(missing.is_err());
    let remove = super::fs::remove_regular_file(&through);
    assert!(remove.is_err());
    assert!(secret.is_file());
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn journal_survives_when_prior_restore_fails() {
    let root = std::env::temp_dir().join(format!(
        "ocg-byok-journalkeep-{}",
        uuid::Uuid::new_v4().simple()
    ));
    let data = root.join("data");
    fs::create_dir_all(&data).unwrap();
    let target = root.join("config.toml");
    fs::write(&target, b"old").unwrap();
    let resolved = ResolvedTarget {
        client: ByokClient::Kimi,
        path: target.clone(),
        discovery_source: "default".into(),
    };
    let store = Store::open(&data, &resolved).unwrap();
    store
        .apply(
            &resolved,
            None,
            store_plan(&target, Some(b"new1".to_vec()), json!({"n": 1}), Some("a")),
            PendingKind::Configure,
        )
        .unwrap();
    fs::remove_file(store.receipt_path()).unwrap();
    fs::create_dir(store.receipt_path()).unwrap();
    *store.fail_after_writes.lock().unwrap() = Some(0);
    store
        .apply(
            &resolved,
            Some(new_receipt(&resolved)),
            store_plan(&target, Some(b"new2".to_vec()), json!({"n": 2}), Some("b")),
            PendingKind::Configure,
        )
        .unwrap_err();
    assert!(store.journal_path().is_file());
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn apply_uses_plan_baseline_each_configure_and_keeps_created_flags() {
    let root =
        std::env::temp_dir().join(format!("ocg-byok-plan-{}", uuid::Uuid::new_v4().simple()));
    let data = root.join("data");
    fs::create_dir_all(&data).unwrap();
    let target = root.join("config.toml");
    fs::write(&target, b"old").unwrap();
    let resolved = ResolvedTarget {
        client: ByokClient::Kimi,
        path: target.clone(),
        discovery_source: "default".into(),
    };
    let store = Store::open(&data, &resolved).unwrap();
    let first = store
        .apply(
            &resolved,
            None,
            store_plan(&target, Some(b"n1".to_vec()), json!({"n": 1}), Some("orig")),
            PendingKind::Configure,
        )
        .unwrap()
        .unwrap();
    assert_eq!(first.first_owned, json!({"n": 1}));
    assert_eq!(first.baseline_default.as_deref(), Some("orig"));
    assert!(first.created_target);
    let mut second_plan = store_plan(
        &target,
        Some(b"n2".to_vec()),
        json!({"n": 2}),
        Some("newer"),
    );
    second_plan.created_target = false;
    let second = store
        .apply(&resolved, Some(first), second_plan, PendingKind::Configure)
        .unwrap()
        .unwrap();
    assert_eq!(second.first_owned, json!({"n": 2}));
    assert_eq!(second.baseline_default.as_deref(), Some("newer"));
    assert!(second.created_target);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn origin_backup_is_rewritten_after_successful_remove() {
    let root = std::env::temp_dir().join(format!(
        "ocg-byok-origin-remove-{}",
        uuid::Uuid::new_v4().simple()
    ));
    let data = root.join("data");
    fs::create_dir_all(&data).unwrap();
    let target = root.join("config.toml");
    fs::write(&target, b"install-a").unwrap();
    let resolved = ResolvedTarget {
        client: ByokClient::Kimi,
        path: target.clone(),
        discovery_source: "default".into(),
    };
    let store = Store::open(&data, &resolved).unwrap();
    let receipt = store
        .apply(
            &resolved,
            None,
            store_plan(&target, Some(b"ocg-a".to_vec()), json!({"n": 1}), None),
            PendingKind::Configure,
        )
        .unwrap()
        .unwrap();
    assert_eq!(
        fs::read(store.origin_dir().join("target.bin")).unwrap(),
        b"install-a"
    );
    store
        .apply(
            &resolved,
            Some(receipt),
            store_plan(&target, None, json!({}), None),
            PendingKind::Remove,
        )
        .unwrap();
    assert!(!store.origin_dir().join("target.bin").is_file());
    fs::write(&target, b"install-c").unwrap();
    store
        .apply(
            &resolved,
            None,
            store_plan(&target, Some(b"ocg-c".to_vec()), json!({"n": 3}), None),
            PendingKind::Configure,
        )
        .unwrap();
    assert_eq!(
        fs::read(store.origin_dir().join("target.bin")).unwrap(),
        b"install-c"
    );
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn stale_minimax_sidecar_does_not_claim_successor_directory() {
    let root = std::env::temp_dir().join(format!(
        "ocg-byok-stale-succ-{}",
        uuid::Uuid::new_v4().simple()
    ));
    fs::create_dir_all(&root).unwrap();
    let target = root.join("config.yaml");
    fs::write(&target, b"x").unwrap();
    let lock_dir = lock_dir_for(&target).unwrap();
    fs::create_dir(&lock_dir).unwrap();
    let stale_id = capture_dir_id(&lock_dir).unwrap();
    let sidecar = ocg_sidecar_for(&lock_dir);
    write_ocg_sidecar(&sidecar, dead_pid(), &lock_dir);
    let sidecar_bytes = fs::read(&sidecar).unwrap();
    fs::remove_dir(&lock_dir).unwrap();
    fs::create_dir(&lock_dir).unwrap();
    let successor_id = capture_dir_id(&lock_dir).unwrap();
    assert_ne!(stale_id, successor_id);
    let err = CrossProcessLock::acquire(ByokClient::Minimax, &target, &lock_policy_fast())
        .err()
        .expect("lock acquisition must fail");
    assert_eq!(
        err.kind,
        crate::byok_application::ByokErrorKind::Precondition
    );
    assert!(lock_dir.is_dir());
    assert_eq!(capture_dir_id(&lock_dir).unwrap(), successor_id);
    assert_eq!(fs::read(&sidecar).unwrap(), sidecar_bytes);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn held_minimax_lock_does_not_delete_replaced_directory() {
    let root = std::env::temp_dir().join(format!(
        "ocg-byok-held-repl-{}",
        uuid::Uuid::new_v4().simple()
    ));
    fs::create_dir_all(&root).unwrap();
    let target = root.join("config.yaml");
    fs::write(&target, b"x").unwrap();
    let held =
        CrossProcessLock::acquire(ByokClient::Minimax, &target, &lock_policy_fast()).unwrap();
    let lock_dir = lock_dir_for(&target).unwrap();
    let sidecar = ocg_sidecar_for(&lock_dir);
    let original_id = capture_dir_id(&lock_dir).unwrap();
    let displaced = root.join("displaced.lock");
    let replaced = fs::rename(&lock_dir, &displaced).is_ok() && fs::create_dir(&lock_dir).is_ok();
    if replaced {
        let successor_id = capture_dir_id(&lock_dir).unwrap();
        assert_ne!(original_id, successor_id);
        assert!(held.assert_held().is_err());
        drop(held);
        assert!(lock_dir.is_dir());
        assert_eq!(capture_dir_id(&lock_dir).unwrap(), successor_id);
    } else {
        let other = root.join("other-id");
        fs::create_dir(&other).unwrap();
        let other_id = capture_dir_id(&other).unwrap();
        assert_ne!(original_id, other_id);
        fs::write(
            &sidecar,
            sidecar_json(std::process::id(), "tamper", other_id),
        )
        .unwrap();
        assert!(held.assert_held().is_err());
        drop(held);
        assert!(lock_dir.is_dir());
        assert_eq!(capture_dir_id(&lock_dir).unwrap(), original_id);
    }
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn preexisting_minimax_sidecar_is_preserved_when_create_new_fails() {
    let root = std::env::temp_dir().join(format!(
        "ocg-byok-side-keep-{}",
        uuid::Uuid::new_v4().simple()
    ));
    fs::create_dir_all(&root).unwrap();
    let target = root.join("config.yaml");
    fs::write(&target, b"x").unwrap();
    let lock_dir = lock_dir_for(&target).unwrap();
    let sidecar = ocg_sidecar_for(&lock_dir);
    fs::write(&sidecar, b"foreign-owner\n").unwrap();
    let err = CrossProcessLock::acquire(ByokClient::Minimax, &target, &lock_policy_fast())
        .err()
        .expect("lock acquisition must fail");
    assert_ne!(
        err.kind,
        crate::byok_application::ByokErrorKind::Precondition
    );
    assert_eq!(fs::read(&sidecar).unwrap(), b"foreign-owner\n");
    assert!(!lock_dir.exists());
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn dead_ocg_lock_recovers_after_upstream_takeover_and_release() {
    let root = std::env::temp_dir().join(format!(
        "ocg-byok-orphan-side-{}",
        uuid::Uuid::new_v4().simple()
    ));
    fs::create_dir_all(&root).unwrap();
    let target = root.join("config.yaml");
    fs::write(&target, b"x").unwrap();
    let lock_dir = lock_dir_for(&target).unwrap();
    fs::create_dir(&lock_dir).unwrap();
    let sidecar = ocg_sidecar_for(&lock_dir);
    write_ocg_sidecar(&sidecar, dead_pid(), &lock_dir);
    fs::remove_dir(&lock_dir).unwrap();
    fs::create_dir(&lock_dir).unwrap();
    fs::remove_dir(&lock_dir).unwrap();
    assert!(sidecar.is_file());
    assert!(!lock_dir.exists());
    let held =
        CrossProcessLock::acquire(ByokClient::Minimax, &target, &lock_policy_fast()).unwrap();
    held.assert_held().unwrap();
    assert!(lock_dir.is_dir());
    let owner: serde_json::Value = serde_json::from_slice(&fs::read(&sidecar).unwrap()).unwrap();
    assert_eq!(owner["kind"], "ocg");
    assert_eq!(
        owner.get("pid").and_then(serde_json::Value::as_u64),
        Some(u64::from(std::process::id()))
    );
    drop(held);
    assert!(!lock_dir.exists());
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn live_minimax_sidecar_without_lock_dir_is_preserved() {
    let root = std::env::temp_dir().join(format!(
        "ocg-byok-live-orphan-{}",
        uuid::Uuid::new_v4().simple()
    ));
    fs::create_dir_all(&root).unwrap();
    let target = root.join("config.yaml");
    fs::write(&target, b"x").unwrap();
    let lock_dir = lock_dir_for(&target).unwrap();
    fs::create_dir(&lock_dir).unwrap();
    let sidecar = ocg_sidecar_for(&lock_dir);
    write_ocg_sidecar(&sidecar, std::process::id(), &lock_dir);
    let sidecar_bytes = fs::read(&sidecar).unwrap();
    fs::remove_dir(&lock_dir).unwrap();
    let err = CrossProcessLock::acquire(ByokClient::Minimax, &target, &lock_policy_fast())
        .err()
        .expect("lock acquisition must fail");
    assert_ne!(
        err.kind,
        crate::byok_application::ByokErrorKind::Precondition
    );
    assert_eq!(fs::read(&sidecar).unwrap(), sidecar_bytes);
    assert!(!lock_dir.exists());
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn missing_target_configure_through_linked_ancestor_refuses_before_writes() {
    let h = harness("link-missing");
    let outside = h.root.join("outside");
    fs::create_dir_all(&outside).unwrap();
    let marker = outside.join("keep.bin");
    fs::write(&marker, b"keep").unwrap();
    let link = h.root.join("home").join("linked");
    if !create_dir_link(&outside, &link) {
        return;
    }
    let target = link.join("missing").join("config.toml");
    let outside_before: Vec<_> = fs::read_dir(&outside)
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect();
    let err = h
        .host
        .execute(ByokHostRequest::Configure {
            client: ByokClient::Codex,
            target_path: Some(target.to_string_lossy().into_owned()),
            expected_fingerprint: "unused".into(),
            gateway_v1_url: GATEWAY.into(),
            secret: secret(),
            models: vec![model("m", 1000, None)],
            default_model_id: None,
            client_closed: true,
        })
        .unwrap_err();
    assert_eq!(err.kind, crate::byok_application::ByokErrorKind::Conflict);
    assert!(!target.exists());
    assert!(!target.parent().unwrap().exists());
    assert!(!outside.join("missing").exists());
    assert!(!outside.join("config.toml").exists());
    let outside_after: Vec<_> = fs::read_dir(&outside)
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect();
    assert_eq!(outside_before, outside_after);
    assert_eq!(fs::read(&marker).unwrap(), b"keep");
}

#[test]
fn recover_refuses_mismatched_prior_receipt_without_touching_files() {
    let root = std::env::temp_dir().join(format!(
        "ocg-byok-prior-id-{}",
        uuid::Uuid::new_v4().simple()
    ));
    let data = root.join("data");
    fs::create_dir_all(&data).unwrap();
    let target = root.join("config.toml");
    fs::write(&target, b"current").unwrap();
    let resolved = ResolvedTarget {
        client: ByokClient::Kimi,
        path: target.clone(),
        discovery_source: "default".into(),
    };
    let store = Store::open(&data, &resolved).unwrap();
    let receipt = store
        .apply(
            &resolved,
            None,
            store_plan(&target, Some(b"current".to_vec()), json!({"n": 1}), None),
            PendingKind::Configure,
        )
        .unwrap()
        .unwrap();
    fs::create_dir_all(store.backup_dir()).unwrap();
    fs::write(store.backup_dir().join("old-target.bin"), b"old").unwrap();
    let old_hash = hex::encode(Sha256::digest(b"old"));
    let new_hash = hex::encode(Sha256::digest(b"current"));
    let mut bad_version = receipt.clone();
    bad_version.version = RECEIPT_VERSION + 7;
    let journal = Journal {
        prior_receipt: Some(bad_version),
        kind: PendingKind::Configure,
        files: vec![PendingFile {
            role: FileRole::Target,
            old_hash: old_hash.clone(),
            new_hash: new_hash.clone(),
        }],
    };
    fs::write(
        store.journal_path(),
        serde_json::to_vec_pretty(&journal).unwrap(),
    )
    .unwrap();
    let journal_bytes = fs::read(store.journal_path()).unwrap();
    store.recover_journal(&resolved, &journal).unwrap_err();
    assert_eq!(fs::read(&target).unwrap(), b"current");
    assert_eq!(fs::read(store.journal_path()).unwrap(), journal_bytes);
    assert_eq!(
        fs::read(store.backup_dir().join("old-target.bin")).unwrap(),
        b"old"
    );

    let mut bad_identity = receipt;
    bad_identity.identity = "other-target-identity".into();
    bad_identity.target_path = root.join("other.toml").to_string_lossy().into_owned();
    let journal = Journal {
        prior_receipt: Some(bad_identity),
        kind: PendingKind::Configure,
        files: vec![PendingFile {
            role: FileRole::Target,
            old_hash,
            new_hash,
        }],
    };
    fs::write(
        store.journal_path(),
        serde_json::to_vec_pretty(&journal).unwrap(),
    )
    .unwrap();
    let journal_bytes = fs::read(store.journal_path()).unwrap();
    store.recover_journal(&resolved, &journal).unwrap_err();
    assert_eq!(fs::read(&target).unwrap(), b"current");
    assert_eq!(fs::read(store.journal_path()).unwrap(), journal_bytes);
    fs::remove_dir_all(root).unwrap();
}

fn create_dir_link(src: &Path, dst: &Path) -> bool {
    #[cfg(windows)]
    {
        Command::new("cmd")
            .args([
                "/C",
                "mklink",
                "/J",
                &dst.to_string_lossy(),
                &src.to_string_lossy(),
            ])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map(|status| status.success())
            .unwrap_or(false)
    }
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(src, dst).is_ok()
    }
    #[cfg(not(any(windows, unix)))]
    {
        let _ = (src, dst);
        false
    }
}

fn make_symlink(src: &Path, dst: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(src, dst)
    }
    #[cfg(windows)]
    {
        std::os::windows::fs::symlink_file(src, dst)
    }
}
