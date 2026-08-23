//! Adaptive official OpenCode Go usage synchronization.
//!
//! Official usage is a periodic calibration baseline. Local forward_logs remain
//! the immediate real-time estimator after the last successful calibration.
//! Manual and background paths share one secure fetch + key CAS implementation.

pub mod provider_adapter;

use crate::go_usage::{GoUsageError, GoUsageSnapshot};
use crate::kernel::pricing::PricingLimits;
use crate::models::{Account, AppConfig, UsageWindow};
use crate::provider::ProviderUsageSyncState;
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
    offering_id: &str,
    enabled: bool,
    setup_ready: bool,
    key_present: bool,
) -> bool {
    supports_authoritative_auto_sync(provider_id, offering_id)
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
                supports_authoritative_auto_sync(&account.provider_id, &account.offering_id)
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
            // GOAT `/alpha/*` usage is experimental/unavailable and must not be
            // coupled to inference cooldown or eligibility. Zen Free uses its
            // separate egress-IP/global cooldown path.
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
            &account.offering_id,
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
            if active {
                if let Some(proposal) = active_cadence_pull_proposal(
                    sync_state.and_then(|s| s.last_success_at),
                    next_at,
                    now,
                ) {
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
    if !supports_authoritative_auto_sync(&account.provider_id, &account.offering_id) {
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
mod tests {
    use super::*;
    use crate::crypto::{KeyCipher, StaticKeyCipher};
    use crate::db::{AccountUsageCalibrationSnapshot, Database};
    use crate::models::{Account, AccountSetupStep, AccountType, AppConfig};
    use crate::provider::{
        COMMAND_CODE_PROVIDER_ID, CreditBalance, GOAT_OFFERING_ID, QUOTA_WINDOW_FIVE_HOURS,
        QUOTA_WINDOW_MONTH, QUOTA_WINDOW_WEEK, QuotaWindow, ZEN_FREE_ACCOUNT_ID,
    };
    use crate::state::{CoreState, CoreStateInner};
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicUsize, Ordering as AtomicOrdering};

    fn fixed(ts: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(ts)
            .unwrap()
            .with_timezone(&Utc)
    }

    fn usage_loop_started(state: &CoreState) -> bool {
        state.usage_sync.loop_started.load(AtomicOrdering::Acquire)
    }

    fn loopback_ephemeral() -> std::net::SocketAddr {
        std::net::SocketAddr::from(([127, 0, 0, 1], 0))
    }

    #[tokio::test]
    async fn bind_does_not_start_usage_loop() {
        let (dir, state) = test_state("bind-no-loop");
        assert!(!usage_loop_started(&state));

        let first =
            crate::gateway::listener::GatewayLifecycle::bind(state.clone(), loopback_ephemeral())
                .await
                .unwrap();
        assert!(first.port != 0, "listener bind must occupy a TCP port");
        assert!(
            state.dashboard_local_mode(),
            "loopback bind must keep dashboard local mode"
        );
        assert!(
            !usage_loop_started(&state),
            "GatewayLifecycle::bind must not start the process-level usage worker"
        );

        crate::gateway::stop_gateway(first);
        assert!(
            !usage_loop_started(&state),
            "listener stop must not start the usage worker"
        );
        let _ = state.config();

        let second =
            crate::gateway::listener::GatewayLifecycle::bind(state.clone(), loopback_ephemeral())
                .await
                .unwrap();
        assert!(
            !usage_loop_started(&state),
            "a later bind still must not start the usage worker"
        );
        crate::gateway::stop_gateway(second);

        drop(state);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn spawn_usage_sync_loop_starts_once_per_core_state() {
        let (dir, state) = test_state("once-loop");
        assert!(!usage_loop_started(&state));
        spawn_usage_sync_loop(state.clone());
        assert!(usage_loop_started(&state));
        spawn_usage_sync_loop(state.clone());
        spawn_usage_sync_loop(state.clone());
        ControlPlaneWorkers::ensure_started(state.clone());
        ControlPlaneWorkers::ensure_started(state.clone());
        assert!(
            usage_loop_started(&state),
            "repeat spawn_usage_sync_loop / ensure_started calls must keep the once-per-CoreState flag"
        );

        let first =
            crate::gateway::listener::GatewayLifecycle::bind(state.clone(), loopback_ephemeral())
                .await
                .unwrap();
        let second =
            crate::gateway::listener::GatewayLifecycle::bind(state.clone(), loopback_ephemeral())
                .await
                .unwrap();
        assert!(
            usage_loop_started(&state),
            "extra listener bind on the same CoreState must not reset the usage loop"
        );
        crate::gateway::stop_gateway(first);
        crate::gateway::stop_gateway(second);
        assert!(
            usage_loop_started(&state),
            "listener stop must not clear the process-level usage worker"
        );
        let _ = state.config();

        let rebound =
            crate::gateway::listener::GatewayLifecycle::bind(state.clone(), loopback_ephemeral())
                .await
                .unwrap();
        assert!(
            usage_loop_started(&state),
            "CoreState must remain usable for another bind after stop"
        );
        crate::gateway::stop_gateway(rebound);

        drop(state);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn start_gateway_on_starts_usage_loop_and_stop_does_not_clear_it() {
        let (dir, state) = test_state("compat-start-loop");
        assert!(!usage_loop_started(&state));
        let handle = crate::gateway::start_gateway_on(state.clone(), loopback_ephemeral())
            .await
            .unwrap();
        assert!(
            usage_loop_started(&state),
            "public start_gateway_on must still start the process-level usage worker"
        );
        crate::gateway::stop_gateway(handle);
        assert!(
            usage_loop_started(&state),
            "stop_gateway is listener-only and must not clear the usage worker"
        );
        let _ = state.config();

        let restarted = crate::gateway::start_gateway_on(state.clone(), loopback_ephemeral())
            .await
            .unwrap();
        assert!(
            usage_loop_started(&state),
            "CoreState must remain usable for another public start after stop"
        );
        crate::gateway::stop_gateway(restarted);

        drop(state);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn rebind_does_not_start_usage_loop() {
        let (dir, state) = test_state("rebind-no-loop");
        assert!(!usage_loop_started(&state));

        let first =
            crate::gateway::listener::GatewayLifecycle::bind(state.clone(), loopback_ephemeral())
                .await
                .unwrap();
        let first_port = first.port;
        *state.gateway.lock() = Some(first);
        assert!(
            !usage_loop_started(&state),
            "storing a bound listener must not start the usage worker"
        );

        let same = crate::gateway::listener::GatewayLifecycle::rebind(
            state.clone(),
            std::net::SocketAddr::from(([127, 0, 0, 1], first_port)),
        )
        .await
        .unwrap();
        assert_eq!(same, first_port);
        assert!(
            !usage_loop_started(&state),
            "same-port rebind must not start the process-level usage worker"
        );

        let moved =
            crate::gateway::listener::GatewayLifecycle::rebind(state.clone(), loopback_ephemeral())
                .await
                .unwrap();
        assert_ne!(moved, first_port);
        assert!(
            !usage_loop_started(&state),
            "new-port rebind must not start the process-level usage worker"
        );

        let handle = state.gateway.lock().take();
        if let Some(handle) = handle {
            crate::gateway::listener::GatewayLifecycle::stop_and_wait(handle).await;
        }
        assert!(
            !usage_loop_started(&state),
            "stop_and_wait must not start the usage worker"
        );

        drop(state);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn rebind_keeps_started_usage_loop_without_duplicate() {
        let (dir, state) = test_state("rebind-keep-loop");
        assert!(!usage_loop_started(&state));
        let handle = crate::gateway::start_gateway_on(state.clone(), loopback_ephemeral())
            .await
            .unwrap();
        let first_port = handle.port;
        *state.gateway.lock() = Some(handle);
        assert!(
            usage_loop_started(&state),
            "public start_gateway_on must start the process-level usage worker"
        );
        ControlPlaneWorkers::ensure_started(state.clone());
        ControlPlaneWorkers::ensure_started(state.clone());
        assert!(
            usage_loop_started(&state),
            "ensure_started after start must stay once-per-CoreState"
        );

        let same = crate::gateway::listener::GatewayLifecycle::rebind(
            state.clone(),
            std::net::SocketAddr::from(([127, 0, 0, 1], first_port)),
        )
        .await
        .unwrap();
        assert_eq!(same, first_port);
        assert!(
            usage_loop_started(&state),
            "same-port rebind must not clear the process-level usage worker"
        );
        ControlPlaneWorkers::ensure_started(state.clone());
        assert!(
            usage_loop_started(&state),
            "ensure_started after same-port rebind must not spawn a second worker"
        );

        let moved =
            crate::gateway::listener::GatewayLifecycle::rebind(state.clone(), loopback_ephemeral())
                .await
                .unwrap();
        assert_ne!(moved, first_port);
        assert!(
            usage_loop_started(&state),
            "new-port rebind must not clear or restart the process-level usage worker"
        );
        ControlPlaneWorkers::ensure_started(state.clone());
        crate::gateway::stop_gateway(state.gateway.lock().take().unwrap());
        assert!(
            usage_loop_started(&state),
            "listener stop after rebind must not clear the usage worker"
        );

        drop(state);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn failure_backoff_ladder_caps_at_six_hours() {
        assert_eq!(failure_backoff(1), Duration::minutes(5));
        assert_eq!(failure_backoff(2), Duration::minutes(15));
        assert_eq!(failure_backoff(3), Duration::hours(1));
        assert_eq!(failure_backoff(4), Duration::hours(6));
        assert_eq!(failure_backoff(99), Duration::hours(6));
    }

    #[test]
    fn active_and_inactive_cadence() {
        assert_eq!(cadence_for(true), ACTIVE_CADENCE);
        assert_eq!(cadence_for(false), INACTIVE_CADENCE);
    }

    #[test]
    fn manual_throttle_exposes_next_allowed() {
        let now = fixed("2026-08-18T12:00:00Z");
        assert_eq!(manual_next_allowed_at(None, now), None);
        assert_eq!(
            manual_next_allowed_at(Some(now - Duration::seconds(16)), now),
            None
        );
        assert_eq!(
            manual_next_allowed_at(Some(now - Duration::seconds(10)), now),
            Some(now + Duration::seconds(5))
        );
    }

    #[test]
    fn success_next_respects_cadence_and_earliest_reset() {
        let now = fixed("2026-08-18T12:00:00Z");
        let next = compute_next_after_success(now, true, 10, 0.0);
        assert_eq!(next, now + Duration::minutes(10));
        // Reset farther than hourly cadence → cadence wins for active accounts.
        let next = compute_next_after_success(now, true, 500, 0.0);
        assert_eq!(next, now + ACTIVE_CADENCE);
        // Reset farther than daily cadence → cadence wins for inactive accounts.
        let next = compute_next_after_success(now, false, 60 * 30, 0.0);
        assert_eq!(next, now + INACTIVE_CADENCE);
        // Reset sooner than daily cadence still schedules around the reset.
        let next = compute_next_after_success(now, false, 500, 0.0);
        assert_eq!(next, now + Duration::minutes(500));
        // A stale official reset must not create a near-immediate polling loop.
        let next = compute_next_after_success(now, true, 0, 1.0);
        assert_eq!(next, now + ACTIVE_CADENCE);
        let next = compute_next_after_success(now, false, -1, 1.0);
        assert_eq!(next, now + INACTIVE_CADENCE);
    }

    #[test]
    fn expedite_guard_is_fifteen_minutes() {
        let now = fixed("2026-08-18T12:00:00Z");
        assert!(!should_run_expedited(79.9, None, None, None, now));
        assert!(should_run_expedited(80.0, None, None, None, now));
        assert!(should_run_expedited(95.0, None, None, None, now));
        assert!(!should_run_expedited(
            95.0,
            Some(now - Duration::minutes(14)),
            None,
            None,
            now
        ));
        assert!(should_run_expedited(
            95.0,
            Some(now - Duration::minutes(15)),
            None,
            None,
            now
        ));
        // Any recent official attempt/success also anchors the 15m guard.
        assert!(!should_run_expedited(
            95.0,
            None,
            Some(now - Duration::minutes(5)),
            None,
            now
        ));
        assert!(!should_run_expedited(
            95.0,
            None,
            None,
            Some(now - Duration::minutes(1)),
            now
        ));
    }

    #[test]
    fn inference_429_delay_stays_within_one_to_two_minutes() {
        let now = fixed("2026-08-18T12:00:00Z");
        assert_eq!(
            compute_inference_429_delay(now, 0.0),
            now + INFERENCE_429_DELAY_MIN
        );
        assert_eq!(
            compute_inference_429_delay(now, 1.0),
            now + INFERENCE_429_DELAY_MAX
        );
    }

    #[test]
    fn auto_sync_excludes_disabled_non_ready_empty_key() {
        let provider = crate::provider::OPENCODE_PROVIDER_ID;
        let offering = crate::provider::GO_OFFERING_ID;
        assert!(!provider_account_is_auto_sync_candidate(
            provider, offering, false, true, true
        ));
        assert!(!provider_account_is_auto_sync_candidate(
            provider, offering, true, false, true
        ));
        assert!(!provider_account_is_auto_sync_candidate(
            provider, offering, true, true, false
        ));
        assert!(provider_account_is_auto_sync_candidate(
            provider, offering, true, true, true
        ));
        assert!(!provider_account_is_auto_sync_candidate(
            crate::provider::COMMAND_CODE_PROVIDER_ID,
            crate::provider::GOAT_OFFERING_ID,
            true,
            true,
            true,
        ));
        assert!(!provider_account_is_auto_sync_candidate(
            crate::provider::OPENCODE_ZEN_FREE_PROVIDER_ID,
            crate::provider::ANONYMOUS_FREE_OFFERING_ID,
            true,
            true,
            false,
        ));
    }

    fn temp_dir(label: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("ocg-usage-sync-{}-{}", label, uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn test_state(label: &str) -> (PathBuf, CoreState) {
        let dir = temp_dir(label);
        let cipher: Arc<dyn KeyCipher + Send + Sync> =
            Arc::new(StaticKeyCipher::new("usage-sync-test"));
        let db = Database::open(dir.clone()).unwrap();
        let state = Arc::new(CoreStateInner::new(db, dir.clone(), cipher).unwrap());
        (dir, state)
    }

    fn ready_account(state: &CoreState, id: &str, key: &str) -> Account {
        Account {
            id: id.to_string(),
            provider_id: crate::provider::default_provider_id(),
            offering_id: crate::provider::default_offering_id(),
            credential_kind: crate::provider::default_credential_kind(),
            quota_scope: crate::provider::default_quota_scope(),
            name: id.to_string(),
            username: None,
            password_cipher: None,
            key_cipher: state.encrypt_key(key).unwrap(),
            enabled: true,
            account_type: AccountType::Key,
            setup_step: AccountSetupStep::Ready,
            referral_code: None,
            purchase_date: "2026-08-01".to_string(),
            expires_on: "2026-09-01".to_string(),
            cooldown_until: None,
            cooldown_generic_until: None,
            cooldown_5h_until: None,
            cooldown_week_until: None,
            cooldown_month_until: None,
            cooldown_free_until: None,
            last_error: None,
            auth_error: None,
            notes: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }
    }

    fn sample_snapshot() -> GoUsageSnapshot {
        GoUsageSnapshot {
            rolling_status: crate::go_usage::GoUsageWindowStatus::RateLimited,
            weekly_status: crate::go_usage::GoUsageWindowStatus::Ok,
            monthly_status: crate::go_usage::GoUsageWindowStatus::Ok,
            rolling_percent: 50.0,
            weekly_percent: 20.0,
            monthly_percent: 10.0,
            rolling_resets_in_minutes: 180,
            weekly_resets_in_minutes: 1_440,
            earliest_resets_in_minutes: 180,
        }
    }

    #[tokio::test]
    async fn manual_throttle_and_dedupe_share_one_upstream_call() {
        let (dir, state) = test_state("throttle-dedupe");
        let account = ready_account(&state, "acc-1", "sk-acc-1");
        state.db.lock().create_account(&account).unwrap();

        let now = fixed("2026-08-18T12:00:00Z");
        state.usage_sync.set_clock_for_test(move || now);
        state.usage_sync.set_jitter_for_test(|| 0.0);
        let calls = Arc::new(AtomicUsize::new(0));
        let calls_fetch = calls.clone();
        let release = Arc::new(tokio::sync::Notify::new());
        let entered = Arc::new(tokio::sync::Notify::new());
        let release_fetch = release.clone();
        let entered_fetch = entered.clone();
        state.usage_sync.set_fetch_for_test(move |_cfg, _key| {
            calls_fetch.fetch_add(1, AtomicOrdering::SeqCst);
            let release_fetch = release_fetch.clone();
            let entered_fetch = entered_fetch.clone();
            let snapshot = sample_snapshot();
            Box::pin(async move {
                entered_fetch.notify_one();
                release_fetch.notified().await;
                Ok(snapshot)
            })
        });

        let state_a = state.clone();
        let state_b = state.clone();
        let a = tokio::spawn(async move {
            refresh_official_usage(&state_a, "acc-1", UsageSyncTrigger::Manual).await
        });
        entered.notified().await;
        let b = tokio::spawn(async move {
            refresh_official_usage(&state_b, "acc-1", UsageSyncTrigger::Manual).await
        });
        // Let the second caller attach to the in-flight future before release.
        tokio::time::sleep(StdDuration::from_millis(20)).await;
        release.notify_waiters();
        let ra = a.await.unwrap().unwrap();
        let rb = b.await.unwrap().unwrap();
        assert_eq!(ra.last_success_at, rb.last_success_at);
        assert_eq!(calls.load(AtomicOrdering::SeqCst), 1);

        let throttled = refresh_official_usage(&state, "acc-1", UsageSyncTrigger::Manual).await;
        match throttled {
            Err(OfficialUsageRefreshError::Throttled {
                retry_after_secs, ..
            }) => assert!(retry_after_secs <= 60),
            other => panic!("expected throttle, got {other:?}"),
        }

        state.usage_sync.clear_test_seams();
        drop(state);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn stale_guarded_leader_blocks_success_for_all_deduped_waiters_without_mutation() {
        let state = FakeUsageHost::new();
        state.insert_ready_go("guarded", "sk-guarded");
        let before = state.inner.sync.lock().get("guarded").cloned().unwrap();
        let authorization = state.guarded_authorization();
        let calls = Arc::new(AtomicUsize::new(0));
        let entered_fetch = Arc::new(Notify::new());
        let release_fetch = Arc::new(Notify::new());
        let calls_hook = calls.clone();
        let entered_fetch_hook = entered_fetch.clone();
        let release_fetch_hook = release_fetch.clone();
        state.inner.runtime.set_fetch_for_test(move |_cfg, _key| {
            calls_hook.fetch_add(1, AtomicOrdering::SeqCst);
            let entered_fetch = entered_fetch_hook.clone();
            let release_fetch = release_fetch_hook.clone();
            Box::pin(async move {
                entered_fetch.notify_one();
                release_fetch.notified().await;
                Ok(sample_snapshot())
            })
        });

        let cleanup_arrived = Arc::new(Notify::new());
        let cleanup_release = Arc::new(Notify::new());
        let cleanup_arrived_hook = cleanup_arrived.clone();
        let cleanup_release_hook = cleanup_release.clone();
        state
            .inner
            .runtime
            .set_before_inflight_cleanup_for_test(move || {
                let cleanup_arrived = cleanup_arrived_hook.clone();
                let cleanup_release = cleanup_release_hook.clone();
                Box::pin(async move {
                    cleanup_arrived.notify_one();
                    cleanup_release.notified().await;
                })
            });

        let guarded_state = state.clone();
        let guarded = tokio::spawn(async move {
            refresh_official_usage_with_authorization(
                &guarded_state,
                "guarded",
                UsageSyncTrigger::Manual,
                authorization,
            )
            .await
            .result
        });
        entered_fetch.notified().await;
        state.bump_settings_revision();
        release_fetch.notify_one();
        cleanup_arrived.notified().await;

        // While the stale V3-owned result remains in the in-flight map, an
        // unconditional V2 waiter must observe the same rejected result rather
        // than converting that stale leader into an authorized writer.
        let v2_state = state.clone();
        let v2_follower = tokio::spawn(async move {
            refresh_official_usage(&v2_state, "guarded", UsageSyncTrigger::Manual).await
        });
        cleanup_arrived.notified().await;
        cleanup_release.notify_waiters();

        for result in [guarded.await.unwrap(), v2_follower.await.unwrap()] {
            assert!(
                matches!(
                    result,
                    Err(OfficialUsageRefreshError::CommitAuthorizationRejected)
                ),
                "stale guarded leader must reject every waiter: {result:?}"
            );
        }
        assert_eq!(calls.load(AtomicOrdering::SeqCst), 1);
        assert_eq!(
            state.inner.sync.lock().get("guarded").cloned().unwrap(),
            before,
            "stale success must not write calibration sync metadata"
        );
    }

    #[tokio::test]
    async fn stale_guarded_failure_does_not_write_attempt_or_backoff_metadata() {
        let state = FakeUsageHost::new();
        state.insert_ready_go("failure", "sk-failure");
        let before = state.inner.sync.lock().get("failure").cloned().unwrap();
        let authorization = state.guarded_authorization();
        let entered = Arc::new(Notify::new());
        let release = Arc::new(Notify::new());
        let entered_hook = entered.clone();
        let release_hook = release.clone();
        state.inner.runtime.set_fetch_for_test(move |_cfg, _key| {
            let entered = entered_hook.clone();
            let release = release_hook.clone();
            Box::pin(async move {
                entered.notify_one();
                release.notified().await;
                Err(GoUsageError::Timeout)
            })
        });

        let refresh_state = state.clone();
        let refresh = tokio::spawn(async move {
            refresh_official_usage_with_authorization(
                &refresh_state,
                "failure",
                UsageSyncTrigger::Manual,
                authorization,
            )
            .await
            .result
        });
        entered.notified().await;
        state.bump_settings_revision();
        release.notify_one();

        let result = refresh.await.unwrap();
        assert!(
            matches!(
                result,
                Err(OfficialUsageRefreshError::CommitAuthorizationRejected)
            ),
            "stale upstream failure must surface authorization conflict: {result:?}"
        );
        assert_eq!(
            state.inner.sync.lock().get("failure").cloned().unwrap(),
            before,
            "stale failure must not write last_attempt/failure_streak/backoff"
        );
    }

    #[tokio::test]
    async fn stale_guarded_follower_cannot_veto_background_owned_inflight_result() {
        let state = FakeUsageHost::new();
        state.insert_ready_go("background", "sk-background");
        let stale_follower_authorization = state.guarded_authorization();
        state
            .inner
            .runtime
            .set_fetch_for_test(move |_cfg, _key| Box::pin(async { Ok(sample_snapshot()) }));

        let cleanup_arrived = Arc::new(Notify::new());
        let cleanup_release = Arc::new(Notify::new());
        let cleanup_arrived_hook = cleanup_arrived.clone();
        let cleanup_release_hook = cleanup_release.clone();
        state
            .inner
            .runtime
            .set_before_inflight_cleanup_for_test(move || {
                let cleanup_arrived = cleanup_arrived_hook.clone();
                let cleanup_release = cleanup_release_hook.clone();
                Box::pin(async move {
                    cleanup_arrived.notify_one();
                    cleanup_release.notified().await;
                })
            });

        let background_state = state.clone();
        let background = tokio::spawn(async move {
            refresh_official_usage(&background_state, "background", UsageSyncTrigger::Scheduled)
                .await
        });
        cleanup_arrived.notified().await;
        state.bump_settings_revision();

        // The background leader has already committed under unconditional V2
        // semantics but remains in-flight until cleanup. A stale guarded
        // follower may observe that result; its unrelated token cannot veto it.
        let follower_state = state.clone();
        let follower = tokio::spawn(async move {
            refresh_official_usage_with_authorization(
                &follower_state,
                "background",
                UsageSyncTrigger::Scheduled,
                stale_follower_authorization,
            )
            .await
            .result
        });
        cleanup_arrived.notified().await;
        cleanup_release.notify_waiters();

        background.await.unwrap().unwrap();
        follower.await.unwrap().unwrap();
        let sync = state.inner.sync.lock().get("background").cloned().unwrap();
        assert!(sync.last_success_at.is_some());
        assert_eq!(sync.failure_streak, 0);
    }

    #[tokio::test]
    async fn failure_preserves_last_success_and_calibration() {
        let (dir, state) = test_state("failure-preserve");
        let account = ready_account(&state, "acc-2", "sk-acc-2");
        state.db.lock().create_account(&account).unwrap();
        let now = fixed("2026-08-18T12:00:00Z");
        state.usage_sync.set_clock_for_test(move || now);
        state.usage_sync.set_jitter_for_test(|| 0.0);
        state.usage_sync.set_fetch_for_test(move |_cfg, _key| {
            let snapshot = sample_snapshot();
            Box::pin(async move { Ok(snapshot) })
        });
        refresh_official_usage(&state, "acc-2", UsageSyncTrigger::Manual)
            .await
            .unwrap();
        let before = state
            .db
            .lock()
            .account_usage_with_limits("acc-2", &state.pricing_snapshot().limits)
            .unwrap();
        let success_at = state
            .db
            .lock()
            .account_usage_sync_state("acc-2")
            .unwrap()
            .unwrap()
            .last_success_at;

        // Advance clock beyond manual throttle.
        let later = now + Duration::minutes(2);
        state.usage_sync.set_clock_for_test(move || later);
        state.usage_sync.set_fetch_for_test(move |_cfg, _key| {
            Box::pin(async move { Err(GoUsageError::Timeout) })
        });
        let err = refresh_official_usage(&state, "acc-2", UsageSyncTrigger::Manual)
            .await
            .unwrap_err();
        assert!(matches!(
            err,
            OfficialUsageRefreshError::Upstream(GoUsageError::Timeout)
        ));

        let after = state
            .db
            .lock()
            .account_usage_with_limits("acc-2", &state.pricing_snapshot().limits)
            .unwrap();
        assert_eq!(after.window_5h, before.window_5h);
        assert_eq!(after.window_week, before.window_week);
        assert_eq!(after.window_month, before.window_month);
        let sync = state
            .db
            .lock()
            .account_usage_sync_state("acc-2")
            .unwrap()
            .unwrap();
        assert_eq!(sync.last_success_at, success_at);
        assert_eq!(sync.failure_streak, 1);
        assert_eq!(sync.next_eligible_at, Some(later + failure_backoff(1)));

        drop(state);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[tokio::test]
    async fn key_cas_leaves_windows_unchanged_when_account_changes() {
        let (dir, state) = test_state("cas");
        let account = ready_account(&state, "acc-3", "sk-acc-3");
        state.db.lock().create_account(&account).unwrap();
        let limits = state.pricing_snapshot().limits.clone();
        {
            let db = state.db.lock();
            db.calibrate_account_usage_snapshot(
                "acc-3",
                &AccountUsageCalibrationSnapshot {
                    rolling_percent: 11.0,
                    weekly_percent: 22.0,
                    monthly_percent: 33.0,
                    rolling_resets_in_minutes: 100,
                    weekly_resets_in_minutes: 200,
                },
                &limits,
            )
            .unwrap();
        }
        let before = state
            .db
            .lock()
            .account_usage_with_limits("acc-3", &limits)
            .unwrap();

        let now = fixed("2026-08-18T12:00:00Z");
        state.usage_sync.set_clock_for_test(move || now);
        state.usage_sync.set_jitter_for_test(|| 0.0);
        let state_for_fetch = state.clone();
        state.usage_sync.set_fetch_for_test(move |_cfg, _key| {
            // Swap the key while the network call is "in flight".
            let rotated = state_for_fetch.encrypt_key("sk-rotated").unwrap();
            state_for_fetch
                .db
                .lock()
                .update_account(
                    "acc-3",
                    &crate::models::AccountUpdate {
                        name: None,
                        username: None,
                        password: None,
                        key: Some("sk-rotated".to_string()),
                        enabled: None,
                        referral_code: None,
                        purchase_date: None,
                        notes: None,
                    },
                    Some(&rotated),
                    None,
                )
                .unwrap();
            let snapshot = sample_snapshot();
            Box::pin(async move { Ok(snapshot) })
        });

        let err = refresh_official_usage(&state, "acc-3", UsageSyncTrigger::Manual)
            .await
            .unwrap_err();
        assert!(matches!(err, OfficialUsageRefreshError::Conflict(_)));
        let after = state
            .db
            .lock()
            .account_usage_with_limits("acc-3", &limits)
            .unwrap();
        assert_eq!(after.window_5h, before.window_5h);
        assert_eq!(after.window_week, before.window_week);
        assert_eq!(after.window_month, before.window_month);
        let sync = state
            .db
            .lock()
            .account_usage_sync_state("acc-3")
            .unwrap()
            .unwrap();
        assert_eq!(sync.failure_streak, 1);
        assert_eq!(sync.next_eligible_at, Some(now + failure_backoff(1)));
        assert_eq!(sync.last_attempt_at, Some(now));
        // Manual 15s throttle still exposed after CAS conflict.
        assert_eq!(
            manual_next_allowed_at(sync.last_attempt_at, now),
            Some(now + MANUAL_THROTTLE)
        );

        state.usage_sync.clear_test_seams();
        drop(state);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn official_rate_limited_status_does_not_write_cooldown() {
        let (dir, state) = test_state("official-rate-limited");
        let account = ready_account(&state, "acc-4", "sk-acc-4");
        state.db.lock().create_account(&account).unwrap();
        let now = fixed("2026-08-18T12:00:00Z");
        state.usage_sync.set_clock_for_test(move || now);
        state.usage_sync.set_jitter_for_test(|| 0.0);
        state.usage_sync.set_fetch_for_test(move |_cfg, _key| {
            let snapshot = sample_snapshot();
            Box::pin(async move { Ok(snapshot) })
        });
        refresh_official_usage(&state, "acc-4", UsageSyncTrigger::Scheduled)
            .await
            .unwrap();
        let stored = state.db.lock().get_account("acc-4").unwrap().unwrap();
        assert!(stored.cooldown_until.is_none());
        assert!(stored.cooldown_5h_until.is_none());
        assert!(stored.cooldown_week_until.is_none());
        assert!(stored.cooldown_month_until.is_none());
        assert!(stored.cooldown_generic_until.is_none());
        assert!(stored.cooldown_free_until.is_none());

        drop(state);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[tokio::test]
    async fn schedule_after_inference_429_only_pulls_next_eligible() {
        let (dir, state) = test_state("infer-429");
        let account = ready_account(&state, "acc-5", "sk-acc-5");
        state.db.lock().create_account(&account).unwrap();
        let now = fixed("2026-08-18T12:00:00Z");
        state.usage_sync.set_clock_for_test(move || now);
        state.usage_sync.set_jitter_for_test(|| 0.0);
        // Far-future cadence baseline.
        state
            .db
            .lock()
            .record_account_usage_sync_success(
                "acc-5",
                now - Duration::hours(1),
                now + Duration::hours(20),
                false,
            )
            .unwrap();
        schedule_after_inference_429(&state, "acc-5");
        let sync = state
            .db
            .lock()
            .account_usage_sync_state("acc-5")
            .unwrap()
            .unwrap();
        assert_eq!(sync.next_eligible_at, Some(now + INFERENCE_429_DELAY_MIN));
        let stored = state.db.lock().get_account("acc-5").unwrap().unwrap();
        assert!(stored.cooldown_until.is_none());

        drop(state);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn sync_metadata_survives_reopen() {
        let dir = temp_dir("persist");
        let cipher: Arc<dyn KeyCipher + Send + Sync> =
            Arc::new(StaticKeyCipher::new("usage-sync-persist"));
        let now = fixed("2026-08-18T12:00:00Z");
        {
            let db = Database::open(dir.clone()).unwrap();
            let state = Arc::new(CoreStateInner::new(db, dir.clone(), cipher.clone()).unwrap());
            let account = ready_account(&state, "persist", "sk-persist");
            state.db.lock().create_account(&account).unwrap();
            state
                .db
                .lock()
                .record_account_usage_sync_success("persist", now, now + ACTIVE_CADENCE, true)
                .unwrap();
            drop(state);
        }
        {
            let db = Database::open(dir.clone()).unwrap();
            let sync = db.account_usage_sync_state("persist").unwrap().unwrap();
            assert_eq!(sync.last_success_at, Some(now));
            assert_eq!(sync.next_eligible_at, Some(now + ACTIVE_CADENCE));
            assert_eq!(sync.failure_streak, 0);
            assert_eq!(sync.last_expedited_at, Some(now));
            // Defaults after migration: missing rows still open.
            assert_eq!(
                db.schema_version().unwrap(),
                crate::db::CURRENT_SCHEMA_VERSION
            );
        }
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn eligibility_lists_active_hourly_vs_inactive_daily_and_exclusions() {
        let dir = temp_dir("eligibility");
        let cipher: Arc<dyn KeyCipher + Send + Sync> =
            Arc::new(StaticKeyCipher::new("usage-sync-elig"));
        let db = Database::open(dir.clone()).unwrap();
        let state = Arc::new(CoreStateInner::new(db, dir.clone(), cipher).unwrap());
        let now = fixed("2026-08-18T12:00:00Z");

        let active = ready_account(&state, "active", "sk-active");
        let inactive = ready_account(&state, "inactive", "sk-inactive");
        let mut disabled = ready_account(&state, "disabled", "sk-disabled");
        disabled.enabled = false;
        let mut pending = ready_account(&state, "pending", "sk-pending");
        pending.setup_step = AccountSetupStep::Payment;
        pending.enabled = false;
        let mut empty = ready_account(&state, "empty", "sk-empty");
        empty.key_cipher.clear();

        {
            let db = state.db.lock();
            db.create_account(&active).unwrap();
            db.create_account(&inactive).unwrap();
            db.create_account(&disabled).unwrap();
            db.create_account(&pending).unwrap();
            db.create_account(&empty).unwrap();
            // Seed due times in the past so both ready accounts are refreshable.
            db.record_account_usage_sync_success(
                "active",
                now - Duration::hours(2),
                now - Duration::minutes(1),
                false,
            )
            .unwrap();
            db.record_account_usage_sync_success(
                "inactive",
                now - Duration::hours(30),
                now - Duration::minutes(1),
                false,
            )
            .unwrap();
            db.log_forward(&crate::models::ForwardLog {
                id: 0,
                timestamp: now - Duration::hours(1),
                model: "mimo-v2.5".into(),
                account_id: "active".into(),
                account_name: "active".into(),
                route_account_id: None,
                provider_id: None,
                offering_id: None,
                credential_account_id: None,
                client_key_id: None,
                client_key_name: None,
                status: "success".into(),
                http_status: Some(200),
                route: String::new(),
                prompt_tokens: 0,
                completion_tokens: 0,
                cached_tokens: 0,
                cache_creation_tokens: 0,
                cost: Some(1.0),
                raw_cost_usd: None,
                quota_debit: None,
                effective_paid_cost_usd: None,
                pricing_revision_id: None,
                quota_multiplier: None,
                local_adjustment_multiplier: None,
                service_tier: None,
                cost_state: "priced".into(),
                error_message: None,
                request_id: None,
                attempt: None,
                error_source: None,
                error_stage: None,
                duration_ms: None,
                diagnostic: None,
            })
            .unwrap();
        }

        let limits = state.pricing_snapshot().limits.clone();
        let candidates = list_auto_candidates(&state, now, &limits).unwrap();
        let ids: Vec<_> = candidates.iter().map(|c| c.account_id.as_str()).collect();
        assert!(ids.contains(&"active"));
        assert!(ids.contains(&"inactive"));
        assert!(!ids.contains(&"disabled"));
        assert!(!ids.contains(&"pending"));
        assert!(!ids.contains(&"empty"));

        let active_next = compute_next_after_success(now, true, 10_000, 0.0);
        let inactive_next = compute_next_after_success(now, false, 10_000, 0.0);
        assert_eq!(active_next, now + ACTIVE_CADENCE);
        assert_eq!(inactive_next, now + INACTIVE_CADENCE);

        drop(state);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn unused_app_config_type_keeps_fetch_signature_honest() {
        let _ = AppConfig::default();
    }

    fn seed_high_usage(state: &CoreState, account_id: &str) {
        let limits = state.pricing_snapshot().limits.clone();
        state
            .db
            .lock()
            .calibrate_account_usage_snapshot(
                account_id,
                &AccountUsageCalibrationSnapshot {
                    rolling_percent: 90.0,
                    weekly_percent: 20.0,
                    monthly_percent: 10.0,
                    rolling_resets_in_minutes: 100,
                    weekly_resets_in_minutes: 1_000,
                },
                &limits,
            )
            .unwrap();
    }

    #[tokio::test]
    async fn failed_expedited_sync_stays_in_backoff_across_scheduler_scans() {
        let (dir, state) = test_state("expedite-backoff");
        let account = ready_account(&state, "hi", "sk-hi");
        state.db.lock().create_account(&account).unwrap();
        let now = fixed("2026-08-18T12:00:00Z");
        state.usage_sync.set_clock_for_test(move || now);
        state.usage_sync.set_jitter_for_test(|| 0.0);
        seed_high_usage(&state, "hi");
        state
            .db
            .lock()
            .record_account_usage_sync_success(
                "hi",
                now - Duration::hours(2),
                now + Duration::hours(20),
                false,
            )
            .unwrap();

        state.usage_sync.set_fetch_for_test(move |_cfg, _key| {
            Box::pin(async move { Err(GoUsageError::Timeout) })
        });
        // First scan pulls expedite and fails into 5m backoff.
        let limits = state.pricing_snapshot().limits.clone();
        let candidates = list_auto_candidates(&state, now, &limits).unwrap();
        assert!(candidates.iter().any(|c| {
            c.account_id == "hi"
                && matches!(
                    c.action,
                    CandidateAction::Refresh {
                        trigger: UsageSyncTrigger::Expedited
                    }
                )
        }));
        let _ = refresh_official_usage(&state, "hi", UsageSyncTrigger::Expedited).await;
        let sync = state
            .db
            .lock()
            .account_usage_sync_state("hi")
            .unwrap()
            .unwrap();
        assert_eq!(sync.failure_streak, 1);
        assert_eq!(sync.next_eligible_at, Some(now + failure_backoff(1)));

        // ~30s later the scheduler must not re-select despite still-high usage.
        let soon = now + Duration::seconds(30);
        let candidates = list_auto_candidates(&state, soon, &limits).unwrap();
        assert!(!candidates.iter().any(|c| c.account_id == "hi"));

        // Repeated failure advances the ladder.
        let after_backoff = now + failure_backoff(1);
        state.usage_sync.set_clock_for_test(move || after_backoff);
        let _ = refresh_official_usage(&state, "hi", UsageSyncTrigger::Scheduled).await;
        let sync = state
            .db
            .lock()
            .account_usage_sync_state("hi")
            .unwrap()
            .unwrap();
        assert_eq!(sync.failure_streak, 2);
        assert_eq!(
            sync.next_eligible_at,
            Some(after_backoff + failure_backoff(2))
        );

        state.usage_sync.clear_test_seams();
        drop(state);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn successful_high_usage_retry_is_not_immediately_re_expedited() {
        let (dir, state) = test_state("no-reexpedite");
        let account = ready_account(&state, "hi2", "sk-hi2");
        state.db.lock().create_account(&account).unwrap();
        // Usage-window reads use the production clock, so keep this integration
        // test's injected sync clock aligned instead of crossing a real reset.
        let now = Utc::now();
        state.usage_sync.set_clock_for_test(move || now);
        state.usage_sync.set_jitter_for_test(|| 0.0);
        seed_high_usage(&state, "hi2");
        // Prior success is old; a recent failure left the account due.
        state
            .db
            .lock()
            .record_account_usage_sync_success(
                "hi2",
                now - Duration::hours(2),
                now + Duration::hours(20),
                false,
            )
            .unwrap();
        state
            .db
            .lock()
            .record_account_usage_sync_failure(
                "hi2",
                now - Duration::minutes(6),
                1,
                now - Duration::minutes(1),
            )
            .unwrap();

        let calls = Arc::new(AtomicUsize::new(0));
        let calls_fetch = calls.clone();
        state.usage_sync.set_fetch_for_test(move |_cfg, _key| {
            calls_fetch.fetch_add(1, AtomicOrdering::SeqCst);
            let mut snapshot = sample_snapshot();
            snapshot.rolling_percent = 90.0;
            Box::pin(async move { Ok(snapshot) })
        });
        refresh_official_usage(&state, "hi2", UsageSyncTrigger::Scheduled)
            .await
            .unwrap();
        assert_eq!(calls.load(AtomicOrdering::SeqCst), 1);

        let limits = state.pricing_snapshot().limits.clone();
        let soon = now + Duration::minutes(1);
        let candidates = list_auto_candidates(&state, soon, &limits).unwrap();
        assert!(
            !candidates.iter().any(|c| c.account_id == "hi2"),
            "successful retry at high usage must not be re-expedited inside 15m"
        );

        // After the 15m guard, high usage may expedite even though cadence/reset
        // next_eligible is still in the future (sample earliest reset is 180m).
        let later = now + EXPEDITE_GUARD;
        let candidates = list_auto_candidates(&state, later, &limits).unwrap();
        assert!(candidates.iter().any(|c| {
            c.account_id == "hi2"
                && matches!(
                    c.action,
                    CandidateAction::Refresh {
                        trigger: UsageSyncTrigger::Expedited
                    }
                )
        }));

        state.usage_sync.clear_test_seams();
        drop(state);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn take_inflight_if_generation_ignores_stale_waiters() {
        let mut map = HashMap::new();
        let success = OfficialUsageRefreshSuccess {
            usage: UsageWindow {
                account_id: "a".into(),
                window_5h: 0.0,
                window_week: 0.0,
                window_month: 0.0,
                resets_in_5h: None,
                resets_in_week: None,
                resets_in_month: None,
            },
            source: "official_go_usage",
            last_success_at: fixed("2026-08-18T12:00:00Z").to_rfc3339(),
            next_allowed_at: fixed("2026-08-18T12:01:00Z").to_rfc3339(),
        };
        let finished = async move { Arc::new(Ok(success) as RefreshResult) }
            .boxed()
            .shared();
        map.insert(
            "a".into(),
            InflightEntry {
                generation: 1,
                future: finished.clone(),
                authorization: UsageSyncCommitAuthorization::Unconditional,
            },
        );
        assert!(take_inflight_if_generation(&mut map, "a", 1));
        map.insert(
            "a".into(),
            InflightEntry {
                generation: 2,
                future: finished,
                authorization: UsageSyncCommitAuthorization::Unconditional,
            },
        );
        assert!(!take_inflight_if_generation(&mut map, "a", 1));
        assert_eq!(map.get("a").map(|e| e.generation), Some(2));
        assert!(take_inflight_if_generation(&mut map, "a", 2));
        assert!(map.is_empty());
    }

    #[tokio::test]
    async fn stale_waiter_does_not_drop_newer_inflight_generation() {
        let (dir, state) = test_state("inflight-gen");
        let account = ready_account(&state, "gen", "sk-gen");
        state.db.lock().create_account(&account).unwrap();
        let now = fixed("2026-08-18T12:00:00Z");
        state.usage_sync.set_clock_for_test(move || now);
        state.usage_sync.set_jitter_for_test(|| 0.0);

        let calls = Arc::new(AtomicUsize::new(0));
        let calls_fetch = calls.clone();
        let release_f1 = Arc::new(Notify::new());
        let entered_f1 = Arc::new(Notify::new());
        let release_f2 = Arc::new(Notify::new());
        let entered_f2 = Arc::new(Notify::new());
        let release_f1_fetch = release_f1.clone();
        let entered_f1_fetch = entered_f1.clone();
        let release_f2_fetch = release_f2.clone();
        let entered_f2_fetch = entered_f2.clone();
        state.usage_sync.set_fetch_for_test(move |_cfg, _key| {
            let n = calls_fetch.fetch_add(1, AtomicOrdering::SeqCst);
            let snapshot = sample_snapshot();
            if n == 0 {
                let release = release_f1_fetch.clone();
                let entered = entered_f1_fetch.clone();
                Box::pin(async move {
                    entered.notify_waiters();
                    release.notified().await;
                    Ok(snapshot)
                })
            } else {
                let release = release_f2_fetch.clone();
                let entered = entered_f2_fetch.clone();
                Box::pin(async move {
                    entered.notify_waiters();
                    release.notified().await;
                    Ok(snapshot)
                })
            }
        });

        let ticket = Arc::new(AtomicUsize::new(0));
        let hold_first = Arc::new(Notify::new());
        let hold_first_hook = hold_first.clone();
        let second_entered = Arc::new(Notify::new());
        let second_entered_hook = second_entered.clone();
        state
            .usage_sync
            .set_before_inflight_cleanup_for_test(move || {
                let ticket = ticket.clone();
                let hold_first = hold_first_hook.clone();
                let second_entered = second_entered_hook.clone();
                Box::pin(async move {
                    let which = ticket.fetch_add(1, AtomicOrdering::SeqCst);
                    if which == 0 {
                        hold_first.notified().await;
                    } else {
                        second_entered.notify_one();
                    }
                })
            });

        let state_w1 = state.clone();
        let state_w2 = state.clone();
        let w1 = tokio::spawn(async move {
            refresh_official_usage(&state_w1, "gen", UsageSyncTrigger::Manual).await
        });
        entered_f1.notified().await;
        let w2 = tokio::spawn(async move {
            refresh_official_usage(&state_w2, "gen", UsageSyncTrigger::Manual).await
        });
        tokio::time::sleep(StdDuration::from_millis(20)).await;
        release_f1.notify_waiters();
        // W2 reaches cleanup first (ticket 1) while W1 is held.
        second_entered.notified().await;

        // Start F2 while W1 still holds the stale cleanup.
        let later = now + Duration::minutes(2);
        state.usage_sync.set_clock_for_test(move || later);
        let state_w3 = state.clone();
        let w3 = tokio::spawn(async move {
            refresh_official_usage(&state_w3, "gen", UsageSyncTrigger::Manual).await
        });
        entered_f2.notified().await;
        assert_eq!(calls.load(AtomicOrdering::SeqCst), 2);

        // Stale W1 cleanup must not delete F2.
        hold_first.notify_one();
        let state_w4 = state.clone();
        let w4 = tokio::spawn(async move {
            refresh_official_usage(&state_w4, "gen", UsageSyncTrigger::Manual).await
        });
        tokio::time::sleep(StdDuration::from_millis(20)).await;
        assert_eq!(
            calls.load(AtomicOrdering::SeqCst),
            2,
            "F2 must remain deduped after stale waiter cleanup"
        );
        release_f2.notify_waiters();
        w1.await.unwrap().unwrap();
        w2.await.unwrap().unwrap();
        w3.await.unwrap().unwrap();
        w4.await.unwrap().unwrap();
        assert_eq!(calls.load(AtomicOrdering::SeqCst), 2);

        state.usage_sync.clear_test_seams();
        drop(state);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn decrypt_internal_failure_records_backoff_not_busy_loop() {
        let (dir, state) = test_state("decrypt-fail");
        let mut account = ready_account(&state, "bad", "sk-bad");
        // Store ciphertext the StaticKeyCipher cannot decrypt.
        account.key_cipher = "not-a-valid-cipher".into();
        state.db.lock().create_account(&account).unwrap();
        let now = fixed("2026-08-18T12:00:00Z");
        state.usage_sync.set_clock_for_test(move || now);
        state.usage_sync.set_jitter_for_test(|| 0.0);
        state
            .db
            .lock()
            .record_account_usage_sync_success(
                "bad",
                now - Duration::hours(2),
                now - Duration::minutes(1),
                false,
            )
            .unwrap();

        let err = refresh_official_usage(&state, "bad", UsageSyncTrigger::Scheduled)
            .await
            .unwrap_err();
        assert!(matches!(err, OfficialUsageRefreshError::Internal(_)));
        let sync = state
            .db
            .lock()
            .account_usage_sync_state("bad")
            .unwrap()
            .unwrap();
        assert_eq!(sync.failure_streak, 1);
        assert_eq!(sync.next_eligible_at, Some(now + failure_backoff(1)));
        assert!(sync.last_success_at.is_some());

        let limits = state.pricing_snapshot().limits.clone();
        let soon = now + Duration::seconds(30);
        let candidates = list_auto_candidates(&state, soon, &limits).unwrap();
        assert!(!candidates.iter().any(|c| c.account_id == "bad"));

        state.usage_sync.clear_test_seams();
        drop(state);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn inactive_to_active_pulls_hourly_without_overriding_failure_backoff() {
        let (dir, state) = test_state("inactive-active");
        let account = ready_account(&state, "wake", "sk-wake");
        state.db.lock().create_account(&account).unwrap();
        let now = fixed("2026-08-18T12:00:00Z");
        let last_success = now - Duration::hours(2);
        state
            .db
            .lock()
            .record_account_usage_sync_success(
                "wake",
                last_success,
                now + Duration::hours(20),
                false,
            )
            .unwrap();
        state
            .db
            .lock()
            .log_forward(&crate::models::ForwardLog {
                id: 0,
                timestamp: now - Duration::minutes(10),
                model: "mimo-v2.5".into(),
                account_id: "wake".into(),
                account_name: "wake".into(),
                route_account_id: None,
                provider_id: None,
                offering_id: None,
                credential_account_id: None,
                client_key_id: None,
                client_key_name: None,
                status: "success".into(),
                http_status: Some(200),
                route: String::new(),
                prompt_tokens: 0,
                completion_tokens: 0,
                cached_tokens: 0,
                cache_creation_tokens: 0,
                cost: Some(1.0),
                raw_cost_usd: None,
                quota_debit: None,
                effective_paid_cost_usd: None,
                pricing_revision_id: None,
                quota_multiplier: None,
                local_adjustment_multiplier: None,
                service_tier: None,
                cost_state: "priced".into(),
                error_message: None,
                request_id: None,
                attempt: None,
                error_source: None,
                error_stage: None,
                duration_ms: None,
                diagnostic: None,
            })
            .unwrap();

        let limits = state.pricing_snapshot().limits.clone();
        let candidates = list_auto_candidates(&state, now, &limits).unwrap();
        assert!(candidates.iter().any(|c| {
            c.account_id == "wake"
                && matches!(
                    c.action,
                    CandidateAction::Refresh {
                        trigger: UsageSyncTrigger::Scheduled
                    }
                )
        }));

        // Same transition must not override an active failure backoff floor.
        state
            .db
            .lock()
            .record_account_usage_sync_failure("wake", now, 1, now + failure_backoff(1))
            .unwrap();
        let candidates =
            list_auto_candidates(&state, now + Duration::seconds(30), &limits).unwrap();
        assert!(!candidates.iter().any(|c| c.account_id == "wake"));
        let sync = state
            .db
            .lock()
            .account_usage_sync_state("wake")
            .unwrap()
            .unwrap();
        assert_eq!(sync.next_eligible_at, Some(now + failure_backoff(1)));

        drop(state);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn inference_429_intentionally_overrides_failure_backoff_floor() {
        let (dir, state) = test_state("429-override");
        let account = ready_account(&state, "rl", "sk-rl");
        state.db.lock().create_account(&account).unwrap();
        let now = fixed("2026-08-18T12:00:00Z");
        state.usage_sync.set_clock_for_test(move || now);
        state.usage_sync.set_jitter_for_test(|| 0.0);
        state
            .db
            .lock()
            .record_account_usage_sync_failure(
                "rl",
                now - Duration::minutes(1),
                2,
                now + failure_backoff(2),
            )
            .unwrap();
        schedule_after_inference_429(&state, "rl");
        let sync = state
            .db
            .lock()
            .account_usage_sync_state("rl")
            .unwrap()
            .unwrap();
        assert_eq!(sync.failure_streak, 2);
        assert_eq!(
            sync.next_eligible_at,
            Some(now + INFERENCE_429_DELAY_MIN),
            "real inference 429 may intentionally pull earlier than failure backoff"
        );

        state.usage_sync.clear_test_seams();
        drop(state);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn goat_key_windows_are_independent_and_usage_failure_is_fail_soft() {
        let (dir, state) = test_state("goat-independent");
        let mut first = ready_account(&state, "goat-a", "goat-key-a");
        first.provider_id = COMMAND_CODE_PROVIDER_ID.to_string();
        first.offering_id = GOAT_OFFERING_ID.to_string();
        first.enabled = false;
        let mut second = ready_account(&state, "goat-b", "goat-key-b");
        second.provider_id = COMMAND_CODE_PROVIDER_ID.to_string();
        second.offering_id = GOAT_OFFERING_ID.to_string();
        second.enabled = false;
        {
            let db = state.db.lock();
            db.create_account(&first).unwrap();
            db.create_account(&second).unwrap();
        }
        let now = fixed("2026-08-18T12:00:00Z");
        for (account_id, used) in [("goat-a", 1.0), ("goat-b", 9.0)] {
            for kind in [
                QUOTA_WINDOW_FIVE_HOURS,
                QUOTA_WINDOW_WEEK,
                QUOTA_WINDOW_MONTH,
            ] {
                state
                    .db
                    .lock()
                    .upsert_quota_window(&QuotaWindow {
                        account_id: account_id.to_string(),
                        window_kind: kind.to_string(),
                        used,
                        // The official GOAT contract is not verified, so the
                        // test exercises key scoping without inventing limits,
                        // units, or reset semantics.
                        limit_value: None,
                        started_at: None,
                        resets_at: None,
                        calibration_offset: 0.0,
                        unit: "unknown".to_string(),
                        source: "test-fixture".to_string(),
                        observed_at: Some(now),
                        updated_at: now,
                    })
                    .unwrap();
            }
            for (balance_kind, amount) in [("purchased", used * 10.0), ("free", used)] {
                state
                    .db
                    .lock()
                    .upsert_credit_balance(&CreditBalance {
                        account_id: account_id.to_string(),
                        balance_kind: balance_kind.to_string(),
                        amount,
                        unit: "unknown".to_string(),
                        source: "test-fixture".to_string(),
                        observed_at: Some(now),
                        updated_at: now,
                    })
                    .unwrap();
            }
        }

        let first_windows = state.db.lock().list_quota_windows("goat-a").unwrap();
        let second_windows = state.db.lock().list_quota_windows("goat-b").unwrap();
        assert_eq!(first_windows.len(), 3);
        assert_eq!(second_windows.len(), 3);
        assert!(first_windows.iter().all(|window| window.used == 1.0));
        assert!(second_windows.iter().all(|window| window.used == 9.0));
        let first_balances = state.db.lock().list_credit_balances("goat-a").unwrap();
        let second_balances = state.db.lock().list_credit_balances("goat-b").unwrap();
        assert_eq!(first_balances[0].amount + first_balances[1].amount, 11.0);
        assert_eq!(second_balances[0].amount + second_balances[1].amount, 99.0);

        let fetches = Arc::new(AtomicUsize::new(0));
        let fetches_seen = fetches.clone();
        state.usage_sync.set_fetch_for_test(move |_cfg, _key| {
            fetches_seen.fetch_add(1, AtomicOrdering::SeqCst);
            Box::pin(async { Err(GoUsageError::Network) })
        });
        let error = refresh_official_usage(&state, "goat-a", UsageSyncTrigger::Manual)
            .await
            .unwrap_err();
        assert!(matches!(error, OfficialUsageRefreshError::NotEligible(_)));
        assert_eq!(fetches.load(AtomicOrdering::SeqCst), 0);
        let sync = state
            .db
            .lock()
            .account_usage_sync_state("goat-a")
            .unwrap()
            .unwrap();
        assert_eq!(sync.failure_streak, 0);
        assert_eq!(sync.last_attempt_at, None);
        assert_eq!(sync.next_eligible_at, None);

        schedule_after_inference_429(&state, "goat-a");
        assert_eq!(
            state
                .db
                .lock()
                .account_usage_sync_state("goat-a")
                .unwrap()
                .unwrap()
                .next_eligible_at,
            None,
            "experimental usage must stay independent from inference 429 handling"
        );
        let limits = state.pricing_snapshot().limits.clone();
        let candidates = list_auto_candidates(&state, now, &limits).unwrap();
        assert!(
            !candidates
                .iter()
                .any(|candidate| candidate.account_id.starts_with("goat-"))
        );

        let zen_windows = state
            .db
            .lock()
            .list_quota_windows(ZEN_FREE_ACCOUNT_ID)
            .unwrap();
        assert_eq!(zen_windows.len(), 1);
        assert_eq!(
            zen_windows[0].window_kind,
            crate::provider::QUOTA_WINDOW_FREE
        );
        assert_eq!(zen_windows[0].limit_value, None);

        state.usage_sync.clear_test_seams();
        drop(state);
        let _ = std::fs::remove_dir_all(dir);
    }

    struct FakeUsageInner {
        runtime: UsageSyncRuntime,
        settings_update: ParkingMutex<()>,
        settings_revision: AtomicU64,
        process_generation: u64,
        accounts: ParkingMutex<HashMap<String, Account>>,
        sync: ParkingMutex<HashMap<String, ProviderUsageSyncState>>,
        decrypts: ParkingMutex<HashMap<String, String>>,
    }

    #[derive(Clone)]
    struct FakeUsageHost {
        inner: Arc<FakeUsageInner>,
    }

    impl FakeUsageHost {
        fn new() -> Self {
            Self {
                inner: Arc::new(FakeUsageInner {
                    runtime: UsageSyncRuntime::new(),
                    settings_update: ParkingMutex::new(()),
                    settings_revision: AtomicU64::new(1),
                    process_generation: 7,
                    accounts: ParkingMutex::new(HashMap::new()),
                    sync: ParkingMutex::new(HashMap::new()),
                    decrypts: ParkingMutex::new(HashMap::new()),
                }),
            }
        }

        fn insert_ready_go(&self, id: &str, key: &str) {
            let account = Account {
                id: id.to_string(),
                provider_id: crate::provider::default_provider_id(),
                offering_id: crate::provider::default_offering_id(),
                credential_kind: crate::provider::default_credential_kind(),
                quota_scope: crate::provider::default_quota_scope(),
                name: id.to_string(),
                username: None,
                password_cipher: None,
                key_cipher: format!("cipher:{key}"),
                enabled: true,
                account_type: AccountType::Key,
                setup_step: AccountSetupStep::Ready,
                referral_code: None,
                purchase_date: "2026-08-01".to_string(),
                expires_on: "2026-09-01".to_string(),
                cooldown_until: None,
                cooldown_generic_until: None,
                cooldown_5h_until: None,
                cooldown_week_until: None,
                cooldown_month_until: None,
                cooldown_free_until: None,
                last_error: None,
                auth_error: None,
                notes: None,
                created_at: Utc::now(),
                updated_at: Utc::now(),
            };
            self.inner
                .decrypts
                .lock()
                .insert(account.key_cipher.clone(), key.to_string());
            self.inner.accounts.lock().insert(id.to_string(), account);
            self.inner.sync.lock().insert(
                id.to_string(),
                ProviderUsageSyncState {
                    account_id: id.to_string(),
                    last_success_at: None,
                    last_attempt_at: None,
                    next_eligible_at: None,
                    failure_streak: 0,
                    last_expedited_at: None,
                },
            );
        }

        fn settings_revision(&self) -> u64 {
            self.inner.settings_revision.load(AtomicOrdering::Acquire)
        }

        fn bump_settings_revision(&self) -> u64 {
            self.inner
                .settings_revision
                .fetch_add(1, AtomicOrdering::AcqRel)
                + 1
        }

        fn guarded_authorization(&self) -> UsageSyncCommitAuthorization {
            UsageSyncCommitAuthorization::control_revision(
                self.settings_revision(),
                self.inner.process_generation,
            )
        }
    }

    impl UsageSyncStore for FakeUsageInner {
        fn list_accounts(&self) -> anyhow::Result<Vec<Account>> {
            Ok(self.accounts.lock().values().cloned().collect())
        }
        fn get_account(&self, account_id: &str) -> anyhow::Result<Option<Account>> {
            Ok(self.accounts.lock().get(account_id).cloned())
        }
        fn account_usage_sync_state(
            &self,
            account_id: &str,
        ) -> anyhow::Result<Option<ProviderUsageSyncState>> {
            Ok(self.sync.lock().get(account_id).cloned())
        }
        fn pull_account_usage_sync_next_eligible(
            &self,
            account_id: &str,
            proposal: DateTime<Utc>,
            respect_failure_backoff: bool,
        ) -> anyhow::Result<()> {
            let mut sync = self.sync.lock();
            let Some(current) = sync.get_mut(account_id) else {
                return Ok(());
            };
            if respect_failure_backoff && current.failure_streak > 0 {
                return Ok(());
            }
            current.next_eligible_at = Some(match current.next_eligible_at {
                Some(existing) => existing.min(proposal),
                None => proposal,
            });
            Ok(())
        }
        fn account_has_local_activity_since(
            &self,
            _account_id: &str,
            _since: DateTime<Utc>,
        ) -> anyhow::Result<bool> {
            Ok(false)
        }
        fn account_usage_with_limits(
            &self,
            account_id: &str,
            _limits: &PricingLimits,
        ) -> anyhow::Result<UsageWindow> {
            Ok(UsageWindow {
                account_id: account_id.to_string(),
                window_5h: 0.0,
                window_week: 0.0,
                window_month: 0.0,
                resets_in_5h: None,
                resets_in_week: None,
                resets_in_month: None,
            })
        }
        fn commit_official_usage_sync_success(
            &self,
            account_id: &str,
            _expected_key_cipher: &str,
            _snapshot: &GoUsageSnapshot,
            _limits: &PricingLimits,
            metadata: OfficialUsageSyncSuccessMetadata,
        ) -> anyhow::Result<Option<UsageWindow>> {
            if let Some(current) = self.sync.lock().get_mut(account_id) {
                current.last_success_at = Some(metadata.now);
                current.last_attempt_at = Some(metadata.now);
                current.next_eligible_at = Some(metadata.next_eligible_at);
                current.failure_streak = 0;
                if metadata.mark_expedited {
                    current.last_expedited_at = Some(metadata.now);
                }
            }
            Ok(Some(UsageWindow {
                account_id: account_id.to_string(),
                window_5h: 0.0,
                window_week: 0.0,
                window_month: 0.0,
                resets_in_5h: None,
                resets_in_week: None,
                resets_in_month: None,
            }))
        }
        fn record_account_usage_sync_failure(
            &self,
            account_id: &str,
            now: DateTime<Utc>,
            failure_streak: i64,
            next_eligible_at: DateTime<Utc>,
        ) -> anyhow::Result<()> {
            if let Some(current) = self.sync.lock().get_mut(account_id) {
                current.last_attempt_at = Some(now);
                current.failure_streak = failure_streak;
                current.next_eligible_at = Some(next_eligible_at);
            }
            Ok(())
        }
        fn log_gateway(&self, _level: &str, _category: &str, _message: &str) -> anyhow::Result<()> {
            Ok(())
        }
    }

    impl UsageSyncHost for FakeUsageHost {
        type Weak = std::sync::Weak<FakeUsageInner>;
        type Store = FakeUsageInner;

        fn downgrade(&self) -> Self::Weak {
            Arc::downgrade(&self.inner)
        }
        fn upgrade(weak: &Self::Weak) -> Option<Self> {
            weak.upgrade().map(|inner| Self { inner })
        }
        fn usage_runtime(&self) -> &UsageSyncRuntime {
            &self.inner.runtime
        }
        fn pricing_limits(&self) -> PricingLimits {
            crate::kernel::pricing::SEED_LIMITS
        }
        fn config(&self) -> AppConfig {
            AppConfig::default()
        }
        fn decrypt_account_key(&self, ciphertext: &str) -> anyhow::Result<String> {
            self.inner
                .decrypts
                .lock()
                .get(ciphertext)
                .cloned()
                .ok_or_else(|| anyhow::anyhow!("missing test key"))
        }
        fn with_sync_store<F, R>(&self, f: F) -> R
        where
            F: FnOnce(&Self::Store) -> R,
        {
            f(&self.inner)
        }

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
                UsageSyncCommitAuthorization::ControlRevision {
                    expected_revision,
                    process_generation,
                } => {
                    let _settings_update = self.inner.settings_update.lock();
                    if *expected_revision != self.settings_revision()
                        || *process_generation != self.inner.process_generation
                    {
                        return Err(UsageSyncCommitAuthorizationRejected);
                    }
                    Ok(f(&self.inner))
                }
            }
        }
    }

    #[test]
    fn usage_sync_host_seam_schedules_inference_429_without_process_host() {
        let host = FakeUsageHost::new();
        host.insert_ready_go("acc-5", "sk-acc-5");
        let now = fixed("2026-08-18T12:00:00Z");
        host.usage_runtime().set_clock_for_test(move || now);
        host.usage_runtime().set_jitter_for_test(|| 0.0);
        host.inner
            .sync
            .lock()
            .get_mut("acc-5")
            .unwrap()
            .next_eligible_at = Some(now + Duration::hours(20));

        schedule_after_inference_429(&host, "acc-5");
        let sync = host.inner.sync.lock().get("acc-5").cloned().unwrap();
        assert_eq!(sync.next_eligible_at, Some(now + INFERENCE_429_DELAY_MIN));
        assert_eq!(sync.failure_streak, 0);
    }

    #[tokio::test]
    async fn usage_sync_host_loop_exits_when_the_host_is_dropped() {
        let host = FakeUsageHost::new();
        spawn_usage_sync_loop(host.clone());
        assert!(
            host.usage_runtime()
                .loop_started
                .load(AtomicOrdering::Acquire)
        );
        spawn_usage_sync_loop(host.clone());
        let weak = host.downgrade();
        drop(host);
        tokio::time::sleep(StdDuration::from_millis(20)).await;
        assert!(
            FakeUsageHost::upgrade(&weak).is_none(),
            "dropping the last host handle must end the scheduler lifetime"
        );
    }
}
