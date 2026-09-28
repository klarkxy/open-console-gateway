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
