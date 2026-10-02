use super::{Owner, inspect, path_for, remove, remove_if_ours, update_endpoint_if_ours, write};
use std::process::{Command, Stdio};

fn temp_dir() -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("ocg-cli-listener-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn live_marker_uses_this_process_and_remove_clears_it() {
    let dir = temp_dir();
    write(&dir, "http://127.0.0.1:9042").unwrap();
    assert_eq!(
        inspect(&dir),
        Owner::Live {
            endpoint: "http://127.0.0.1:9042".into()
        }
    );
    remove(&dir);
    assert_eq!(inspect(&dir), Owner::Absent);
    assert!(!path_for(&dir).exists());
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn dead_pid_is_absent_and_garbage_is_unreadable() {
    let dir = temp_dir();
    let mut child = if cfg!(windows) {
        Command::new("cmd")
    } else {
        Command::new("true")
    };
    if cfg!(windows) {
        child.args(["/C", "exit", "0"]);
    }
    let mut child = child
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let pid = child.id();
    child.wait().unwrap();
    std::fs::write(
        path_for(&dir),
        format!(r#"{{"pid":{pid},"endpoint":"http://127.0.0.1:9"}}"#),
    )
    .unwrap();
    assert_eq!(inspect(&dir), Owner::Absent);

    std::fs::write(path_for(&dir), b"{").unwrap();
    assert!(matches!(inspect(&dir), Owner::Unreadable { .. }));
    std::fs::write(
        path_for(&dir),
        br#"{"pid":0,"endpoint":"http://127.0.0.1:9"}"#,
    )
    .unwrap();
    assert!(matches!(inspect(&dir), Owner::Unreadable { .. }));
    std::fs::write(path_for(&dir), br#"{"pid":4,"endpoint":""}"#).unwrap();
    assert!(matches!(inspect(&dir), Owner::Unreadable { .. }));
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn cleanup_and_endpoint_refresh_touch_only_this_process() {
    let dir = temp_dir();
    let foreign = if cfg!(windows) { 4 } else { 1 };
    std::fs::write(
        path_for(&dir),
        format!(r#"{{"pid":{foreign},"endpoint":"http://127.0.0.1:9"}}"#),
    )
    .unwrap();
    assert!(!update_endpoint_if_ours(&dir, "http://127.0.0.1:10").unwrap());
    remove_if_ours(&dir);
    assert_eq!(
        std::fs::read_to_string(path_for(&dir)).unwrap(),
        format!(r#"{{"pid":{foreign},"endpoint":"http://127.0.0.1:9"}}"#)
    );

    write(&dir, "http://127.0.0.1:11").unwrap();
    assert!(update_endpoint_if_ours(&dir, "http://127.0.0.1:12").unwrap());
    assert_eq!(
        inspect(&dir),
        Owner::Live {
            endpoint: "http://127.0.0.1:12".into()
        }
    );
    remove_if_ours(&dir);
    assert_eq!(inspect(&dir), Owner::Absent);
    let _ = std::fs::remove_dir_all(dir);
}
