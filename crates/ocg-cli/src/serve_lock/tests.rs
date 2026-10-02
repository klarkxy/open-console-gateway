use super::{ServeLock, lock_path};

#[test]
fn a_second_acquire_fails_until_the_first_lock_drops() {
    let dir = std::env::temp_dir().join(format!("ocg-cli-serve-lock-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let first = ServeLock::acquire(&dir).unwrap();
    let second = ServeLock::acquire(&dir).unwrap_err();
    assert!(second.to_string().contains("serve"), "{second:#}");
    assert!(lock_path(&dir).is_file());
    drop(first);
    let third = ServeLock::acquire(&dir).unwrap();
    drop(third);
    assert!(
        lock_path(&dir).is_file(),
        "the lock file stays so later serves lock the same inode"
    );
    let _ = std::fs::remove_dir_all(dir);
}
