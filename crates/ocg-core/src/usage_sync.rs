//! Adaptive official OpenCode Go usage synchronization.
//!
//! Official usage is a periodic calibration baseline. Local forward_logs remain
//! the immediate real-time estimator after the last successful calibration.
//! Manual and background paths share one secure fetch + key CAS implementation.

pub mod provider_adapter;

use crate::go_usage::{GoUsageError, GoUsageSnapshot};
use crate::kernel::pricing::PricingLimits;
use crate::models::{Account, AppConfig, ProviderUsageSyncState, UsageWindow};
use crate::usage_sync::provider_adapter::supports_authoritative_auto_sync;
use chrono::{DateTime, Duration, Utc};
use futures_util::future::FutureExt;
use parking_lot::Mutex as ParkingMutex;
use serde::Serialize;
use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration as StdDuration;
use tokio::sync::{Mutex as AsyncMutex, Notify};

/// Manual refresh may not re-attempt the same account more often than this.
pub const MANUAL_THROTTLE: Duration = Duration::seconds(15);
/// Ready accounts with local activity in the last day refresh about hourly.
pub const ACTIVE_CADENCE: Duration = Duration::hours(1);
/// Ready accounts without recent local activity refresh about daily.
pub const INACTIVE_CADENCE: Duration = Duration::hours(24);
/// Lookback used to classify an account as locally active.
pub const ACTIVITY_LOOKBACK: Duration = Duration::hours(24);
/// Expedited sync when local max Go usage is at or above this percent.
pub const EXPEDITE_THRESHOLD_PERCENT: f64 = 80.0;
/// Minimum gap between expedited reconciliations for one account.
pub const EXPEDITE_GUARD: Duration = Duration::minutes(15);
/// Lower bound for delayed official sync after a real inference 429.
pub const INFERENCE_429_DELAY_MIN: Duration = Duration::minutes(1);
/// Upper bound for delayed official sync after a real inference 429.
pub const INFERENCE_429_DELAY_MAX: Duration = Duration::minutes(2);
/// Bounded jitter after an official window reset before reconciling.
pub const RESET_JITTER_MAX: Duration = Duration::minutes(3);
/// Startup deferral spread so a restart does not stampede official fetches.
pub const STARTUP_SPREAD_MAX: Duration = Duration::minutes(15);
/// Idle sleep when nothing is due; wake notifications interrupt this.
pub const SCHEDULER_IDLE_TICK: StdDuration = StdDuration::from_secs(30);
/// Serial pacing between background refreshes.
pub const SCHEDULER_PACE: StdDuration = StdDuration::from_secs(2);

const FAILURE_BACKOFF: &[Duration] = &[
    Duration::minutes(5),
    Duration::minutes(15),
    Duration::hours(1),
    Duration::hours(6),
];

/// Why a refresh was requested. Does not change network/CAS behavior.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UsageSyncTrigger {
    Manual,
    Scheduled,
    Expedited,
    Reset,
    Inference429,
}

impl UsageSyncTrigger {
    fn as_str(self) -> &'static str {
        match self {
            Self::Manual => "manual",
            Self::Scheduled => "scheduled",
            Self::Expedited => "expedited",
            Self::Reset => "reset",
            Self::Inference429 => "inference_429",
        }
    }
}

/// Successful official refresh outcome shared by dashboard and scheduler.
#[derive(Debug, Clone, Serialize)]
pub struct OfficialUsageRefreshSuccess {
    pub usage: UsageWindow,
    pub source: &'static str,
    pub last_success_at: String,
    pub next_allowed_at: String,
}

/// Typed refresh failure. Display never includes keys or upstream bodies.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OfficialUsageRefreshError {
    NotFound,
    NotEligible(&'static str),
    Conflict(&'static str),
    CommitAuthorizationRejected,
    Throttled {
        next_allowed_at: DateTime<Utc>,
        retry_after_secs: u64,
    },
    Upstream(GoUsageError),
    Internal(String),
}

impl std::fmt::Display for OfficialUsageRefreshError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotFound => f.write_str("account not found"),
            Self::NotEligible(message) | Self::Conflict(message) => f.write_str(message),
            Self::CommitAuthorizationRejected => {
                f.write_str("control-plane revision changed while refreshing official Go usage")
            }
            Self::Throttled {
                retry_after_secs, ..
            } => write!(
                f,
                "official Go usage refresh is temporarily throttled; retry after {retry_after_secs}s"
            ),
            Self::Upstream(error) => write!(f, "{error}"),
            Self::Internal(message) => f.write_str(message),
        }
    }
}

impl std::error::Error for OfficialUsageRefreshError {}

type RefreshResult = Result<OfficialUsageRefreshSuccess, OfficialUsageRefreshError>;
type RefreshFuture =
    futures_util::future::Shared<Pin<Box<dyn Future<Output = Arc<RefreshResult>> + Send>>>;
type ClockFn = Arc<dyn Fn() -> DateTime<Utc> + Send + Sync>;
type JitterFn = Arc<dyn Fn() -> f64 + Send + Sync>;
type FetchFuture = Pin<Box<dyn Future<Output = Result<GoUsageSnapshot, GoUsageError>> + Send>>;
type FetchFn = Arc<dyn Fn(AppConfig, String) -> FetchFuture + Send + Sync>;
type CleanupHook = Arc<dyn Fn() -> Pin<Box<dyn Future<Output = ()> + Send>> + Send + Sync>;

/// Metadata committed with a successful official calibration. Hosts map this
/// onto the persistence row without exposing database types here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OfficialUsageSyncSuccessMetadata {
    pub now: DateTime<Utc>,
    pub next_eligible_at: DateTime<Utc>,
    pub mark_expedited: bool,
}

/// Authorization owned by the caller that creates an in-flight refresh.
///
/// Unconditional authorization preserves the V2/manual/background behavior.
/// Dashboard V3 supplies its control-plane CAS tokens so the host can hold its
/// mutation gate while the coordinator performs the final database write.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum UsageSyncCommitAuthorization {
    #[default]
    Unconditional,
    ControlRevision {
        expected_revision: u64,
        process_generation: u64,
    },
}

impl UsageSyncCommitAuthorization {
    pub fn control_revision(expected_revision: u64, process_generation: u64) -> Self {
        Self::ControlRevision {
            expected_revision,
            process_generation,
        }
    }
}

/// Result plus the authorization owned by the in-flight leader. Dashboard V3
/// uses this to distinguish its own guarded execution from a shared
/// V2/background-owned execution before applying its caller-side CAS check.
#[derive(Debug, Clone)]
pub(crate) struct OfficialUsageRefreshObservation {
    pub result: RefreshResult,
    pub owner_authorization: UsageSyncCommitAuthorization,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UsageSyncCommitAuthorizationRejected;

/// Persistence operations held under one database lock by [`UsageSyncHost`].
pub trait UsageSyncStore {
    fn list_accounts(&self) -> anyhow::Result<Vec<Account>>;
    fn get_account(&self, account_id: &str) -> anyhow::Result<Option<Account>>;
    fn account_usage_sync_state(
        &self,
        account_id: &str,
    ) -> anyhow::Result<Option<ProviderUsageSyncState>>;
    fn pull_account_usage_sync_next_eligible(
        &self,
        account_id: &str,
        proposal: DateTime<Utc>,
        respect_failure_backoff: bool,
    ) -> anyhow::Result<()>;
    fn account_has_local_activity_since(
        &self,
        account_id: &str,
        since: DateTime<Utc>,
    ) -> anyhow::Result<bool>;
    fn account_usage_with_limits(
        &self,
        account_id: &str,
        limits: &PricingLimits,
    ) -> anyhow::Result<UsageWindow>;
    fn commit_official_usage_sync_success(
        &self,
        account_id: &str,
        expected_key_cipher: &str,
        snapshot: &GoUsageSnapshot,
        limits: &PricingLimits,
        metadata: OfficialUsageSyncSuccessMetadata,
    ) -> anyhow::Result<Option<UsageWindow>>;
    fn record_account_usage_sync_failure(
        &self,
        account_id: &str,
        now: DateTime<Utc>,
        failure_streak: i64,
        next_eligible_at: DateTime<Utc>,
    ) -> anyhow::Result<()>;
    fn log_gateway(&self, level: &str, category: &str, message: &str) -> anyhow::Result<()>;
}

/// Process-level usage-sync host: database/config/proxy/fetch/clock/scheduler
/// seams the reconciler actually needs. Concrete adapters live in `state`.
pub trait UsageSyncHost: Clone + Send + Sync + 'static {
    type Weak: Send + Sync + 'static;
    type Store: UsageSyncStore + ?Sized;

    fn downgrade(&self) -> Self::Weak;
    fn upgrade(weak: &Self::Weak) -> Option<Self>;
    fn usage_runtime(&self) -> &UsageSyncRuntime;
    fn pricing_limits(&self) -> PricingLimits;
    fn config(&self) -> AppConfig;
    fn decrypt_account_key(&self, ciphertext: &str) -> anyhow::Result<String>;
    fn with_sync_store<F, R>(&self, f: F) -> R
    where
        F: FnOnce(&Self::Store) -> R;

    /// Runs a persistence operation under the caller-owned commit guard.
    /// Hosts that support guarded commits override this method and keep the
    /// authorization check atomic with the store operation. The default keeps
    /// all existing unconditional callers backward compatible and rejects an
    /// unsupported guarded request fail-closed.
    fn with_authorized_sync_store<F, R>(
        &self,
        authorization: &UsageSyncCommitAuthorization,
        f: F,
    ) -> Result<R, UsageSyncCommitAuthorizationRejected>
    where
        F: FnOnce(&Self::Store) -> R,
    {
        match authorization {
            UsageSyncCommitAuthorization::Unconditional => Ok(self.with_sync_store(f)),
            UsageSyncCommitAuthorization::ControlRevision { .. } => {
                Err(UsageSyncCommitAuthorizationRejected)
            }
        }
    }
}

#[derive(Clone)]
struct InflightEntry {
    generation: u64,
    future: RefreshFuture,
    authorization: UsageSyncCommitAuthorization,
}

/// Process-wide gates for concurrency-1, in-flight dedupe, and wakeups.
pub struct UsageSyncRuntime {
    global: AsyncMutex<()>,
    inflight: AsyncMutex<HashMap<String, InflightEntry>>,
    inflight_generation: AtomicU64,
    /// Arc so the scheduler can wait without pinning the process host alive.
    wake: Arc<Notify>,
    loop_started: AtomicBool,
    /// Optional injectable clock for tests. Production uses `Utc::now`.
    clock: ParkingMutex<Option<ClockFn>>,
    /// Optional injectable jitter (0.0..1.0) for tests.
    jitter: ParkingMutex<Option<JitterFn>>,
    /// Optional fetch seam for tests. Production uses `go_usage::fetch_go_usage`.
    fetch: ParkingMutex<Option<FetchFn>>,
    /// Optional hook run after an in-flight future resolves and before
    /// generation-scoped cleanup (tests only).
    before_inflight_cleanup: ParkingMutex<Option<CleanupHook>>,
}

impl Default for UsageSyncRuntime {
    fn default() -> Self {
        Self::new()
    }
}

impl UsageSyncRuntime {
    pub fn new() -> Self {
        Self {
            global: AsyncMutex::new(()),
            inflight: AsyncMutex::new(HashMap::new()),
            inflight_generation: AtomicU64::new(1),
            wake: Arc::new(Notify::new()),
            loop_started: AtomicBool::new(false),
            clock: ParkingMutex::new(None),
            jitter: ParkingMutex::new(None),
            fetch: ParkingMutex::new(None),
            before_inflight_cleanup: ParkingMutex::new(None),
        }
    }

    pub fn set_clock_for_test(&self, clock: impl Fn() -> DateTime<Utc> + Send + Sync + 'static) {
        *self.clock.lock() = Some(Arc::new(clock));
    }

    pub fn set_jitter_for_test(&self, jitter: impl Fn() -> f64 + Send + Sync + 'static) {
        *self.jitter.lock() = Some(Arc::new(jitter));
    }

    pub fn set_fetch_for_test(
        &self,
        fetch: impl Fn(AppConfig, String) -> FetchFuture + Send + Sync + 'static,
    ) {
        *self.fetch.lock() = Some(Arc::new(fetch));
    }

    pub fn set_before_inflight_cleanup_for_test(
        &self,
        hook: impl Fn() -> Pin<Box<dyn Future<Output = ()> + Send>> + Send + Sync + 'static,
    ) {
        *self.before_inflight_cleanup.lock() = Some(Arc::new(hook));
    }

    pub fn clear_test_seams(&self) {
        *self.clock.lock() = None;
        *self.jitter.lock() = None;
        *self.fetch.lock() = None;
        *self.before_inflight_cleanup.lock() = None;
    }

    pub fn now(&self) -> DateTime<Utc> {
        self.clock
            .lock()
            .as_ref()
            .map(|clock| clock())
            .unwrap_or_else(Utc::now)
    }

    fn jitter01(&self) -> f64 {
        self.jitter
            .lock()
            .as_ref()
            .map(|jitter| jitter().clamp(0.0, 1.0))
            .unwrap_or_else(random_jitter01)
    }

    fn wake(&self) {
        self.wake.notify_one();
    }

    fn wake_handle(&self) -> Arc<Notify> {
        self.wake.clone()
    }
}

fn random_jitter01() -> f64 {
    // Cheap deterministic-enough mix from UUID bits; tests inject an exact seam.
    let bits = uuid::Uuid::new_v4().as_u128();
    ((bits % 10_000) as f64) / 10_000.0
}

fn scale_duration(base: Duration, jitter01: f64) -> Duration {
    let millis = base.num_milliseconds().max(0) as f64;
    Duration::milliseconds((millis * jitter01.clamp(0.0, 1.0)).round() as i64)
}

fn duration_between(min: Duration, max: Duration, jitter01: f64) -> Duration {
    if max <= min {
        return min;
    }
    min + scale_duration(max - min, jitter01)
}

/// Failure backoff ladder: 5m → 15m → 1h → 6h (capped).
pub fn failure_backoff(failure_streak_after: u32) -> Duration {
    let index = failure_streak_after.saturating_sub(1) as usize;
    FAILURE_BACKOFF
        .get(index)
        .copied()
        .unwrap_or(*FAILURE_BACKOFF.last().expect("backoff ladder non-empty"))
}

pub fn cadence_for(active_in_lookback: bool) -> Duration {
    if active_in_lookback {
        ACTIVE_CADENCE
    } else {
        INACTIVE_CADENCE
    }
}

pub fn manual_next_allowed_at(
    last_attempt_at: Option<DateTime<Utc>>,
    now: DateTime<Utc>,
) -> Option<DateTime<Utc>> {
    let last = last_attempt_at?;
    let until = last + MANUAL_THROTTLE;
    (until > now).then_some(until)
}

pub fn max_go_usage_percent(usage: &UsageWindow, limits: &PricingLimits) -> f64 {
    let pct = |cost: f64, limit: f64| {
        if limit <= 0.0 {
            0.0
        } else {
            ((cost / limit) * 100.0).clamp(0.0, 100.0)
        }
    };
    pct(usage.window_5h, limits.window_5h)
        .max(pct(usage.window_week, limits.window_week))
        .max(pct(usage.window_month, limits.window_month))
}

pub fn account_is_auto_sync_candidate(enabled: bool, setup_ready: bool, key_present: bool) -> bool {
    enabled && setup_ready && key_present
}

pub fn provider_account_is_auto_sync_candidate(
    provider_id: &str,

    enabled: bool,
    setup_ready: bool,
    key_present: bool,
) -> bool {
    supports_authoritative_auto_sync(provider_id)
        && account_is_auto_sync_candidate(enabled, setup_ready, key_present)
}

pub fn compute_next_after_success(
    now: DateTime<Utc>,
    active: bool,
    earliest_resets_in_minutes: i64,
    jitter01: f64,
) -> DateTime<Utc> {
    let cadence_at = now + cadence_for(active);
    if earliest_resets_in_minutes <= 0 {
        return cadence_at;
    }
    let reset_delay =
        Duration::minutes(earliest_resets_in_minutes) + scale_duration(RESET_JITTER_MAX, jitter01);
    let reset_at = now + reset_delay;
    cadence_at.min(reset_at)
}

pub fn compute_next_after_failure(
    now: DateTime<Utc>,
    failure_streak_after: u32,
    jitter01: f64,
) -> DateTime<Utc> {
    let base = failure_backoff(failure_streak_after);
    // Keep backoff dominant; add a little positive jitter up to 10% of the step.
    let jitter = scale_duration(base / 10, jitter01);
    now + base + jitter
}

pub fn compute_inference_429_delay(now: DateTime<Utc>, jitter01: f64) -> DateTime<Utc> {
    now + duration_between(INFERENCE_429_DELAY_MIN, INFERENCE_429_DELAY_MAX, jitter01)
}

pub fn compute_startup_deferral(
    now: DateTime<Utc>,
    account_id: &str,
    jitter01: f64,
) -> DateTime<Utc> {
    // Mix a stable per-account offset with runtime jitter so restarts spread
    // work without requiring a fetch on boot.
    let stable = deterministic_unit(account_id);
    let mixed = (0.5 * stable + 0.5 * jitter01).clamp(0.0, 1.0);
    now + scale_duration(STARTUP_SPREAD_MAX, mixed)
}

fn deterministic_unit(account_id: &str) -> f64 {
    let mut hash = 0u64;
    for byte in account_id.as_bytes() {
        hash = hash.wrapping_mul(131).wrapping_add(u64::from(*byte));
    }
    (hash % 10_000) as f64 / 10_000.0
}

/// Pull `next_eligible_at` earlier. `None` current means "unset"; the proposal wins.
pub fn pull_next_eligible_earlier(
    current: Option<DateTime<Utc>>,
    proposal: DateTime<Utc>,
) -> DateTime<Utc> {
    match current {
        Some(existing) => existing.min(proposal),
        None => proposal,
    }
}

/// True while a failure backoff floor must not be pulled forward by
/// threshold / cadence / reset logic.
pub fn in_failure_backoff(failure_streak: i64) -> bool {
    failure_streak > 0
}

/// High-usage expedite is allowed only when local max Go usage is high enough
/// and no official call (attempt, success, or prior expedite) happened inside
/// the 15-minute guard. Failure backoff is enforced separately by callers and
/// must never be pulled forward by this check alone.
pub fn should_run_expedited(
    max_percent: f64,
    last_expedited_at: Option<DateTime<Utc>>,
    last_attempt_at: Option<DateTime<Utc>>,
    last_success_at: Option<DateTime<Utc>>,
    now: DateTime<Utc>,
) -> bool {
    if max_percent < EXPEDITE_THRESHOLD_PERCENT {
        return false;
    }
    let most_recent = [last_expedited_at, last_attempt_at, last_success_at]
        .into_iter()
        .flatten()
        .max();
    match most_recent {
        None => true,
        Some(last) => now >= last + EXPEDITE_GUARD,
    }
}

/// Propose an earlier next-eligible time after inactive→active local traffic.
/// Returns `None` when no pull is warranted.
pub fn active_cadence_pull_proposal(
    last_success_at: Option<DateTime<Utc>>,
    current_next: DateTime<Utc>,
    now: DateTime<Utc>,
) -> Option<DateTime<Utc>> {
    let last_success = last_success_at?;
    let active_due = last_success + ACTIVE_CADENCE;
    let proposal = if active_due <= now { now } else { active_due };
    (proposal < current_next).then_some(proposal)
}

/// Schedule a delayed official reconciliation after a real inference 429.
/// Never performs the network call inline and never touches cooldown state.
///
/// Intentionally allowed to pull earlier than a failure-backoff floor: the
/// 1–2 minute post-429 event is an explicit override of cadence/backoff
/// scheduling, tested separately from threshold/cadence pulls.
pub fn schedule_after_inference_429(state: &impl UsageSyncHost, account_id: &str) {
    let now = state.usage_runtime().now();
    let jitter = state.usage_runtime().jitter01();
    let proposal = compute_inference_429_delay(now, jitter);
    let scheduled = state.with_sync_store(|store| {
        let supported = match store.get_account(account_id) {
            Ok(Some(account)) => {
                supports_authoritative_auto_sync(&account.provider_id)
            }
            Ok(None) => false,
            Err(error) => {
                let _ = store.log_gateway(
                    "warn",
                    "usage_sync",
                    &format!(
                        "failed to resolve provider before post-429 usage sync for {account_id}: {error}"
                    ),
                );
                return false;
            }
        };
        if !supported {
            // GOAT has manual official calibration but no automatic-sync
            // contract, and must not be coupled to inference cooldown or
            // eligibility. Zen Free uses its separate egress-IP/global path.
            return false;
        }
        if let Err(error) = store.pull_account_usage_sync_next_eligible(account_id, proposal, false)
        {
            let _ = store.log_gateway(
                "warn",
                "usage_sync",
                &format!("failed to schedule post-429 usage sync for {account_id}: {error}"),
            );
            return false;
        }
        true
    });
    if scheduled {
        state.usage_runtime().wake();
    }
}

/// Process-level workers owned by a [`UsageSyncHost`].
///
/// [`ControlPlaneWorkers::ensure_started`] is idempotent once per host and has
/// no public cancel API. The usage loop is independent of Gateway listener
/// bind/stop and exits only when the owning host is dropped (weak upgrade
/// fails).
pub struct ControlPlaneWorkers;

impl ControlPlaneWorkers {
    /// Start the background usage reconciler once per process host.
    pub fn ensure_started<H: UsageSyncHost>(host: H) {
        if host
            .usage_runtime()
            .loop_started
            .swap(true, Ordering::AcqRel)
        {
            return;
        }
        host.with_sync_store(|store| {
            let _ = store.log_gateway("info", "usage_sync", "event=official_usage_worker_started");
        });
        let weak = host.downgrade();
        tokio::spawn(async move {
            loop {
                let Some(state) = H::upgrade(&weak) else {
                    return;
                };
                if let Err(error) = run_scheduler_once(&state).await {
                    state.with_sync_store(|store| {
                        let _ = store.log_gateway(
                            "warn",
                            "usage_sync",
                            &format!("official usage scheduler tick failed: {error}"),
                        );
                    });
                }
                // Clone the wake handle, then drop state before awaiting so tests
                // and shutdown can release the SQLite file promptly.
                let wake = state.usage_runtime().wake_handle();
                drop(state);
                tokio::select! {
                    _ = tokio::time::sleep(SCHEDULER_IDLE_TICK) => {}
                    _ = wake.notified() => {}
                }
            }
        });
    }
}

/// Compatibility wrapper around [`ControlPlaneWorkers::ensure_started`].
///
/// Safe to call repeatedly; the loop is not cancelled by Gateway stop and
/// exits only when the owning host is dropped (weak upgrade fails).
pub fn spawn_usage_sync_loop<H: UsageSyncHost>(state: H) {
    ControlPlaneWorkers::ensure_started(state);
}

async fn run_scheduler_once(state: &impl UsageSyncHost) -> anyhow::Result<()> {
    let now = state.usage_runtime().now();
    let limits = state.pricing_limits();
    let candidates = list_auto_candidates(state, now, &limits)?;
    for candidate in candidates {
        match candidate.action {
            CandidateAction::DeferStartup { until } => {
                state.with_sync_store(|store| {
                    store.pull_account_usage_sync_next_eligible(&candidate.account_id, until, true)
                })?;
            }
            CandidateAction::Refresh { trigger } => {
                let _ = refresh_official_usage(state, &candidate.account_id, trigger).await;
                tokio::time::sleep(SCHEDULER_PACE).await;
            }
        }
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CandidateAction {
    DeferStartup { until: DateTime<Utc> },
    Refresh { trigger: UsageSyncTrigger },
}

#[derive(Debug, Clone)]
struct SyncCandidate {
    account_id: String,
    action: CandidateAction,
}

fn list_auto_candidates(
    host: &impl UsageSyncHost,
    now: DateTime<Utc>,
    limits: &PricingLimits,
) -> anyhow::Result<Vec<SyncCandidate>> {
    host.with_sync_store(|store| list_auto_candidates_on(store, now, limits))
}

fn list_auto_candidates_on(
    store: &(impl UsageSyncStore + ?Sized),
    now: DateTime<Utc>,
    limits: &PricingLimits,
) -> anyhow::Result<Vec<SyncCandidate>> {
    let accounts = store.list_accounts()?;
    let mut out = Vec::new();
    for account in accounts {
        if !provider_account_is_auto_sync_candidate(
            &account.provider_id,
            account.enabled,
            account.setup_step.is_ready(),
            !account.key_cipher.is_empty(),
        ) {
            continue;
        }
        let sync = store.account_usage_sync_state(&account.id)?;
        let next = sync.as_ref().and_then(|s| s.next_eligible_at);
        if next.is_none() {
            let until = compute_startup_deferral(now, &account.id, deterministic_unit(&account.id));
            out.push(SyncCandidate {
                account_id: account.id,
                action: CandidateAction::DeferStartup { until },
            });
            continue;
        }
        let Some(next_at) = next else { continue };
        let sync_state = sync.as_ref();
        let failure_streak = sync_state.map(|s| s.failure_streak).unwrap_or(0);
        let backing_off = in_failure_backoff(failure_streak);

        if next_at > now {
            // Failure backoff floor is never pulled forward by threshold,
            // inactive→active cadence, or reset-style scheduling.
            if backing_off {
                continue;
            }

            let active =
                store.account_has_local_activity_since(&account.id, now - ACTIVITY_LOOKBACK)?;
            if active
                && let Some(proposal) = active_cadence_pull_proposal(
                    sync_state.and_then(|s| s.last_success_at),
                    next_at,
                    now,
                )
            {
                store.pull_account_usage_sync_next_eligible(&account.id, proposal, true)?;
                if proposal <= now {
                    out.push(SyncCandidate {
                        account_id: account.id.clone(),
                        action: CandidateAction::Refresh {
                            trigger: UsageSyncTrigger::Scheduled,
                        },
                    });
                    continue;
                }
            }

            let usage = store.account_usage_with_limits(&account.id, limits)?;
            let max_pct = max_go_usage_percent(&usage, limits);
            if should_run_expedited(
                max_pct,
                sync_state.and_then(|s| s.last_expedited_at),
                sync_state.and_then(|s| s.last_attempt_at),
                sync_state.and_then(|s| s.last_success_at),
                now,
            ) {
                let proposal = now;
                store.pull_account_usage_sync_next_eligible(&account.id, proposal, true)?;
                out.push(SyncCandidate {
                    account_id: account.id,
                    action: CandidateAction::Refresh {
                        trigger: UsageSyncTrigger::Expedited,
                    },
                });
            }
            continue;
        }

        let usage = store.account_usage_with_limits(&account.id, limits)?;
        let max_pct = max_go_usage_percent(&usage, limits);
        let trigger = if !backing_off
            && should_run_expedited(
                max_pct,
                sync_state.and_then(|s| s.last_expedited_at),
                sync_state.and_then(|s| s.last_attempt_at),
                sync_state.and_then(|s| s.last_success_at),
                now,
            ) {
            UsageSyncTrigger::Expedited
        } else {
            UsageSyncTrigger::Scheduled
        };
        out.push(SyncCandidate {
            account_id: account.id,
            action: CandidateAction::Refresh { trigger },
        });
    }
    Ok(out)
}

/// Remove an in-flight map entry only when it is still the same generation the
/// waiter observed. Stale waiters must not delete a newer F2 entry.
fn take_inflight_if_generation(
    map: &mut HashMap<String, InflightEntry>,
    account_id: &str,
    generation: u64,
) -> bool {
    match map.get(account_id) {
        Some(entry) if entry.generation == generation => {
            map.remove(account_id);
            true
        }
        _ => false,
    }
}

/// Shared manual/background entry. Enforces throttle (manual), global
/// concurrency 1, in-flight dedupe, secure fetch, key CAS, and sync metadata.
pub async fn refresh_official_usage<H: UsageSyncHost>(
    state: &H,
    account_id: &str,
    trigger: UsageSyncTrigger,
) -> RefreshResult {
    refresh_official_usage_with_authorization(
        state,
        account_id,
        trigger,
        UsageSyncCommitAuthorization::Unconditional,
    )
    .await
    .result
}

/// Guarded refresh entry used by Dashboard V3.
///
/// Dedupe ownership is intentionally leader-scoped: the authorization passed
/// by the caller that creates the in-flight future governs its writes. A later
/// follower observes that shared result but cannot veto an already-running
/// V2/background-owned refresh with its unrelated CAS token.
pub(crate) async fn refresh_official_usage_with_authorization<H: UsageSyncHost>(
    state: &H,
    account_id: &str,
    trigger: UsageSyncTrigger,
    authorization: UsageSyncCommitAuthorization,
) -> OfficialUsageRefreshObservation {
    if trigger == UsageSyncTrigger::Manual {
        let now = state.usage_runtime().now();
        let sync = match state.with_sync_store(|store| store.account_usage_sync_state(account_id)) {
            Ok(sync) => sync,
            Err(error) => {
                return OfficialUsageRefreshObservation {
                    result: Err(OfficialUsageRefreshError::Internal(error.to_string())),
                    owner_authorization: authorization,
                };
            }
        };
        if let Some(until) = manual_next_allowed_at(sync.and_then(|s| s.last_attempt_at), now) {
            let retry_after_secs = (until - now).num_seconds().max(1) as u64;
            return OfficialUsageRefreshObservation {
                result: Err(OfficialUsageRefreshError::Throttled {
                    next_allowed_at: until,
                    retry_after_secs,
                }),
                owner_authorization: authorization,
            };
        }
    }

    let (future, generation, owner_authorization) = {
        let mut inflight = state.usage_runtime().inflight.lock().await;
        if let Some(existing) = inflight.get(account_id) {
            (
                existing.future.clone(),
                existing.generation,
                existing.authorization,
            )
        } else {
            let account_id_owned = account_id.to_string();
            let state_cloned = state.clone();
            let generation = state
                .usage_runtime()
                .inflight_generation
                .fetch_add(1, Ordering::Relaxed);
            let shared = async move {
                let result = execute_official_usage_refresh(
                    &state_cloned,
                    &account_id_owned,
                    trigger,
                    &authorization,
                )
                .await;
                audit_official_usage_result(&state_cloned, &account_id_owned, trigger, &result);
                Arc::new(result)
            }
            .boxed()
            .shared();
            inflight.insert(
                account_id.to_string(),
                InflightEntry {
                    generation,
                    future: shared.clone(),
                    authorization,
                },
            );
            (shared, generation, authorization)
        }
    };

    let result = future.await;
    let cleanup_hook = state.usage_runtime().before_inflight_cleanup.lock().clone();
    if let Some(hook) = cleanup_hook {
        hook().await;
    }
    {
        let mut inflight = state.usage_runtime().inflight.lock().await;
        take_inflight_if_generation(&mut inflight, account_id, generation);
    }

    OfficialUsageRefreshObservation {
        result: match &*result {
            Ok(success) => Ok(success.clone()),
            Err(error) => Err(error.clone()),
        },
        owner_authorization,
    }
}

fn audit_official_usage_result(
    state: &impl UsageSyncHost,
    account_id: &str,
    trigger: UsageSyncTrigger,
    result: &RefreshResult,
) {
    let (level, message) = match result {
        Ok(success) if trigger == UsageSyncTrigger::Manual => (
            "info",
            format!(
                "event=official_usage_refresh_succeeded account_id={account_id} trigger={} next_allowed_at={}",
                trigger.as_str(),
                success.next_allowed_at
            ),
        ),
        Ok(_) => return,
        Err(error) => {
            let Some(reason) = official_usage_failure_kind(error) else {
                return;
            };
            (
                "warn",
                format!(
                    "event=official_usage_refresh_failed account_id={account_id} trigger={} reason={reason}",
                    trigger.as_str()
                ),
            )
        }
    };
    state.with_sync_store(|store| {
        let _ = store.log_gateway(level, "usage_sync", &message);
    });
}

fn official_usage_failure_kind(error: &OfficialUsageRefreshError) -> Option<&'static str> {
    match error {
        OfficialUsageRefreshError::Upstream(GoUsageError::Unauthorized) => {
            Some("upstream_unauthorized")
        }
        OfficialUsageRefreshError::Upstream(GoUsageError::Forbidden) => Some("upstream_forbidden"),
        OfficialUsageRefreshError::Upstream(GoUsageError::RateLimited) => {
            Some("upstream_rate_limited")
        }
        OfficialUsageRefreshError::Upstream(GoUsageError::Http(_)) => Some("upstream_http"),
        OfficialUsageRefreshError::Upstream(GoUsageError::Timeout) => Some("upstream_timeout"),
        OfficialUsageRefreshError::Upstream(GoUsageError::Network) => Some("upstream_network"),
        OfficialUsageRefreshError::Upstream(GoUsageError::Oversize) => Some("upstream_oversize"),
        OfficialUsageRefreshError::Upstream(GoUsageError::Schema) => Some("upstream_schema"),
        OfficialUsageRefreshError::Upstream(GoUsageError::Window) => Some("upstream_window"),
        OfficialUsageRefreshError::Internal(_) => Some("internal"),
        OfficialUsageRefreshError::Conflict(_) => Some("account_conflict"),
        OfficialUsageRefreshError::CommitAuthorizationRejected => Some("revision_conflict"),
        OfficialUsageRefreshError::NotFound
        | OfficialUsageRefreshError::NotEligible(_)
        | OfficialUsageRefreshError::Throttled { .. } => None,
    }
}

async fn execute_official_usage_refresh(
    state: &impl UsageSyncHost,
    account_id: &str,
    trigger: UsageSyncTrigger,
    authorization: &UsageSyncCommitAuthorization,
) -> Result<OfficialUsageRefreshSuccess, OfficialUsageRefreshError> {
    let _guard = state.usage_runtime().global.lock().await;
    let now = state.usage_runtime().now();
    let limits = state.pricing_limits();
    let config = state.config();

    // Policy exclusions: do not begin an attempt / do not write backoff.
    let account = {
        match state.with_sync_store(|store| store.get_account(account_id)) {
            Ok(Some(account)) => account,
            Ok(None) => return Err(OfficialUsageRefreshError::NotFound),
            Err(error) => {
                // DB read failed after scheduler selected the account: treat as
                // a begun attempt so the due stamp cannot busy-loop.
                record_attempt_failure(state, account_id, now, authorization)?;
                return Err(OfficialUsageRefreshError::Internal(error.to_string()));
            }
        }
    };
    if !supports_authoritative_auto_sync(&account.provider_id) {
        return Err(OfficialUsageRefreshError::NotEligible(
            "verified official usage refresh is unavailable for this provider offering",
        ));
    }
    if !account.setup_step.is_ready() || account.key_cipher.is_empty() {
        return Err(OfficialUsageRefreshError::NotEligible(
            "only ready accounts with a stored key can refresh official Go usage",
        ));
    }
    if trigger != UsageSyncTrigger::Manual && !account.enabled {
        return Err(OfficialUsageRefreshError::NotEligible(
            "disabled accounts are not auto-synced",
        ));
    }
    let key_cipher = account.key_cipher.clone();
    let plaintext = match state.decrypt_account_key(&key_cipher) {
        Ok(key) => key,
        Err(error) => {
            record_attempt_failure(state, account_id, now, authorization)?;
            return Err(OfficialUsageRefreshError::Internal(error.to_string()));
        }
    };

    let snapshot = {
        let fetch = state.usage_runtime().fetch.lock().clone();
        let result = if let Some(fetch) = fetch {
            fetch(config.clone(), plaintext.clone()).await
        } else {
            crate::go_usage::fetch_go_usage(&config, &plaintext).await
        };
        drop(plaintext);
        result
    };

    let snapshot = match snapshot {
        Ok(snapshot) => snapshot,
        Err(error) => {
            record_attempt_failure(state, account_id, now, authorization)?;
            return Err(OfficialUsageRefreshError::Upstream(error));
        }
    };

    let active = {
        match state.with_sync_store(|store| {
            store.account_has_local_activity_since(account_id, now - ACTIVITY_LOOKBACK)
        }) {
            Ok(active) => active,
            Err(error) => {
                record_attempt_failure(state, account_id, now, authorization)?;
                return Err(OfficialUsageRefreshError::Internal(error.to_string()));
            }
        }
    };
    let jitter = state.usage_runtime().jitter01();
    let next_eligible =
        compute_next_after_success(now, active, snapshot.earliest_resets_in_minutes, jitter);
    let next_allowed = now + MANUAL_THROTTLE;
    let usage = {
        let committed = state
            .with_authorized_sync_store(authorization, |store| {
                store.commit_official_usage_sync_success(
                    account_id,
                    &key_cipher,
                    &snapshot,
                    &limits,
                    OfficialUsageSyncSuccessMetadata {
                        now,
                        next_eligible_at: next_eligible,
                        mark_expedited: trigger == UsageSyncTrigger::Expedited,
                    },
                )
            })
            .map_err(|_| commit_authorization_conflict())?;
        match committed {
            Ok(Some(usage)) => usage,
            Ok(None) => {
                record_attempt_failure(state, account_id, now, authorization)?;
                return Err(OfficialUsageRefreshError::Conflict(
                    "account key or setup changed while refreshing official Go usage",
                ));
            }
            Err(error) => {
                record_attempt_failure(state, account_id, now, authorization)?;
                return Err(OfficialUsageRefreshError::Internal(error.to_string()));
            }
        }
    };

    Ok(OfficialUsageRefreshSuccess {
        usage,
        source: "official_go_usage",
        last_success_at: now.to_rfc3339(),
        next_allowed_at: next_allowed.to_rfc3339(),
    })
}

/// Record a safe retry/backoff outcome for any begun attempt that did not
/// succeed. Never logs keys, ciphertext, or upstream bodies. If persistence
/// itself fails, emit only a sanitized scheduler diagnostic.
fn record_attempt_failure(
    state: &impl UsageSyncHost,
    account_id: &str,
    now: DateTime<Utc>,
    authorization: &UsageSyncCommitAuthorization,
) -> Result<(), OfficialUsageRefreshError> {
    let jitter = state.usage_runtime().jitter01();
    state
        .with_authorized_sync_store(authorization, |store| {
            let current = store.account_usage_sync_state(account_id).ok().flatten();
            let streak = current.as_ref().map(|s| s.failure_streak).unwrap_or(0) + 1;
            let next = compute_next_after_failure(now, streak as u32, jitter);
            if let Err(error) =
                store.record_account_usage_sync_failure(account_id, now, streak, next)
            {
                let _ = store.log_gateway(
                    "warn",
                    "usage_sync",
                    &format!("failed to persist usage-sync backoff for {account_id}: {error}"),
                );
            }
        })
        .map_err(|_| commit_authorization_conflict())?;
    Ok(())
}

fn commit_authorization_conflict() -> OfficialUsageRefreshError {
    OfficialUsageRefreshError::CommitAuthorizationRejected
}

/// Dashboard helper: map sync metadata onto API fields.
pub fn dashboard_sync_fields(
    sync: Option<&ProviderUsageSyncState>,
    now: DateTime<Utc>,
) -> (Option<String>, Option<String>) {
    let last_success = sync.and_then(|s| s.last_success_at.map(|t| t.to_rfc3339()));
    let next_allowed = sync
        .and_then(|s| manual_next_allowed_at(s.last_attempt_at, now))
        .map(|t| t.to_rfc3339());
    (last_success, next_allowed)
}

#[cfg(test)]
mod tests;
