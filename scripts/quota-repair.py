from pathlib import Path


def replace(path, old, new):
    p = Path(path)
    text = p.read_text()
    assert text.count(old) == 1, (path, old[:100], text.count(old))
    p.write_text(text.replace(old, new))


def write(path, content):
    p = Path(path)
    assert not p.exists(), path
    p.parent.mkdir(parents=True, exist_ok=True)
    p.write_text(content.lstrip('\n'))


write('crates/ocg-core/src/usage_sync/singleflight.rs', r'''
//! Share unfinished work, not completed results. The last cancelled waiter
//! drops the future, including any network concurrency permit it owns.
use futures_util::future::{BoxFuture, FutureExt, Shared};
use parking_lot::Mutex;
use std::collections::HashMap;
use std::future::Future;
use std::sync::atomic::{AtomicU64, Ordering};

struct Flight<T> {
    generation: u64,
    waiters: usize,
    future: Shared<BoxFuture<'static, T>>,
}

pub(crate) struct SingleFlight<T> {
    flights: Mutex<HashMap<String, Flight<T>>>,
    sequence: AtomicU64,
}

impl<T> Default for SingleFlight<T> {
    fn default() -> Self {
        Self { flights: Mutex::new(HashMap::new()), sequence: AtomicU64::new(1) }
    }
}

struct Waiter<'a, T> {
    flights: &'a Mutex<HashMap<String, Flight<T>>>,
    key: String,
    generation: u64,
}

impl<T> Drop for Waiter<'_, T> {
    fn drop(&mut self) {
        let mut flights = self.flights.lock();
        if let Some(flight) = flights.get_mut(&self.key)
            && flight.generation == self.generation
        {
            flight.waiters -= 1;
            if flight.waiters == 0 { flights.remove(&self.key); }
        }
    }
}

impl<T: Clone + Send + Sync + 'static> SingleFlight<T> {
    pub(crate) async fn run<F, Fut>(&self, key: String, work: F) -> T
    where
        F: FnOnce() -> Fut + Send + 'static,
        Fut: Future<Output = T> + Send + 'static,
    {
        let (future, generation) = {
            let mut flights = self.flights.lock();
            if flights.get(&key).is_some_and(|flight| flight.future.peek().is_some()) {
                flights.remove(&key);
            }
            let flight = flights.entry(key.clone()).or_insert_with(|| Flight {
                generation: self.sequence.fetch_add(1, Ordering::Relaxed),
                waiters: 0,
                future: async move { work().await }.boxed().shared(),
            });
            flight.waiters += 1;
            (flight.future.clone(), flight.generation)
        };
        let _waiter = Waiter { flights: &self.flights, key: key.clone(), generation };
        let result = future.await;
        let mut flights = self.flights.lock();
        if flights.get(&key).is_some_and(|flight| flight.generation == generation) {
            flights.remove(&key);
        }
        result
    }
}

#[cfg(test)]
mod tests;
''')
write('crates/ocg-core/src/usage_sync/singleflight/tests.rs', r'''
use super::*;
use std::sync::Arc;
use tokio::sync::oneshot;

#[tokio::test]
async fn followers_share_success_and_failure_but_later_calls_run_again() {
    for outcome in [Ok(42), Err("upstream unavailable")] {
        let flights = Arc::new(SingleFlight::default());
        let (started, entered) = oneshot::channel();
        let (release, released) = oneshot::channel();
        let leader_flights = flights.clone();
        let leader = tokio::spawn(async move {
            leader_flights.run("account:v1".into(), move || async move {
                started.send(()).unwrap();
                released.await.unwrap();
                outcome
            }).await
        });
        entered.await.unwrap();
        let follower_flights = flights.clone();
        let follower = tokio::spawn(async move {
            follower_flights.run("account:v1".into(), || async {
                panic!("duplicate work must not run")
            }).await
        });
        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            while flights.flights.lock().get("account:v1").unwrap().waiters != 2 {
                tokio::task::yield_now().await;
            }
        }).await.unwrap();
        release.send(()).unwrap();
        assert_eq!(leader.await.unwrap(), outcome);
        assert_eq!(follower.await.unwrap(), outcome);
        assert!(flights.flights.lock().is_empty());
        assert_eq!(flights.run("account:v1".into(), || async { Ok(99) }).await, Ok(99));
    }
}

#[tokio::test]
async fn cancelling_all_waiters_drops_the_work_and_its_permit() {
    let flights = Arc::new(SingleFlight::<()>::default());
    let limit = Arc::new(tokio::sync::Semaphore::new(1));
    let (started, entered) = oneshot::channel();
    let worker_flights = flights.clone();
    let worker_limit = limit.clone();
    let worker = tokio::spawn(async move {
        worker_flights.run("old-key".into(), move || async move {
            let _permit = worker_limit.acquire_owned().await.unwrap();
            started.send(()).unwrap();
            std::future::pending().await
        }).await;
    });
    entered.await.unwrap();
    worker.abort();
    let _ = worker.await;
    assert_eq!(limit.available_permits(), 1);
    assert!(flights.flights.lock().is_empty());
}

#[tokio::test]
async fn cancelling_leader_keeps_follower_alive_and_versions_do_not_join() {
    let flights = Arc::new(SingleFlight::<u32>::default());
    let (started, entered) = oneshot::channel();
    let (release, released) = oneshot::channel();
    let one = flights.clone();
    let leader = tokio::spawn(async move {
        one.run("account:v1".into(), move || async move {
            started.send(()).unwrap();
            released.await.unwrap();
            1
        }).await
    });
    entered.await.unwrap();
    let two = flights.clone();
    let follower = tokio::spawn(async move {
        two.run("account:v1".into(), || async { panic!("must join") }).await
    });
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        while flights.flights.lock().get("account:v1").unwrap().waiters != 2 {
            tokio::task::yield_now().await;
        }
    }).await.unwrap();
    leader.abort();
    let _ = leader.await;
    assert_eq!(flights.run("account:v2".into(), || async { 2 }).await, 2);
    release.send(()).unwrap();
    assert_eq!(follower.await.unwrap(), 1);
}
''')
replace('crates/ocg-core/src/usage_sync.rs', 'mod provider_refresh;', 'mod provider_refresh;\nmod singleflight;\npub(crate) use singleflight::SingleFlight;')
replace('crates/ocg-core/src/usage_sync.rs', 'impl UsageRefreshIdentity {', '''impl UsageRefreshIdentity {
    pub(crate) fn flight_key(&self, operation: &str) -> String {
        format!("{operation}:{}:{}:{}", self.credential.credential_id,
            self.credential.credential_version, self.updated_at)
    }
''')
replace('crates/ocg-core/src/dashboard_v3/mod.rs', '#[derive(Debug)]\npub(crate) struct V3ApiError', '#[derive(Debug, Clone)]\npub(crate) struct V3ApiError')
replace('crates/ocg-core/src/state.rs', '    pub(crate) provider_usage_refresh: crate::usage_sync::ProviderUsageRefreshGate,', '''    pub(crate) provider_usage_refresh: crate::usage_sync::ProviderUsageRefreshGate,
    pub(crate) balance_refresh: crate::usage_sync::SingleFlight<Result<(), crate::dashboard_v3::V3ApiError>>,
    /// Leaf lock, acquired only after settings_update and db; never over I/O.
    pub(crate) billing_cache: Mutex<crate::dashboard_v4::billing_cache::BillingReadCache>,''')
replace('crates/ocg-core/src/state.rs', '            cpa_operations: tokio::sync::Mutex::new(()),', '''            balance_refresh: crate::usage_sync::SingleFlight::default(),
            billing_cache: Mutex::new(crate::dashboard_v4::billing_cache::BillingReadCache::default()),
            cpa_operations: tokio::sync::Mutex::new(()),''')

p = Path('crates/ocg-core/src/dashboard_v3/usage.rs')
s = p.read_text()
start = s.index('async fn refresh_official_balance(')
end = s.index('\nasync fn refresh_go_provider_usage(', start)
old = s[start:end]
inner = old.replace('async fn refresh_official_balance(', 'async fn refresh_official_balance_inner(', 1)
inner = inner.replace(') -> Result<Json<ProviderUsage>, RefreshApiError> {', ') -> Result<(), V3ApiError> {', 1)
a = inner.index('    let _refresh = state')
b = inner.index('    let (account_snapshot', a)
inner = inner[:a] + inner[b:]
inner = inner.replace('\n            .into());', ');').replace('state, message).into());', 'state, message));')
inner = inner.replace('''        let key = state
            .decrypt_key''', '''        if crate::usage_sync::manual_next_allowed_at(
            db.account_usage_sync_state(id).map_err(V3ApiError::internal)?
                .and_then(|row| row.last_attempt_at), state.usage_sync.now(),
        ).is_some() {
            return Err(V3ApiError::throttled_at(state, "balance refresh is limited to once per 15 seconds"));
        }
        let key = state
            .decrypt_key''')
inner = inner.replace('        (account, state.config(), key)', '''        db.touch_account_usage_sync_attempt(id, state.usage_sync.now())
            .map_err(V3ApiError::internal)?;
        (account, state.config(), key)''')
inner = inner.replace('''    load_provider_usage(state, id)
        .map(Json)
        .map_err(RefreshApiError::from)''', '    Ok(())')
wrapper = r'''
/// Both balance APIs share this gate. The shared result is a commit receipt,
/// never a cached HTTP envelope. Each caller rechecks its own identity/CAS.
pub(crate) async fn coalesce_balance<F, Fut>(
    state: &CoreState,
    id: &str,
    expectation: &MutationExpectation,
    operation: &str,
    work: F,
) -> Result<(), V3ApiError>
where
    F: FnOnce() -> Fut + Send + 'static,
    Fut: std::future::Future<Output = Result<(), V3ApiError>> + Send + 'static,
{
    let (identity, key) = {
        let _settings = state.settings_update.lock();
        check_expectation(state, expectation)?;
        let db = state.db.lock();
        let identity = crate::usage_sync::UsageRefreshIdentity::capture(&db, id)
            .map_err(V3ApiError::internal)?
            .ok_or_else(|| V3ApiError::not_found_at(state, "account credential not found"))?;
        let key = format!("{}:{}:{}", identity.flight_key(operation),
            expectation.expected_revision, expectation.process_generation);
        (identity, key)
    };
    let captured = identity.clone();
    let worker_state = state.clone();
    let worker_id = id.to_string();
    let expected = expectation.clone();
    state.balance_refresh.run(key, move || async move {
        let _permit = worker_state.provider_usage_refresh
            .exclusive(ProviderUsageRefreshGate::balance_key(&worker_id)).await;
        {
            let _settings = worker_state.settings_update.lock();
            check_expectation(&worker_state, &expected)?;
            if !captured.is_current(&worker_state.db.lock()).map_err(V3ApiError::internal)? {
                return Err(V3ApiError::conflict_at(&worker_state, "balance credential changed before refresh"));
            }
        }
        work().await
    }).await?;
    let _settings = state.settings_update.lock();
    check_expectation(state, expectation)?;
    if !identity.is_current(&state.db.lock()).map_err(V3ApiError::internal)? {
        return Err(V3ApiError::conflict_at(state, "balance credential changed during refresh"));
    }
    Ok(())
}

async fn refresh_official_balance(
    state: &CoreState,
    id: &str,
    expectation: &MutationExpectation,
    endpoint_url: String,
) -> Result<Json<ProviderUsage>, RefreshApiError> {
    let worker_state = state.clone();
    let worker_id = id.to_string();
    let expected = expectation.clone();
    coalesce_balance(state, id, expectation, "provider-balance", move || async move {
        refresh_official_balance_inner(&worker_state, &worker_id, &expected, endpoint_url).await
    }).await?;
    let _settings = state.settings_update.lock();
    check_expectation(state, expectation)?;
    provider_usage_from_db(state, &state.db.lock(), id).map(Json).map_err(RefreshApiError::from)
}
'''
s = s[:start] + wrapper + '\n' + inner + s[end:]
p.write_text(s)

p = Path('crates/ocg-core/src/dashboard_v4/official_api.rs')
s = p.read_text()
a = s.index('pub(super) async fn refresh_balance(')
b = s.index('\npub(super) async fn refresh_prices(', a)
inner = s[a:b]
old_prefix = '''pub(super) async fn refresh_balance(
    State(state): State<CoreState>,
    Path(id): Path<String>,
    body: Bytes,
) -> Result<Json<OfficialApiStatus>, V3ApiError> {
    let expectation = parse_mutation_json::<MutationExpectation>(&body)?;
'''
assert inner.startswith(old_prefix)
inner = inner.replace(old_prefix, '''async fn refresh_balance_inner(
    state: CoreState,
    id: String,
    expectation: MutationExpectation,
) -> Result<(), V3ApiError> {
''', 1)
x = inner.index('    let _refresh = state')
y = inner.index('    let (snapshot', x)
inner = inner[:x] + inner[y:]
inner = inner.replace('        (account, runtime, state.config(), key)', '''        db.touch_account_usage_sync_attempt(&id, state.usage_sync.now())
            .map_err(V3ApiError::internal)?;
        (account, runtime, state.config(), key)''')
inner = inner.replace('    status(&state, &id).map(Json)', '    Ok(())')
wrapper = r'''
pub(super) async fn refresh_balance(
    State(state): State<CoreState>,
    Path(id): Path<String>,
    body: Bytes,
) -> Result<Json<OfficialApiStatus>, V3ApiError> {
    let expectation = parse_mutation_json::<MutationExpectation>(&body)?;
    let worker_state = state.clone();
    let worker_id = id.clone();
    let expected = expectation.clone();
    crate::dashboard_v3::usage::coalesce_balance(
        &state, &id, &expectation, "official-api-balance", move || async move {
            refresh_balance_inner(worker_state, worker_id, expected).await
        },
    ).await?;
    let _settings = state.settings_update.lock();
    check_expectation(&state, &expectation)?;
    status_locked(&state, &state.db.lock(), &id).map(Json)
}
'''
p.write_text(s[:a] + wrapper + '\n' + inner + s[b:])

write('crates/ocg-core/src/dashboard_v4/billing_cache.rs', r'''
//! Rebuildable local read cache. Persisted official evidence stays authoritative.
//! Every SQLite write (including usage settlement without a settings revision)
//! invalidates the projection. Other DB connections are covered by data_version.
use crate::billing_types::BillingStatus;
use crate::db::Database;
use crate::state::CoreState;
use chrono::{DateTime, Datelike, Duration, Utc};
use std::collections::HashMap;
use std::time::Instant;

const MAX_ENTRIES: usize = 256;
const MAX_AGE: std::time::Duration = std::time::Duration::from_secs(15);

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct ReadVersion {
    revision: u64,
    changes: u64,
    data_version: u64,
    pricing: String,
    official_month: (i32, u32),
}
impl ReadVersion {
    pub(super) fn capture(state: &CoreState, db: &Database) -> rusqlite::Result<Self> {
        let now = state.usage_sync.now();
        Ok(Self {
            revision: state.settings_revision(),
            changes: db.conn.query_row("SELECT total_changes()", [], |row| row.get(0))?,
            data_version: db.conn.pragma_query_value(None, "data_version", |row| row.get(0))?,
            pricing: state.pricing_snapshot().revision.clone(),
            official_month: (now.year(), now.month()),
        })
    }
}

struct Entry {
    status: BillingStatus,
    inserted: Instant,
    sampled_at: DateTime<Utc>,
    valid_until: DateTime<Utc>,
}

#[derive(Default)]
pub(crate) struct BillingReadCache {
    version: Option<ReadVersion>,
    entries: HashMap<String, Entry>,
}
impl BillingReadCache {
    fn bind(&mut self, version: &ReadVersion) {
        if self.version.as_ref() != Some(version) {
            self.entries.clear();
            self.version = Some(version.clone());
        }
    }
    pub(super) fn get(&mut self, id: &str, version: &ReadVersion, now: DateTime<Utc>) -> Option<BillingStatus> {
        self.bind(version);
        let entry = self.entries.get(id)?;
        if entry.inserted.elapsed() >= MAX_AGE || now < entry.sampled_at || now >= entry.valid_until {
            self.entries.remove(id);
            return None;
        }
        Some(entry.status.clone())
    }
    pub(super) fn insert(&mut self, version: &ReadVersion, status: BillingStatus, now: DateTime<Utc>) {
        self.bind(version);
        if self.entries.len() >= MAX_ENTRIES && !self.entries.contains_key(&status.account_id)
            && let Some(oldest) = self.entries.iter().min_by_key(|(_, value)| value.inserted).map(|(id, _)| id.clone())
        {
            self.entries.remove(&oldest);
        }
        let valid_until = next_change(&status, now);
        self.entries.insert(status.account_id.clone(), Entry { status, inserted: Instant::now(), sampled_at: now, valid_until });
    }
}

fn next_change(status: &BillingStatus, now: DateTime<Utc>) -> DateTime<Utc> {
    let mut deadline = now + Duration::seconds(15);
    let mut consider = |at: DateTime<Utc>| {
        if at > now { deadline = deadline.min(at); }
    };
    if let Some(usage) = &status.usage {
        for at in usage.quota_windows.iter().filter_map(|window| window.resets_at.as_deref())
            .chain(usage.free_cooldown_until.as_deref())
        {
            if let Ok(at) = DateTime::parse_from_rfc3339(at) { consider(at.with_timezone(&Utc)); }
        }
    }
    if let Some(credits) = &status.credits {
        for bucket in &credits.buckets {
            consider(bucket.starts_at);
            if let Some(at) = bucket.expires_at { consider(at); }
        }
        if let Some(at) = credits.next_reset_at { consider(at); }
    }
    // Calendar-based presets and month-to-date projections must not survive midnight.
    if let Some(tomorrow) = now.date_naive().succ_opt().and_then(|day| day.and_hms_opt(0, 0, 0)) {
        consider(tomorrow.and_utc());
    }
    deadline
}

#[cfg(test)]
mod tests;
''')
write('crates/ocg-core/src/dashboard_v4/billing_cache/tests.rs', r'''
use super::*;
use crate::billing_types::{BillingModel, BillingSource};

fn status(id: &str) -> BillingStatus {
    BillingStatus {
        account_id: id.into(), model: BillingModel::Cash, source: BillingSource::Unavailable,
        unit: "currency".into(), configurable_credits: false, manual_calibration: false,
        official_refresh: true, usage: None, cash: None, credits: None, presets: vec![],
        revision: 1, process_generation: 1,
    }
}
fn version() -> ReadVersion {
    ReadVersion { revision: 1, changes: 0, data_version: 1, pricing: "v1".into(), official_month: (2026, 10) }
}

#[test]
fn cache_is_bounded_expires_and_rejects_reversed_clock() {
    let mut cache = BillingReadCache::default();
    let now = Utc::now();
    let version = version();
    for id in 0..=MAX_ENTRIES { cache.insert(&version, status(&id.to_string()), now); }
    assert_eq!(cache.entries.len(), MAX_ENTRIES);
    assert!(cache.get("0", &version, now).is_none());
    assert!(cache.get("1", &version, now).is_some());
    assert!(cache.get("1", &version, now + Duration::seconds(15)).is_none());
    assert!(cache.get("2", &version, now - Duration::seconds(1)).is_none());
}

#[test]
fn usage_writes_external_writes_pricing_and_settings_each_invalidate() {
    for changed in [
        ReadVersion { changes: 1, ..version() },
        ReadVersion { data_version: 2, ..version() },
        ReadVersion { revision: 2, ..version() },
        ReadVersion { pricing: "v2".into(), ..version() },
        ReadVersion { official_month: (2026, 11), ..version() },
    ] {
        let mut cache = BillingReadCache::default();
        let now = Utc::now();
        cache.insert(&version(), status("a"), now);
        assert!(cache.get("a", &version(), now).is_some());
        assert!(cache.get("a", &changed, now).is_none());
    }
}
''')
replace('crates/ocg-core/src/dashboard_v4/mod.rs', 'mod billing;', 'mod billing;\npub(crate) mod billing_cache;')
replace('crates/ocg-core/src/dashboard_v4/mod.rs', '        .route("/accounts/{id}/billing", get(billing::get_status))', '''        .route("/accounts/{id}/billing", get(billing::get_status))
        .route("/billing/snapshots", post(billing::snapshots))''')

p = Path('crates/ocg-core/src/dashboard_v4/billing.rs')
s = p.read_text()
a = s.index('pub(super) async fn get_status(')
b = s.index('\nfn destination(', a)
s = s[:a] + r'''
pub(super) async fn get_status(
    State(state): State<CoreState>,
    Path(id): Path<String>,
) -> Result<Json<BillingStatus>, V3ApiError> {
    tokio::task::spawn_blocking(move || status(&state, &id).map(Json))
        .await.map_err(V3ApiError::internal)?
}

fn status(state: &CoreState, id: &str) -> Result<BillingStatus, V3ApiError> {
    let _settings = state.settings_update.lock();
    let db = state.db.lock();
    cached_status(state, &db, id)
}

fn cached_status(state: &CoreState, db: &crate::db::Database, id: &str) -> Result<BillingStatus, V3ApiError> {
    use super::billing_cache::ReadVersion;
    for _ in 0..3 {
        let before = ReadVersion::capture(state, db).map_err(V3ApiError::internal)?;
        let now = Utc::now();
        if let Some(status) = state.billing_cache.lock().get(id, &before, now) {
            return Ok(status);
        }
        let status = compose_status(state, db, id)?;
        let after = ReadVersion::capture(state, db).map_err(V3ApiError::internal)?;
        // Read helpers may advance windows, and another SQLite connection may write.
        // Retry instead of caching a composition under a version it did not observe.
        if before != after { continue; }
        state.billing_cache.lock().insert(&after, status.clone(), now);
        return Ok(status);
    }
    Err(V3ApiError::conflict_at(state, "billing data changed during read"))
}

fn compose_status(state: &CoreState, db: &crate::db::Database, id: &str) -> Result<BillingStatus, V3ApiError> {
    let account = db.get_account(id).map_err(V3ApiError::internal)?
        .ok_or_else(|| V3ApiError::not_found_at(state, "account not found"))?;
    let (adapter, endpoint, legacy_kind) = destination(&db.conn, id).map_err(V3ApiError::internal)?
        .ok_or_else(|| V3ApiError::not_found_at(state, "account destination not found"))?;
    let configurable = adapter == AdapterKind::Http && matches!(legacy_kind.as_str(), "dynamic" | "custom_account");
    let credits = storage::read_view_on(&db.conn, id, Utc::now()).map_err(V3ApiError::internal)?;
    let official_cash = db.get_dynamic_provider(&account.provider_id).map_err(V3ApiError::internal)?
        .as_ref().and_then(crate::official_api::kind_for_runtime).is_some();
    let usage = crate::dashboard_v3::usage::provider_usage_from_db(state, db, id)?;
    let model = if credits.is_some() { BillingModel::Credits } else { billing_model_for_destination(adapter, &endpoint) };
    let cash = if official_cash && model == BillingModel::Cash {
        Some(super::official_api::status_locked(state, db, id)?)
    } else { None };
    Ok(project_billing(state, id, state.settings_revision(), &account.provider_id, adapter,
        &endpoint, configurable, credits, usage, cash))
}

/// Local read only. Never fetches upstream and never starts background I/O.
/// Per-account errors cannot prevent other accounts from receiving snapshots.
pub(super) async fn snapshots(
    State(state): State<CoreState>,
    body: Bytes,
) -> Result<Json<crate::billing_types::BillingSnapshots>, V3ApiError> {
    let input = parse_mutation_json::<crate::billing_types::BillingSnapshotRequest>(&body)?;
    if input.account_ids.len() > 64 || input.account_ids.iter().any(|id| id.is_empty() || id.len() > 256) {
        return Err(V3ApiError::invalid_request_at(&state, "at most 64 valid account ids are allowed"));
    }
    tokio::task::spawn_blocking(move || {
        let _settings = state.settings_update.lock();
        let db = state.db.lock();
        let mut statuses = Vec::new();
        let mut errors = std::collections::BTreeMap::new();
        let mut seen = std::collections::HashSet::new();
        for id in input.account_ids {
            if !seen.insert(id.clone()) { continue; }
            match cached_status(&state, &db, &id) {
                Ok(status) => statuses.push(status),
                Err(error) => { errors.insert(id, error.envelope().clone()); }
            }
        }
        Json(crate::billing_types::BillingSnapshots {
            statuses, errors, revision: state.settings_revision(), process_generation: state.process_generation(),
        })
    }).await.map_err(V3ApiError::internal)
}
''' + s[b:]
p.write_text(s)
replace('crates/ocg-core/src/dashboard_v3/mod.rs', 'impl V3ApiError {', '''impl V3ApiError {
    pub(crate) fn envelope(&self) -> &V3Error { &self.body }
''')
p = Path('crates/ocg-core/src/billing_types.rs')
p.write_text(p.read_text() + r'''

/// Bounded, local-only batch read; it is not a control-plane mutation.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BillingSnapshotRequest {
    pub account_ids: Vec<String>,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BillingSnapshots {
    pub statuses: Vec<BillingStatus>,
    pub errors: std::collections::BTreeMap<String, crate::dashboard_v3::V3Error>,
    pub revision: u64,
    pub process_generation: u64,
}
''')
replace('crates/ocg-core/src/dashboard_v4/types.rs', '    "BillingStatus",', '    "BillingStatus",\n    "BillingSnapshotRequest",\n    "BillingSnapshots",')
replace('crates/ocg-core/src/dashboard_v4/types.rs', '    include_type::<BillingStatus>(&mut serialize);', '''    include_type::<BillingStatus>(&mut serialize);
    include_type::<crate::billing_types::BillingSnapshotRequest>(&mut serialize);
    include_type::<crate::billing_types::BillingSnapshots>(&mut serialize);''')

p = Path('crates/ocg-core/src/dashboard_v4/billing/tests.rs')
p.write_text(p.read_text() + r'''

#[tokio::test]
async fn cached_reads_see_usage_writes_without_a_settings_revision_and_external_writes() {
    let (dir, state) = state();
    account(&state, "credits");
    configure(State(state.clone()), Path("credits".into()), body(&state, serde_json::json!({
        "configuration": configuration(), "initialBuckets": [bucket(75.0)]
    }))).await.unwrap();
    let first = status(&state, "credits").unwrap();
    let second = status(&state, "credits").unwrap();
    assert_eq!(first.credits.as_ref().unwrap().estimated_at, second.credits.as_ref().unwrap().estimated_at);
    let revision = state.settings_revision();
    storage::grant_on(&state.db.lock().conn, "credits", "local settlement".into(), 5.0, None, Utc::now()).unwrap();
    assert_eq!(state.settings_revision(), revision);
    assert_eq!(remaining(&status(&state, "credits").unwrap()), 80.0);
    let path = state.db.lock().conn.path().unwrap().to_string();
    let external = rusqlite::Connection::open(path).unwrap();
    storage::grant_on(&external, "credits", "other connection".into(), 7.0, None, Utc::now()).unwrap();
    assert_eq!(remaining(&status(&state, "credits").unwrap()), 87.0);
    external.execute("DELETE FROM credentials WHERE legacy_account_id = 'credits'", []).unwrap();
    assert!(status(&state, "credits").is_err(), "deleted credentials cannot return a cached balance");
    drop(external);
    drop(state);
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn batch_reads_isolate_errors_deduplicate_and_enforce_a_bound() {
    let (dir, state) = state();
    account(&state, "valid");
    let batch = snapshots(State(state.clone()), Bytes::from_static(br#"{"accountIds":["valid","missing","valid"]}"#))
        .await.unwrap().0;
    assert_eq!(batch.statuses.len(), 1);
    assert_eq!(batch.statuses[0].account_id, "valid");
    assert_eq!(batch.errors.len(), 1);
    assert!(batch.errors.contains_key("missing"));
    let too_many = serde_json::json!({"accountIds": vec!["valid"; 65]});
    assert!(snapshots(State(state.clone()), Bytes::from(too_many.to_string())).await.is_err());
    drop(state);
    let _ = std::fs::remove_dir_all(dir);
}
''')

# The existing soft-limit fixture has a September billing period; wall-clock
# timestamps made its receipts disappear after October 1. Keep receipts in the
# declared period instead of weakening the over-limit assertion.
p = Path('crates/ocg-core/tests/dashboard_v3_ollama_usage.rs')
s = p.read_text()
a = s.index('async fn ollama_paid_tier_requires_purchase_date_and_publishes_month_credits')
b = s.find('\n#[tokio::test', a)
if b < 0: b = len(s)
chunk = s[a:b]
assert chunk.count('timestamp: Utc::now(),') == 2
chunk = chunk.replace('timestamp: Utc::now(),', 'timestamp: chrono::DateTime::parse_from_rfc3339("2026-09-15T12:00:00Z").unwrap().with_timezone(&Utc),')
p.write_text(s[:a] + chunk + s[b:])

# Frontend batching is additive; single-account actions keep their existing API.
replace('src/api/billing.ts', '  BillingStatus,\n  CreditCalibrationRequest,', '  BillingStatus,\n  BillingSnapshots,\n  CreditCalibrationRequest,')
replace('src/api/billing.ts', 'export const billingApi = {', '''export const billingApi = {
  snapshots: (accountIds: string[]) => requestV4<BillingSnapshots>("/billing/snapshots", {
    method: "POST", body: { accountIds },
  }),''')
replace('src/stores/billing.ts', '  async function refreshUsage(', '''  async function loadMany(accounts: { accountId: string; binding: string }[]): Promise<void> {
    const unique = [...new Map(accounts.map(account => [account.accountId, account])).values()];
    const tokens = unique.filter(({ accountId, binding }) => {
      const slot = slots.get(accountId)?.value;
      return !(slot?.boundVersion === binding && (slot.loading || slot.mutating));
    }).map(({ accountId, binding }) => begin(accountId, binding, {
      loading: true, mutating: false, clearError: false,
    }));
    for (let offset = 0; offset < tokens.length; offset += 32) {
      const batch = tokens.slice(offset, offset + 32).filter(owns);
      if (batch.length === 0) continue;
      try {
        const result = await billingApi.snapshots(batch.map(token => token.accountId));
        const statuses = new Map(result.statuses.map(status => [status.accountId, status]));
        for (const token of batch) {
          if (!owns(token)) continue;
          const status = statuses.get(token.accountId);
          if (status && !result.errors[token.accountId]) applyStatus(token.accountId, status);
          else write(token.accountId, { error: "load_failed" });
        }
      } catch (error) {
        for (const token of batch) if (owns(token)) write(token.accountId, { error: clientErrorFrom(error) });
      } finally {
        for (const token of batch) if (owns(token)) write(token.accountId, { loading: false });
      }
    }
  }

  async function refreshUsage(''')
# Insert export only into the returned store API, not other appearances.
p = Path('src/stores/billing.ts')
s = p.read_text()
a = s.rindex('  return {')
assert '    load,' in s[a:]
p.write_text(s[:a] + s[a:].replace('    load,', '    load,\n    loadMany,', 1))

replace('src/domain/useAccountUsage.ts', '  function ensureAccountUsage(accountId: string): Promise<void> {', '''  async function loadAccountUsageSnapshots(ids: string[], refresh = false): Promise<void> {
    if (disposed) return;
    const targets = ids.flatMap(id => {
      const account = accounts.value.find(account => account.id === id);
      if (!account || !accountIsReady(account)) return [];
      const binding = bindingFor(account);
      const slot = billing.slotFor(id).value;
      if (!refresh && slot?.loaded && !slot.error && slot.boundVersion === binding) return [];
      return [{ accountId: id, binding }];
    });
    await billing.loadMany(targets);
  }

  function ensureAccountUsage(accountId: string): Promise<void> {''')
replace('src/domain/useAccountUsage.ts', '    ensureAccountUsage,', '    ensureAccountUsage,\n    loadAccountUsageSnapshots,')
replace('src/views/Accounts.vue', '  ensureAccountUsage,', '  loadAccountUsageSnapshots,')
p = Path('src/views/Accounts.vue')
s = p.read_text()
a = s.index('async function loadUsageSnapshots(')
b = s.index('\nasync function loadRegistrationOptions(', a)
s = s[:a] + '''async function loadUsageSnapshots(list = accounts.value, refresh = false): Promise<void> {
  usageReadGeneration += 1;
  if (!accountsViewActive || route.name !== "accounts" || !sessionStore.authenticated) return;
  const ids = list.filter(account => accountIsReady(account)
    && (accountHasUsageDisplay(account) || (!providerCatalog.value && !platformStore.linkForAccount(account.id))))
    .map(account => account.id);
  await loadAccountUsageSnapshots(ids, refresh);
}
''' + s[b:]
p.write_text(s)

p = Path('src/stores/billing.test.ts')
p.write_text(p.read_text() + r'''

test("batch reads retain good data on per-account errors and ignore logged-out replies", async () => {
  setActivePinia(createPinia());
  const store = useBillingStore();
  const original = billingApi.snapshots;
  try {
    billingApi.snapshots = async ids => ({
      statuses: ids.map(accountId => billingStatus({ accountId })), errors: {}, revision: 3, processGeneration: 99,
    });
    await store.loadMany([{ accountId: "a", binding: "v1" }, { accountId: "b", binding: "v1" }]);
    const previous = store.slotFor("b").value?.status;
    billingApi.snapshots = async () => ({
      statuses: [billingStatus({ accountId: "a", revision: 4 })],
      errors: { b: { code: "internal_error", message: "failed", currentRevision: null, processGeneration: null } },
      revision: 4, processGeneration: 99,
    });
    await store.loadMany([{ accountId: "a", binding: "v1" }, { accountId: "b", binding: "v1" }]);
    assert.equal(store.slotFor("a").value?.status?.revision, 4);
    assert.equal(store.slotFor("b").value?.status, previous);
    assert.equal(store.slotFor("b").value?.error, "load_failed");
    let resolve!: (value: Awaited<ReturnType<typeof billingApi.snapshots>>) => void;
    billingApi.snapshots = () => new Promise(done => { resolve = done; });
    const pending = store.loadMany([{ accountId: "a", binding: "v2" }]);
    store.dropSession();
    resolve({ statuses: [billingStatus({ accountId: "a" })], errors: {}, revision: 3, processGeneration: 99 });
    await pending;
    assert.equal(store.slotFor("a").value, undefined);
  } finally { billingApi.snapshots = original; }
});
''')

for path, text in [
    ('docs/maintainer/runtime-invariants.md', '''\n### Account quota read cache\n\n- Rust owns a bounded, 15-second local billing projection cache, backed by persisted official evidence. SQLite local write counts, external `data_version`, settings and pricing revisions invalidate it; quota reset, credit expiry/renewal and calendar boundaries shorten its lifetime. Settlement and calibration do not need a settings revision bump to invalidate a balance. No cached value is used for inference authorization.\n- Account-page startup reads local snapshots in batches of 32 (server maximum 64), with per-account errors. Neither individual nor batch reads perform upstream I/O. Slow local projections run on blocking workers rather than Tokio I/O workers.\n- Balance refresh callers for the same operation, credential version and control revision share unfinished work. All callers recheck identity/CAS; cancellation releases work when the last waiter leaves. Manual refresh never reuses a completed flight. The existing global four-request limit, origin grants and last-good evidence remain authoritative.\n'''),
    ('docs/maintainer/runtime-invariants.zh-CN.md', '''\n### 账号额度读取缓存\n\n- Rust 持有容量有界、最长 15 秒的本地账单投影缓存，官方事实仍以持久化快照为准。SQLite 本连接写计数、其他连接的 `data_version`、配置与定价版本均使缓存失效；额度重置、积分到期或续期、日期边界会缩短有效期。扣费与校准无需增加配置版本即可使缓存失效。缓存不得用于推理授权。\n- 账号页首屏按每批 32 个读取本地快照，服务端上限为 64 个，错误按账号隔离。单账号与批量读取均不访问上游；较慢的本地投影在阻塞工作线程执行，不阻塞 Tokio 网络工作线程。\n- 相同操作、凭据版本与控制版本的余额刷新共享进行中的任务，每个调用者均重新检查身份与 CAS；最后一个等待者取消时释放任务。手动刷新不复用已完成任务。原有全局四路并发、来源授权与最后成功快照保护保持不变。\n'''),
]:
    p = Path(path)
    p.write_text(p.read_text() + text)

print('Applied scoped quota repair; run cargo fmt and generated contract tools before committing.')
