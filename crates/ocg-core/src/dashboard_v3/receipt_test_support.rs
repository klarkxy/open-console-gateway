//! Pause an actual SQLite write before commit while an unrelated CPA writer
//! advances the shared revision. No production hook or timing guess is used.

use crate::state::CoreState;
use std::time::Duration;

pub(super) fn fail_after_cpa_revision_advance<T: Send + 'static>(
    state: &CoreState,
    failure: &'static str,
    call: impl std::future::Future<Output = T> + Send + 'static,
) -> T {
    let (entered_sender, entered_receiver) = std::sync::mpsc::channel();
    let (release_sender, release_receiver) = std::sync::mpsc::channel();
    state
        .db
        .lock()
        .conn
        .create_scalar_function(
            "ocg_test_before_commit",
            0,
            rusqlite::functions::FunctionFlags::SQLITE_UTF8,
            move |_| -> rusqlite::Result<i64> {
                entered_sender.send(()).unwrap();
                release_receiver
                    .recv_timeout(Duration::from_secs(5))
                    .unwrap();
                Err(rusqlite::Error::UserFunctionError(Box::new(
                    std::io::Error::other(failure),
                )))
            },
        )
        .unwrap();
    let (sender, receiver) = std::sync::mpsc::channel();
    let runtime = tokio::runtime::Handle::current();
    let worker = std::thread::spawn(move || {
        sender.send(runtime.block_on(call)).ok();
    });
    entered_receiver
        .recv_timeout(Duration::from_secs(5))
        .expect("write barrier");
    assert!(state.settings_update.try_lock().is_none());
    {
        let _cpa_operation = state.cpa_operations.try_lock().unwrap();
        state.bump_settings_revision();
    }
    release_sender.send(()).unwrap();
    let result = receiver
        .recv_timeout(Duration::from_secs(5))
        .expect("mutation finished");
    worker.join().unwrap();
    result
}
