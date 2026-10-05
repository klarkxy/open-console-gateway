use super::*;
use fs2::FileExt;
use rusqlite::Connection;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

const KEY_MATERIAL: &str = "synthetic-encryption-key-material";
const OAUTH_TOKEN: &str = "synthetic-oauth-token";
const CLIENT_KEY: &str = "synthetic-cpa-client-key";

fn temp_root(label: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("ocg-backup-{label}-{}", uuid::Uuid::new_v4()));
    fs::create_dir_all(&dir).unwrap();
    dir
}

fn witness(explicit_key: bool) -> CipherWitness {
    if explicit_key {
        CipherWitness::explicit(KEY_MATERIAL)
    } else {
        CipherWitness::none()
    }
}

fn write_sqlite(path: &Path, schema: i64, user_version: i64) {
    let connection = Connection::open(path).unwrap();
    connection
        .execute_batch(&format!(
            "CREATE TABLE schema_version (version INTEGER PRIMARY KEY);
             INSERT INTO schema_version (version) VALUES ({schema});
             PRAGMA user_version = {user_version};"
        ))
        .unwrap();
}

fn auth_dir_for(data_dir: &Path) -> String {
    format!(
        "{}/cpa/auth",
        portable_data_dir(&absolute_real_path(data_dir).unwrap())
    )
}

fn write_owned_cpa(source: &Path, auth_dir: &str) {
    fs::create_dir_all(source.join("cpa/auth")).unwrap();
    fs::create_dir_all(source.join("cpa/versions")).unwrap();
    let managed = br#"{"desiredRunning":true,"note":"keep-bytes"}"#;
    fs::write(source.join("cpa/managed.json"), managed).unwrap();
    let yaml = format!(
        "host: \"127.0.0.1\"\nport: 8085\nauth-dir: \"{auth_dir}\"\ndebug: false\napi-keys:\n  - \"{CLIENT_KEY}\"\nprojection:\n  generation: 7\n  note: keep-projection\n"
    );
    fs::write(source.join("cpa/config.yaml"), &yaml).unwrap();
    fs::write(source.join("cpa/config.yaml.previous"), &yaml).unwrap();
    fs::write(source.join("cpa/auth/token.json"), OAUTH_TOKEN).unwrap();
    fs::write(source.join("cpa/versions/note.txt"), b"installed").unwrap();
}

fn write_product(source: &Path, schema: i64, user_version: i64) {
    fs::create_dir_all(source).unwrap();
    write_owned_cpa(source, &auth_dir_for(source));
    fs::create_dir_all(source.join("plans")).unwrap();
    fs::create_dir_all(source.join("policy")).unwrap();
    fs::create_dir_all(source.join("keep/empty-dir")).unwrap();
    fs::write(source.join(".encryption-key"), KEY_MATERIAL).unwrap();
    fs::write(source.join("plans/rank.txt"), b"rank-a").unwrap();
    fs::write(source.join("policy/future.txt"), b"future-policy").unwrap();
    fs::write(
        source.join("cli-listener.json"),
        br#"{"pid":1,"endpoint":"http://127.0.0.1:1"}"#,
    )
    .unwrap();
    fs::write(source.join("cli-listener.json.tmp"), b"tmp").unwrap();
    write_sqlite(&source.join("data.sqlite"), schema, user_version);
    for name in LOCK_FILES {
        fs::write(source.join(name), b"sentinel").unwrap();
    }
}

fn parsed_auth_dir(path: &Path) -> String {
    let text = fs::read_to_string(path).unwrap();
    let value: serde_yaml_ng::Value = serde_yaml_ng::from_str(&text).unwrap();
    let mapping = match &value {
        serde_yaml_ng::Value::Mapping(mapping) => mapping,
        other => panic!("config is not a mapping: {other:?}"),
    };
    let key = serde_yaml_ng::Value::String("auth-dir".to_string());
    match mapping.get(&key) {
        Some(serde_yaml_ng::Value::String(auth_dir)) => auth_dir.clone(),
        other => panic!("auth-dir missing: {other:?}"),
    }
}

fn yaml_value(path: &Path, field: &str) -> serde_yaml_ng::Value {
    let text = fs::read_to_string(path).unwrap();
    let value: serde_yaml_ng::Value = serde_yaml_ng::from_str(&text).unwrap();
    let mapping = match &value {
        serde_yaml_ng::Value::Mapping(mapping) => mapping,
        other => panic!("config is not a mapping: {other:?}"),
    };
    mapping
        .get(serde_yaml_ng::Value::String(field.to_string()))
        .cloned()
        .unwrap_or_else(|| panic!("missing {field}"))
}

fn receipt_value(receipt: &SnapshotReceipt) -> serde_json::Value {
    let line = receipt.to_json_line();
    assert!(!line.contains(KEY_MATERIAL), "{line}");
    assert!(!line.contains(OAUTH_TOKEN), "{line}");
    assert!(!line.contains(CLIENT_KEY), "{line}");
    assert!(!line.contains("portable"), "{line}");
    assert!(!line.contains("prerequisite"), "{line}");
    let value: serde_json::Value = serde_json::from_str(line.trim()).unwrap();
    let mut keys = value
        .as_object()
        .unwrap()
        .keys()
        .cloned()
        .collect::<Vec<_>>();
    keys.sort();
    assert_eq!(keys, vec!["count", "format", "path", "state", "version"]);
    value
}

fn manifest_of(archive: &Path) -> (Manifest, String) {
    let file = File::open(archive).unwrap();
    let decoder = flate2::read::GzDecoder::new(file);
    let mut tar = tar::Archive::new(decoder);
    for entry in tar.entries().unwrap() {
        let mut entry = entry.unwrap();
        let path = entry.path().unwrap().to_path_buf();
        if path.as_os_str() == MANIFEST_NAME {
            let mut text = String::new();
            entry.read_to_string(&mut text).unwrap();
            let manifest: Manifest = serde_json::from_str(&text).unwrap();
            return (manifest, text);
        }
    }
    panic!("manifest missing");
}

fn assert_no_stage(parent: &Path) {
    for entry in fs::read_dir(parent).unwrap() {
        let name = entry.unwrap().file_name().to_string_lossy().into_owned();
        assert!(
            !name.starts_with(".ocg-restore-stage-"),
            "stage left behind: {name}"
        );
        assert!(
            !name.starts_with(".ocg-restore-empty-"),
            "empty aside left behind: {name}"
        );
    }
}

fn sqlite_identity(path: &Path) -> (i64, i64) {
    let connection =
        Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
    let user_version: i64 = connection
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .unwrap();
    let schema: i64 = connection
        .query_row(
            "SELECT version FROM schema_version ORDER BY version DESC LIMIT 1",
            [],
            |row| row.get(0),
        )
        .unwrap();
    (user_version, schema)
}

#[test]
fn roundtrip_restores_owned_cpa_sqlite_and_omits_runtime_markers() {
    let root = temp_root("roundtrip");
    let source = root.join("source-root");
    let target = root.join("target-root");
    let output = root.join("snapshot.tar.gz");
    write_product(&source, 64, 11);
    let sqlite_before = fs::read(source.join("data.sqlite")).unwrap();
    let managed_before = fs::read(source.join("cpa/managed.json")).unwrap();
    let source_auth = auth_dir_for(&source);
    fs::create_dir(&target).unwrap();

    let created = create_snapshot(&source, &output, witness(false)).unwrap();
    let created_json = receipt_value(&created);
    assert_eq!(created_json["format"], FORMAT);
    assert_eq!(created_json["version"], FORMAT_VERSION);
    assert_eq!(created_json["state"], "created");
    assert_eq!(fs::read(source.join("data.sqlite")).unwrap(), sqlite_before);
    assert_eq!(
        fs::read(source.join("cli-listener.json")).unwrap(),
        br#"{"pid":1,"endpoint":"http://127.0.0.1:1"}"#
    );
    assert_eq!(
        parsed_auth_dir(&source.join("cpa/config.yaml")),
        source_auth
    );

    let (manifest, manifest_text) = manifest_of(&output);
    assert!(!manifest_text.contains(KEY_MATERIAL), "{manifest_text}");
    assert!(!manifest_text.contains(OAUTH_TOKEN), "{manifest_text}");
    assert!(!manifest_text.contains(CLIENT_KEY), "{manifest_text}");
    assert_eq!(manifest.format, FORMAT);
    assert_eq!(manifest.version, FORMAT_VERSION);
    assert_eq!(manifest.app_version, env!("CARGO_PKG_VERSION"));
    assert_eq!(manifest.sqlite_user_version, Some(11));
    assert_eq!(manifest.schema_version, Some(64));
    assert_eq!(manifest.cipher.prerequisite, "file");
    assert!(manifest.cipher.portable);
    assert!(manifest.cipher.witness.starts_with("v2:"));
    assert_eq!(manifest.cipher.legacy_ciphertext, "none");
    assert!(!manifest_text.contains("validated"), "{manifest_text}");
    assert_eq!(created_json["count"], manifest.entries.len() as u64);
    assert!(
        manifest
            .entries
            .iter()
            .any(|entry| entry.path == "cpa/auth/token.json")
    );
    assert!(
        manifest
            .entries
            .iter()
            .all(|entry| !EXCLUDED_ROOT_FILES.contains(&entry.path.as_str()))
    );

    let restored = restore_snapshot(&target, &output, witness(false)).unwrap();
    let restored_json = receipt_value(&restored);
    assert_eq!(restored_json["state"], "restored");
    assert_eq!(restored_json["count"], created_json["count"]);
    let target_auth = auth_dir_for(&target);
    assert_eq!(
        parsed_auth_dir(&target.join("cpa/config.yaml")),
        target_auth
    );
    assert_eq!(
        parsed_auth_dir(&target.join("cpa/config.yaml.previous")),
        target_auth
    );
    assert!(!target_auth.contains("source-root"));
    match yaml_value(&target.join("cpa/config.yaml"), "projection") {
        serde_yaml_ng::Value::Mapping(projection) => {
            let generation = projection
                .get(serde_yaml_ng::Value::String("generation".to_string()))
                .and_then(serde_yaml_ng::Value::as_i64);
            assert_eq!(generation, Some(7));
        }
        other => panic!("projection missing: {other:?}"),
    }
    match yaml_value(&target.join("cpa/config.yaml"), "api-keys") {
        serde_yaml_ng::Value::Sequence(keys) => {
            assert_eq!(
                keys.first().and_then(serde_yaml_ng::Value::as_str),
                Some(CLIENT_KEY)
            );
        }
        other => panic!("api-keys missing: {other:?}"),
    }
    assert_eq!(
        fs::read(target.join("cpa/managed.json")).unwrap(),
        managed_before
    );
    assert_eq!(
        fs::read(target.join("cpa/auth/token.json")).unwrap(),
        OAUTH_TOKEN.as_bytes()
    );
    assert_eq!(
        fs::read(target.join("policy/future.txt")).unwrap(),
        b"future-policy"
    );
    assert_eq!(fs::read(target.join("data.sqlite")).unwrap(), sqlite_before);
    assert_eq!(sqlite_identity(&target.join("data.sqlite")), (11, 64));
    assert!(target.join("keep/empty-dir").is_dir());
    assert!(!target.join("cli-listener.json").exists());
    assert!(!target.join("cli-listener.json.tmp").exists());
    for name in LOCK_FILES {
        assert!(!target.join(name).exists(), "{name}");
    }
    assert!(!target.join(MANIFEST_NAME).exists());
    assert_eq!(fs::read(source.join("data.sqlite")).unwrap(), sqlite_before);
    assert_no_stage(&root);
    let _ = fs::remove_dir_all(root);
}

#[test]
fn held_writer_locks_reject_create_and_leave_the_source_unchanged() {
    for name in LOCK_FILES {
        let root = temp_root("held-lock");
        let source = root.join("source-root");
        let output = root.join("snapshot.tar.gz");
        write_product(&source, 64, 11);
        let sqlite_before = fs::read(source.join("data.sqlite")).unwrap();
        let rank_before = fs::read(source.join("plans/rank.txt")).unwrap();
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(source.join(name))
            .unwrap();
        lock.try_lock_exclusive().unwrap();
        let error = create_snapshot(&source, &output, witness(false)).unwrap_err();
        let message = format!("{error:#}");
        assert!(message.contains(name), "{message}");
        assert!(message.contains("Stop"), "{message}");
        assert!(!message.contains(OAUTH_TOKEN), "{message}");
        assert!(!message.contains(KEY_MATERIAL), "{message}");
        assert!(!output.exists());
        assert_eq!(fs::read(source.join("data.sqlite")).unwrap(), sqlite_before);
        assert_eq!(
            fs::read(source.join("plans/rank.txt")).unwrap(),
            rank_before
        );
        drop(lock);
        let _ = fs::remove_dir_all(root);
    }
}

#[test]
fn existing_output_and_nonempty_target_are_rejected() {
    let root = temp_root("existing");
    let source = root.join("source-root");
    let output = root.join("snapshot.tar.gz");
    write_product(&source, 64, 11);
    fs::write(&output, b"keep-me").unwrap();
    let error = create_snapshot(&source, &output, witness(false)).unwrap_err();
    assert!(format!("{error:#}").contains("already exists"));
    assert_eq!(fs::read(&output).unwrap(), b"keep-me");

    fs::remove_file(&output).unwrap();
    create_snapshot(&source, &output, witness(false)).unwrap();
    let archive_before = fs::read(&output).unwrap();
    let target = root.join("target-root");
    fs::create_dir(&target).unwrap();
    fs::write(target.join("user.txt"), b"keep-user").unwrap();
    let error = restore_snapshot(&target, &output, witness(false)).unwrap_err();
    assert!(format!("{error:#}").contains("not empty"), "{error:#}");
    assert_eq!(fs::read(target.join("user.txt")).unwrap(), b"keep-user");
    assert!(!target.join("data.sqlite").exists());
    assert_eq!(fs::read(&output).unwrap(), archive_before);
    assert_no_stage(&root);

    let missing = root.join("missing.tar.gz");
    let empty = root.join("empty-target");
    let error = restore_snapshot(&empty, &missing, witness(false)).unwrap_err();
    assert!(!format!("{error:#}").contains(KEY_MATERIAL));
    assert!(!empty.exists());
    let _ = fs::remove_dir_all(root);
}

#[test]
fn tamper_and_incompatible_schema_leave_input_and_target_unusable() {
    let root = temp_root("tamper");
    let source = root.join("source-root");
    let output = root.join("snapshot.tar.gz");
    write_product(&source, 64, 11);
    create_snapshot(&source, &output, witness(false)).unwrap();
    let original = fs::read(&output).unwrap();
    let mut entries = read_entries(&output);
    let rank = entries
        .iter_mut()
        .find(|(name, _, _)| name == "plans/rank.txt")
        .unwrap();
    rank.1 = b"rank-tampered".to_vec();
    write_entries(&output, &entries);
    let tampered = fs::read(&output).unwrap();
    let target = root.join("target-root");
    let error = restore_snapshot(&target, &output, witness(false)).unwrap_err();
    assert!(format!("{error:#}").contains("hash"), "{error:#}");
    assert!(!target.join("data.sqlite").exists());
    assert!(!target.exists() || fs::read_dir(&target).unwrap().next().is_none());
    assert_eq!(fs::read(&output).unwrap(), tampered);
    assert_no_stage(&root);

    let good = root.join("good.tar.gz");
    fs::write(&good, &original).unwrap();
    fs::create_dir_all(&target).unwrap();
    if target.join("data.sqlite").exists() {
        panic!("tampered restore published a database");
    }
    restore_snapshot(&target, &good, witness(false)).unwrap();
    assert_eq!(fs::read(target.join("plans/rank.txt")).unwrap(), b"rank-a");

    let newer_source = root.join("newer-source");
    let newer_output = root.join("newer.tar.gz");
    let newer_target = root.join("newer-target");
    write_product(&newer_source, 9999, 11);
    let created = create_snapshot(&newer_source, &newer_output, witness(false)).unwrap();
    assert_eq!(receipt_value(&created)["state"], "created");
    let newer_bytes = fs::read(&newer_output).unwrap();
    let error = restore_snapshot(&newer_target, &newer_output, witness(false)).unwrap_err();
    assert!(format!("{error:#}").contains("schema"), "{error:#}");
    assert!(!newer_target.exists());
    assert_eq!(fs::read(&newer_output).unwrap(), newer_bytes);
    assert_no_stage(&root);
    let _ = fs::remove_dir_all(root);
}

#[test]
fn older_schema_restores_and_keeps_user_version() {
    let root = temp_root("older");
    let source = root.join("source-root");
    let target = root.join("target-root");
    let output = root.join("snapshot.tar.gz");
    write_product(&source, 40, 11);
    create_snapshot(&source, &output, witness(false)).unwrap();
    restore_snapshot(&target, &output, witness(false)).unwrap();
    assert_eq!(sqlite_identity(&target.join("data.sqlite")), (11, 40));
    let _ = fs::remove_dir_all(root);
}

#[test]
fn unsafe_paths_links_and_case_conflicts_are_rejected() {
    let root = temp_root("unsafe");
    let target = root.join("target-root");
    let cases = [
        "../escape.txt",
        "foo/../../escape.txt",
        "/tmp/ocg-backup-escape.txt",
        "C:/ocg-backup-escape.txt",
    ];
    for (index, path) in cases.iter().enumerate() {
        let archive = root.join(format!("bad-{index}.tar.gz"));
        write_raw_named_archive(&archive, path, b"escape");
        let error = restore_snapshot(&target, &archive, witness(false)).unwrap_err();
        assert!(format!("{error:#}").contains("unsafe"), "{path}: {error:#}");
        assert!(!root.join("escape.txt").exists(), "{path}");
        assert!(!target.exists(), "{path}");
        assert_no_stage(&root);
    }

    let symlink = root.join("symlink.tar.gz");
    write_link_archive(
        &symlink,
        tar::EntryType::Symlink,
        "inside.txt",
        "../escape.txt",
    );
    let error = restore_snapshot(&target, &symlink, witness(false)).unwrap_err();
    assert!(format!("{error:#}").contains("link"), "{error:#}");
    assert!(!root.join("escape.txt").exists());

    let hardlink = root.join("hardlink.tar.gz");
    write_link_archive(&hardlink, tar::EntryType::Link, "inside.txt", "other.txt");
    let error = restore_snapshot(&target, &hardlink, witness(false)).unwrap_err();
    assert!(format!("{error:#}").contains("link"), "{error:#}");

    let mode_link = root.join("mode-link.tar.gz");
    write_mode_archive(&mode_link, "inside.txt", b"x", 0o120_777);
    let error = restore_snapshot(&target, &mode_link, witness(false)).unwrap_err();
    assert!(format!("{error:#}").contains("link"), "{error:#}");

    let duplicate = root.join("duplicate.tar.gz");
    write_regular_archive(&duplicate, &[("same.txt", b"one"), ("same.txt", b"two")]);
    let error = restore_snapshot(&target, &duplicate, witness(false)).unwrap_err();
    assert!(format!("{error:#}").contains("duplicate"), "{error:#}");

    let case_conflict = root.join("case.tar.gz");
    write_regular_archive(
        &case_conflict,
        &[("Tree/A.txt", b"one"), ("tree/a.txt", b"two")],
    );
    let error = restore_snapshot(&target, &case_conflict, witness(false)).unwrap_err();
    assert!(
        format!("{error:#}").contains("duplicate") || format!("{error:#}").contains("case"),
        "{error:#}"
    );
    assert!(!target.exists());
    assert_no_stage(&root);

    let source = root.join("source-root");
    fs::create_dir_all(source.join("plans")).unwrap();
    fs::write(source.join("plans/rank.txt"), b"rank-a").unwrap();
    let link = source.join("plans/rank-link.txt");
    fs::hard_link(source.join("plans/rank.txt"), &link).unwrap();
    let output = root.join("hard.tar.gz");
    let error = create_snapshot(&source, &output, witness(false)).unwrap_err();
    assert!(format!("{error:#}").contains("hard link"), "{error:#}");
    assert!(!output.exists());
    assert_eq!(fs::read(source.join("plans/rank.txt")).unwrap(), b"rank-a");

    let live = source.join("plans/live-link.txt");
    let live_result = {
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(source.join("plans/rank.txt"), &live)
        }
        #[cfg(windows)]
        {
            std::os::windows::fs::symlink_file(source.join("plans/rank.txt"), &live)
        }
        #[cfg(not(any(unix, windows)))]
        {
            Err(std::io::Error::other("no symlink"))
        }
    };
    if live_result.is_ok() {
        let output = root.join("live-link.tar.gz");
        assert!(create_snapshot(&source, &output, witness(false)).is_err());
        assert!(!output.exists());
    }
    let _ = fs::remove_dir_all(root);
}

#[test]
fn external_cpa_auth_dir_is_rejected_without_writing_output() {
    let root = temp_root("external-cpa");
    let source = root.join("source-root");
    fs::create_dir_all(source.join("plans")).unwrap();
    write_owned_cpa(&source, "D:/outside/auth");
    fs::write(source.join("plans/rank.txt"), b"rank-a").unwrap();
    let output = root.join("snapshot.tar.gz");
    let error = create_snapshot(&source, &output, witness(false)).unwrap_err();
    assert!(format!("{error:#}").contains("auth-dir"), "{error:#}");
    assert!(!format!("{error:#}").contains(CLIENT_KEY), "{error:#}");
    assert!(!output.exists());
    assert_eq!(fs::read(source.join("plans/rank.txt")).unwrap(), b"rank-a");

    let unmanaged = root.join("unmanaged");
    fs::create_dir_all(unmanaged.join("cpa")).unwrap();
    fs::write(
        unmanaged.join("cpa/config.yaml"),
        "auth-dir: \"D:/outside/auth\"\n",
    )
    .unwrap();
    let error = create_snapshot(&unmanaged, &output, witness(false)).unwrap_err();
    assert!(format!("{error:#}").contains("owned"), "{error:#}");
    assert!(!output.exists());
    let _ = fs::remove_dir_all(root);
}

#[test]
fn file_cipher_is_the_only_portable_snapshot() {
    let root = temp_root("cipher-file");
    let source = root.join("source-root");
    let target = root.join("target-root");
    let output = root.join("snapshot.tar.gz");
    write_product(&source, 64, 11);
    let created = create_snapshot(&source, &output, witness(false)).unwrap();
    let line = created.to_json_line();
    assert!(!line.contains(KEY_MATERIAL));
    let (manifest, text) = manifest_of(&output);
    assert_eq!(manifest.cipher.prerequisite, "file");
    assert!(manifest.cipher.portable);
    assert!(manifest.cipher.witness.starts_with("v2:"));
    assert_eq!(manifest.cipher.legacy_ciphertext, "none");
    assert!(!text.contains(KEY_MATERIAL));
    assert!(!text.contains("validated"));
    restore_snapshot(&target, &output, witness(false)).unwrap();
    assert_eq!(
        fs::read(target.join(".encryption-key")).unwrap(),
        KEY_MATERIAL.as_bytes()
    );
    let _ = fs::remove_dir_all(root);
}

#[test]
fn external_cipher_requires_a_caller_key_and_is_not_portable() {
    let root = temp_root("cipher-external");
    let source = root.join("source-root");
    let output = root.join("snapshot.tar.gz");
    fs::create_dir_all(source.join("notes")).unwrap();
    fs::write(source.join("notes/secret.txt"), OAUTH_TOKEN).unwrap();
    let created = create_snapshot(&source, &output, witness(true)).unwrap();
    assert!(!created.to_json_line().contains("portable"));
    assert!(!created.to_json_line().contains(OAUTH_TOKEN));
    let (manifest, text) = manifest_of(&output);
    assert_eq!(manifest.cipher.prerequisite, "explicit");
    assert!(!manifest.cipher.portable);
    assert!(manifest.cipher.witness.starts_with("v2:"));
    assert!(!text.contains(OAUTH_TOKEN));
    assert!(!text.contains(KEY_MATERIAL));
    assert!(!source.join(".encryption-key").exists());
    let blocked = root.join("blocked");
    let error = restore_snapshot(&blocked, &output, witness(false)).unwrap_err();
    let message = format!("{error:#}");
    assert!(
        message.contains("witness") || message.contains("did not create a key file"),
        "{message}"
    );
    assert!(!message.contains(OAUTH_TOKEN), "{message}");
    assert!(!message.contains(KEY_MATERIAL), "{message}");
    assert!(!blocked.exists());
    let wrong = root.join("wrong");
    let error = restore_snapshot(
        &wrong,
        &output,
        CipherWitness::explicit("synthetic-wrong-explicit-key"),
    )
    .unwrap_err();
    let message = format!("{error:#}");
    assert!(message.contains("witness"), "{message}");
    assert!(
        !message.contains("synthetic-wrong-explicit-key"),
        "{message}"
    );
    assert!(!message.contains(KEY_MATERIAL), "{message}");
    assert!(!wrong.exists());
    let target = root.join("target-root");
    restore_snapshot(&target, &output, witness(true)).unwrap();
    assert_eq!(
        fs::read(target.join("notes/secret.txt")).unwrap(),
        OAUTH_TOKEN.as_bytes()
    );
    assert_no_stage(&root);
    let _ = fs::remove_dir_all(root);
}

#[test]
fn injected_machine_context_rejects_a_different_cipher() {
    let root = temp_root("cipher-machine");
    let source = root.join("source-root");
    let output = root.join("snapshot.tar.gz");
    fs::create_dir_all(source.join("notes")).unwrap();
    fs::write(source.join("notes/secret.txt"), OAUTH_TOKEN).unwrap();
    let machine_a: std::sync::Arc<dyn crate::crypto::KeyCipher + Send + Sync> =
        std::sync::Arc::new(crate::crypto::StaticKeyCipher::new("synthetic-machine-a"));
    let machine_b: std::sync::Arc<dyn crate::crypto::KeyCipher + Send + Sync> =
        std::sync::Arc::new(crate::crypto::StaticKeyCipher::new("synthetic-machine-b"));
    create_snapshot(
        &source,
        &output,
        CipherWitness::injected_machine(std::sync::Arc::clone(&machine_a)),
    )
    .unwrap();
    let (manifest, text) = manifest_of(&output);
    assert!(!text.contains(OAUTH_TOKEN));
    assert!(!text.contains("synthetic-machine-a"));
    assert!(!text.contains("synthetic-machine-b"));
    assert!(!manifest.cipher.portable);
    assert_eq!(manifest.cipher.prerequisite, "machine");
    assert!(manifest.cipher.witness.starts_with("v2:"));
    let wrong = root.join("wrong-machine");
    let error =
        restore_snapshot(&wrong, &output, CipherWitness::injected_machine(machine_b)).unwrap_err();
    let message = format!("{error:#}");
    assert!(message.contains("witness"), "{message}");
    assert!(!message.contains("synthetic-machine-a"), "{message}");
    assert!(!message.contains("synthetic-machine-b"), "{message}");
    assert!(!message.contains("validated"), "{message}");
    assert!(!wrong.exists());
    let target = root.join("target-root");
    restore_snapshot(&target, &output, CipherWitness::injected_machine(machine_a)).unwrap();
    assert_eq!(
        fs::read(target.join("notes/secret.txt")).unwrap(),
        OAUTH_TOKEN.as_bytes()
    );
    assert_no_stage(&root);
    let _ = fs::remove_dir_all(root);
}

#[test]
fn manifest_format_version_and_cipher_claims_are_rejected() {
    let root = temp_root("manifest-claims");
    let source = root.join("source-root");
    let output = root.join("snapshot.tar.gz");
    write_product(&source, 64, 11);
    create_snapshot(&source, &output, witness(false)).unwrap();

    let unknown = root.join("unknown.tar.gz");
    fs::copy(&output, &unknown).unwrap();
    rewrite_manifest(&unknown, |manifest| manifest.format = "other".to_string());
    let target = root.join("target-root");
    let error = restore_snapshot(&target, &unknown, witness(false)).unwrap_err();
    assert!(format!("{error:#}").contains("format"), "{error:#}");
    assert!(!target.exists());

    let version = root.join("version.tar.gz");
    fs::copy(&output, &version).unwrap();
    rewrite_manifest(&version, |manifest| manifest.version = 2);
    assert!(restore_snapshot(&target, &version, witness(false)).is_err());
    assert!(!target.exists());

    let extra = root.join("extra.tar.gz");
    fs::copy(&output, &extra).unwrap();
    inject_unknown_manifest_field(&extra);
    assert!(restore_snapshot(&target, &extra, witness(false)).is_err());
    assert!(!target.exists());

    let mismatched = root.join("mismatched.tar.gz");
    fs::copy(&output, &mismatched).unwrap();
    rewrite_manifest(&mismatched, |manifest| {
        manifest.cipher.prerequisite = "external".to_string();
        manifest.cipher.portable = false;
    });
    let error = restore_snapshot(&target, &mismatched, witness(true)).unwrap_err();
    assert!(
        format!("{error:#}").contains("cipher") || format!("{error:#}").contains("key file"),
        "{error:#}"
    );
    assert!(!target.exists());
    assert_no_stage(&root);
    let _ = fs::remove_dir_all(root);
}

#[test]
fn declared_oversize_entry_is_rejected() {
    let root = temp_root("oversize");
    let archive = root.join("big.tar.gz");
    let mut header = tar::Header::new_gnu();
    header.set_path("big.bin").unwrap();
    header.set_entry_type(tar::EntryType::Regular);
    header.set_mode(0o600);
    header.set_size(3 * 1024 * 1024 * 1024);
    header.set_cksum();
    let mut raw = header.as_bytes().to_vec();
    raw.extend_from_slice(&[0u8; 1024]);
    let file = File::create(&archive).unwrap();
    let mut encoder = flate2::write::GzEncoder::new(file, flate2::Compression::default());
    encoder.write_all(&raw).unwrap();
    encoder.finish().unwrap();
    let target = root.join("target-root");
    let error = restore_snapshot(&target, &archive, witness(false)).unwrap_err();
    assert!(
        format!("{error:#}").contains("size") || format!("{error:#}").contains("manifest"),
        "{error:#}"
    );
    assert!(!target.exists());
    assert_no_stage(&root);
    let _ = fs::remove_dir_all(root);
}

fn read_entries(path: &Path) -> Vec<(String, Vec<u8>, bool)> {
    let file = File::open(path).unwrap();
    let decoder = flate2::read::GzDecoder::new(file);
    let mut tar = tar::Archive::new(decoder);
    let mut entries = Vec::new();
    for entry in tar.entries().unwrap() {
        let mut entry = entry.unwrap();
        let name = entry.path().unwrap().to_string_lossy().into_owned();
        let directory = entry.header().entry_type().is_dir();
        let mut bytes = Vec::new();
        entry.read_to_end(&mut bytes).unwrap();
        entries.push((name, bytes, directory));
    }
    entries
}

fn write_entries(path: &Path, entries: &[(String, Vec<u8>, bool)]) {
    let file = File::create(path).unwrap();
    let encoder = flate2::write::GzEncoder::new(file, flate2::Compression::default());
    let mut builder = tar::Builder::new(encoder);
    for (name, bytes, directory) in entries {
        let mut header = tar::Header::new_gnu();
        if *directory {
            header.set_entry_type(tar::EntryType::Directory);
            header.set_size(0);
            header.set_mode(0o700);
        } else {
            header.set_entry_type(tar::EntryType::Regular);
            header.set_size(bytes.len() as u64);
            header.set_mode(0o600);
        }
        header.set_mtime(0);
        header.set_cksum();
        builder
            .append_data(&mut header, name, bytes.as_slice())
            .unwrap();
    }
    builder.into_inner().unwrap().finish().unwrap();
}

fn write_raw_named_archive(path: &Path, name: &str, bytes: &[u8]) {
    assert!(name.len() < 100, "{name}");
    let mut header = tar::Header::new_gnu();
    header.set_entry_type(tar::EntryType::Regular);
    header.set_mode(0o600);
    header.set_size(bytes.len() as u64);
    header.set_mtime(0);
    header.set_path("placeholder").unwrap();
    let raw = header.as_mut_bytes();
    raw[..100].fill(0);
    raw[..name.len()].copy_from_slice(name.as_bytes());
    raw[148..156].fill(b' ');
    let sum: u32 = raw.iter().map(|byte| u32::from(*byte)).sum();
    let field = format!("{sum:06o}\0 ");
    assert_eq!(field.len(), 8);
    raw[148..156].copy_from_slice(field.as_bytes());
    let mut body = raw.to_vec();
    body.extend_from_slice(bytes);
    let pad = (512 - (bytes.len() % 512)) % 512;
    body.extend(vec![0u8; pad]);
    body.extend(vec![0u8; 1024]);
    let file = File::create(path).unwrap();
    let mut encoder = flate2::write::GzEncoder::new(file, flate2::Compression::default());
    encoder.write_all(&body).unwrap();
    encoder.finish().unwrap();
}

fn write_regular_archive(path: &Path, files: &[(&str, &[u8])]) {
    let entries = files
        .iter()
        .map(|(name, bytes)| ((*name).to_string(), bytes.to_vec(), false))
        .collect::<Vec<_>>();
    write_entries(path, &entries);
}

fn write_link_archive(path: &Path, kind: tar::EntryType, name: &str, link_target: &str) {
    let file = File::create(path).unwrap();
    let encoder = flate2::write::GzEncoder::new(file, flate2::Compression::default());
    let mut builder = tar::Builder::new(encoder);
    let mut header = tar::Header::new_gnu();
    header.set_entry_type(kind);
    header.set_size(0);
    header.set_mode(0o777);
    header.set_cksum();
    builder.append_link(&mut header, name, link_target).unwrap();
    builder.into_inner().unwrap().finish().unwrap();
}

fn write_mode_archive(path: &Path, name: &str, bytes: &[u8], mode: u32) {
    let file = File::create(path).unwrap();
    let encoder = flate2::write::GzEncoder::new(file, flate2::Compression::default());
    let mut builder = tar::Builder::new(encoder);
    let mut header = tar::Header::new_gnu();
    header.set_entry_type(tar::EntryType::Regular);
    header.set_size(bytes.len() as u64);
    header.set_mode(mode);
    header.set_cksum();
    builder.append_data(&mut header, name, bytes).unwrap();
    builder.into_inner().unwrap().finish().unwrap();
}

fn rewrite_manifest(path: &Path, mutate: impl FnOnce(&mut Manifest)) {
    let mut entries = read_entries(path);
    let manifest_bytes = entries
        .iter()
        .find(|(name, _, _)| name == MANIFEST_NAME)
        .unwrap()
        .1
        .clone();
    let mut manifest: Manifest = serde_json::from_slice(&manifest_bytes).unwrap();
    mutate(&mut manifest);
    manifest.content_hash = content_hash_of(&manifest).unwrap();
    let encoded = serde_json::to_vec(&manifest).unwrap();
    for entry in &mut entries {
        if entry.0 == MANIFEST_NAME {
            entry.1 = encoded.clone();
        }
    }
    write_entries(path, &entries);
}

fn inject_unknown_manifest_field(path: &Path) {
    let mut entries = read_entries(path);
    for entry in &mut entries {
        if entry.0 == MANIFEST_NAME {
            let mut value: serde_json::Value = serde_json::from_slice(&entry.1).unwrap();
            value
                .as_object_mut()
                .unwrap()
                .insert("unexpected".to_string(), serde_json::json!(1));
            entry.1 = serde_json::to_vec(&value).unwrap();
        }
    }
    write_entries(path, &entries);
}

fn sample_manifest() -> Manifest {
    Manifest {
        format: FORMAT.to_string(),
        version: FORMAT_VERSION,
        app_version: "test".into(),
        sqlite_user_version: None,
        schema_version: None,
        source_data_dir: "C:/synthetic-source".into(),
        cipher: CipherRecord {
            prerequisite: "explicit".into(),
            portable: false,
            witness: "v2:c3ludGhldGlj".into(),
            legacy_ciphertext: "none".into(),
        },
        entries: Vec::new(),
        content_hash: "abc".into(),
    }
}

struct PermissionGuard;

impl PermissionGuard {
    fn fail() -> Self {
        set_fail_owned_file_permission(true);
        Self
    }
}

impl Drop for PermissionGuard {
    fn drop(&mut self) {
        set_fail_owned_file_permission(false);
    }
}

struct PublishGuard;

impl PublishGuard {
    fn before(hook: fn(&Path)) -> Self {
        set_publish_hooks(Some(hook), None);
        Self
    }

    fn after(hook: fn(&Path)) -> Self {
        set_publish_hooks(None, Some(hook));
        Self
    }
}

impl Drop for PublishGuard {
    fn drop(&mut self) {
        set_publish_hooks(None, None);
    }
}

fn plant_sentinel_and_user_file(target: &Path) {
    fs::create_dir_all(target).unwrap();
    fs::write(target.join(".cli-serve.lock"), b"sentinel").unwrap();
    fs::write(target.join("user.txt"), b"user-file").unwrap();
}

#[test]
fn write_archive_keeps_a_preexisting_output_when_create_new_fails() {
    let root = temp_root("output-guard");
    let output = root.join("snapshot.tar.gz");
    let neighbor = root.join("neighbor.txt");
    fs::write(&output, b"original-bytes").unwrap();
    fs::write(&neighbor, b"keep").unwrap();
    let error = write_archive(&output, &[], &sample_manifest()).unwrap_err();
    assert!(format!("{error:#}").contains("create"), "{error:#}");
    assert_eq!(fs::read(&output).unwrap(), b"original-bytes");
    assert_eq!(fs::read(&neighbor).unwrap(), b"keep");
    let _ = fs::remove_dir_all(root);
}

#[test]
fn permission_failure_removes_only_the_file_this_operation_created() {
    let root = temp_root("output-permission");
    let _guard = PermissionGuard::fail();
    let output = root.join("owned.tar.gz");
    let neighbor = root.join("neighbor.txt");
    fs::write(&neighbor, b"keep").unwrap();
    let error = write_archive(&output, &[], &sample_manifest()).unwrap_err();
    assert!(
        format!("{error:#}").contains("private permission setup failed"),
        "{error:#}"
    );
    assert!(!output.exists());
    assert_eq!(fs::read(&neighbor).unwrap(), b"keep");
    let _ = fs::remove_dir_all(root);
}

#[test]
fn empty_target_race_does_not_publish_over_new_files() {
    let root = temp_root("publish-race");
    let source = root.join("source");
    let output = root.join("snapshot.tar.gz");
    fs::create_dir_all(&source).unwrap();
    fs::write(source.join(".encryption-key"), KEY_MATERIAL).unwrap();
    write_sqlite(&source.join("data.sqlite"), 40, 3);
    create_snapshot(&source, &output, CipherWitness::none()).unwrap();

    let occupied = root.join("occupied");
    fs::create_dir(&occupied).unwrap();
    let error = {
        let _guard = PublishGuard::before(plant_sentinel_and_user_file);
        restore_snapshot(&occupied, &output, CipherWitness::none()).unwrap_err()
    };
    assert!(format!("{error:#}").contains("not empty"), "{error:#}");
    assert_eq!(fs::read(occupied.join("user.txt")).unwrap(), b"user-file");
    assert_eq!(
        fs::read(occupied.join(".cli-serve.lock")).unwrap(),
        b"sentinel"
    );
    assert!(!occupied.join("data.sqlite").exists());
    assert_no_stage(&root);

    let missing = root.join("missing");
    let error = {
        let _guard = PublishGuard::before(plant_sentinel_and_user_file);
        restore_snapshot(&missing, &output, CipherWitness::none()).unwrap_err()
    };
    assert!(format!("{error:#}").contains("not empty"), "{error:#}");
    assert_eq!(fs::read(missing.join("user.txt")).unwrap(), b"user-file");
    assert_eq!(
        fs::read(missing.join(".cli-serve.lock")).unwrap(),
        b"sentinel"
    );
    assert!(!missing.join("data.sqlite").exists());

    let after = root.join("after");
    fs::create_dir(&after).unwrap();
    let error = {
        let _guard = PublishGuard::after(plant_sentinel_and_user_file);
        restore_snapshot(&after, &output, CipherWitness::none()).unwrap_err()
    };
    let message = format!("{error:#}");
    assert!(!message.contains(KEY_MATERIAL), "{message}");
    assert_eq!(fs::read(after.join("user.txt")).unwrap(), b"user-file");
    assert_eq!(
        fs::read(after.join(".cli-serve.lock")).unwrap(),
        b"sentinel"
    );
    assert!(!after.join("data.sqlite").exists());
    assert_no_stage(&root);
    let _ = fs::remove_dir_all(root);
}

#[test]
fn owned_config_over_1_mib_is_rejected_before_snapshot_output() {
    let root = temp_root("yaml-bound");
    let source = root.join("source");
    let output = root.join("snapshot.tar.gz");
    fs::create_dir_all(source.join("cpa")).unwrap();
    fs::write(source.join("cpa/managed.json"), b"{}").unwrap();
    let mut yaml = b"auth-dir: \"x\"\n".to_vec();
    yaml.extend(std::iter::repeat_n(b'a', 1024 * 1024));
    fs::write(source.join("cpa/config.yaml"), &yaml).unwrap();
    let error = create_snapshot(&source, &output, witness(true)).unwrap_err();
    let message = format!("{error:#}");
    assert!(message.contains("1 MiB"), "{message}");
    assert!(!message.contains("2 GiB"), "{message}");
    assert!(!output.exists());
    assert_eq!(
        fs::metadata(source.join("cpa/config.yaml")).unwrap().len(),
        yaml.len() as u64
    );
    let _ = fs::remove_dir_all(root);
}

#[test]
fn env_override_beats_a_stale_key_file_and_is_not_portable() {
    let root = temp_root("cipher-env");
    let source = root.join("source");
    let output = root.join("snapshot.tar.gz");
    fs::create_dir_all(&source).unwrap();
    fs::write(source.join("notes.txt"), b"note").unwrap();
    fs::write(source.join(".encryption-key"), b"synthetic-stale-file-key").unwrap();
    let env_secret = "synthetic-env-key";
    let explicit = "synthetic-explicit-over-env";
    let shown = format!(
        "{:?}",
        CipherWitness::from_sources(Some(explicit.into()), Some(env_secret.into()))
    );
    assert!(!shown.contains(explicit), "{shown}");
    assert!(!shown.contains(env_secret), "{shown}");
    assert!(shown.contains("[redacted]"), "{shown}");
    let created = create_snapshot(
        &source,
        &output,
        CipherWitness::from_sources(None, Some(env_secret.into())),
    )
    .unwrap();
    let (manifest, text) = manifest_of(&output);
    assert_eq!(manifest.cipher.prerequisite, "env");
    assert!(!manifest.cipher.portable);
    assert!(manifest.cipher.witness.starts_with("v2:"));
    assert!(!text.contains(env_secret), "{text}");
    assert!(!text.contains("synthetic-stale-file-key"), "{text}");
    assert!(!created.to_json_line().contains(env_secret));
    let stale_target = root.join("stale");
    let error = restore_snapshot(&stale_target, &output, CipherWitness::none()).unwrap_err();
    let message = format!("{error:#}");
    assert!(message.contains("witness"), "{message}");
    assert!(!message.contains(env_secret), "{message}");
    assert!(!stale_target.join("notes.txt").exists());
    let target = root.join("target");
    restore_snapshot(
        &target,
        &output,
        CipherWitness::from_sources(None, Some(env_secret.into())),
    )
    .unwrap();
    assert_eq!(fs::read(target.join("notes.txt")).unwrap(), b"note");
    let output_explicit = root.join("explicit.tar.gz");
    create_snapshot(
        &source,
        &output_explicit,
        CipherWitness::from_sources(Some(explicit.into()), Some(env_secret.into())),
    )
    .unwrap();
    let (manifest, text) = manifest_of(&output_explicit);
    assert_eq!(manifest.cipher.prerequisite, "explicit");
    assert!(!manifest.cipher.portable);
    assert!(!text.contains(explicit), "{text}");
    assert!(!text.contains(env_secret), "{text}");
    let _ = fs::remove_dir_all(root);
}

fn synthetic_account(
    id: &str,
    provider_id: &str,
    key_cipher: String,
    password_cipher: Option<String>,
) -> crate::models::Account {
    let now = chrono::Utc::now();
    crate::models::Account {
        id: id.into(),
        provider_id: provider_id.into(),
        credential_kind: crate::provider::default_credential_kind(),
        quota_scope: crate::provider::default_quota_scope(),
        name: id.into(),
        username: None,
        password_cipher,
        key_cipher,
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
    }
}

fn seed_encrypted_database(
    dir: &Path,
    cipher: &std::sync::Arc<dyn crate::crypto::KeyCipher + Send + Sync>,
) {
    fs::create_dir_all(dir).unwrap();
    let db =
        crate::db::Database::open_with_cipher(dir.to_path_buf(), std::sync::Arc::clone(cipher))
            .unwrap();
    let provider_id = crate::provider::default_provider_id();
    let inference = synthetic_account(
        "inference-synthetic",
        &provider_id,
        cipher.encrypt("synthetic-inference-secret").unwrap(),
        Some(cipher.encrypt("synthetic-password-secret").unwrap()),
    );
    db.create_account(&inference).unwrap();
    let mut cpa = synthetic_account(
        crate::provider::CPA_ACCOUNT_ID,
        crate::provider::CPA_PROVIDER_ID,
        cipher.encrypt("synthetic-cpa-route-secret").unwrap(),
        None,
    );
    cpa.credential_kind = crate::provider::CredentialKind::ApiKey;
    cpa.quota_scope = crate::provider::QuotaScope::Key;
    cpa.name = crate::provider::CPA_ACCOUNT_NAME.into();
    cpa.enabled = false;
    let management = cipher.encrypt("synthetic-cpa-management-secret").unwrap();
    db.upsert_cpa_integration(&cpa, "http://127.0.0.1:9", &management)
        .unwrap();
    let platform = cipher
        .encrypt("synthetic-platform-observer-secret")
        .unwrap();
    db.conn
        .execute(
            "INSERT INTO credentials (
                id, legacy_account_id, destination_id, name, has_secret, enabled,
                routing_rank, scope_json, auth_state, key_cipher, credential_purpose
             ) VALUES (
                'platform-observer-synthetic', 'platform-observer-synthetic', 'dest-synthetic',
                'platform-observer', 1, 0, -1, '{}', 'unknown', ?1, 'platform_observer'
             )",
            [&platform],
        )
        .unwrap();
    db.conn
        .execute_batch("PRAGMA wal_checkpoint(TRUNCATE)")
        .unwrap();
}

fn assert_no_secret_text(text: &str) {
    for secret in [
        "synthetic-explicit-override",
        "synthetic-stale-file-key",
        "synthetic-wrong-override",
        "synthetic-inference-secret",
        "synthetic-password-secret",
        "synthetic-cpa-route-secret",
        "synthetic-cpa-management-secret",
        "synthetic-platform-observer-secret",
        "synthetic-file-key",
        "synthetic-machine-seed",
        "synthetic-other-machine",
        "synthetic-legacy-key",
        "synthetic-legacy-secret",
        "synthetic-wrong-legacy-key",
    ] {
        assert!(!text.contains(secret), "{text}");
    }
}

fn copy_sqlite_dir(from: &Path, to: &Path) {
    fs::create_dir_all(to).unwrap();
    fs::copy(from.join("data.sqlite"), to.join("data.sqlite")).unwrap();
}

#[test]
fn restore_decrypts_real_credentials_and_rejects_the_wrong_key_before_publish() {
    let root = temp_root("cipher-database");
    let source = root.join("source");
    let explicit = "synthetic-explicit-override";
    let cipher: std::sync::Arc<dyn crate::crypto::KeyCipher + Send + Sync> =
        std::sync::Arc::new(crate::crypto::StaticKeyCipher::new(explicit));
    seed_encrypted_database(&source, &cipher);
    fs::write(source.join(".encryption-key"), b"synthetic-stale-file-key").unwrap();
    let sqlite_before = fs::read(source.join("data.sqlite")).unwrap();

    let wrong_output = root.join("wrong.tar.gz");
    let error = create_snapshot(
        &source,
        &wrong_output,
        CipherWitness::explicit("synthetic-wrong-override"),
    )
    .unwrap_err();
    let message = format!("{error:#}");
    assert!(message.contains("authenticated"), "{message}");
    assert!(!message.contains("validated"), "{message}");
    assert_no_secret_text(&message);
    assert!(!wrong_output.exists());
    assert_eq!(fs::read(source.join("data.sqlite")).unwrap(), sqlite_before);
    assert_eq!(
        fs::read(source.join(".encryption-key")).unwrap(),
        b"synthetic-stale-file-key"
    );

    let platform_only = root.join("platform-only");
    copy_sqlite_dir(&source, &platform_only);
    let conn = Connection::open(platform_only.join("data.sqlite")).unwrap();
    conn.execute(
        "UPDATE credentials SET key_cipher = '', password_cipher = NULL
         WHERE COALESCE(credential_purpose, 'inference') <> 'platform_observer'",
        [],
    )
    .unwrap();
    drop(conn);
    let platform_output = root.join("platform.tar.gz");
    let error = create_snapshot(
        &platform_only,
        &platform_output,
        CipherWitness::explicit("synthetic-wrong-override"),
    )
    .unwrap_err();
    assert!(format!("{error:#}").contains("authenticated"), "{error:#}");
    assert!(!platform_output.exists());
    create_snapshot(
        &platform_only,
        &platform_output,
        CipherWitness::explicit(explicit),
    )
    .unwrap();
    let platform_target = root.join("platform-target");
    restore_snapshot(
        &platform_target,
        &platform_output,
        CipherWitness::explicit(explicit),
    )
    .unwrap();
    let platform_db = crate::db::Database::open_with_cipher(
        platform_target.clone(),
        std::sync::Arc::clone(&cipher),
    )
    .unwrap();
    let platform_cipher: String = platform_db
        .conn
        .query_row(
            "SELECT key_cipher FROM credentials WHERE credential_purpose = 'platform_observer'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        cipher.decrypt(&platform_cipher).unwrap(),
        "synthetic-platform-observer-secret"
    );
    drop(platform_db);

    let output = root.join("snapshot.tar.gz");
    let created = create_snapshot(&source, &output, CipherWitness::explicit(explicit)).unwrap();
    assert!(!created.to_json_line().contains("validated"));
    assert_no_secret_text(&created.to_json_line());
    let (manifest, text) = manifest_of(&output);
    assert_eq!(manifest.cipher.prerequisite, "explicit");
    assert!(!manifest.cipher.portable);
    assert!(manifest.cipher.witness.starts_with("v2:"));
    assert_eq!(manifest.cipher.legacy_ciphertext, "none");
    assert_no_secret_text(&text);
    assert_eq!(fs::read(source.join("data.sqlite")).unwrap(), sqlite_before);

    let wrong_target = root.join("wrong-target");
    fs::create_dir(&wrong_target).unwrap();
    let error = restore_snapshot(
        &wrong_target,
        &output,
        CipherWitness::explicit("synthetic-wrong-override"),
    )
    .unwrap_err();
    let message = format!("{error:#}");
    assert!(message.contains("witness"), "{message}");
    assert!(!message.contains("validated"), "{message}");
    assert_no_secret_text(&message);
    assert!(!wrong_target.join("data.sqlite").exists());
    assert!(fs::read_dir(&wrong_target).unwrap().next().is_none());

    let stale_target = root.join("stale-target");
    let error = restore_snapshot(&stale_target, &output, CipherWitness::none()).unwrap_err();
    assert!(format!("{error:#}").contains("witness"), "{error:#}");
    assert!(!stale_target.exists());

    let target = root.join("target");
    restore_snapshot(&target, &output, CipherWitness::explicit(explicit)).unwrap();
    let db = crate::db::Database::open_with_cipher(target.clone(), std::sync::Arc::clone(&cipher))
        .unwrap();
    let account = db.get_account("inference-synthetic").unwrap().unwrap();
    assert_eq!(
        cipher.decrypt(&account.key_cipher).unwrap(),
        "synthetic-inference-secret"
    );
    assert_eq!(
        cipher
            .decrypt(account.password_cipher.as_deref().unwrap())
            .unwrap(),
        "synthetic-password-secret"
    );
    let cpa = db.cpa_integration().unwrap().unwrap();
    assert_eq!(
        cipher.decrypt(&cpa.management_key_cipher).unwrap(),
        "synthetic-cpa-management-secret"
    );
    let platform_cipher: String = db
        .conn
        .query_row(
            "SELECT key_cipher FROM credentials WHERE credential_purpose = 'platform_observer'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        cipher.decrypt(&platform_cipher).unwrap(),
        "synthetic-platform-observer-secret"
    );
    drop(db);
    assert_no_stage(&root);
    let _ = fs::remove_dir_all(root);
}

#[test]
fn file_key_restore_rejects_a_wrong_override_and_decrypts_with_the_file() {
    let root = temp_root("cipher-file-db");
    let source = root.join("source");
    let secret = "synthetic-file-key";
    let cipher: std::sync::Arc<dyn crate::crypto::KeyCipher + Send + Sync> =
        std::sync::Arc::new(crate::crypto::StaticKeyCipher::new(secret));
    seed_encrypted_database(&source, &cipher);
    fs::write(source.join(".encryption-key"), secret.as_bytes()).unwrap();
    let output = root.join("snapshot.tar.gz");
    create_snapshot(&source, &output, CipherWitness::none()).unwrap();
    let (manifest, text) = manifest_of(&output);
    assert_eq!(manifest.cipher.prerequisite, "file");
    assert!(manifest.cipher.portable);
    assert_no_secret_text(&text);
    let wrong = root.join("wrong");
    fs::create_dir(&wrong).unwrap();
    let error = restore_snapshot(
        &wrong,
        &output,
        CipherWitness::explicit("synthetic-wrong-override"),
    )
    .unwrap_err();
    let message = format!("{error:#}");
    assert!(message.contains("witness"), "{message}");
    assert_no_secret_text(&message);
    assert!(fs::read_dir(&wrong).unwrap().next().is_none());
    let target = root.join("target");
    restore_snapshot(&target, &output, CipherWitness::none()).unwrap();
    let db = crate::db::Database::open_with_cipher(target, cipher.clone()).unwrap();
    let account = db.get_account("inference-synthetic").unwrap().unwrap();
    assert_eq!(
        cipher.decrypt(&account.key_cipher).unwrap(),
        "synthetic-inference-secret"
    );
    let cpa = db.cpa_integration().unwrap().unwrap();
    assert_eq!(
        cipher.decrypt(&cpa.management_key_cipher).unwrap(),
        "synthetic-cpa-management-secret"
    );
    drop(db);
    let _ = fs::remove_dir_all(root);
}

#[test]
fn injected_machine_database_decrypts_only_for_that_cipher() {
    let root = temp_root("cipher-machine-db");
    let source = root.join("source");
    let cipher: std::sync::Arc<dyn crate::crypto::KeyCipher + Send + Sync> = std::sync::Arc::new(
        crate::crypto::StaticKeyCipher::new("synthetic-machine-seed"),
    );
    let other: std::sync::Arc<dyn crate::crypto::KeyCipher + Send + Sync> = std::sync::Arc::new(
        crate::crypto::StaticKeyCipher::new("synthetic-other-machine"),
    );
    seed_encrypted_database(&source, &cipher);
    assert!(!source.join(".encryption-key").exists());
    let output = root.join("snapshot.tar.gz");
    create_snapshot(
        &source,
        &output,
        CipherWitness::injected_machine(std::sync::Arc::clone(&cipher)),
    )
    .unwrap();
    let (manifest, text) = manifest_of(&output);
    assert_eq!(manifest.cipher.prerequisite, "machine");
    assert!(!manifest.cipher.portable);
    assert_no_secret_text(&text);
    let wrong = root.join("wrong");
    let error = restore_snapshot(
        &wrong,
        &output,
        CipherWitness::injected_machine(std::sync::Arc::clone(&other)),
    )
    .unwrap_err();
    assert!(format!("{error:#}").contains("witness"), "{error:#}");
    assert_no_secret_text(&format!("{error:#}"));
    assert!(!wrong.exists());
    let target = root.join("target");
    restore_snapshot(
        &target,
        &output,
        CipherWitness::injected_machine(cipher.clone()),
    )
    .unwrap();
    let db = crate::db::Database::open_with_cipher(target, cipher.clone()).unwrap();
    let account = db.get_account("inference-synthetic").unwrap().unwrap();
    assert_eq!(
        cipher.decrypt(&account.key_cipher).unwrap(),
        "synthetic-inference-secret"
    );
    drop(db);
    let _ = fs::remove_dir_all(root);
}

#[test]
fn legacy_xor_restores_without_claiming_a_validated_cipher_state() {
    let root = temp_root("cipher-legacy");
    let source = root.join("source");
    fs::create_dir_all(&source).unwrap();
    let cipher = crate::crypto::StaticKeyCipher::new("synthetic-legacy-key");
    let legacy = cipher.encrypt_legacy("synthetic-legacy-secret").unwrap();
    let aead = cipher.encrypt("synthetic-inference-secret").unwrap();
    let conn = Connection::open(source.join("data.sqlite")).unwrap();
    conn.execute_batch(
        "CREATE TABLE schema_version (version INTEGER PRIMARY KEY);
         INSERT INTO schema_version (version) VALUES (40);
         PRAGMA user_version = 7;
         CREATE TABLE accounts (id TEXT, key_cipher TEXT, password_cipher TEXT);
         CREATE TABLE credentials (id TEXT, key_cipher TEXT, password_cipher TEXT, credential_purpose TEXT);
         CREATE TABLE cpa_integration (id TEXT, management_key_cipher TEXT);",
    )
    .unwrap();
    conn.execute(
        "INSERT INTO accounts (id, key_cipher, password_cipher) VALUES ('legacy', ?1, ?1)",
        [&legacy],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO credentials (id, key_cipher, password_cipher, credential_purpose) VALUES ('inference', ?1, NULL, 'inference')",
        [&aead],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO credentials (id, key_cipher, password_cipher, credential_purpose) VALUES ('platform', ?1, NULL, 'platform_observer')",
        [&aead],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO cpa_integration (id, management_key_cipher) VALUES ('cpa', ?1)",
        [&legacy],
    )
    .unwrap();
    drop(conn);

    let xor_only = root.join("xor-only.sqlite");
    let xor_conn = Connection::open(&xor_only).unwrap();
    xor_conn
        .execute_batch("CREATE TABLE accounts (id TEXT, key_cipher TEXT)")
        .unwrap();
    xor_conn
        .execute(
            "INSERT INTO accounts (id, key_cipher) VALUES ('a', ?1)",
            [&legacy],
        )
        .unwrap();
    drop(xor_conn);
    let xor_conn =
        Connection::open_with_flags(&xor_only, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
    let wrong = crate::crypto::StaticKeyCipher::new("synthetic-wrong-legacy-key");
    match crate::db::probe_snapshot_ciphertext(&xor_conn, &wrong) {
        Err(error) => {
            let message = format!("{error:#}");
            assert!(message.contains("not proof"), "{message}");
            assert!(
                message.contains("not a validated cipher state"),
                "{message}"
            );
            assert_no_secret_text(&message);
            assert!(!message.contains(&legacy), "{message}");
        }
        Ok(report) => {
            assert!(report.legacy_unauthenticated);
        }
    }
    let report = crate::db::probe_snapshot_ciphertext(&xor_conn, &cipher).unwrap();
    assert!(report.legacy_unauthenticated);
    drop(xor_conn);

    let output = root.join("snapshot.tar.gz");
    let created = create_snapshot(
        &source,
        &output,
        CipherWitness::explicit("synthetic-legacy-key"),
    )
    .unwrap();
    assert_eq!(created.state, "created");
    assert_no_secret_text(&created.to_json_line());
    let (manifest, text) = manifest_of(&output);
    assert_eq!(manifest.cipher.legacy_ciphertext, "present-not-proof");
    assert!(!text.contains("validated"), "{text}");
    assert_no_secret_text(&text);
    let wrong_target = root.join("wrong");
    let error = restore_snapshot(
        &wrong_target,
        &output,
        CipherWitness::explicit("synthetic-wrong-legacy-key"),
    )
    .unwrap_err();
    let message = format!("{error:#}");
    assert!(message.contains("witness"), "{message}");
    assert!(!message.contains("validated"), "{message}");
    assert_no_secret_text(&message);
    assert!(!wrong_target.exists());
    let target = root.join("target");
    let restored = restore_snapshot(
        &target,
        &output,
        CipherWitness::explicit("synthetic-legacy-key"),
    )
    .unwrap();
    assert_eq!(restored.state, "restored");
    let conn = Connection::open_with_flags(
        target.join("data.sqlite"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .unwrap();
    let stored_legacy: String = conn
        .query_row("SELECT key_cipher FROM accounts", [], |row| row.get(0))
        .unwrap();
    let stored_aead: String = conn
        .query_row(
            "SELECT key_cipher FROM credentials WHERE credential_purpose = 'platform_observer'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    let stored_cpa: String = conn
        .query_row(
            "SELECT management_key_cipher FROM cpa_integration",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        cipher.decrypt(&stored_legacy).unwrap(),
        "synthetic-legacy-secret"
    );
    assert_eq!(
        cipher.decrypt(&stored_aead).unwrap(),
        "synthetic-inference-secret"
    );
    assert_eq!(
        cipher.decrypt(&stored_cpa).unwrap(),
        "synthetic-legacy-secret"
    );
    assert!(crate::crypto::is_legacy_local_ciphertext(&stored_legacy));
    assert!(!crate::crypto::is_legacy_local_ciphertext(&stored_aead));
    let _ = fs::remove_dir_all(root);
}

#[cfg(windows)]
#[test]
fn host_machine_cipher_roundtrips_without_a_key_file() {
    let root = temp_root("cipher-host-machine");
    let source = root.join("source");
    let output = root.join("snapshot.tar.gz");
    fs::create_dir_all(source.join("notes")).unwrap();
    fs::write(source.join("notes/plain.txt"), b"note").unwrap();
    create_snapshot(&source, &output, CipherWitness::none()).unwrap();
    assert!(!source.join(".encryption-key").exists());
    let (manifest, text) = manifest_of(&output);
    assert_eq!(manifest.cipher.prerequisite, "machine");
    assert!(!manifest.cipher.portable);
    assert!(manifest.cipher.witness.starts_with("v2:"));
    assert!(!text.contains("validated"), "{text}");
    let shadowed = root.join("shadowed");
    let error = restore_snapshot(
        &shadowed,
        &output,
        CipherWitness::explicit("synthetic-wrong-override"),
    )
    .unwrap_err();
    let message = format!("{error:#}");
    assert!(message.contains("witness"), "{message}");
    assert_no_secret_text(&message);
    assert!(!shadowed.exists());
    let target = root.join("target");
    restore_snapshot(&target, &output, CipherWitness::none()).unwrap();
    assert_eq!(fs::read(target.join("notes/plain.txt")).unwrap(), b"note");
    assert!(!target.join(".encryption-key").exists());
    let _ = fs::remove_dir_all(root);
}

fn canonical_cpa_yaml(auth_dir: &str, generation: u64, revision: u64) -> String {
    format!(
        "\
host: \"127.0.0.1\"
port: 8085
auth-dir: \"{auth_dir}\"
api-keys:
  - \"{CLIENT_KEY}\"
ocg:
  process-generation: \"{generation}\"
  projection-revision: \"{revision}\"
  ready-key: \"ready-secret\"
  policy:
    url: \"http://127.0.0.1:9/_internal/ocg/cpa-policy\"
    token: \"policy-secret\"
    origin: \"http://127.0.0.1:9\"
provider-secret: \"keep-provider\"
"
    )
}

fn insert_execution_record(database: &Path, value: &str) {
    let connection = Connection::open(database).unwrap();
    connection
        .execute_batch(
            "CREATE TABLE IF NOT EXISTS settings (key TEXT PRIMARY KEY, value TEXT NOT NULL);",
        )
        .unwrap();
    connection
        .execute(
            "INSERT OR REPLACE INTO settings (key, value) VALUES (?1, ?2)",
            rusqlite::params![crate::cpa_execution::EXECUTION_RECORD_KEY, value],
        )
        .unwrap();
}

fn execution_value(database: &Path) -> serde_json::Value {
    let connection =
        Connection::open_with_flags(database, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
    let json: String = connection
        .query_row(
            "SELECT value FROM settings WHERE key = ?1",
            [crate::cpa_execution::EXECUTION_RECORD_KEY],
            |row| row.get(0),
        )
        .unwrap();
    serde_json::from_str(&json).unwrap()
}

fn staged_record(applied_digest: &str, previous: Option<serde_json::Value>) -> String {
    let mut value = serde_json::json!({
        "version": 1,
        "childGeneration": "11",
        "appliedGeneration": "11",
        "desiredRevision": "9",
        "appliedRevision": "3",
        "desiredDigest": "c".repeat(64),
        "appliedDigest": applied_digest,
        "artifactSha256": "b".repeat(64),
        "listenPort": 8085,
        "applyStatus": "applied",
        "desiredRunning": true,
        "publicOrigin": "http://127.0.0.1:9",
        "ownedOrigin": "http://127.0.0.1:8085",
        "policyReady": true,
        "unavailable": false,
        "desiredAuth": [],
        "appliedAuth": [],
        "desiredRoutes": [],
        "appliedRoutes": [{
            "authId": "auth-a",
            "credentialId": "cred-a",
            "credentialVersion": "1",
            "bindingId": "bind-a",
            "materialFingerprint": "material-a",
            "routingRank": 1,
            "fingerprint": "route-keep",
            "routes": [{
                "publicModel": "public",
                "upstreamModel": "upstream",
                "protocol": "chat_completions",
                "endpointId": "endpoint-a",
                "origin": "https://lab.example",
                "endpointFingerprint": "a".repeat(64),
                "validationOnly": false
            }]
        }],
        "oauth": [],
        "observer": "keep"
    });
    if let Some(previous) = previous {
        value["previousAccepted"] = previous;
    }
    value.to_string()
}

#[test]
fn staged_backup_rebinds_known_execution_digests() {
    let root = temp_root("staged-meta");
    let source = root.join("source-root");
    let target = root.join("target-root");
    let output = root.join("snapshot.tar.gz");
    write_product(&source, 64, 11);
    let auth_dir = auth_dir_for(&source);
    let current = canonical_cpa_yaml(&auth_dir, 11, 3);
    let previous = canonical_cpa_yaml(&auth_dir, 4, 2);
    fs::write(source.join("cpa/config.yaml"), &current).unwrap();
    fs::write(source.join("cpa/config.yaml.previous"), &previous).unwrap();
    let current_digest = crate::cpa_projection::wire_digest(&current);
    let previous_digest = crate::cpa_projection::wire_digest(&previous);
    let snapshot = serde_json::json!({
        "generation": "4",
        "revision": "2",
        "wireDigest": previous_digest,
        "auth": [],
        "routes": [{
            "authId": "auth-old",
            "credentialId": "cred-old",
            "credentialVersion": "1",
            "bindingId": "bind-old",
            "materialFingerprint": "material-old",
            "routingRank": 1,
            "fingerprint": "previous-keep",
            "routes": []
        }],
        "artifactSha256": "b".repeat(64),
        "listenPort": 8085,
        "ownedOrigin": "http://127.0.0.1:8085",
        "publicOrigin": "http://127.0.0.1:9"
    });
    insert_execution_record(
        &source.join("data.sqlite"),
        &staged_record(&current_digest, Some(snapshot)),
    );
    fs::create_dir(&target).unwrap();
    create_snapshot(&source, &output, witness(false)).unwrap();
    restore_snapshot(&target, &output, witness(false)).unwrap();
    let restored_current = fs::read_to_string(target.join("cpa/config.yaml")).unwrap();
    let restored_previous = fs::read_to_string(target.join("cpa/config.yaml.previous")).unwrap();
    assert_eq!(
        parsed_auth_dir(&target.join("cpa/config.yaml")),
        auth_dir_for(&target)
    );
    assert!(restored_current.contains("keep-provider"));
    let record = execution_value(&target.join("data.sqlite"));
    assert_eq!(
        record["appliedDigest"],
        crate::cpa_projection::wire_digest(&restored_current)
    );
    assert_eq!(record["desiredDigest"], "c".repeat(64));
    assert_eq!(record["observer"], "keep");
    assert_eq!(record["appliedRoutes"][0]["fingerprint"], "route-keep");
    assert_eq!(
        record["previousAccepted"]["wireDigest"],
        crate::cpa_projection::wire_digest(&restored_previous)
    );
    assert_eq!(
        record["previousAccepted"]["routes"][0]["fingerprint"],
        "previous-keep"
    );
    assert_ne!(record["appliedDigest"], current_digest);

    let plain = root.join("plain-root");
    let plain_target = root.join("plain-target");
    let plain_output = root.join("plain.tar.gz");
    write_product(&plain, 64, 11);
    let plain_auth = auth_dir_for(&plain);
    let plain_yaml = canonical_cpa_yaml(&plain_auth, 11, 3);
    fs::write(plain.join("cpa/config.yaml"), &plain_yaml).unwrap();
    fs::write(plain.join("cpa/config.yaml.previous"), &plain_yaml).unwrap();
    insert_execution_record(
        &plain.join("data.sqlite"),
        &staged_record(&crate::cpa_projection::wire_digest(&plain_yaml), None),
    );
    fs::create_dir(&plain_target).unwrap();
    create_snapshot(&plain, &plain_output, witness(false)).unwrap();
    restore_snapshot(&plain_target, &plain_output, witness(false)).unwrap();
    let plain_record = execution_value(&plain_target.join("data.sqlite"));
    assert!(plain_record.get("previousAccepted").is_none());
    assert_eq!(
        plain_record["appliedDigest"],
        crate::cpa_projection::wire_digest(
            &fs::read_to_string(plain_target.join("cpa/config.yaml")).unwrap()
        )
    );
    let _ = fs::remove_dir_all(root);
}

#[test]
fn staged_backup_corrupt_execution_record_is_not_published() {
    let root = temp_root("staged-corrupt");
    let source = root.join("source-root");
    let target = root.join("target-root");
    let output = root.join("snapshot.tar.gz");
    write_product(&source, 64, 11);
    insert_execution_record(&source.join("data.sqlite"), "{");
    fs::create_dir(&target).unwrap();
    create_snapshot(&source, &output, witness(false)).unwrap();
    let error = restore_snapshot(&target, &output, witness(false)).unwrap_err();
    let message = format!("{error:#}");
    assert!(message.contains("could not be read"), "{message}");
    assert!(!target.join("data.sqlite").exists());
    assert_no_stage(&root);
    let _ = fs::remove_dir_all(root);
}
