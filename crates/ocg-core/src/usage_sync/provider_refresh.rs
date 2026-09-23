//! Coalesce one credential version's official usage refresh, and cap how many
//! refreshes run at once. Those are separate gates: a second request for the
//! same credential joins the in-flight fetch, while a different credential
//! waits only for a free concurrency slot.
//!
//! The in-flight entry is removed when the shared fetch finishes, and also
//! when the last waiter leaves early. A permit is taken only after the
//! per-account lock, so a same-account queue does not occupy global slots.

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
    waiters: usize,
    future:
        futures_util::future::Shared<futures_util::future::BoxFuture<'static, CalibrationOutcome>>,
}

pub(crate) struct ProviderUsageRefreshGate {
    inflight: parking_lot::Mutex<HashMap<String, InflightEntry>>,
    key_locks: Arc<parking_lot::Mutex<HashMap<String, Arc<Mutex<()>>>>>,
    limit: Arc<Semaphore>,
    generation: AtomicU64,
}

pub(crate) struct ExclusiveRefresh {
    key: String,
    locks: Arc<parking_lot::Mutex<HashMap<String, Arc<Mutex<()>>>>>,
    guard: Option<OwnedMutexGuard<()>>,
    permit: Option<OwnedSemaphorePermit>,
}

struct WaiterGuard<'a> {
    inflight: &'a parking_lot::Mutex<HashMap<String, InflightEntry>>,
    key: String,
    generation: u64,
}

impl Drop for WaiterGuard<'_> {
    fn drop(&mut self) {
        let mut map = self.inflight.lock();
        let Some(entry) = map.get_mut(&self.key) else {
            return;
        };
        if entry.generation != self.generation {
            return;
        }
        entry.waiters = entry.waiters.saturating_sub(1);
        if entry.waiters == 0 {
            map.remove(&self.key);
        }
    }
}

impl Drop for ExclusiveRefresh {
    fn drop(&mut self) {
        self.guard.take();
        self.permit.take();
        let mut locks = self.locks.lock();
        if locks
            .get(&self.key)
            .is_some_and(|lock| Arc::strong_count(lock) == 1)
        {
            locks.remove(&self.key);
        }
    }
}

impl ProviderUsageRefreshGate {
    pub(crate) fn new(concurrency: usize) -> Self {
        Self {
            inflight: parking_lot::Mutex::new(HashMap::new()),
            key_locks: Arc::new(parking_lot::Mutex::new(HashMap::new())),
            limit: Arc::new(Semaphore::new(concurrency.max(1))),
            generation: AtomicU64::new(1),
        }
    }

    pub(crate) fn balance_key(account_id: &str) -> String {
        format!("balance:{account_id}")
    }

    /// Join an in-flight calibration for `key`, or start one. The semaphore
    /// is taken inside the shared future, so waiters of the same key do not
    /// each consume a slot. Leaving early drops the entry once nobody remains
    /// to drive it, which releases a permit held by that future.
    pub(crate) async fn run<F, Fut>(&self, key: String, work: F) -> CalibrationOutcome
    where
        F: FnOnce() -> Fut + Send + 'static,
        Fut: Future<Output = CalibrationOutcome> + Send + 'static,
    {
        let (future, generation) = {
            let mut map = self.inflight.lock();
            if let Some(entry) = map.get_mut(&key) {
                entry.waiters += 1;
                (entry.future.clone(), entry.generation)
            } else {
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
                        waiters: 1,
                        future: shared.clone(),
                    },
                );
                (shared, generation)
            }
        };
        let _guard = WaiterGuard {
            inflight: &self.inflight,
            key: key.clone(),
            generation,
        };
        let result = future.await;
        clear_inflight(&self.inflight, &key, generation);
        result
    }

    /// Serialize one balance refresh per account. The account lock is taken
    /// before a global permit, so waiters for that account do not occupy the
    /// shared concurrency cap. The per-account lock entry is removed when the
    /// last holder leaves.
    pub(crate) async fn exclusive(&self, key: impl Into<String>) -> ExclusiveRefresh {
        let key = key.into();
        let key_lock = {
            let mut locks = self.key_locks.lock();
            locks
                .entry(key.clone())
                .or_insert_with(|| Arc::new(Mutex::new(())))
                .clone()
        };
        let guard = key_lock.lock_owned().await;
        let permit = self
            .limit
            .clone()
            .acquire_owned()
            .await
            .expect("provider refresh semaphore stays open");
        ExclusiveRefresh {
            key,
            locks: Arc::clone(&self.key_locks),
            guard: Some(guard),
            permit: Some(permit),
        }
    }

    #[cfg(test)]
    fn available_permits(&self) -> usize {
        self.limit.available_permits()
    }

    #[cfg(test)]
    fn key_lock_holders(&self, key: &str) -> usize {
        self.key_locks
            .lock()
            .get(key)
            .map(Arc::strong_count)
            .unwrap_or(0)
    }

    #[cfg(test)]
    fn key_lock_len(&self) -> usize {
        self.key_locks.lock().len()
    }

    #[cfg(test)]
    fn inflight_waiters(&self, key: &str) -> usize {
        self.inflight
            .lock()
            .get(key)
            .map(|entry| entry.waiters)
            .unwrap_or(0)
    }
}

fn clear_inflight(
    inflight: &parking_lot::Mutex<HashMap<String, InflightEntry>>,
    key: &str,
    generation: u64,
) {
    let mut map = inflight.lock();
    if map
        .get(key)
        .is_some_and(|entry| entry.generation == generation)
    {
        map.remove(key);
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

    #[tokio::test(flavor = "current_thread")]
    async fn cancelled_leader_does_not_reuse_a_finished_result() {
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
        for _ in 0..50 {
            if gate.inflight_waiters("usage:same:1") >= 2 {
                break;
            }
            tokio::task::yield_now().await;
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        assert_eq!(gate.inflight_waiters("usage:same:1"), 2);
        leader.abort();
        let _ = leader.await;
        release_tx.send(()).unwrap();
        assert_eq!(follower.await.unwrap(), CalibrationOutcome::Applied);

        let third_runs = Arc::clone(&runs);
        let third = gate
            .run("usage:same:1".into(), move || {
                let third_runs = Arc::clone(&third_runs);
                async move {
                    third_runs.fetch_add(1, Ordering::SeqCst);
                    CalibrationOutcome::FetchFailed("fresh".into())
                }
            })
            .await;
        assert_eq!(third, CalibrationOutcome::FetchFailed("fresh".into()));
        assert_eq!(runs.load(Ordering::SeqCst), 2);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn dropping_every_waiter_releases_the_concurrency_slot() {
        let gate = Arc::new(ProviderUsageRefreshGate::new(1));
        let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
        let gate_holder = Arc::clone(&gate);
        let holder = tokio::spawn(async move {
            gate_holder
                .run("usage:stuck:1".into(), move || async move {
                    let _ = entered_tx.send(());
                    std::future::pending::<CalibrationOutcome>().await
                })
                .await
        });
        entered_rx.await.unwrap();
        holder.abort();
        let _ = holder.await;
        tokio::task::yield_now().await;
        let result = tokio::time::timeout(std::time::Duration::from_secs(1), async {
            gate.run("usage:other:1".into(), || async {
                CalibrationOutcome::Applied
            })
            .await
        })
        .await;
        assert_eq!(result.expect("slot released"), CalibrationOutcome::Applied);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn same_account_queue_does_not_hold_a_global_slot() {
        let gate = Arc::new(ProviderUsageRefreshGate::new(2));
        let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = tokio::sync::oneshot::channel::<()>();
        let leader_gate = Arc::clone(&gate);
        let leader = tokio::spawn(async move {
            let _refresh = leader_gate.exclusive("balance:a").await;
            let _ = entered_tx.send(());
            let _ = release_rx.await;
        });
        entered_rx.await.unwrap();
        let queued_gate = Arc::clone(&gate);
        let queued = tokio::spawn(async move {
            let _refresh = queued_gate.exclusive("balance:a").await;
        });
        for _ in 0..50 {
            if gate.key_lock_holders("balance:a") >= 3 {
                break;
            }
            tokio::task::yield_now().await;
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        assert!(
            gate.key_lock_holders("balance:a") >= 3,
            "the second refresh should be waiting on the account lock"
        );
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        assert_eq!(
            gate.available_permits(),
            1,
            "a same-account waiter must not consume a global slot"
        );
        let other = tokio::time::timeout(std::time::Duration::from_secs(1), async {
            let _refresh = gate.exclusive("balance:b").await;
        })
        .await;
        assert!(other.is_ok(), "another account must still obtain a slot");
        release_tx.send(()).unwrap();
        leader.await.unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(1), queued)
            .await
            .expect("queued refresh finishes after the account lock is released")
            .unwrap();
        assert_eq!(gate.key_lock_len(), 0);
    }
}
