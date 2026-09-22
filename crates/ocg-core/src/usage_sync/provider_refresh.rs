//! Coalesce one credential version's official usage refresh, and cap how many
//! refreshes run at once. Those are separate gates: a second request for the
//! same credential joins the in-flight fetch, while a different credential
//! waits only for a free concurrency slot.

use chrono::{DateTime, Utc};
use futures_util::future::FutureExt;
use std::collections::HashMap;
use std::future::Future;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use tokio::sync::{Mutex, OwnedMutexGuard, OwnedSemaphorePermit, Semaphore};

/// How many provider usage or balance refreshes may hit the network at once.
pub(crate) const PROVIDER_REFRESH_CONCURRENCY: usize = 4;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ControlRevision {
    pub revision: u64,
    pub process_generation: u64,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum CalibrationOutcome {
    Applied,
    Throttled {
        next_allowed_at: DateTime<Utc>,
        retry_after_secs: u64,
    },
    RejectedKey,
    FetchFailed(String),
    Stale,
    Skipped,
}

struct InflightEntry {
    generation: u64,
    future:
        futures_util::future::Shared<futures_util::future::BoxFuture<'static, CalibrationOutcome>>,
}

pub(crate) struct ProviderUsageRefreshGate {
    inflight: Mutex<HashMap<String, InflightEntry>>,
    key_locks: Mutex<HashMap<String, Arc<Mutex<()>>>>,
    limit: Arc<Semaphore>,
    generation: AtomicU64,
}

pub(crate) struct ExclusiveRefresh {
    _guard: OwnedMutexGuard<()>,
    _permit: OwnedSemaphorePermit,
}

impl ProviderUsageRefreshGate {
    pub(crate) fn new(concurrency: usize) -> Self {
        Self {
            inflight: Mutex::new(HashMap::new()),
            key_locks: Mutex::new(HashMap::new()),
            limit: Arc::new(Semaphore::new(concurrency.max(1))),
            generation: AtomicU64::new(1),
        }
    }

    pub(crate) fn balance_key(account_id: &str) -> String {
        format!("balance:{account_id}")
    }

    /// Join an in-flight calibration for `key`, or start one. The semaphore
    /// is taken inside the shared future, so waiters of the same key do not
    /// each consume a slot.
    pub(crate) async fn run<F, Fut>(&self, key: String, work: F) -> CalibrationOutcome
    where
        F: FnOnce() -> Fut + Send + 'static,
        Fut: Future<Output = CalibrationOutcome> + Send + 'static,
    {
        let mut map = self.inflight.lock().await;
        if let Some(entry) = map.get(&key) {
            let future = entry.future.clone();
            drop(map);
            return future.await;
        }
        let generation = self.generation.fetch_add(1, Ordering::Relaxed);
        let limit = Arc::clone(&self.limit);
        let shared = async move {
            let _permit = limit
                .acquire_owned()
                .await
                .expect("provider refresh semaphore stays open");
            work().await
        }
        .boxed()
        .shared();
        map.insert(
            key.clone(),
            InflightEntry {
                generation,
                future: shared.clone(),
            },
        );
        drop(map);
        let result = shared.await;
        let mut map = self.inflight.lock().await;
        if map
            .get(&key)
            .is_some_and(|entry| entry.generation == generation)
        {
            map.remove(&key);
        }
        result
    }

    /// Serialize one balance refresh per account without blocking another
    /// account's usage calibration. The permit is the shared concurrency cap.
    pub(crate) async fn exclusive(&self, key: impl Into<String>) -> ExclusiveRefresh {
        let key = key.into();
        let key_lock = {
            let mut locks = self.key_locks.lock().await;
            locks
                .entry(key)
                .or_insert_with(|| Arc::new(Mutex::new(())))
                .clone()
        };
        let permit = self
            .limit
            .clone()
            .acquire_owned()
            .await
            .expect("provider refresh semaphore stays open");
        let guard = key_lock.lock_owned().await;
        ExclusiveRefresh {
            _guard: guard,
            _permit: permit,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;

    #[tokio::test(flavor = "current_thread")]
    async fn same_key_runs_once_while_distinct_keys_overlap() {
        let gate = Arc::new(ProviderUsageRefreshGate::new(2));
        let runs = Arc::new(AtomicUsize::new(0));
        let barrier = Arc::new(tokio::sync::Barrier::new(2));
        let run_one = |key: &'static str| {
            let gate = Arc::clone(&gate);
            let runs = Arc::clone(&runs);
            let barrier = Arc::clone(&barrier);
            async move {
                gate.run(key.to_string(), move || {
                    let runs = Arc::clone(&runs);
                    let barrier = Arc::clone(&barrier);
                    async move {
                        runs.fetch_add(1, Ordering::SeqCst);
                        barrier.wait().await;
                        CalibrationOutcome::Applied
                    }
                })
                .await
            }
        };
        let (left, right) = tokio::join!(run_one("usage:a:1"), run_one("usage:b:1"));
        assert_eq!(left, CalibrationOutcome::Applied);
        assert_eq!(right, CalibrationOutcome::Applied);
        assert_eq!(runs.load(Ordering::SeqCst), 2);

        let gate = Arc::new(ProviderUsageRefreshGate::new(2));
        let runs = Arc::new(AtomicUsize::new(0));
        let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = tokio::sync::oneshot::channel::<()>();
        let leader_gate = Arc::clone(&gate);
        let leader_runs = Arc::clone(&runs);
        let leader = tokio::spawn(async move {
            leader_gate
                .run("usage:same:1".into(), move || {
                    let leader_runs = Arc::clone(&leader_runs);
                    async move {
                        leader_runs.fetch_add(1, Ordering::SeqCst);
                        let _ = entered_tx.send(());
                        let _ = release_rx.await;
                        CalibrationOutcome::Applied
                    }
                })
                .await
        });
        entered_rx.await.unwrap();
        let follower_gate = Arc::clone(&gate);
        let follower = tokio::spawn(async move {
            follower_gate
                .run("usage:same:1".into(), || async {
                    unreachable!("an in-flight credential refresh is joined, not started again");
                    #[allow(unreachable_code)]
                    CalibrationOutcome::Skipped
                })
                .await
        });
        tokio::task::yield_now().await;
        release_tx.send(()).unwrap();
        assert_eq!(leader.await.unwrap(), CalibrationOutcome::Applied);
        assert_eq!(follower.await.unwrap(), CalibrationOutcome::Applied);
        assert_eq!(runs.load(Ordering::SeqCst), 1);
    }
}
