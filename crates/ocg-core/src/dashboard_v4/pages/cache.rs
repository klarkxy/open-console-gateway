use super::*;
use std::hash::{Hash, Hasher};
use std::time::Instant;

#[derive(Debug, Clone)]
pub(super) struct ReadVersion {
    generation: u64,
    revision: u64,
    changes: u64,
    data: u64,
    domain: u64,
    runtime: u64,
    pricing: String,
}
impl PartialEq for ReadVersion {
    fn eq(&self, other: &Self) -> bool {
        self.generation == other.generation
            && self.revision == other.revision
            && self.domain == other.domain
            && self.runtime == other.runtime
            && self.pricing == other.pricing
    }
}
impl Eq for ReadVersion {}
impl ReadVersion {
    pub(super) fn capture(state: &CoreState) -> Result<Self, V3ApiError> {
        let db = state.db.lock();
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        format!("{:?}", state.quota_probes.lock()).hash(&mut hasher);
        // models.dev metadata may be reloaded without a settings mutation.
        (Arc::as_ptr(&state.modelsdev_catalog()) as usize).hash(&mut hasher);
        let cpa = state.cpa_runtime_snapshot();
        (
            cpa.owned,
            cpa.installed,
            cpa.running,
            cpa.current_version,
            cpa.asset_sha256,
            cpa.error.is_some(),
            format!("{:?}", cpa.phase),
        )
            .hash(&mut hasher);
        let changes = db
            .conn
            .query_row("SELECT total_changes()", [], |r| r.get(0))
            .map_err(V3ApiError::internal)?;
        let data = db
            .conn
            .pragma_query_value(None, "data_version", |r| r.get(0))
            .map_err(V3ApiError::internal)?;
        let previous = state
            .management_page_cache
            .lock()
            .entry
            .as_ref()
            .map(|(v, _, _)| v.clone());
        let domain =
            if let Some(previous) = previous.filter(|v| v.changes == changes && v.data == data) {
                previous.domain
            } else {
                source_fingerprint(&db)?
            };
        Ok(Self {
            generation: state.process_generation(),
            revision: state.settings_revision(),
            changes,
            data,
            domain,
            runtime: hasher.finish(),
            pricing: state.pricing_snapshot().revision.clone(),
        })
    }
    pub(super) fn token(&self) -> String {
        let mut h = std::collections::hash_map::DefaultHasher::new();
        (
            self.generation,
            self.revision,
            self.domain,
            self.runtime,
            &self.pricing,
        )
            .hash(&mut h);
        format!("{:016x}", h.finish())
    }
}

/// Telemetry appends are not management mutations. SQLite counters detect
/// writes first; a local source fingerprint decides whether page facts changed.
/// Billing projections are observed at asOf and expire in at most 15 seconds.
fn source_fingerprint(db: &crate::db::Database) -> Result<u64, V3ApiError> {
    use rusqlite::types::ValueRef;
    let tx = db
        .conn
        .unchecked_transaction()
        .map_err(V3ApiError::internal)?;
    let tables = {
        let mut query = tx
            .prepare("SELECT name FROM sqlite_master WHERE type='table' ORDER BY name")
            .map_err(V3ApiError::internal)?;
        query
            .query_map([], |r| r.get::<_, String>(0))
            .map_err(V3ApiError::internal)?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(V3ApiError::internal)?
    };
    let mut hash = std::collections::hash_map::DefaultHasher::new();
    for table in tables {
        if table.starts_with("sqlite_")
            || table.ends_with("_logs")
            || matches!(
                table.as_str(),
                "request_attempts"
                    | "dashboard_operations"
                    | "access_keys"
                    | "sub_gateway_keys"
                    | "schema_version"
            )
        {
            continue;
        }
        table.hash(&mut hash);
        let quoted = table.replace('"', "\"\"");
        let mut stmt = tx
            .prepare(&format!("SELECT * FROM \"{quoted}\" ORDER BY rowid"))
            .map_err(V3ApiError::internal)?;
        let columns = stmt.column_count();
        let mut rows = stmt.query([]).map_err(V3ApiError::internal)?;
        while let Some(row) = rows.next().map_err(V3ApiError::internal)? {
            for index in 0..columns {
                match row.get_ref(index).map_err(V3ApiError::internal)? {
                    ValueRef::Null => 0_u8.hash(&mut hash),
                    ValueRef::Integer(v) => {
                        1_u8.hash(&mut hash);
                        v.hash(&mut hash);
                    }
                    ValueRef::Real(v) => {
                        2_u8.hash(&mut hash);
                        v.to_bits().hash(&mut hash);
                    }
                    ValueRef::Text(v) => {
                        3_u8.hash(&mut hash);
                        v.hash(&mut hash);
                    }
                    ValueRef::Blob(v) => {
                        4_u8.hash(&mut hash);
                        v.hash(&mut hash);
                    }
                }
            }
        }
    }
    tx.commit().map_err(V3ApiError::internal)?;
    Ok(hash.finish())
}
#[derive(Default)]
pub(crate) struct PageReadCache {
    entry: Option<(ReadVersion, Instant, Arc<PageSnapshot>)>,
}
impl PageReadCache {
    pub(super) fn get(
        &mut self,
        version: &ReadVersion,
        now: DateTime<Utc>,
    ) -> Option<Arc<PageSnapshot>> {
        let (cached, at, snapshot) = self.entry.as_ref()?;
        if cached != version
            || at.elapsed() >= std::time::Duration::from_secs(15)
            || now < snapshot.as_of
            || now >= snapshot.valid_until
        {
            self.entry = None;
            return None;
        }
        let result = snapshot.clone();
        if let Some((cached, _, _)) = &mut self.entry {
            cached.changes = version.changes;
            cached.data = version.data;
        }
        Some(result)
    }
    pub(super) fn insert(&mut self, version: ReadVersion, snapshot: Arc<PageSnapshot>) {
        self.entry = Some((version, Instant::now(), snapshot));
    }
}
