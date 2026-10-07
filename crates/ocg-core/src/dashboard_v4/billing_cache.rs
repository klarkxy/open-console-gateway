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
            changes: db
                .conn
                .query_row("SELECT total_changes()", [], |row| row.get(0))?,
            data_version: db
                .conn
                .pragma_query_value(None, "data_version", |row| row.get(0))?,
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
    pub(super) fn get(
        &mut self,
        id: &str,
        version: &ReadVersion,
        now: DateTime<Utc>,
    ) -> Option<BillingStatus> {
        self.bind(version);
        let entry = self.entries.get(id)?;
        if entry.inserted.elapsed() >= MAX_AGE || now < entry.sampled_at || now >= entry.valid_until
        {
            self.entries.remove(id);
            return None;
        }
        Some(entry.status.clone())
    }
    pub(super) fn shorten_lifetime(&mut self, id: &str, at: DateTime<Utc>) {
        if let Some(entry) = self.entries.get_mut(id) {
            entry.valid_until = entry.valid_until.min(at);
        }
    }
    pub(super) fn insert(
        &mut self,
        version: &ReadVersion,
        status: BillingStatus,
        now: DateTime<Utc>,
    ) {
        self.bind(version);
        if self.entries.len() >= MAX_ENTRIES
            && !self.entries.contains_key(&status.account_id)
            && let Some(oldest) = self
                .entries
                .iter()
                .min_by_key(|(_, value)| value.inserted)
                .map(|(id, _)| id.clone())
        {
            self.entries.remove(&oldest);
        }
        let valid_until = next_change(&status, now);
        self.entries.insert(
            status.account_id.clone(),
            Entry {
                status,
                inserted: Instant::now(),
                sampled_at: now,
                valid_until,
            },
        );
    }
}

pub(super) fn next_change(status: &BillingStatus, now: DateTime<Utc>) -> DateTime<Utc> {
    let mut deadline = now + Duration::seconds(15);
    let mut consider = |at: DateTime<Utc>| {
        if at > now {
            deadline = deadline.min(at);
        }
    };
    if let Some(usage) = &status.usage {
        for at in usage
            .quota_windows
            .iter()
            .filter_map(|window| window.resets_at.as_deref())
            .chain(usage.free_cooldown_until.as_deref())
        {
            if let Ok(at) = DateTime::parse_from_rfc3339(at) {
                consider(at.with_timezone(&Utc));
            }
        }
    }
    if let Some(credits) = &status.credits {
        for bucket in &credits.buckets {
            consider(bucket.starts_at);
            if let Some(at) = bucket.expires_at {
                consider(at);
            }
        }
        if let Some(at) = credits.next_reset_at {
            consider(at);
        }
    }
    for limit in &status.quota_editor_limits {
        if let Some(at) = limit.editable_at {
            consider(at);
        }
    }
    // Calendar-based presets and month-to-date projections must not survive midnight.
    if let Some(tomorrow) = now
        .date_naive()
        .succ_opt()
        .and_then(|day| day.and_hms_opt(0, 0, 0))
    {
        consider(tomorrow.and_utc());
    }
    deadline
}

#[cfg(test)]
mod tests;
