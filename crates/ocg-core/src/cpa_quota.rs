//! Official OpenCode Go quota applied to the existing CPA policy document.
//!
//! The refresh captures identity before HTTP. [`apply_accepted`] re-reads that
//! identity inside the settings transaction and calls
//! `cpa_policy::apply_official_quota_on_connection` on this connection. That
//! helper and `PolicyService::apply_official_quota` share one mutation.
//! Rounded usage minutes never enter this path.

use std::collections::HashMap;
use std::fmt;

use chrono::{DateTime, Utc};
use ocg_domain::credential::ModelScope;
use ocg_domain::ids::OPENCODE_PROVIDER_ID;
use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior};
use serde_json::Value;

use crate::cpa_policy::{
    AppliedProjection, CurrentCredential, CurrentFacts, DeclaredScope, DeclaredSubject,
    OfficialQuotaObservation, OpportunityPolicy, PolicyDocument, PolicyFacts, PolicyFault,
    PoolMembership, QuotaApply, Reset, Restriction, SETTINGS_KEY, Scope, Subject, Window,
    read_settings,
};
use crate::routing_snapshot::ExecutionCredential;

const POOL_VERSION: u64 = 1;
const MATERIAL_REVISION: &str = "quota-fence";
const UNFENCED: &str = "unfenced";

/// Result of the leased persistence hook. `Unhooked` keeps the legacy
/// quota-recovery reconcile until `state` implements the method.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OfficialPlanHook {
    Unhooked,
    Applied,
    Stale,
    Rejected,
}

/// Pre-HTTP credential fence. `key_cipher` is compared and never logged.
#[derive(Clone, PartialEq, Eq)]
pub struct QuotaFence {
    pub credential_id: String,
    pub legacy_account_id: String,
    pub credential_version: u64,
    pub provider_id: String,
    pub binding_id: String,
    pub key_cipher: String,
    pub scope: ModelScope,
    pub endpoint_ids: Vec<String>,
    pub origins: Vec<String>,
    pub pool_ids: Vec<String>,
}

impl fmt::Debug for QuotaFence {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("QuotaFence")
            .field("credential_id", &self.credential_id)
            .field("legacy_account_id", &self.legacy_account_id)
            .field("credential_version", &self.credential_version)
            .field("provider_id", &self.provider_id)
            .field("binding_id", &self.binding_id)
            .field("key_cipher", &"[redacted]")
            .field("scope", &self.scope)
            .field("endpoint_ids", &self.endpoint_ids)
            .field("origins", &self.origins)
            .field("pool_ids", &self.pool_ids)
            .finish()
    }
}

/// Accepted official usage body plus the fence captured before the fetch.
pub struct OfficialPlanCommit {
    pub fence: QuotaFence,
    pub observation_id: String,
    pub fetched_at: DateTime<Utc>,
    pub body: Vec<u8>,
    pub provider_id: String,
}

impl fmt::Debug for OfficialPlanCommit {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("OfficialPlanCommit")
            .field("fence", &self.fence)
            .field("observation_id", &self.observation_id)
            .field("fetched_at", &self.fetched_at)
            .field("body_len", &self.body.len())
            .field("provider_id", &self.provider_id)
            .finish()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PlanStatus {
    Waiting,
    Ready,
    Probing,
}

/// Card window. Official `Free` has no `QuotaRecoveryDto` variant.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PlanWindowLabel {
    FiveHours,
    Week,
    Month,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PlanCard {
    pub status: PlanStatus,
    pub window: PlanWindowLabel,
    pub observed_at: DateTime<Utc>,
    pub resets_at: Option<DateTime<Utc>>,
    pub next_retry_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PlanRow {
    pub id: String,
    pub window: Window,
    pub public_model: Option<String>,
    pub status: PlanStatus,
    pub observed_at: DateTime<Utc>,
    pub resets_at: Option<DateTime<Utc>>,
    pub next_retry_at: DateTime<Utc>,
    pub probe_in_flight: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct EffectivePlan {
    pub credential_id: String,
    pub destination_id: String,
    pub card: PlanCard,
    pub rows: Vec<PlanRow>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ManualOpportunity {
    Opened,
    Unchanged,
    NotRestricted,
}

/// Sorted pool ids for the pre-HTTP fence. A missing table is an empty set.
pub(crate) fn pool_ids(
    conn: &Connection,
    credential_id: &str,
    legacy_account_id: &str,
) -> anyhow::Result<Vec<String>> {
    match read_pools(conn, credential_id, legacy_account_id) {
        Ok(PoolRead::MissingTable) => Ok(Vec::new()),
        Ok(PoolRead::Ids(ids)) => Ok(ids),
        Err(_) => anyhow::bail!("quota pool membership is unavailable"),
    }
}

/// Fence from the routing credential captured before HTTP, plus pool ids.
pub(crate) fn fence_with_pools(
    credential: &ExecutionCredential,
    pool_ids: &[String],
) -> QuotaFence {
    let mut endpoints = credential.grants.allowed_endpoint_ids.clone();
    let mut origins = credential.grants.allowed_origins.clone();
    endpoints.sort();
    origins.sort();
    QuotaFence {
        credential_id: credential.credential_id.clone(),
        legacy_account_id: credential.id.clone(),
        credential_version: credential.credential_version,
        provider_id: credential.provider_id.clone(),
        binding_id: credential.binding_id.clone(),
        key_cipher: credential.key_cipher.clone(),
        scope: credential.scope.clone(),
        endpoint_ids: endpoints,
        origins,
        pool_ids: sorted_unique(pool_ids.iter().cloned().collect()),
    }
}

/// Current row as a fence. Used by tests and manual callers that did not fetch.
pub(crate) fn capture_live_fence(
    conn: &Connection,
    credential_id: &str,
) -> Result<QuotaFence, PolicyFault> {
    let Some(row) = load_row(conn, credential_id)? else {
        return Err(PolicyFault::Unavailable);
    };
    let scope = parse_scope(&row.scope_json).ok_or(PolicyFault::Malformed)?;
    let (endpoint_ids, origins) = match read_grants(conn, credential_id)? {
        GrantRead::MissingTable => (Vec::new(), Vec::new()),
        GrantRead::Rows { endpoints, origins } => (endpoints, origins),
        GrantRead::UnknownKind => return Err(PolicyFault::Malformed),
    };
    let pool_ids = match read_pools(conn, credential_id, &row.legacy_account_id)? {
        PoolRead::MissingTable => Vec::new(),
        PoolRead::Ids(ids) => ids,
    };
    Ok(QuotaFence {
        credential_id: row.credential_id,
        legacy_account_id: row.legacy_account_id,
        credential_version: row.version,
        provider_id: row.provider_id,
        binding_id: row.binding_id,
        key_cipher: row.key_cipher,
        scope,
        endpoint_ids,
        origins,
        pool_ids,
    })
}

/// Apply one accepted body on this connection. No network.
pub(crate) fn apply_accepted(
    conn: &Connection,
    commit: &OfficialPlanCommit,
) -> Result<QuotaApply, PolicyFault> {
    apply_on_connection(conn, commit)
}

/// Same connection as [`apply_accepted`]. The database mutex already serializes
/// `conn`. The policy helper opens the immediate transaction there.
pub(crate) fn apply_sharing(
    conn: &Connection,
    commit: &OfficialPlanCommit,
) -> Result<QuotaApply, PolicyFault> {
    apply_on_connection(conn, commit)
}

fn apply_on_connection(
    conn: &Connection,
    commit: &OfficialPlanCommit,
) -> Result<QuotaApply, PolicyFault> {
    let mut facts = LiveFacts {
        fence: commit.fence.clone(),
        now: commit.fetched_at,
    };
    crate::cpa_policy::apply_official_quota_on_connection(
        conn,
        &OfficialQuotaObservation {
            observation_id: commit.observation_id.clone(),
            fetched_at: commit.fetched_at,
            body: commit.body.clone(),
            provider_id: commit.provider_id.clone(),
        },
        &mut facts,
    )
}

pub(crate) fn present_all(
    conn: &Connection,
    now: DateTime<Utc>,
) -> Result<HashMap<String, EffectivePlan>, PolicyFault> {
    let document = read_settings(conn, SETTINGS_KEY)?;
    let recovery = recovery_facts(&document)?;
    let mut plans = HashMap::new();
    for row in list_rows(conn)? {
        if let Some(plan) = plan_for_row(conn, &document, &recovery, &row, now)? {
            plans.insert(plan.credential_id.clone(), plan);
        }
    }
    Ok(plans)
}

pub(crate) fn present_credential(
    conn: &Connection,
    credential_id: &str,
    now: DateTime<Utc>,
) -> Result<Option<EffectivePlan>, PolicyFault> {
    let Some(row) = load_row(conn, credential_id)? else {
        return Ok(None);
    };
    let document = read_settings(conn, SETTINGS_KEY)?;
    let recovery = recovery_facts(&document)?;
    plan_for_row(conn, &document, &recovery, &row, now)
}

/// Clear only the unknown-reset gap clock on applicable rows.
/// A known deadline and a live inflight attempt stay.
pub(crate) fn mark_manual_opportunity(
    conn: &mut Connection,
    credential_id: &str,
    now: DateTime<Utc>,
) -> Result<ManualOpportunity, PolicyFault> {
    let tx = conn
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(|_| PolicyFault::Unavailable)?;
    let mut document = load_document_tx(&tx)?;
    let Some(row) = load_row(&tx, credential_id)? else {
        tx.rollback().map_err(|_| PolicyFault::Unavailable)?;
        return Ok(ManualOpportunity::NotRestricted);
    };
    let scopes = authorized_scopes(&tx, &row)?;
    if scopes.is_empty() || !document_has_scope(&document, &scopes) {
        tx.rollback().map_err(|_| PolicyFault::Unavailable)?;
        return Ok(ManualOpportunity::NotRestricted);
    }
    let changed = clear_unknown_gap(&mut document, &scopes)?;
    if !changed {
        tx.rollback().map_err(|_| PolicyFault::Unavailable)?;
        return Ok(ManualOpportunity::Unchanged);
    }
    let json = document.to_json()?;
    tx.execute(
        "INSERT OR REPLACE INTO settings (key, value) VALUES (?1, ?2)",
        rusqlite::params![SETTINGS_KEY, json],
    )
    .map_err(|_| PolicyFault::Unavailable)?;
    tx.commit().map_err(|_| PolicyFault::Unavailable)?;
    Ok(ManualOpportunity::Opened)
}

pub(crate) fn is_plan_restriction_id(id: &str) -> bool {
    id.starts_with("plan:")
}

struct LiveFacts {
    fence: QuotaFence,
    now: DateTime<Utc>,
}

impl CurrentFacts for LiveFacts {
    fn revalidate(&mut self, tx: &Transaction<'_>) -> Result<PolicyFacts, PolicyFault> {
        let current = match load_row(tx, &self.fence.credential_id)? {
            None => placeholder(&self.fence, true, false),
            Some(row) => {
                if identity_matches(tx, &self.fence, &row)? {
                    matched_current(tx, &row)?
                } else {
                    placeholder(&self.fence, false, true)
                }
            }
        };
        Ok(PolicyFacts {
            applied: AppliedProjection {
                process_generation: 0,
                revision: 0,
                digest: [0; 32],
            },
            attempt: None,
            current: Some(current),
            deadline_at: self.now,
            now: self.now,
            caller_stop: None,
            validated_pin: false,
        })
    }
}

struct LiveRow {
    credential_id: String,
    legacy_account_id: String,
    destination_id: String,
    provider_id: String,
    binding_id: String,
    version: u64,
    key_cipher: String,
    enabled: bool,
    scope_json: String,
}

struct RecoveryFacts {
    last_admit_at: Vec<Option<DateTime<Utc>>>,
    inflight: Vec<Vec<String>>,
}

fn identity_matches(
    conn: &Connection,
    fence: &QuotaFence,
    row: &LiveRow,
) -> Result<bool, PolicyFault> {
    if fence.provider_id != OPENCODE_PROVIDER_ID
        || row.provider_id != fence.provider_id
        || fence.binding_id.is_empty()
        || row.binding_id != fence.binding_id
        || fence.credential_version == 0
        || row.version == 0
        || row.version != fence.credential_version
        || fence.key_cipher.is_empty()
        || row.key_cipher != fence.key_cipher
        || row.credential_id != fence.credential_id
        || row.legacy_account_id != fence.legacy_account_id
    {
        return Ok(false);
    }
    let Some(scope) = parse_scope(&row.scope_json) else {
        return Ok(false);
    };
    if scope != fence.scope || !scope_usable(&scope) {
        return Ok(false);
    }
    if !grants_match(
        conn,
        &row.credential_id,
        &fence.endpoint_ids,
        &fence.origins,
    )? {
        return Ok(false);
    }
    pools_match(
        conn,
        &row.credential_id,
        &row.legacy_account_id,
        &fence.pool_ids,
    )
}

fn matched_current(conn: &Connection, row: &LiveRow) -> Result<CurrentCredential, PolicyFault> {
    let (credential_scopes, pool_scopes, memberships) = declared_scopes(conn, row)?;
    let mut scopes = credential_scopes;
    scopes.extend(pool_scopes);
    if scopes.is_empty() {
        return Ok(placeholder_from_row(row, true));
    }
    let granted = granted(conn, &row.credential_id)?;
    Ok(CurrentCredential {
        credential_id: row.credential_id.clone(),
        credential_version: row.version,
        provider_id: row.provider_id.clone(),
        binding_id: row.binding_id.clone(),
        material_revision: MATERIAL_REVISION.to_string(),
        registration_epoch: 0,
        auth_id: row.credential_id.clone(),
        memberships,
        scopes,
        granted,
        enabled: row.enabled,
        deleted: false,
        rebound: false,
    })
}

fn placeholder(fence: &QuotaFence, deleted: bool, rebound: bool) -> CurrentCredential {
    CurrentCredential {
        credential_id: nonempty(&fence.credential_id),
        credential_version: fence.credential_version.max(1),
        provider_id: nonempty_or(&fence.provider_id, OPENCODE_PROVIDER_ID),
        binding_id: nonempty_or(&fence.binding_id, UNFENCED),
        material_revision: MATERIAL_REVISION.to_string(),
        registration_epoch: 0,
        auth_id: nonempty(&fence.credential_id),
        memberships: Vec::new(),
        scopes: vec![DeclaredScope {
            subject: DeclaredSubject::Credential,
            public_model: None,
        }],
        granted: false,
        enabled: false,
        deleted,
        rebound,
    }
}

fn placeholder_from_row(row: &LiveRow, rebound: bool) -> CurrentCredential {
    CurrentCredential {
        credential_id: nonempty(&row.credential_id),
        credential_version: row.version.max(1),
        provider_id: nonempty_or(&row.provider_id, OPENCODE_PROVIDER_ID),
        binding_id: nonempty_or(&row.binding_id, UNFENCED),
        material_revision: MATERIAL_REVISION.to_string(),
        registration_epoch: 0,
        auth_id: nonempty(&row.credential_id),
        memberships: Vec::new(),
        scopes: vec![DeclaredScope {
            subject: DeclaredSubject::Credential,
            public_model: None,
        }],
        granted: false,
        enabled: row.enabled,
        deleted: false,
        rebound,
    }
}

fn authorized_scopes(conn: &Connection, row: &LiveRow) -> Result<Vec<Scope>, PolicyFault> {
    let Some(scope) = parse_scope(&row.scope_json) else {
        return Ok(Vec::new());
    };
    if !scope_usable(&scope) || row.version == 0 || row.binding_id.is_empty() {
        return Ok(Vec::new());
    }
    let (credential_scopes, pool_scopes, memberships) = declared_scopes(conn, row)?;
    let mut declared = credential_scopes;
    declared.extend(pool_scopes);
    let mut scopes = Vec::with_capacity(declared.len());
    for item in declared {
        match &item.subject {
            DeclaredSubject::Credential => scopes.push(Scope {
                subject: Subject::Credential {
                    credential_id: row.credential_id.clone(),
                    credential_version: row.version,
                    provider_id: row.provider_id.clone(),
                    binding_id: row.binding_id.clone(),
                },
                public_model: item.public_model.clone(),
            }),
            DeclaredSubject::Pool {
                pool_id,
                pool_version,
            } => {
                let Some(member) = memberships.iter().find(|member| {
                    member.pool_id == *pool_id && member.pool_version == *pool_version
                }) else {
                    return Err(PolicyFault::Unavailable);
                };
                let covered = match &item.public_model {
                    Some(model) => {
                        member.all_models || member.public_models.iter().any(|item| item == model)
                    }
                    None => member.all_models,
                };
                if !covered {
                    return Err(PolicyFault::Unavailable);
                }
                scopes.push(Scope {
                    subject: Subject::Pool {
                        pool_id: pool_id.clone(),
                        pool_version: *pool_version,
                    },
                    public_model: item.public_model.clone(),
                });
            }
        }
    }
    Ok(scopes)
}

fn declared_scopes(
    conn: &Connection,
    row: &LiveRow,
) -> Result<(Vec<DeclaredScope>, Vec<DeclaredScope>, Vec<PoolMembership>), PolicyFault> {
    let Some(scope) = parse_scope(&row.scope_json) else {
        return Ok((Vec::new(), Vec::new(), Vec::new()));
    };
    if !scope_usable(&scope) {
        return Ok((Vec::new(), Vec::new(), Vec::new()));
    }
    let credential_scopes = match &scope {
        ModelScope::All => vec![DeclaredScope {
            subject: DeclaredSubject::Credential,
            public_model: None,
        }],
        ModelScope::Only { models } => models
            .iter()
            .map(|model| DeclaredScope {
                subject: DeclaredSubject::Credential,
                public_model: Some(model.clone()),
            })
            .collect(),
    };
    let pools = match read_pools(conn, &row.credential_id, &row.legacy_account_id)? {
        PoolRead::MissingTable => Vec::new(),
        PoolRead::Ids(ids) => ids,
    };
    if pools.is_empty() {
        return Ok((credential_scopes, Vec::new(), Vec::new()));
    }
    let mut pool_scopes = Vec::new();
    let mut members = Vec::new();
    for pool_id in pools {
        match &scope {
            ModelScope::All => {
                members.push(PoolMembership {
                    pool_id: pool_id.clone(),
                    pool_version: POOL_VERSION,
                    public_models: Vec::new(),
                    all_models: true,
                });
                pool_scopes.push(DeclaredScope {
                    subject: DeclaredSubject::Pool {
                        pool_id,
                        pool_version: POOL_VERSION,
                    },
                    public_model: None,
                });
            }
            ModelScope::Only { models } => {
                members.push(PoolMembership {
                    pool_id: pool_id.clone(),
                    pool_version: POOL_VERSION,
                    public_models: models.clone(),
                    all_models: false,
                });
                for model in models {
                    pool_scopes.push(DeclaredScope {
                        subject: DeclaredSubject::Pool {
                            pool_id: pool_id.clone(),
                            pool_version: POOL_VERSION,
                        },
                        public_model: Some(model.clone()),
                    });
                }
            }
        }
    }
    Ok((credential_scopes, pool_scopes, members))
}

fn plan_for_row(
    conn: &Connection,
    document: &PolicyDocument,
    recovery: &RecoveryFacts,
    row: &LiveRow,
    now: DateTime<Utc>,
) -> Result<Option<EffectivePlan>, PolicyFault> {
    let scopes = authorized_scopes(conn, row)?;
    let mut plan_rows = Vec::new();
    for (index, restriction) in document.restrictions.iter().enumerate() {
        if scopes.iter().any(|scope| scope == &restriction.scope) {
            plan_rows.push(row_view(
                document,
                recovery,
                index,
                &row.credential_id,
                now,
            )?);
        }
    }
    if plan_rows.is_empty() {
        return Ok(None);
    }
    plan_rows.sort_by(|left, right| left.id.cmp(&right.id));
    let card = collapse_card(&plan_rows, now);
    Ok(Some(EffectivePlan {
        credential_id: row.credential_id.clone(),
        destination_id: row.destination_id.clone(),
        card,
        rows: plan_rows,
    }))
}

fn row_view(
    document: &PolicyDocument,
    recovery: &RecoveryFacts,
    index: usize,
    credential_id: &str,
    now: DateTime<Utc>,
) -> Result<PlanRow, PolicyFault> {
    let restriction = document
        .restrictions
        .get(index)
        .ok_or(PolicyFault::Malformed)?;
    let (status, resets_at, next_retry_at, probe_in_flight) = match restriction.reset {
        Reset::Known { at } if at > now => (PlanStatus::Waiting, Some(at), at, false),
        Reset::Known { at } => (PlanStatus::Ready, Some(at), now, false),
        Reset::Unknown => unknown_view(document, recovery, index, now),
    };
    Ok(PlanRow {
        id: plan_id(credential_id, restriction),
        window: restriction.window,
        public_model: restriction.scope.public_model.clone(),
        status,
        observed_at: restriction.observed_at,
        resets_at,
        next_retry_at,
        probe_in_flight,
    })
}

fn unknown_view(
    document: &PolicyDocument,
    recovery: &RecoveryFacts,
    index: usize,
    now: DateTime<Utc>,
) -> (PlanStatus, Option<DateTime<Utc>>, DateTime<Utc>, bool) {
    if let Some(until) = live_inflight_until(document, recovery, index, now) {
        return (PlanStatus::Probing, None, until, true);
    }
    if let Some(last) = recovery.last_admit_at.get(index).copied().flatten() {
        if let Some(until) = last.checked_add_signed(gap()) {
            if until > now {
                return (PlanStatus::Waiting, None, until, false);
            }
        }
    }
    (PlanStatus::Ready, None, now, false)
}

fn live_inflight_until(
    document: &PolicyDocument,
    recovery: &RecoveryFacts,
    index: usize,
    now: DateTime<Utc>,
) -> Option<DateTime<Utc>> {
    let ids = recovery.inflight.get(index)?;
    let mut earliest: Option<DateTime<Utc>> = None;
    for id in ids {
        let Ok(attempt_id) = uuid::Uuid::parse_str(id) else {
            continue;
        };
        let Some(attempt) = document
            .attempts
            .iter()
            .find(|row| row.attempt_id == attempt_id)
        else {
            continue;
        };
        if attempt.retain_until > now {
            earliest = Some(match earliest {
                Some(current) => current.min(attempt.retain_until),
                None => attempt.retain_until,
            });
        }
    }
    earliest
}

fn collapse_card(rows: &[PlanRow], now: DateTime<Utc>) -> PlanCard {
    let known_future = rows.iter().filter(|row| {
        row.status == PlanStatus::Waiting && row.resets_at.is_some_and(|at| at > now)
    });
    if let Some(winner) = known_future.max_by(|left, right| {
        left.resets_at
            .cmp(&right.resets_at)
            .then(window_rank(left.window).cmp(&window_rank(right.window)))
            .then(left.observed_at.cmp(&right.observed_at))
    }) {
        return card_from(winner);
    }
    if let Some(winner) = rows
        .iter()
        .filter(|row| row.probe_in_flight)
        .min_by_key(|row| row.next_retry_at)
    {
        return card_from(winner);
    }
    if let Some(winner) = rows
        .iter()
        .filter(|row| row.status == PlanStatus::Waiting)
        .max_by_key(|row| row.next_retry_at)
    {
        return card_from(winner);
    }
    card_from(
        rows.iter()
            .max_by_key(|row| row.observed_at)
            .expect("applicable plan rows"),
    )
}

fn card_from(row: &PlanRow) -> PlanCard {
    PlanCard {
        status: row.status,
        window: match row.window {
            Window::FiveHours => PlanWindowLabel::FiveHours,
            Window::Week => PlanWindowLabel::Week,
            Window::Month => PlanWindowLabel::Month,
            Window::Free => PlanWindowLabel::Unknown,
        },
        observed_at: row.observed_at,
        resets_at: row.resets_at,
        next_retry_at: row.next_retry_at,
    }
}

fn plan_id(credential_id: &str, restriction: &Restriction) -> String {
    let model = restriction
        .scope
        .public_model
        .as_deref()
        .filter(|model| !model.is_empty())
        .unwrap_or("-");
    let subject = match &restriction.scope.subject {
        Subject::Credential { .. } => "credential".to_string(),
        Subject::Pool {
            pool_id,
            pool_version,
        } => format!("pool-{pool_id}-{pool_version}"),
    };
    format!(
        "plan:{credential_id}:{}:{model}:{subject}",
        window_token(restriction.window)
    )
}

fn window_token(window: Window) -> &'static str {
    match window {
        Window::FiveHours => "five_hours",
        Window::Week => "week",
        Window::Month => "month",
        Window::Free => "free",
    }
}

fn window_rank(window: Window) -> u8 {
    match window {
        Window::Month => 3,
        Window::Week => 2,
        Window::FiveHours => 1,
        Window::Free => 0,
    }
}

fn document_has_scope(document: &PolicyDocument, scopes: &[Scope]) -> bool {
    document
        .restrictions
        .iter()
        .any(|row| scopes.iter().any(|scope| scope == &row.scope))
}

fn clear_unknown_gap(document: &mut PolicyDocument, scopes: &[Scope]) -> Result<bool, PolicyFault> {
    let json = document.to_json()?;
    let mut value: Value = serde_json::from_str(&json).map_err(|_| PolicyFault::Malformed)?;
    let rows = value
        .get_mut("restrictions")
        .and_then(Value::as_array_mut)
        .ok_or(PolicyFault::Malformed)?;
    let mut changed = false;
    for (index, raw) in rows.iter_mut().enumerate() {
        let Some(typed) = document.restrictions.get(index) else {
            return Err(PolicyFault::Malformed);
        };
        if !matches!(typed.reset, Reset::Unknown)
            || !scopes.iter().any(|scope| scope == &typed.scope)
        {
            continue;
        }
        let Some(object) = raw.get_mut("recovery").and_then(Value::as_object_mut) else {
            continue;
        };
        if object
            .get("last_admit_at")
            .is_some_and(|value| !value.is_null())
        {
            object.remove("last_admit_at");
            changed = true;
        }
    }
    if !changed {
        return Ok(false);
    }
    let text = serde_json::to_string(&value).map_err(|_| PolicyFault::Unavailable)?;
    *document = PolicyDocument::from_json(&text)?;
    Ok(true)
}

fn recovery_facts(document: &PolicyDocument) -> Result<RecoveryFacts, PolicyFault> {
    let json = document.to_json()?;
    let value: Value = serde_json::from_str(&json).map_err(|_| PolicyFault::Malformed)?;
    let rows = value
        .get("restrictions")
        .and_then(Value::as_array)
        .ok_or(PolicyFault::Malformed)?;
    if rows.len() != document.restrictions.len() {
        return Err(PolicyFault::Malformed);
    }
    let mut last_admit_at = Vec::with_capacity(rows.len());
    let mut inflight = Vec::with_capacity(rows.len());
    for row in rows {
        let admit = row
            .pointer("/recovery/last_admit_at")
            .and_then(Value::as_str)
            .map(parse_time)
            .transpose()?;
        let attempts = row
            .pointer("/recovery/inflight")
            .and_then(Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .filter_map(|item| {
                        item.get("attempt_id")
                            .and_then(Value::as_str)
                            .map(str::to_string)
                    })
                    .collect()
            })
            .unwrap_or_default();
        last_admit_at.push(admit);
        inflight.push(attempts);
    }
    Ok(RecoveryFacts {
        last_admit_at,
        inflight,
    })
}

fn parse_time(text: &str) -> Result<DateTime<Utc>, PolicyFault> {
    chrono::DateTime::parse_from_rfc3339(text)
        .map(|value| value.with_timezone(&Utc))
        .map_err(|_| PolicyFault::Malformed)
}

fn gap() -> chrono::Duration {
    chrono::Duration::seconds(
        i64::try_from(OpportunityPolicy::default_bounded().gap_seconds).unwrap_or(90),
    )
}

enum GrantRead {
    MissingTable,
    Rows {
        endpoints: Vec<String>,
        origins: Vec<String>,
    },
    UnknownKind,
}

enum PoolRead {
    MissingTable,
    Ids(Vec<String>),
}

fn grants_match(
    conn: &Connection,
    credential_id: &str,
    endpoints: &[String],
    origins: &[String],
) -> Result<bool, PolicyFault> {
    match read_grants(conn, credential_id)? {
        GrantRead::MissingTable => Ok(endpoints.is_empty() && origins.is_empty()),
        GrantRead::UnknownKind => Ok(false),
        GrantRead::Rows {
            endpoints: live_endpoints,
            origins: live_origins,
        } => Ok(live_endpoints == endpoints && live_origins == origins),
    }
}

fn pools_match(
    conn: &Connection,
    credential_id: &str,
    legacy_account_id: &str,
    expected: &[String],
) -> Result<bool, PolicyFault> {
    let live = match read_pools(conn, credential_id, legacy_account_id)? {
        PoolRead::MissingTable => Vec::new(),
        PoolRead::Ids(ids) => ids,
    };
    if live.iter().any(|id| id.is_empty()) || expected.iter().any(|id| id.is_empty()) {
        return Ok(false);
    }
    Ok(live == expected)
}

fn read_grants(conn: &Connection, credential_id: &str) -> Result<GrantRead, PolicyFault> {
    if !table_exists(conn, "credential_grants")? {
        return Ok(GrantRead::MissingTable);
    }
    let mut statement = conn
        .prepare(
            "SELECT kind, value FROM credential_grants WHERE credential_id = ?1 ORDER BY rowid",
        )
        .map_err(|_| PolicyFault::Unavailable)?;
    let rows = statement
        .query_map([credential_id], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })
        .map_err(|_| PolicyFault::Unavailable)?;
    let mut endpoints = Vec::new();
    let mut origins = Vec::new();
    for row in rows {
        let (kind, value) = row.map_err(|_| PolicyFault::Unavailable)?;
        match kind.as_str() {
            "endpoint_id" => endpoints.push(value),
            "origin" => origins.push(value),
            _ => return Ok(GrantRead::UnknownKind),
        }
    }
    endpoints.sort();
    origins.sort();
    Ok(GrantRead::Rows { endpoints, origins })
}

fn read_pools(
    conn: &Connection,
    credential_id: &str,
    legacy_account_id: &str,
) -> Result<PoolRead, PolicyFault> {
    if !table_exists(conn, "quota_pool_members")? {
        return Ok(PoolRead::MissingTable);
    }
    let mut statement = conn
        .prepare(
            "SELECT pool_id FROM quota_pool_members
             WHERE account_id = ?1 OR account_id = ?2
             ORDER BY pool_id",
        )
        .map_err(|_| PolicyFault::Unavailable)?;
    let rows = statement
        .query_map([credential_id, legacy_account_id], |row| {
            row.get::<_, String>(0)
        })
        .map_err(|_| PolicyFault::Unavailable)?;
    let mut ids = Vec::new();
    for row in rows {
        ids.push(row.map_err(|_| PolicyFault::Unavailable)?);
    }
    Ok(PoolRead::Ids(sorted_unique(ids)))
}

fn granted(conn: &Connection, credential_id: &str) -> Result<bool, PolicyFault> {
    if !table_exists(conn, "credential_grants")? {
        return Ok(true);
    }
    let count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM credential_grants WHERE credential_id = ?1",
            [credential_id],
            |row| row.get(0),
        )
        .map_err(|_| PolicyFault::Unavailable)?;
    Ok(count > 0)
}

fn load_row(conn: &Connection, credential_id: &str) -> Result<Option<LiveRow>, PolicyFault> {
    let row = conn
        .query_row(
            "SELECT id, legacy_account_id, destination_id, provider_id, binding_id,
                    credential_version, key_cipher, enabled, scope_json
             FROM credentials WHERE id = ?1",
            [credential_id],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, Option<String>>(2)?,
                    row.get::<_, Option<String>>(3)?,
                    row.get::<_, Option<String>>(4)?,
                    row.get::<_, Option<i64>>(5)?,
                    row.get::<_, Option<String>>(6)?,
                    row.get::<_, Option<i64>>(7)?,
                    row.get::<_, Option<String>>(8)?,
                ))
            },
        )
        .optional()
        .map_err(|_| PolicyFault::Unavailable)?;
    let Some((id, legacy, destination, provider, binding, version, key, enabled, scope)) = row
    else {
        return Ok(None);
    };
    let Some(version) = version.filter(|value| *value > 0) else {
        return Ok(Some(LiveRow {
            credential_id: id,
            legacy_account_id: legacy,
            destination_id: destination.unwrap_or_default(),
            provider_id: provider.unwrap_or_default(),
            binding_id: binding.unwrap_or_default(),
            version: 0,
            key_cipher: key.unwrap_or_default(),
            enabled: enabled.unwrap_or(0) != 0,
            scope_json: scope.unwrap_or_default(),
        }));
    };
    Ok(Some(LiveRow {
        credential_id: id,
        legacy_account_id: legacy,
        destination_id: destination.unwrap_or_default(),
        provider_id: provider.unwrap_or_default(),
        binding_id: binding.unwrap_or_default(),
        version: version as u64,
        key_cipher: key.unwrap_or_default(),
        enabled: enabled.unwrap_or(0) != 0,
        scope_json: scope.unwrap_or_default(),
    }))
}

fn list_rows(conn: &Connection) -> Result<Vec<LiveRow>, PolicyFault> {
    if !table_exists(conn, "credentials")? {
        return Err(PolicyFault::Unavailable);
    }
    let ids = {
        let mut statement = conn
            .prepare(
                "SELECT id FROM credentials
                 WHERE COALESCE(credential_purpose, 'inference') = 'inference'
                 ORDER BY id",
            )
            .map_err(|_| PolicyFault::Unavailable)?;
        let mapped = statement
            .query_map([], |row| row.get::<_, String>(0))
            .map_err(|_| PolicyFault::Unavailable)?;
        let mut ids = Vec::new();
        for id in mapped {
            ids.push(id.map_err(|_| PolicyFault::Unavailable)?);
        }
        ids
    };
    let mut rows = Vec::new();
    for id in ids {
        if let Some(row) = load_row(conn, &id)? {
            rows.push(row);
        }
    }
    Ok(rows)
}

fn load_document_tx(tx: &Transaction<'_>) -> Result<PolicyDocument, PolicyFault> {
    let value: Option<String> = tx
        .query_row(
            "SELECT value FROM settings WHERE key = ?1",
            [SETTINGS_KEY],
            |row| row.get(0),
        )
        .optional()
        .map_err(|_| PolicyFault::Unavailable)?;
    match value {
        None => Ok(PolicyDocument::default()),
        Some(json) => PolicyDocument::from_json(&json),
    }
}

fn table_exists(conn: &Connection, name: &str) -> Result<bool, PolicyFault> {
    let found: Option<String> = conn
        .query_row(
            "SELECT name FROM sqlite_master WHERE type = 'table' AND name = ?1",
            [name],
            |row| row.get(0),
        )
        .optional()
        .map_err(|_| PolicyFault::Unavailable)?;
    Ok(found.is_some())
}

fn parse_scope(json: &str) -> Option<ModelScope> {
    serde_json::from_str(json).ok()
}

fn scope_usable(scope: &ModelScope) -> bool {
    match scope {
        ModelScope::All => true,
        ModelScope::Only { models } => {
            !models.is_empty() && models.iter().all(|model| !model.is_empty())
        }
    }
}

fn sorted_unique(mut values: Vec<String>) -> Vec<String> {
    values.sort();
    values.dedup();
    values
}

fn nonempty(value: &str) -> String {
    nonempty_or(value, UNFENCED)
}

fn nonempty_or(value: &str, fallback: &str) -> String {
    if value.is_empty() {
        fallback.to_string()
    } else {
        value.to_string()
    }
}

#[cfg(test)]
mod tests;
