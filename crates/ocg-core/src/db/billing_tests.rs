//! Behavioral tests split from the root db test module.

use super::tests::*;
use super::*;
use crate::crypto::KeyCipher;
use std::fs;
use std::sync::Arc;

#[test]
pub(super) fn billing_open_live_receipt_survives_concurrent_open_and_settles_once() {
    let dir = temp_data_dir("billing-live-open");
    let (db, log_id, attempt) = billing_open_fixture(&dir);
    let second = open_with_host_cipher(dir.clone()).unwrap();
    assert_billing_open_state(&second, 75.0, 1, 0);
    finish_billing_open_attempt(&db, log_id, &attempt);
    assert_billing_open_state(&second, 65.0, 0, 0);
    drop(db);
    drop(second);
    let reopened = open_with_host_cipher(dir.clone()).unwrap();
    assert_billing_open_state(&reopened, 65.0, 0, 0);
    drop(reopened);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
pub(super) fn billing_open_recovers_uncertainty_only_after_last_handle_closes() {
    let dir = temp_data_dir("billing-cold-open");
    let (db, _, _) = billing_open_fixture(&dir);
    let second = open_with_host_cipher(dir.clone()).unwrap();
    drop(db);
    let third = open_with_host_cipher(dir.clone()).unwrap();
    assert_billing_open_state(&third, 75.0, 1, 0);
    drop(second);
    drop(third);
    for _ in 0..2 {
        let reopened = open_with_host_cipher(dir.clone()).unwrap();
        assert_billing_open_state(&reopened, 75.0, 0, 1);
    }
    fs::remove_dir_all(dir).unwrap();
}

#[test]
pub(super) fn billing_open_failed_recovery_rolls_back_and_releases_lifetime_lock() {
    let dir = temp_data_dir("billing-failed-open");
    let (db, _, _) = billing_open_fixture(&dir);
    db.conn.execute_batch("CREATE TRIGGER block_receipt BEFORE UPDATE OF credit_receipt_json ON forward_logs BEGIN SELECT RAISE(ABORT,'fixture recovery failure'); END;").unwrap();
    drop(db);
    assert!(open_with_host_cipher(dir.clone()).is_err());
    let guard = open_guard::DatabaseOpenGuard::acquire(&dir).unwrap();
    assert!(
        guard.can_recover_pending(),
        "failed open must release its lock"
    );
    let conn = Connection::open(dir.join("data.sqlite")).unwrap();
    let state = billing::load_on(&conn, "billing-open").unwrap().unwrap();
    assert_eq!(state.unpriced_requests, 0, "failed recovery must roll back");
    assert_eq!(
        billing::pending_count_on(&conn, &state.credential_id, &state.meter_id).unwrap(),
        1
    );
    conn.execute_batch("DROP TRIGGER block_receipt;").unwrap();
    drop(conn);
    drop(guard);
    let recovered = open_with_host_cipher(dir.clone()).unwrap();
    assert_billing_open_state(&recovered, 75.0, 0, 1);
    drop(recovered);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
pub(super) fn billing_open_waiter_recovers_when_cold_initializer_fails() {
    use std::sync::{Mutex, mpsc};
    use std::time::Duration as StdDuration;

    struct BlockedFailingCipher {
        entered: mpsc::Sender<()>,
        fail: Mutex<mpsc::Receiver<()>>,
    }

    impl KeyCipher for BlockedFailingCipher {
        fn encrypt(&self, _plaintext: &str) -> Result<String> {
            anyhow::bail!("fixture does not encrypt")
        }

        fn decrypt(&self, _ciphertext: &str) -> Result<String> {
            self.entered.send(()).unwrap();
            self.fail
                .lock()
                .unwrap()
                .recv_timeout(StdDuration::from_secs(5))
                .unwrap();
            anyhow::bail!("fixture cold initializer failure")
        }
    }

    let dir = temp_data_dir("billing-failed-initializer-waiter");
    let (db, _, _) = billing_open_fixture(&dir);
    drop(db);
    let (entered_tx, entered_rx) = mpsc::channel();
    let (fail_tx, fail_rx) = mpsc::channel();
    let cipher = Arc::new(BlockedFailingCipher {
        entered: entered_tx,
        fail: Mutex::new(fail_rx),
    });
    let first_dir = dir.clone();
    let first = std::thread::spawn(move || Database::open_with_cipher(first_dir, cipher).is_err());
    entered_rx.recv_timeout(StdDuration::from_secs(5)).unwrap();
    let (started_tx, started_rx) = mpsc::channel();
    let (opened_tx, opened_rx) = mpsc::channel();
    let second_dir = dir.clone();
    let second = std::thread::spawn(move || {
        started_tx.send(()).unwrap();
        opened_tx.send(open_with_host_cipher(second_dir)).unwrap();
    });
    started_rx.recv().unwrap();
    assert!(matches!(
        opened_rx.recv_timeout(StdDuration::from_millis(100)),
        Err(mpsc::RecvTimeoutError::Timeout)
    ));
    fail_tx.send(()).unwrap();
    assert!(first.join().unwrap());
    let recovered = opened_rx
        .recv_timeout(StdDuration::from_secs(5))
        .unwrap()
        .unwrap();
    second.join().unwrap();
    assert_billing_open_state(&recovered, 75.0, 0, 1);
    drop(recovered);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
pub(super) fn billing_open_early_failure_releases_lifetime_lock() {
    let dir = temp_data_dir("billing-early-failed-open");
    fs::create_dir(dir.join("data.sqlite")).unwrap();
    assert!(Database::open(dir.clone()).is_err());
    let guard = open_guard::DatabaseOpenGuard::acquire(&dir).unwrap();
    assert!(guard.can_recover_pending());
    drop(guard);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
pub(super) fn billing_open_subprocess() {
    let Some(dir) = std::env::var_os("OCG_TEST_BILLING_OPEN_DIR") else {
        return;
    };
    let dir = PathBuf::from(dir);
    let db = open_with_host_cipher(dir.clone()).unwrap();
    assert_billing_open_state(&db, 75.0, 1, 0);
    fs::write(dir.join("child-opened"), b"ready").unwrap();
    let mut signal = [0];
    std::io::stdin().read_exact(&mut signal).unwrap();
    assert_billing_open_state(&db, 65.0, 0, 0);
}

#[test]
pub(super) fn billing_open_cross_process_receipt_survives_and_settles_once() {
    use std::process::{Command, Stdio};
    use std::time::{Duration as StdDuration, Instant};
    let dir = temp_data_dir("billing-process-open");
    let (db, log_id, attempt) = billing_open_fixture(&dir);
    let mut child = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "db::billing_tests::billing_open_subprocess",
            "--nocapture",
        ])
        .env("OCG_TEST_BILLING_OPEN_DIR", &dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + StdDuration::from_secs(30);
    while !dir.join("child-opened").exists() {
        if child.try_wait().unwrap().is_some() || Instant::now() >= deadline {
            let _ = child.kill();
            let output = child.wait_with_output().unwrap();
            panic!("concurrent child open failed: {output:?}");
        }
        std::thread::sleep(StdDuration::from_millis(10));
    }
    finish_billing_open_attempt(&db, log_id, &attempt);
    child.stdin.take().unwrap().write_all(&[1]).unwrap();
    while child.try_wait().unwrap().is_none() {
        if Instant::now() >= deadline {
            let _ = child.kill();
            let output = child.wait_with_output().unwrap();
            panic!("child settlement check timed out: {output:?}");
        }
        std::thread::sleep(StdDuration::from_millis(10));
    }
    let output = child.wait_with_output().unwrap();
    assert!(output.status.success(), "{output:?}");
    drop(db);
    let reopened = open_with_host_cipher(dir.clone()).unwrap();
    assert_billing_open_state(&reopened, 65.0, 0, 0);
    drop(reopened);
    fs::remove_dir_all(dir).unwrap();
}
