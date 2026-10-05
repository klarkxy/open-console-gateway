//! Authoritative restriction document stored in the existing `settings` table.
//!
//! There is no second database. [`transact_settings`] is the transaction facade
//! a host adapter runs on the connection `Database` already owns. Credential
//! revalidation belongs in that same transaction.

use chrono::{DateTime, Utc};
use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub const SETTINGS_KEY: &str = "cpa_policy_restrictions_v1";
const DOCUMENT_VERSION: u32 = 1;
const MAX_DOCUMENT_BYTES: usize = 256 * 1024;
const MAX_RESTRICTIONS: usize = 1024;
const MAX_ATTEMPTS: usize = 256;
const MAX_RECOVERY_ENTRIES: usize = 32;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PolicyFault {
    Malformed,
    Unavailable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Window {
    FiveHours,
    Week,
    Month,
    Free,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceSource {
    GoUsage,
    GoLimit,
    GoatPlan,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SendKind {
    Accepted,
    Validated,
}

/// Durable subject. Registration epoch and material revision are attempt fences
/// and are intentionally absent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Subject {
    Credential {
        credential_id: String,
        credential_version: u64,
        provider_id: String,
        binding_id: String,
    },
    Pool {
        pool_id: String,
        pool_version: u64,
    },
}

/// Model filter is independent of the credential or pool subject.
/// `public_model: null` is an explicit subject-wide scope, never an inference.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Scope {
    pub subject: Subject,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub public_model: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Reset {
    Known { at: DateTime<Utc> },
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InflightAdmit {
    pub request_id: Uuid,
    pub attempt_id: Uuid,
    pub at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RequestUse {
    pub request_id: Uuid,
    pub used: u32,
}

/// Frequency, concurrency, and per-request budget for one unknown reset.
/// This is not a lifetime exhaustion counter.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct UnknownRecovery {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_admit_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub inflight: Vec<InflightAdmit>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub requests: Vec<RequestUse>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Restriction {
    pub scope: Scope,
    pub window: Window,
    pub reset: Reset,
    pub observed_at: DateTime<Utc>,
    pub observation_id: String,
    pub source: EvidenceSource,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recovery: Option<UnknownRecovery>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AdmittedAttempt {
    pub request_id: Uuid,
    pub attempt_id: Uuid,
    pub credential_id: String,
    pub credential_version: u64,
    pub provider_id: String,
    pub binding_id: String,
    pub public_model: String,
    pub upstream_model: String,
    pub auth_id: String,
    pub material_revision: String,
    pub registration_epoch: u64,
    pub process_generation: u64,
    pub projection_revision: u64,
    pub projection_digest: [u8; 32],
    pub kind: SendKind,
    pub admitted_at: DateTime<Utc>,
    pub retain_until: DateTime<Utc>,
    pub scopes: Vec<Scope>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct StoredDocument {
    version: u32,
    restrictions: Vec<Restriction>,
    #[serde(default)]
    attempts: Vec<AdmittedAttempt>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PolicyDocument {
    pub restrictions: Vec<Restriction>,
    pub attempts: Vec<AdmittedAttempt>,
}

impl PolicyDocument {
    pub fn from_json(bytes: &str) -> Result<Self, PolicyFault> {
        let stored: StoredDocument =
            serde_json::from_str(bytes).map_err(|_| PolicyFault::Malformed)?;
        if stored.version != DOCUMENT_VERSION
            || stored.restrictions.len() > MAX_RESTRICTIONS
            || stored.attempts.len() > MAX_ATTEMPTS
        {
            return Err(PolicyFault::Malformed);
        }
        if stored.restrictions.iter().any(restriction_invalid) {
            return Err(PolicyFault::Malformed);
        }
        Ok(Self {
            restrictions: stored.restrictions,
            attempts: stored.attempts,
        })
    }

    pub fn to_json(&self) -> Result<String, PolicyFault> {
        if self.restrictions.len() > MAX_RESTRICTIONS || self.attempts.len() > MAX_ATTEMPTS {
            return Err(PolicyFault::Unavailable);
        }
        let stored = StoredDocument {
            version: DOCUMENT_VERSION,
            restrictions: self.restrictions.clone(),
            attempts: self.attempts.clone(),
        };
        let json = serde_json::to_string(&stored).map_err(|_| PolicyFault::Unavailable)?;
        if json.len() > MAX_DOCUMENT_BYTES {
            return Err(PolicyFault::Unavailable);
        }
        Ok(json)
    }
}

fn restriction_invalid(row: &Restriction) -> bool {
    let unknown = matches!(row.reset, Reset::Unknown);
    if unknown != row.recovery.is_some() {
        return true;
    }
    if let Some(recovery) = &row.recovery {
        if recovery.inflight.len() > MAX_RECOVERY_ENTRIES
            || recovery.requests.len() > MAX_RECOVERY_ENTRIES
        {
            return true;
        }
    }
    row.observation_id.is_empty()
}

/// Persistence facade. `update` commits before returning. Implementations must
/// not call back into [`super::PolicyService`] or perform network I/O.
/// The transaction handed to the mutate callback is the place to re-read
/// credential version, binding, grants, and pool membership.
pub trait PolicyStore: Send + Sync {
    fn read(
        &self,
        read: &mut dyn for<'tx> FnMut(
            &Transaction<'tx>,
            &PolicyDocument,
        ) -> Result<(), PolicyFault>,
    ) -> Result<(), PolicyFault>;
    fn update(
        &self,
        mutate: &mut dyn for<'tx> FnMut(
            &Transaction<'tx>,
            &mut PolicyDocument,
        ) -> Result<(), PolicyFault>,
    ) -> Result<(), PolicyFault>;
}

/// Read the settings row. A missing key is an empty document. Invalid JSON or
/// an unexpected version is [`PolicyFault::Malformed`] and must not be treated
/// as "no restrictions".
pub fn read_settings(conn: &Connection, key: &str) -> Result<PolicyDocument, PolicyFault> {
    let value = query_setting(conn, key)?;
    match value {
        None => Ok(PolicyDocument::default()),
        Some(json) => PolicyDocument::from_json(&json),
    }
}

/// Read inside one deferred transaction. `read` sees the document and may
/// revalidate credential rows on the same transaction. Nothing is written.
pub fn read_transaction(
    conn: &mut Connection,
    key: &str,
    read: &mut dyn for<'tx> FnMut(&Transaction<'tx>, &PolicyDocument) -> Result<(), PolicyFault>,
) -> Result<(), PolicyFault> {
    let tx = conn.transaction().map_err(|_| PolicyFault::Unavailable)?;
    let document = read_in_transaction(&tx, key)?;
    read(&tx, &document)?;
    tx.commit().map_err(|_| PolicyFault::Unavailable)?;
    Ok(())
}

/// Insert or replace the settings row inside one immediate transaction.
/// `mutate` errors roll the transaction back.
pub fn transact_settings(
    conn: &mut Connection,
    key: &str,
    mutate: &mut dyn for<'tx> FnMut(
        &Transaction<'tx>,
        &mut PolicyDocument,
    ) -> Result<(), PolicyFault>,
) -> Result<(), PolicyFault> {
    let tx = conn
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(|_| PolicyFault::Unavailable)?;
    let mut document = read_in_transaction(&tx, key)?;
    mutate(&tx, &mut document)?;
    let json = document.to_json()?;
    tx.execute(
        "INSERT OR REPLACE INTO settings (key, value) VALUES (?1, ?2)",
        rusqlite::params![key, json],
    )
    .map_err(|_| PolicyFault::Unavailable)?;
    tx.commit().map_err(|_| PolicyFault::Unavailable)?;
    Ok(())
}

fn query_setting(conn: &Connection, key: &str) -> Result<Option<String>, PolicyFault> {
    conn.query_row("SELECT value FROM settings WHERE key = ?1", [key], |row| {
        row.get(0)
    })
    .optional()
    .map_err(|_| PolicyFault::Unavailable)
}

pub(super) fn read_in_transaction(
    tx: &Transaction<'_>,
    key: &str,
) -> Result<PolicyDocument, PolicyFault> {
    let value: Option<String> = tx
        .query_row("SELECT value FROM settings WHERE key = ?1", [key], |row| {
            row.get(0)
        })
        .optional()
        .map_err(|_| PolicyFault::Unavailable)?;
    match value {
        None => Ok(PolicyDocument::default()),
        Some(json) => PolicyDocument::from_json(&json),
    }
}

pub(crate) fn remember_request(recovery: &mut UnknownRecovery, request_id: Uuid) -> bool {
    if let Some(request) = recovery
        .requests
        .iter_mut()
        .find(|item| item.request_id == request_id)
    {
        request.used = request.used.saturating_add(1);
        return true;
    }
    while recovery.requests.len() >= MAX_RECOVERY_ENTRIES {
        let inflight: Vec<Uuid> = recovery
            .inflight
            .iter()
            .map(|item| item.request_id)
            .collect();
        let Some(index) = recovery
            .requests
            .iter()
            .position(|item| !inflight.contains(&item.request_id))
        else {
            return false;
        };
        recovery.requests.remove(index);
    }
    recovery.requests.push(RequestUse {
        request_id,
        used: 1,
    });
    if recovery.inflight.len() > MAX_RECOVERY_ENTRIES {
        let overflow = recovery.inflight.len() - MAX_RECOVERY_ENTRIES;
        recovery.inflight.drain(0..overflow);
    }
    true
}
