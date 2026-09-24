use super::*;

#[test]
fn lifetime_guard_keeps_directory_warm_until_last_handle_drops() {
    let dir = std::env::temp_dir().join(format!("ocg-open-guard-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let mut first = DatabaseOpenGuard::acquire(&dir).unwrap();
    assert!(first.can_recover_pending());
    first.finish_open().unwrap();
    let mut second = DatabaseOpenGuard::acquire(&dir).unwrap();
    assert!(!second.can_recover_pending());
    second.finish_open().unwrap();
    drop(first);
    let third = DatabaseOpenGuard::acquire(&dir).unwrap();
    assert!(!third.can_recover_pending());
    drop(second);
    drop(third);
    let cold = DatabaseOpenGuard::acquire(&dir).unwrap();
    assert!(cold.can_recover_pending());
    drop(cold);
    assert!(dir.join(".database-open.lock").is_file());
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn lifetime_guard_waits_for_cold_initialization_then_opens_shared() {
    let dir = std::env::temp_dir().join(format!("ocg-open-guard-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let mut first = DatabaseOpenGuard::acquire(&dir).unwrap();
    let (started_tx, started_rx) = std::sync::mpsc::channel();
    let (opened_tx, opened_rx) = std::sync::mpsc::channel();
    let worker_dir = dir.clone();
    let worker = std::thread::spawn(move || {
        started_tx.send(()).unwrap();
        let mut second = DatabaseOpenGuard::acquire(&worker_dir).unwrap();
        let can_recover = second.can_recover_pending();
        second.finish_open().unwrap();
        opened_tx.send((second, can_recover)).unwrap();
    });
    started_rx.recv().unwrap();
    assert!(matches!(
        opened_rx.recv_timeout(std::time::Duration::from_millis(100)),
        Err(std::sync::mpsc::RecvTimeoutError::Timeout)
    ));
    first.finish_open().unwrap();
    let (second, can_recover) = opened_rx
        .recv_timeout(std::time::Duration::from_secs(5))
        .unwrap();
    assert!(!can_recover);
    worker.join().unwrap();
    drop(first);
    drop(second);
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn lifetime_guard_waiter_recovers_after_cold_initializer_fails() {
    let dir = std::env::temp_dir().join(format!("ocg-open-guard-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let first = DatabaseOpenGuard::acquire(&dir).unwrap();
    assert!(first.can_recover_pending());
    let (started_tx, started_rx) = std::sync::mpsc::channel();
    let (opened_tx, opened_rx) = std::sync::mpsc::channel();
    let worker_dir = dir.clone();
    let worker = std::thread::spawn(move || {
        started_tx.send(()).unwrap();
        let second = DatabaseOpenGuard::acquire(&worker_dir).unwrap();
        opened_tx.send(second).unwrap();
    });
    started_rx.recv().unwrap();
    assert!(matches!(
        opened_rx.recv_timeout(std::time::Duration::from_millis(100)),
        Err(std::sync::mpsc::RecvTimeoutError::Timeout)
    ));
    // Simulate any failed initialization before recovery/finish_open.
    drop(first);
    let second = opened_rx
        .recv_timeout(std::time::Duration::from_secs(5))
        .unwrap();
    assert!(second.can_recover_pending());
    worker.join().unwrap();
    drop(second);
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn lifetime_guard_fails_closed_when_lock_file_cannot_open() {
    let dir = std::env::temp_dir().join(format!("ocg-open-guard-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(dir.join(".database-open.lock")).unwrap();
    assert!(DatabaseOpenGuard::acquire(&dir).is_err());
    // An error after taking the gate must release it, too.
    let gate = OpenOptions::new()
        .read(true)
        .write(true)
        .open(dir.join(".database-open-gate.lock"))
        .unwrap();
    FileExt::try_lock_exclusive(&gate).unwrap();
    drop(gate);
    std::fs::remove_dir_all(dir).unwrap();
}
