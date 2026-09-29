use super::*;
use std::fs;
use std::process::{Command, Stdio};

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

#[test]
fn abandoned_lock_without_sidecar_is_reclaimed_once_stale() {
    let root = std::env::temp_dir().join(format!(
        "ocg-byok-abandoned-lock-{}",
        uuid::Uuid::new_v4().simple()
    ));
    fs::create_dir_all(&root).unwrap();
    let target = root.join("config.yaml");
    fs::write(&target, "logLevel: info\n").unwrap();
    let lock_dir = lock_dir_for(&target).unwrap();
    fs::create_dir(&lock_dir).unwrap();
    let sidecar = ocg_sidecar_for(&lock_dir);

    let fresh = LockPolicy {
        minimax_max_wait: Duration::from_secs(3_600),
        ..LockPolicy::default()
    };
    assert!(!reclaim_abandoned_minimax(&lock_dir, &sidecar, &fresh).unwrap());
    assert!(lock_dir.is_dir(), "a recent anonymous lock is left alone");

    let stale_after = fresh.minimax_max_wait * 2 + Duration::from_secs(5);
    backdate_directory_mtime(&lock_dir, SystemTime::now() - stale_after).unwrap();
    assert!(reclaim_abandoned_minimax(&lock_dir, &sidecar, &fresh).unwrap());
    assert!(!lock_dir.exists(), "a stale anonymous lock is reclaimed");
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn retire_orphan_sidecar_with_same_directory_identity() {
    let root = std::env::temp_dir().join(format!(
        "ocg-byok-same-id-orphan-{}",
        uuid::Uuid::new_v4().simple()
    ));
    fs::create_dir_all(&root).unwrap();
    let lock_dir = root.join("config.yaml.lock");
    fs::create_dir(&lock_dir).unwrap();
    let handle = open_directory_for_times(&lock_dir).unwrap();
    let identity = directory_identity(&handle).unwrap();
    let sidecar = ocg_sidecar_for(&lock_dir);
    fs::write(&sidecar, sidecar_json(dead_pid(), "dead", identity)).unwrap();
    retire_orphan_former_sidecar(&lock_dir, &sidecar, &handle, identity).unwrap();
    assert!(!sidecar.exists());
    assert!(lock_dir.is_dir());
    assert_eq!(path_dir_id(&lock_dir).unwrap(), Some(identity));
    fs::remove_dir_all(root).unwrap();
}
