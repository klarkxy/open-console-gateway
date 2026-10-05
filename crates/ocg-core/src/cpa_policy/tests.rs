//! Behavior of the synchronous policy service against real SQLite settings rows.

use super::auth::authorize_token;
use super::store::{
    EvidenceSource, PolicyDocument, PolicyFault, PolicyStore, Reset, Restriction, SETTINGS_KEY,
    SendKind, Subject, Window, read_settings, transact_settings,
};
use super::wire::{
    Action, CurrentFacts, DeclaredScope, DeclaredSubject, OfficialQuotaObservation,
    OpportunityPolicy, PolicyFacts, PoolMembership, QuotaApply, Reason,
};
use super::{Decision, PolicyService};
use chrono::{Duration, TimeZone, Utc};
use ocg_domain::ids::{COMMAND_CODE_PROVIDER_ID, OPENCODE_PROVIDER_ID};
use rusqlite::{Connection, Transaction};
use serde_json::{Value, json};
use std::cell::{Cell, RefCell};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration as StdDuration;
use uuid::Uuid;

const TOKEN: &str = "policy-token";

struct Scratch(PathBuf);

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

struct SqliteStore {
    conn: parking_lot::Mutex<Connection>,
}

impl SqliteStore {
    fn open(path: PathBuf) -> Self {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS settings (
                key TEXT PRIMARY KEY,
                value TEXT NOT NULL
            );",
        )
        .unwrap();
        conn.pragma_update(None, "busy_timeout", 2000).unwrap();
        Self {
            conn: parking_lot::Mutex::new(conn),
        }
    }
}

impl PolicyStore for SqliteStore {
    fn read(
        &self,
        read: &mut dyn for<'tx> FnMut(
            &Transaction<'tx>,
            &PolicyDocument,
        ) -> Result<(), PolicyFault>,
    ) -> Result<(), PolicyFault> {
        let mut conn = self.conn.lock();
        super::store::read_transaction(&mut conn, SETTINGS_KEY, read)
    }

    fn update(
        &self,
        mutate: &mut dyn for<'tx> FnMut(
            &Transaction<'tx>,
            &mut PolicyDocument,
        ) -> Result<(), PolicyFault>,
    ) -> Result<(), PolicyFault> {
        let mut conn = self.conn.lock();
        transact_settings(&mut conn, SETTINGS_KEY, mutate)
    }
}

struct Snapshot(PolicyFacts);

impl CurrentFacts for Snapshot {
    fn revalidate(&mut self, _tx: &Transaction<'_>) -> Result<PolicyFacts, PolicyFault> {
        Ok(self.0.clone())
    }
}

struct Denial;

impl CurrentFacts for Denial {
    fn revalidate(&mut self, _tx: &Transaction<'_>) -> Result<PolicyFacts, PolicyFault> {
        Err(PolicyFault::Unavailable)
    }
}

struct Counting {
    facts: PolicyFacts,
    freezes: Cell<u32>,
    records: Cell<u32>,
}

impl CurrentFacts for Counting {
    fn revalidate(&mut self, _tx: &Transaction<'_>) -> Result<PolicyFacts, PolicyFault> {
        Ok(self.facts.clone())
    }

    fn freeze_admitted_attempt(
        &mut self,
        _tx: &Transaction<'_>,
        _identity: &super::wire::AttemptIdentity,
        _allowed_at: chrono::DateTime<Utc>,
    ) -> Result<Option<Vec<crate::cpa_projection::NativeEndpointPin>>, PolicyFault> {
        self.freezes.set(self.freezes.get() + 1);
        Ok(None)
    }

    fn record_admitted_result(
        &mut self,
        _tx: &Transaction<'_>,
        _decision: &Decision,
        _identity: &super::wire::AttemptIdentity,
        _result: &super::wire::ResultBody,
        _raw_body: &str,
    ) -> Result<(), PolicyFault> {
        self.records.set(self.records.get() + 1);
        Ok(())
    }
}

struct Observed {
    facts: PolicyFacts,
    records: Cell<u32>,
    auth_id: RefCell<String>,
    provider_id: RefCell<String>,
    public_model: RefCell<String>,
    credential_id: RefCell<String>,
    credential_version: Cell<u64>,
    binding_id: RefCell<String>,
}

impl Observed {
    fn new(facts: PolicyFacts) -> Self {
        Self {
            facts,
            records: Cell::new(0),
            auth_id: RefCell::new(String::new()),
            provider_id: RefCell::new(String::new()),
            public_model: RefCell::new(String::new()),
            credential_id: RefCell::new(String::new()),
            credential_version: Cell::new(0),
            binding_id: RefCell::new(String::new()),
        }
    }

    fn assert_admitted(&self, world: &World) {
        assert_eq!(self.records.get(), 1);
        assert_eq!(self.auth_id.borrow().as_str(), world.auth_id);
        assert_eq!(self.provider_id.borrow().as_str(), world.provider);
        assert_eq!(self.public_model.borrow().as_str(), world.public_model);
        assert_eq!(self.credential_id.borrow().as_str(), world.credential_id);
        assert_eq!(self.credential_version.get(), world.version);
        assert_eq!(self.binding_id.borrow().as_str(), world.binding);
    }
}

impl CurrentFacts for Observed {
    fn revalidate(&mut self, _tx: &Transaction<'_>) -> Result<PolicyFacts, PolicyFault> {
        Ok(self.facts.clone())
    }

    fn record_admitted_result(
        &mut self,
        _tx: &Transaction<'_>,
        _decision: &Decision,
        identity: &super::wire::AttemptIdentity,
        _result: &super::wire::ResultBody,
        _raw_body: &str,
    ) -> Result<(), PolicyFault> {
        self.records.set(self.records.get() + 1);
        *self.auth_id.borrow_mut() = identity.auth_id.clone();
        *self.provider_id.borrow_mut() = identity.provider_id.clone();
        *self.public_model.borrow_mut() = identity.public_model.clone();
        *self.credential_id.borrow_mut() = identity.credential_id.clone();
        self.credential_version.set(identity.credential_version);
        *self.binding_id.borrow_mut() = identity.binding_id.clone();
        Ok(())
    }
}

#[derive(Clone)]
struct World {
    now: chrono::DateTime<Utc>,
    deadline: chrono::DateTime<Utc>,
    generation: u64,
    revision: u64,
    digest: [u8; 32],
    request_id: Uuid,
    attempt_id: Uuid,
    credential_id: String,
    version: u64,
    provider: String,
    public_model: String,
    upstream_model: String,
    binding: String,
    material: String,
    epoch: u64,
    auth_id: String,
    kind: SendKind,
    memberships: Vec<PoolMembership>,
    scopes: Vec<DeclaredScope>,
    granted: bool,
    enabled: bool,
    deleted: bool,
    rebound: bool,
    live_version: Option<u64>,
    live_binding: Option<String>,
    live_epoch: Option<u64>,
    live_material: Option<String>,
    live_auth_id: Option<String>,
    live_memberships: Option<Vec<PoolMembership>>,
    observation_seq: Cell<u64>,
}

impl World {
    fn goat() -> Self {
        Self::new(
            COMMAND_CODE_PROVIDER_ID,
            "alpha",
            "deepseek/deepseek-v4-flash",
        )
    }

    fn go() -> Self {
        Self::new(OPENCODE_PROVIDER_ID, "alpha", "alpha-upstream")
    }

    fn new(provider: &str, public_model: &str, upstream_model: &str) -> Self {
        let now = Utc.with_ymd_and_hms(2026, 10, 4, 12, 0, 0).unwrap();
        Self {
            now,
            deadline: now + Duration::minutes(5),
            generation: 4,
            revision: 9,
            digest: [0xab; 32],
            request_id: Uuid::from_u128(1),
            attempt_id: Uuid::from_u128(2),
            credential_id: "cred-a".into(),
            version: 3,
            provider: provider.into(),
            public_model: public_model.into(),
            upstream_model: upstream_model.into(),
            binding: "bind-a".into(),
            material: "material-a".into(),
            epoch: 1,
            auth_id: "auth-a".into(),
            kind: SendKind::Accepted,
            memberships: Vec::new(),
            scopes: vec![DeclaredScope {
                subject: DeclaredSubject::Credential,
                public_model: None,
            }],
            granted: true,
            enabled: true,
            deleted: false,
            rebound: false,
            live_version: None,
            live_binding: None,
            live_epoch: None,
            live_material: None,
            live_auth_id: None,
            live_memberships: None,
            observation_seq: Cell::new(0),
        }
    }

    fn credential_model(&mut self, model: &str) {
        self.scopes = vec![DeclaredScope {
            subject: DeclaredSubject::Credential,
            public_model: Some(model.to_string()),
        }];
    }

    fn pool_scope(&mut self, pool_id: &str, pool_version: u64, model: &str) {
        self.scopes = vec![DeclaredScope {
            subject: DeclaredSubject::Pool {
                pool_id: pool_id.to_string(),
                pool_version,
            },
            public_model: Some(model.to_string()),
        }];
    }

    fn facts(&self) -> PolicyFacts {
        PolicyFacts {
            applied: super::wire::AppliedProjection {
                process_generation: self.generation,
                revision: self.revision,
                digest: self.digest,
            },
            attempt: Some(super::wire::AttemptIdentity {
                request_id: self.request_id,
                attempt_id: self.attempt_id,
                auth_id: self.auth_id.clone(),
                credential_id: self.credential_id.clone(),
                credential_version: self.version,
                provider_id: self.provider.clone(),
                public_model: self.public_model.clone(),
                upstream_model: self.upstream_model.clone(),
                binding_id: self.binding.clone(),
                material_revision: self.material.clone(),
                registration_epoch: self.epoch,
                kind: self.kind,
            }),
            current: Some(super::wire::CurrentCredential {
                credential_id: self.credential_id.clone(),
                credential_version: self.live_version.unwrap_or(self.version),
                provider_id: self.provider.clone(),
                binding_id: self
                    .live_binding
                    .clone()
                    .unwrap_or_else(|| self.binding.clone()),
                material_revision: self
                    .live_material
                    .clone()
                    .unwrap_or_else(|| self.material.clone()),
                registration_epoch: self.live_epoch.unwrap_or(self.epoch),
                auth_id: self
                    .live_auth_id
                    .clone()
                    .unwrap_or_else(|| self.auth_id.clone()),
                memberships: self
                    .live_memberships
                    .clone()
                    .unwrap_or_else(|| self.memberships.clone()),
                scopes: self.scopes.clone(),
                granted: self.granted,
                enabled: self.enabled,
                deleted: self.deleted,
                rebound: self.rebound,
            }),
            deadline_at: self.deadline,
            now: self.now,
            caller_stop: None,
            validated_pin: false,
        }
    }

    fn ready_facts(&self) -> PolicyFacts {
        let mut facts = self.facts();
        facts.attempt = None;
        facts.current = None;
        facts
    }

    fn common(&self, operation: &str) -> Value {
        let mut value = json!({
            "protocolVersion": 1,
            "operation": operation,
            "processGeneration": self.generation.to_string(),
            "projectionRevision": self.revision.to_string(),
            "projectionDigest": hex::encode(self.digest),
        });
        if operation != "ready" {
            value["requestId"] = json!(self.request_id.to_string());
            value["attemptId"] = json!(self.attempt_id.to_string());
            value["authId"] = json!(self.auth_id);
            value["credentialId"] = json!(self.credential_id);
            value["credentialVersion"] = json!(self.version.to_string());
            value["providerId"] = json!(self.provider);
            value["publicModel"] = json!(self.public_model);
            value["upstreamModel"] = json!(self.upstream_model);
            value["registrationEpoch"] = json!(self.epoch.to_string());
            value["materialRevision"] = json!(self.material);
            value["kind"] = json!(kind_name(self.kind));
        }
        if operation == "admit" {
            value["callableProtocol"] = json!("chat_completions");
            value["generationKind"] = json!("execute");
        }
        if operation == "result" {
            value["endpointPin"] = Value::Null;
        }
        value
    }

    fn admit_body(&self) -> Vec<u8> {
        serde_json::to_vec(&self.common("admit")).unwrap()
    }

    fn ready_body(&self) -> Vec<u8> {
        serde_json::to_vec(&self.common("ready")).unwrap()
    }

    #[allow(clippy::too_many_arguments)]
    fn result_body(
        &self,
        outcome: &str,
        status: Option<u16>,
        sent: bool,
        body_complete: bool,
        stream_started: bool,
        error_code: &str,
        response_body: &str,
        retry_after: Option<&str>,
    ) -> Vec<u8> {
        let mut value = self.common("result");
        value["sent"] = json!(sent);
        value["status"] = match status {
            Some(code) => json!(code),
            None => Value::Null,
        };
        value["bodyComplete"] = json!(body_complete);
        value["streamStarted"] = json!(stream_started);
        value["outcome"] = json!(outcome);
        value["errorCode"] = json!(error_code);
        value["responseBody"] = json!(response_body);
        let seq = self.observation_seq.get() + 1;
        self.observation_seq.set(seq);
        value["observation"] = json!({
            "id": format!("obs-{seq}"),
            "fetchedAt": self.now.to_rfc3339(),
        });
        if let Some(retry) = retry_after {
            value["headers"] = json!({ "Retry-After": retry });
        }
        serde_json::to_vec(&value).unwrap()
    }
}

fn kind_name(kind: SendKind) -> &'static str {
    match kind {
        SendKind::Accepted => "accepted",
        SendKind::Validated => "validated",
    }
}

fn scratch() -> Scratch {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tmp/ocg3-cli-delivery/orchestration-20261004/policy-work/fixtures")
        .join(format!(
            "{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
    std::fs::create_dir_all(&path).unwrap();
    Scratch(path)
}

fn service_at(
    path: &Path,
    gap: u64,
    inflight: u32,
    per_request: u32,
) -> PolicyService<SqliteStore> {
    PolicyService::new(
        TOKEN,
        OpportunityPolicy {
            gap_seconds: gap,
            max_inflight: inflight,
            max_per_request: per_request,
        },
        SqliteStore::open(path.join("policy.sqlite")),
    )
    .unwrap()
}

fn harness(gap: u64, inflight: u32, per_request: u32) -> (Scratch, PolicyService<SqliteStore>) {
    let dir = scratch();
    let service = service_at(&dir.0, gap, inflight, per_request);
    (dir, service)
}

fn call(service: &PolicyService<SqliteStore>, world: &World, body: &[u8]) -> Decision {
    let mut facts = Snapshot(world.facts());
    let reply = service.handle(TOKEN, body, &mut facts);
    assert!(reply.bytes.len() <= super::wire::MAX_RESPONSE_BYTES);
    let text = String::from_utf8(reply.bytes).unwrap();
    assert!(!text.contains("sk-policy-secret"), "{text}");
    assert!(!text.contains(TOKEN), "{text}");
    let decision = reply.decision.expect("decision");
    if body_operation(body) == "result" {
        assert_ne!(decision.action, Action::Allow);
    }
    decision
}

fn body_operation(body: &[u8]) -> String {
    serde_json::from_slice::<Value>(body)
        .ok()
        .and_then(|value| value.get("operation")?.as_str().map(str::to_string))
        .unwrap_or_default()
}

fn document(service: &PolicyService<SqliteStore>) -> PolicyDocument {
    let conn = service.store.conn.lock();
    read_settings(&conn, SETTINGS_KEY).unwrap()
}

fn raw_settings(service: &PolicyService<SqliteStore>) -> Option<String> {
    service
        .store
        .conn
        .lock()
        .query_row(
            "SELECT value FROM settings WHERE key = ?1",
            [SETTINGS_KEY],
            |row| row.get(0),
        )
        .ok()
}

fn goat_message(window: &str, reset: &str) -> String {
    format!(
        r#"{{"error":{{"code":"RATE_LIMITED","type":"rate_limit_error","message":"You've reached your {window} usage limit for your plan. Your limit resets at {reset}. Please wait for the window to reset or upgrade your plan to continue.","echo":"sk-policy-secret"}}}}"#
    )
}

fn reopen(service: &PolicyService<SqliteStore>, world: &World, attempt: u128) -> World {
    let mut next = world.clone();
    next.attempt_id = Uuid::from_u128(attempt);
    next.request_id = Uuid::from_u128(attempt + 10_000);
    assert_eq!(
        call(service, &next, &next.admit_body()).action,
        Action::Allow
    );
    next
}

fn definite(world: &World, body: &str) -> Vec<u8> {
    world.result_body(
        "explicit_rejection",
        Some(429),
        true,
        true,
        false,
        "provider_rejected",
        body,
        None,
    )
}

fn credential_version(row: &Restriction) -> Option<u64> {
    match &row.scope.subject {
        Subject::Credential {
            credential_version, ..
        } => Some(*credential_version),
        Subject::Pool { .. } => None,
    }
}

fn pool_version(row: &Restriction) -> Option<u64> {
    match &row.scope.subject {
        Subject::Pool { pool_version, .. } => Some(*pool_version),
        Subject::Credential { .. } => None,
    }
}

fn member(pool_id: &str, version: u64, models: &[&str]) -> PoolMembership {
    PoolMembership {
        pool_id: pool_id.to_string(),
        pool_version: version,
        public_models: models.iter().map(|model| (*model).to_string()).collect(),
        all_models: false,
    }
}

fn official_usage(
    rolling_status: &str,
    rolling_reset: &str,
    weekly_status: &str,
    weekly_reset: &str,
    monthly_status: &str,
    monthly_reset: &str,
) -> Vec<u8> {
    format!(
        r#"{{"usage":{{"rolling":{{"status":"{rolling_status}","percent":91.5,"resetsAt":"{rolling_reset}","extra":true}},"weekly":{{"status":"{weekly_status}","percent":37.0,"resetsAt":"{weekly_reset}"}},"monthly":{{"status":"{monthly_status}","percent":3,"resetsAt":"{monthly_reset}"}},"newWindow":{{}}}},"ignored":true}}"#
    )
    .into_bytes()
}

fn apply_quota(
    service: &PolicyService<SqliteStore>,
    world: &World,
    id: &str,
    fetched_at: chrono::DateTime<Utc>,
    body: Vec<u8>,
) -> Result<QuotaApply, PolicyFault> {
    let mut facts = Snapshot(world.facts());
    service.apply_official_quota(
        &OfficialQuotaObservation {
            observation_id: id.to_string(),
            fetched_at,
            body,
            provider_id: world.provider.clone(),
        },
        &mut facts,
    )
}

#[test]
fn token_helper_fails_closed_for_empty_wrong_and_overlong_tokens() {
    assert!(authorize_token(b"abc", b"abc"));
    assert!(!authorize_token(b"abc", b"abd"));
    assert!(!authorize_token(b"abc", b"abcd"));
    assert!(!authorize_token(b"", b"abc"));
    assert!(!authorize_token(b"abc", b""));
    let long = vec![b'a'; super::wire::MAX_TOKEN_BYTES + 1];
    assert!(!authorize_token(b"abc", &long));
    assert!(
        PolicyService::new(
            "",
            OpportunityPolicy::default(),
            SqliteStore::open(scratch().0.join("policy.sqlite"))
        )
        .is_err()
    );
    let policy = OpportunityPolicy::default();
    assert_eq!(policy.gap_seconds, 90);
    assert_eq!(policy.max_inflight, 1);
    assert_eq!(policy.max_per_request, 3);
    assert!(!matches!(policy.gap_seconds, 900 | 3_600 | 21_600));
}

#[test]
fn explicit_goat_429_lets_the_next_credential_proceed() {
    let (_dir, service) = harness(90, 1, 3);
    let mut alpha = World::goat();
    let body = goat_message("weekly", "2026-10-11T00:00:00Z");
    assert_eq!(
        call(&service, &alpha, &alpha.admit_body()).action,
        Action::Allow
    );
    let decided = call(&service, &alpha, &definite(&alpha, &body));
    assert_eq!(
        (decided.action, decided.reason),
        (Action::Skip, Reason::ExplicitRejection)
    );
    assert!(document(&service).restrictions.iter().any(|row| {
        matches!(row.reset, Reset::Known { .. }) && row.source == EvidenceSource::GoatPlan
    }));
    let mut beta = alpha.clone();
    beta.credential_id = "cred-b".into();
    beta.auth_id = "auth-b".into();
    beta.binding = "bind-b".into();
    beta.material = "material-b".into();
    beta.epoch = 90;
    beta.attempt_id = Uuid::from_u128(8);
    assert_eq!(
        call(&service, &beta, &beta.admit_body()).action,
        Action::Allow
    );
    alpha.attempt_id = Uuid::from_u128(9);
    alpha.epoch = 91;
    alpha.material = "material-reloaded".into();
    let again = call(&service, &alpha, &alpha.admit_body());
    assert_eq!(
        (again.action, again.reason),
        (Action::Skip, Reason::Restricted)
    );
    assert_eq!(
        again.restriction_deadline.unwrap(),
        chrono::DateTime::parse_from_rfc3339("2026-10-11T00:00:00Z")
            .unwrap()
            .with_timezone(&Utc)
    );
    let stored = raw_settings(&service).unwrap();
    assert!(!stored.contains("sk-policy-secret"));
    assert!(!stored.contains("Retry-After"));
}

#[test]
fn ordinary_429_and_goat_calibration_do_not_persist_a_plan() {
    let (_dir, service) = harness(90, 1, 3);
    let mut world = World::goat();
    assert_eq!(
        call(&service, &world, &world.admit_body()).action,
        Action::Allow
    );
    let ordinary = r#"{"error":{"type":"rate_limit_error","code":"RATE_LIMITED","message":"Upstream model provider is temporarily unavailable. Please try again in a moment.","echo":"sk-policy-secret"}}"#;
    let decided = call(&service, &world, &definite(&world, ordinary));
    assert_eq!(
        (decided.action, decided.reason),
        (Action::Skip, Reason::ProviderRejected)
    );
    assert!(document(&service).restrictions.is_empty());
    world = reopen(&service, &world, 101);
    let prose = call(
        &service,
        &world,
        &definite(&world, "Weekly usage limit reached. Resets in 4 days."),
    );
    assert_eq!(prose.action, Action::Skip);
    assert!(document(&service).restrictions.is_empty());
    world = reopen(&service, &world, 102);
    let calibration = r#"{"credits":{"monthlyCredits":0},"windowLimits":{"limited":true,"fiveHour":{"used":100,"cap":100,"resetAt":1}},"echo":"sk-policy-secret"}"#;
    assert_eq!(
        call(&service, &world, &definite(&world, calibration)).reason,
        Reason::ProviderRejected
    );
    assert!(document(&service).restrictions.is_empty());
    let stale = apply_quota(
        &service,
        &world,
        "goat-refresh",
        world.now,
        calibration.as_bytes().to_vec(),
    )
    .unwrap();
    assert_eq!(stale, QuotaApply::Stale);
    assert!(document(&service).restrictions.is_empty());
}

#[test]
fn go_known_unknown_and_model_scope_stay_independent() {
    let (_dir, service) = harness(90, 1, 3);
    let mut world = World::go();
    let known = r#"{"type":"GoUsageLimitError","message":"Weekly usage limit reached. Resets in 4 days.","echo":"sk-policy-secret"}"#;
    assert_eq!(
        call(&service, &world, &world.admit_body()).action,
        Action::Allow
    );
    let decided = call(&service, &world, &definite(&world, known));
    assert_eq!(decided.reason, Reason::ExplicitRejection);
    let week = document(&service).restrictions;
    assert_eq!(week.len(), 1);
    assert_eq!(week[0].window, Window::Week);
    assert_eq!(week[0].source, EvidenceSource::GoLimit);
    assert!(week[0].scope.public_model.is_none());
    match week[0].reset {
        Reset::Known { at } => assert_eq!(at, world.now + Duration::days(4)),
        Reset::Unknown => panic!("known reset missing"),
    }
    world.public_model = "beta".into();
    world.upstream_model = "beta-upstream".into();
    world.attempt_id = Uuid::from_u128(11);
    assert_eq!(
        call(&service, &world, &world.admit_body()).reason,
        Reason::Restricted
    );

    let mut unknown = World::go();
    unknown.credential_id = "cred-unknown".into();
    unknown.auth_id = "auth-unknown".into();
    unknown.binding = "bind-unknown".into();
    unknown.attempt_id = Uuid::from_u128(12);
    assert_eq!(
        call(&service, &unknown, &unknown.admit_body()).action,
        Action::Allow
    );
    call(
        &service,
        &unknown,
        &definite(
            &unknown,
            r#"{"type":"GoUsageLimitError","message":"5-hour usage limit reached."}"#,
        ),
    );
    let row = document(&service)
        .restrictions
        .into_iter()
        .find(|row| match &row.scope.subject {
            Subject::Credential { credential_id, .. } => credential_id == "cred-unknown",
            Subject::Pool { .. } => false,
        })
        .unwrap();
    assert_eq!(row.window, Window::FiveHours);
    assert_eq!(row.reset, Reset::Unknown);

    let mut scoped = World::go();
    scoped.credential_id = "cred-model".into();
    scoped.auth_id = "auth-model".into();
    scoped.binding = "bind-model".into();
    scoped.credential_model("alpha");
    scoped.attempt_id = Uuid::from_u128(13);
    assert_eq!(
        call(&service, &scoped, &scoped.admit_body()).action,
        Action::Allow
    );
    call(&service, &scoped, &definite(&scoped, known));
    scoped.public_model = "beta".into();
    scoped.attempt_id = Uuid::from_u128(14);
    assert_eq!(
        call(&service, &scoped, &scoped.admit_body()).action,
        Action::Allow
    );
    scoped.public_model = "alpha".into();
    scoped.attempt_id = Uuid::from_u128(15);
    assert_eq!(
        call(&service, &scoped, &scoped.admit_body()).action,
        Action::Skip
    );
}

#[test]
fn retry_after_does_not_replace_a_declared_plan_deadline() {
    let (_dir, service) = harness(90, 1, 3);
    let world = World::goat();
    assert_eq!(
        call(&service, &world, &world.admit_body()).action,
        Action::Allow
    );
    let body = goat_message("5-hour", "2026-10-04T18:00:00Z");
    let bytes = world.result_body(
        "explicit_rejection",
        Some(429),
        true,
        true,
        false,
        "provider_rejected",
        &body,
        Some("1"),
    );
    call(&service, &world, &bytes);
    match document(&service).restrictions[0].reset {
        Reset::Known { at } => assert_eq!(
            at,
            chrono::DateTime::parse_from_rfc3339("2026-10-04T18:00:00Z")
                .unwrap()
                .with_timezone(&Utc)
        ),
        Reset::Unknown => panic!("declared reset dropped"),
    }
}

#[test]
fn later_deadline_wins_and_a_shorter_one_does_not_shorten_it() {
    let (_dir, service) = harness(90, 1, 3);
    let world = World::go();
    assert_eq!(
        call(&service, &world, &world.admit_body()).action,
        Action::Allow
    );
    let shorter = reopen(&service, &world, 103);
    let longer = reopen(&service, &world, 104);
    shorter.observation_seq.set(10);
    longer.observation_seq.set(20);
    call(
        &service,
        &world,
        &definite(
            &world,
            r#"{"message":"Weekly usage limit reached. Resets in 4 days."}"#,
        ),
    );
    call(
        &service,
        &shorter,
        &definite(
            &shorter,
            r#"{"message":"Weekly usage limit reached. Resets in 1 day."}"#,
        ),
    );
    match document(&service).restrictions[0].reset {
        Reset::Known { at } => assert_eq!(at, world.now + Duration::days(4)),
        Reset::Unknown => panic!("deadline lost"),
    }
    call(
        &service,
        &longer,
        &definite(
            &longer,
            r#"{"message":"Weekly usage limit reached. Resets in 10 days."}"#,
        ),
    );
    match document(&service).restrictions[0].reset {
        Reset::Known { at } => assert_eq!(at, world.now + Duration::days(10)),
        Reset::Unknown => panic!("later deadline lost"),
    }
    assert_eq!(document(&service).restrictions.len(), 1);
}

#[test]
fn healthy_evidence_must_be_newer_and_official() {
    let (_dir, service) = harness(90, 1, 3);
    let mut world = World::go();
    let started = world.now;
    assert_eq!(
        call(&service, &world, &world.admit_body()).action,
        Action::Allow
    );
    let weekly = reopen(&service, &world, 105);
    let usage_attempt = reopen(&service, &world, 106);
    let inference_attempt = reopen(&service, &world, 107);
    call(
        &service,
        &world,
        &definite(&world, "5-hour usage limit reached. Resets in 13min."),
    );
    call(
        &service,
        &weekly,
        &definite(&weekly, "Weekly usage limit reached. Resets in 4 days."),
    );
    assert_eq!(document(&service).restrictions.len(), 2);
    let observed = call(
        &service,
        &usage_attempt,
        &usage_attempt.result_body(
            "success",
            Some(200),
            true,
            true,
            false,
            "usage_observation",
            "Monthly usage limit reached. Resets in 13 days.",
            None,
        ),
    );
    assert_eq!(observed.reason, Reason::Completed);
    assert!(
        document(&service)
            .restrictions
            .iter()
            .all(|row| row.window != Window::Month)
    );

    let rolling_at = (started + Duration::minutes(13)).to_rfc3339();
    let weekly_at = (started + Duration::days(4)).to_rfc3339();
    let monthly_at = (started + Duration::days(20)).to_rfc3339();
    let usage = official_usage(
        "rate-limited",
        &rolling_at,
        "ok",
        &weekly_at,
        "ok",
        &monthly_at,
    );
    let inference = call(
        &service,
        &inference_attempt,
        &inference_attempt.result_body(
            "success",
            Some(200),
            true,
            true,
            false,
            "none",
            &String::from_utf8(usage.clone()).unwrap(),
            None,
        ),
    );
    assert_eq!(inference.reason, Reason::Completed);
    assert_eq!(document(&service).restrictions.len(), 2);

    let stale = apply_quota(
        &service,
        &world,
        "fetch-early",
        started - Duration::minutes(1),
        official_usage("ok", &rolling_at, "ok", &weekly_at, "ok", &monthly_at),
    )
    .unwrap();
    assert_eq!(stale, QuotaApply::Applied);
    assert_eq!(document(&service).restrictions.len(), 2);

    world.now = started + Duration::seconds(1);
    let cleared = apply_quota(
        &service,
        &world,
        "fetch-later",
        world.now,
        official_usage(
            "rate-limited",
            &rolling_at,
            "ok",
            &weekly_at,
            "ok",
            &monthly_at,
        ),
    )
    .unwrap();
    assert_eq!(cleared, QuotaApply::Applied);
    let rows = document(&service).restrictions;
    assert!(rows.iter().all(|row| row.window != Window::Week));
    assert!(rows.iter().any(|row| row.window == Window::FiveHours));
    assert!(!raw_settings(&service).unwrap().contains("91.5"));

    let rejected = apply_quota(
        &service,
        &world,
        "fetch-bad",
        world.now,
        br#"{"usage":{"rolling":{"status":"ok","percent":"NaN","resetsAt":"2026-10-04T15:00:00Z"},"weekly":{"status":"ok","percent":0,"resetsAt":"2026-10-05T12:00:00Z"},"monthly":{"status":"ok","percent":0,"resetsAt":"2026-11-01T00:00:00Z"}}}"#.to_vec(),
    );
    assert!(rejected.is_err());
    assert!(
        document(&service)
            .restrictions
            .iter()
            .any(|row| row.window == Window::FiveHours)
    );

    // Week was cleared by the newer official snapshot. The 13-minute
    // FiveHours row is behind this request clock and expires on admit.
    world.deadline = started + Duration::hours(2);
    world.now = started + Duration::minutes(14);
    world.request_id = Uuid::from_u128(121);
    world.attempt_id = Uuid::from_u128(21);
    assert_eq!(
        call(&service, &world, &world.admit_body()).reason,
        Reason::Eligible
    );
    let rows = document(&service).restrictions;
    assert!(rows.iter().all(|row| row.window != Window::FiveHours));
    assert!(rows.iter().all(|row| row.window != Window::Week));
}

#[test]
fn model_scoped_pool_blocks_members_without_covering_other_models() {
    let (_dir, service) = harness(90, 1, 3);
    let mut alpha = World::goat();
    alpha.memberships = vec![
        member("pool-1", 7, &["alpha"]),
        member("pool-2", 1, &["beta"]),
    ];
    alpha.pool_scope("pool-1", 7, "alpha");
    let mut moved = World::goat();
    moved.credential_id = "cred-moved".into();
    moved.auth_id = "auth-moved".into();
    moved.binding = "bind-moved".into();
    moved.memberships = vec![member("pool-1", 7, &["alpha"])];
    moved.pool_scope("pool-1", 7, "alpha");
    moved.attempt_id = Uuid::from_u128(37);
    let body = goat_message("monthly", "2026-12-01T00:00:00Z");
    assert_eq!(
        call(&service, &moved, &moved.admit_body()).action,
        Action::Allow
    );
    let moved_result = definite(&moved, &body);
    assert_eq!(
        call(&service, &alpha, &alpha.admit_body()).action,
        Action::Allow
    );
    call(&service, &alpha, &definite(&alpha, &body));
    assert_eq!(pool_version(&document(&service).restrictions[0]), Some(7));
    assert_eq!(
        document(&service).restrictions[0]
            .scope
            .public_model
            .as_deref(),
        Some("alpha")
    );

    let mut beta = alpha.clone();
    beta.credential_id = "cred-b".into();
    beta.auth_id = "auth-b".into();
    beta.binding = "bind-b".into();
    beta.material = "material-b".into();
    beta.epoch = 4;
    beta.attempt_id = Uuid::from_u128(31);
    assert_eq!(
        call(&service, &beta, &beta.admit_body()).action,
        Action::Skip
    );
    beta.public_model = "beta".into();
    beta.attempt_id = Uuid::from_u128(32);
    assert_eq!(
        call(&service, &beta, &beta.admit_body()).action,
        Action::Allow
    );

    alpha.public_model = "beta".into();
    alpha.scopes = vec![DeclaredScope {
        subject: DeclaredSubject::Credential,
        public_model: None,
    }];
    alpha.attempt_id = Uuid::from_u128(33);
    assert_eq!(
        call(&service, &alpha, &alpha.admit_body()).action,
        Action::Allow
    );
    call(
        &service,
        &alpha,
        &definite(&alpha, &goat_message("weekly", "2026-10-20T00:00:00Z")),
    );
    alpha.attempt_id = Uuid::from_u128(34);
    assert_eq!(
        call(&service, &alpha, &alpha.admit_body()).reason,
        Reason::Restricted
    );
    beta.attempt_id = Uuid::from_u128(35);
    assert_eq!(
        call(&service, &beta, &beta.admit_body()).action,
        Action::Allow
    );
    beta.public_model = "alpha".into();
    beta.attempt_id = Uuid::from_u128(36);
    assert_eq!(
        call(&service, &beta, &beta.admit_body()).action,
        Action::Skip
    );
    assert!(document(&service).restrictions.iter().all(|row| {
        !matches!(
            &row.scope.subject,
            Subject::Pool { pool_id, .. } if pool_id == "pool-2"
        )
    }));

    moved.live_memberships = Some(vec![member("pool-1", 8, &["alpha"])]);
    let before = document(&service).restrictions.len();
    call(&service, &moved, &moved_result);
    assert!(
        document(&service)
            .restrictions
            .iter()
            .all(|row| { pool_version(row) != Some(8) && credential_version(row) != Some(0) })
    );
    assert_eq!(document(&service).restrictions.len(), before);

    let mut missing = World::goat();
    missing.credential_id = "cred-c".into();
    missing.auth_id = "auth-c".into();
    missing.binding = "bind-c".into();
    missing.pool_scope("pool-missing", 1, "alpha");
    missing.attempt_id = Uuid::from_u128(38);
    let failed = call(&service, &missing, &missing.admit_body());
    assert!(failed.unavailable);
    assert_eq!(document(&service).restrictions.len(), before);
}

#[test]
fn stale_version_delete_and_rebind_do_not_resurrect_restrictions() {
    let (_dir, service) = harness(90, 1, 3);
    let mut world = World::goat();
    let body = goat_message("weekly", "2026-10-20T00:00:00Z");
    assert_eq!(
        call(&service, &world, &world.admit_body()).action,
        Action::Allow
    );
    let mut inflight = world.clone();
    inflight.attempt_id = Uuid::from_u128(40);
    inflight.request_id = Uuid::from_u128(140);
    assert_eq!(
        call(&service, &inflight, &inflight.admit_body()).action,
        Action::Allow
    );
    call(&service, &world, &definite(&world, &body));
    assert_eq!(document(&service).restrictions.len(), 1);

    world.version = 4;
    world.attempt_id = Uuid::from_u128(41);
    assert_eq!(
        call(&service, &world, &world.admit_body()).action,
        Action::Allow
    );

    let mut late = inflight.clone();
    late.live_version = Some(4);
    let decision = call(&service, &late, &definite(&late, &body));
    assert_eq!(decision.action, Action::Skip);
    let versions: Vec<u64> = document(&service)
        .restrictions
        .iter()
        .filter_map(credential_version)
        .collect();
    assert_eq!(versions, vec![3]);

    let mut deleted = World::goat();
    deleted.deleted = true;
    deleted.attempt_id = Uuid::from_u128(42);
    assert_eq!(
        call(&service, &deleted, &deleted.admit_body()).reason,
        Reason::Deleted
    );
    let before = document(&service).restrictions.len();
    assert_eq!(
        call(&service, &deleted, &definite(&deleted, &body)).reason,
        Reason::Uncorrelated
    );
    assert_eq!(document(&service).restrictions.len(), before);

    let mut rebound = World::goat();
    rebound.rebound = true;
    rebound.binding = "bind-new".into();
    rebound.material = "material-new".into();
    rebound.attempt_id = Uuid::from_u128(43);
    assert_eq!(
        call(&service, &rebound, &rebound.admit_body()).reason,
        Reason::Rebound
    );
    call(&service, &rebound, &definite(&rebound, &body));
    assert!(document(&service).restrictions.iter().all(|row| {
        match &row.scope.subject {
            Subject::Credential { binding_id, .. } => binding_id != "bind-new",
            Subject::Pool { .. } => true,
        }
    }));
    rebound.rebound = false;
    rebound.attempt_id = Uuid::from_u128(44);
    assert_eq!(
        call(&service, &rebound, &rebound.admit_body()).action,
        Action::Allow
    );
}

#[test]
fn restart_keeps_the_deadline_across_epoch_and_generation() {
    let dir = scratch();
    let path = dir.0.clone();
    let world = World::goat();
    let body = goat_message("weekly", "2026-10-18T00:00:00Z");
    {
        let service = service_at(&path, 90, 1, 3);
        assert_eq!(
            call(&service, &world, &world.admit_body()).action,
            Action::Allow
        );
        call(&service, &world, &definite(&world, &body));
    }
    let service = service_at(&path, 90, 1, 3);
    let mut reloaded = world.clone();
    reloaded.epoch = 77;
    reloaded.material = "material-refreshed".into();
    reloaded.generation = 8;
    reloaded.revision = 11;
    reloaded.digest = [0xcd; 32];
    reloaded.attempt_id = Uuid::from_u128(70);
    let admitted = call(&service, &reloaded, &reloaded.admit_body());
    assert_eq!(admitted.reason, Reason::Restricted);
    assert!(admitted.restriction_deadline.unwrap() > world.now);
    assert_eq!(
        credential_version(&document(&service).restrictions[0]),
        Some(3)
    );

    let mut conn = service.store.conn.lock();
    let before = read_settings(&conn, SETTINGS_KEY).unwrap();
    let error = transact_settings(&mut conn, SETTINGS_KEY, &mut |_tx, _document| {
        Err(PolicyFault::Malformed)
    });
    assert!(error.is_err());
    let after = read_settings(&conn, SETTINGS_KEY).unwrap();
    assert_eq!(before, after);
    drop(conn);

    conn_execute(&service, "UPDATE settings SET value = '{' WHERE key = ?1");
    let broken = call(&service, &reloaded, &reloaded.admit_body());
    assert!(broken.unavailable);
    assert_eq!(raw_settings(&service).as_deref(), Some("{"));
}

#[test]
fn missing_settings_allow_and_bad_version_stops() {
    let (_dir, service) = harness(90, 1, 3);
    let world = World::goat();
    assert_eq!(
        call(&service, &world, &world.admit_body()).action,
        Action::Allow
    );
    conn_execute(
        &service,
        "INSERT OR REPLACE INTO settings (key, value) VALUES (?1, '{\"version\":2,\"restrictions\":[]}')",
    );
    let decision = call(&service, &world, &world.admit_body());
    assert!(decision.unavailable);
    let mut ready_facts = Snapshot(world.ready_facts());
    let ready = service.handle(TOKEN, &world.ready_body(), &mut ready_facts);
    let report = ready.ready.unwrap();
    assert!(!report.policy_ready);
    assert!(report.unavailable);
}

#[test]
fn cancellation_persists_observed_evidence_and_stops() {
    let (_dir, service) = harness(90, 1, 3);
    let world = World::goat();
    let body = goat_message("weekly", "2026-10-19T00:00:00Z");
    assert_eq!(
        call(&service, &world, &world.admit_body()).action,
        Action::Allow
    );
    let decision = call(
        &service,
        &world,
        &world.result_body(
            "cancelled",
            Some(499),
            true,
            true,
            true,
            "cancelled",
            &body,
            None,
        ),
    );
    assert_eq!(
        (decision.action, decision.reason, decision.unavailable),
        (Action::Stop, Reason::Cancelled, false)
    );
    assert_eq!(document(&service).restrictions.len(), 1);
    let mut later = world.clone();
    later.attempt_id = Uuid::from_u128(46);
    assert_eq!(
        call(&service, &later, &later.admit_body()).reason,
        Reason::Restricted
    );
}

#[test]
fn uncertain_results_stop_without_using_status_as_replay_permission() {
    let (_dir, service) = harness(90, 1, 3);
    let world = World::go();
    let plan = "Weekly usage limit reached. Resets in 2 days.";
    let first = reopen(&service, &world, 2);
    let status_only = call(
        &service,
        &first,
        &first.result_body(
            "uncertain",
            Some(429),
            true,
            false,
            false,
            "provider_rejected",
            plan,
            None,
        ),
    );
    assert_eq!(status_only.reason, Reason::Uncertain);
    assert!(document(&service).restrictions.is_empty());

    let second = reopen(&service, &world, 110);
    let accepted = call(
        &service,
        &second,
        &second.result_body(
            "uncertain",
            Some(200),
            true,
            false,
            false,
            "body_lost",
            "",
            None,
        ),
    );
    assert_eq!(accepted.action, Action::Stop);
    let third = reopen(&service, &world, 111);
    let empty_stream = call(
        &service,
        &third,
        &third.result_body(
            "uncertain",
            Some(200),
            true,
            false,
            true,
            "stream_lost",
            "",
            None,
        ),
    );
    assert_eq!(empty_stream.reason, Reason::Uncertain);

    let fourth = reopen(&service, &world, 112);
    let neutral = reopen(&service, &world, 113);
    let post_output = call(
        &service,
        &fourth,
        &fourth.result_body(
            "explicit_rejection",
            Some(429),
            true,
            true,
            true,
            "provider_rejected",
            plan,
            None,
        ),
    );
    assert_eq!(post_output.action, Action::Stop);
    assert_eq!(document(&service).restrictions.len(), 1);

    let not_rejection = call(
        &service,
        &neutral,
        &neutral.result_body(
            "explicit_rejection",
            Some(200),
            true,
            true,
            false,
            "provider_rejected",
            "rate limit exceeded",
            None,
        ),
    );
    assert_eq!(not_rejection.reason, Reason::Uncertain);

    let mut ordered = world.clone();
    ordered.credential_id = "cred-404".into();
    ordered.auth_id = "auth-404".into();
    ordered.binding = "bind-404".into();
    let ordered = reopen(&service, &ordered, 114);
    let ordered_route = call(
        &service,
        &ordered,
        &ordered.result_body(
            "explicit_rejection",
            Some(404),
            true,
            true,
            false,
            "provider_rejected",
            "model route was not found",
            None,
        ),
    );
    assert_eq!(ordered_route.reason, Reason::Uncertain);
    assert_eq!(ordered_route.action, Action::Stop);
}

#[test]
fn unsent_validation_stops_and_unsent_transport_can_continue() {
    let (_dir, service) = harness(90, 1, 3);
    let mut world = World::goat();
    world.kind = SendKind::Validated;
    assert_eq!(
        call(&service, &world, &world.admit_body()).action,
        Action::Allow
    );
    let validation = call(
        &service,
        &world,
        &world.result_body(
            "local_failure",
            None,
            false,
            false,
            false,
            "validation",
            "missing model",
            None,
        ),
    );
    assert_eq!(validation.reason, Reason::LocalValidation);
    assert!(document(&service).restrictions.is_empty());

    world.kind = SendKind::Accepted;
    world.attempt_id = Uuid::from_u128(52);
    assert_eq!(
        call(&service, &world, &world.admit_body()).action,
        Action::Allow
    );
    let transport = call(
        &service,
        &world,
        &world.result_body(
            "local_failure",
            None,
            false,
            false,
            false,
            "transport",
            "",
            None,
        ),
    );
    assert_eq!(
        (transport.action, transport.reason),
        (Action::Skip, Reason::UnsentTransport)
    );
    world.now = world.deadline;
    world.attempt_id = Uuid::from_u128(51);
    let late_admit = call(&service, &world, &world.admit_body());
    assert_eq!(
        (late_admit.action, late_admit.reason),
        (Action::Stop, Reason::Deadline)
    );
    assert!(
        document(&service)
            .attempts
            .iter()
            .all(|row| row.attempt_id != Uuid::from_u128(51))
    );
}

#[test]
fn deadline_stops_admission_and_one_result_publishes_inside_grace() {
    let (_dir, service) = harness(90, 1, 3);
    let mut world = World::goat();
    let body = goat_message("weekly", "2026-10-18T00:00:00Z");
    assert_eq!(
        call(&service, &world, &world.admit_body()).action,
        Action::Allow
    );
    let published = definite(&world, &body);
    world.now = world.deadline;
    let blocked = call(&service, &world, &world.admit_body());
    assert_eq!(
        (blocked.action, blocked.reason),
        (Action::Stop, Reason::Deadline)
    );
    let decision = call(&service, &world, &published);
    assert_eq!(
        (decision.action, decision.reason),
        (Action::Skip, Reason::ExplicitRejection)
    );
    assert_ne!(decision.action, Action::Allow);
    assert_eq!(document(&service).restrictions.len(), 1);
    assert!(document(&service).attempts.is_empty());
    let duplicate = call(&service, &world, &published);
    assert_eq!(duplicate.reason, Reason::Uncorrelated);
    assert_eq!(document(&service).restrictions.len(), 1);

    world.attempt_id = Uuid::from_u128(80);
    world.now = world.deadline + Duration::seconds(super::decide::RESULT_PUBLICATION_GRACE_SECONDS);
    assert_eq!(
        call(&service, &world, &world.admit_body()).reason,
        Reason::Deadline
    );
}

#[test]
fn omitted_observation_still_persists_and_grace_expiry_drops_correlation() {
    let (_dir, service) = harness(90, 1, 3);
    let mut world = World::goat();
    assert_eq!(
        call(&service, &world, &world.admit_body()).action,
        Action::Allow
    );
    let mut omitted = world.common("result");
    omitted["sent"] = json!(true);
    omitted["status"] = json!(429);
    omitted["bodyComplete"] = json!(true);
    omitted["streamStarted"] = json!(false);
    omitted["outcome"] = json!("explicit_rejection");
    omitted["errorCode"] = json!("provider_rejected");
    omitted["responseBody"] = json!(goat_message("weekly", "2026-10-19T00:00:00Z"));
    let decision = call(&service, &world, &serde_json::to_vec(&omitted).unwrap());
    assert_eq!(decision.reason, Reason::ExplicitRejection);
    assert_eq!(
        document(&service).restrictions[0].observation_id,
        world.attempt_id.to_string()
    );
    assert!(document(&service).attempts.is_empty());

    world.attempt_id = Uuid::from_u128(81);
    world.now = world.now + Duration::seconds(30);
    assert_eq!(
        call(&service, &world, &world.admit_body()).reason,
        Reason::Restricted
    );

    let mut later = World::goat();
    later.credential_id = "cred-grace".into();
    later.auth_id = "auth-grace".into();
    later.binding = "bind-grace".into();
    later.attempt_id = Uuid::from_u128(82);
    assert_eq!(
        call(&service, &later, &later.admit_body()).action,
        Action::Allow
    );
    let late_body = definite(&later, &goat_message("weekly", "2026-10-21T00:00:00Z"));
    later.now = later.deadline + Duration::seconds(super::decide::RESULT_PUBLICATION_GRACE_SECONDS);
    assert_eq!(
        call(&service, &later, &late_body).reason,
        Reason::Uncorrelated
    );
    assert!(
        document(&service)
            .restrictions
            .iter()
            .all(|row| credential_version(row) != Some(0))
    );
    assert!(
        !raw_settings(&service)
            .unwrap_or_default()
            .contains("cred-grace")
    );
}

#[test]
fn reload_keeps_previous_projection_result_and_fences_the_next_admit() {
    let (_dir, service) = harness(90, 1, 3);
    let mut world = World::goat();
    let admitted = world.admit_body();
    let published = definite(&world, &goat_message("weekly", "2026-10-22T00:00:00Z"));
    assert_eq!(call(&service, &world, &admitted).action, Action::Allow);
    world.generation = 12;
    world.revision = 3;
    world.digest = [0x11; 32];
    let decision = call(&service, &world, &published);
    assert_eq!(decision.reason, Reason::ExplicitRejection);
    assert_eq!(document(&service).restrictions.len(), 1);
    assert!(document(&service).attempts.is_empty());
    assert_eq!(
        call(&service, &world, &admitted).reason,
        Reason::ProjectionFence
    );
    world.attempt_id = Uuid::from_u128(83);
    assert_eq!(
        call(&service, &world, &world.admit_body()).reason,
        Reason::Restricted
    );
    world.live_version = Some(9);
    world.attempt_id = Uuid::from_u128(84);
    assert_eq!(
        call(&service, &world, &world.admit_body()).reason,
        Reason::StaleCredential
    );
    assert_eq!(
        credential_version(&document(&service).restrictions[0]),
        Some(3)
    );
}

#[test]
fn repeated_uncertain_recovery_keeps_a_later_opportunity() {
    let (_dir, service) = harness(45, 1, 2);
    let mut world = World::goat();
    let body = goat_message("weekly", "not-a-timestamp");
    assert_eq!(
        call(&service, &world, &world.admit_body()).action,
        Action::Allow
    );
    call(&service, &world, &definite(&world, &body));
    assert_eq!(document(&service).restrictions[0].reset, Reset::Unknown);
    assert!(
        raw_settings(&service)
            .unwrap()
            .contains("\"kind\":\"unknown\"")
    );

    world.request_id = Uuid::from_u128(60);
    world.attempt_id = Uuid::from_u128(61);
    assert_eq!(
        call(&service, &world, &world.admit_body()).action,
        Action::Allow
    );
    world.attempt_id = Uuid::from_u128(62);
    let crowded = call(&service, &world, &world.admit_body());
    assert_eq!(crowded.reason, Reason::UnknownReset);
    assert_eq!(
        crowded.restriction_deadline,
        Some(world.deadline + Duration::seconds(super::decide::RESULT_PUBLICATION_GRACE_SECONDS))
    );
    world.attempt_id = Uuid::from_u128(61);
    let uncertain = call(
        &service,
        &world,
        &world.result_body(
            "uncertain",
            Some(429),
            true,
            false,
            false,
            "provider_rejected",
            "reset unknown",
            None,
        ),
    );
    assert_eq!(uncertain.reason, Reason::Uncertain);
    assert_eq!(document(&service).restrictions[0].reset, Reset::Unknown);

    world.request_id = Uuid::from_u128(63);
    world.attempt_id = Uuid::from_u128(64);
    assert_eq!(
        call(&service, &world, &world.admit_body()).reason,
        Reason::UnknownReset
    );
    world.now += Duration::seconds(45);
    assert_eq!(
        call(&service, &world, &world.admit_body()).action,
        Action::Allow
    );
    call(
        &service,
        &world,
        &world.result_body(
            "cancelled",
            None,
            false,
            false,
            false,
            "cancelled",
            "",
            None,
        ),
    );
    world.now += Duration::seconds(45);
    world.attempt_id = Uuid::from_u128(65);
    assert_eq!(
        call(&service, &world, &world.admit_body()).action,
        Action::Allow
    );
    world.now += Duration::seconds(45);
    world.attempt_id = Uuid::from_u128(66);
    let capped = call(&service, &world, &world.admit_body());
    assert_eq!(capped.reason, Reason::UnknownReset);
    assert_eq!(capped.restriction_deadline, None);

    world.attempt_id = Uuid::from_u128(65);
    call(
        &service,
        &world,
        &world.result_body(
            "cancelled",
            None,
            false,
            false,
            false,
            "cancelled",
            "",
            None,
        ),
    );
    world.request_id = Uuid::from_u128(67);
    world.attempt_id = Uuid::from_u128(68);
    world.now += Duration::seconds(45);
    assert_eq!(
        call(&service, &world, &world.admit_body()).action,
        Action::Allow
    );
    let known = goat_message("weekly", "2026-10-18T00:00:00Z");
    call(&service, &world, &definite(&world, &known));
    match document(&service).restrictions[0].reset {
        Reset::Known { at } => assert_eq!(
            at,
            chrono::DateTime::parse_from_rfc3339("2026-10-18T00:00:00Z")
                .unwrap()
                .with_timezone(&Utc)
        ),
        Reset::Unknown => panic!("known evidence did not replace unknown"),
    }
    let stored = raw_settings(&service).unwrap();
    assert!(stored.contains("\"kind\":\"unknown\"") || stored.contains("\"kind\":\"known\""));
    assert!(!stored.contains("900"));
}

#[test]
fn completed_sse_is_completed_and_truncated_sse_stays_uncertain() {
    let (_dir, service) = harness(90, 1, 3);
    let mut world = World::go();
    assert_eq!(
        call(&service, &world, &world.admit_body()).action,
        Action::Allow
    );
    let mut done = serde_json::from_slice::<Value>(&world.result_body(
        "success",
        Some(200),
        true,
        true,
        true,
        "none",
        "data: [DONE]\n",
        None,
    ))
    .unwrap();
    done["reportedUsage"] = json!({"inputTokens": 11, "outputTokens": 7});
    let completed = call(&service, &world, &serde_json::to_vec(&done).unwrap());
    assert_eq!(
        (completed.action, completed.reason),
        (Action::Stop, Reason::Completed)
    );
    assert!(document(&service).restrictions.is_empty());
    assert!(document(&service).attempts.is_empty());

    world.attempt_id = Uuid::from_u128(220);
    world.request_id = Uuid::from_u128(221);
    assert_eq!(
        call(&service, &world, &world.admit_body()).action,
        Action::Allow
    );
    let truncated = call(
        &service,
        &world,
        &world.result_body(
            "success",
            Some(200),
            true,
            false,
            true,
            "none",
            "data: {\"delta\":\"partial\"}\n",
            None,
        ),
    );
    assert_eq!(
        (truncated.action, truncated.reason),
        (Action::Stop, Reason::Uncertain)
    );
    assert!(document(&service).restrictions.is_empty());
    assert!(document(&service).attempts.is_empty());

    world.attempt_id = Uuid::from_u128(222);
    world.request_id = Uuid::from_u128(223);
    assert_eq!(
        call(&service, &world, &world.admit_body()).action,
        Action::Allow
    );
    let post_output = call(
        &service,
        &world,
        &world.result_body(
            "explicit_rejection",
            Some(429),
            true,
            true,
            true,
            "provider_rejected",
            "Weekly usage limit reached. Resets in 4 days.",
            None,
        ),
    );
    assert_eq!(post_output.action, Action::Stop);
    assert_ne!(post_output.reason, Reason::ExplicitRejection);
    assert_eq!(document(&service).restrictions.len(), 1);
}

#[test]
fn unknown_lease_outlives_the_gap_until_deadline_grace() {
    let dir = scratch();
    let path = dir.0.clone();
    let mut world = World::goat();
    assert_eq!(world.deadline - world.now, Duration::seconds(300));
    let started = world.now;
    {
        let service = service_at(&path, 90, 1, 3);
        assert_eq!(
            call(&service, &world, &world.admit_body()).action,
            Action::Allow
        );
        call(
            &service,
            &world,
            &definite(&world, &goat_message("weekly", "not-a-timestamp")),
        );
        assert_eq!(document(&service).restrictions[0].reset, Reset::Unknown);
        world.request_id = Uuid::from_u128(301);
        world.attempt_id = Uuid::from_u128(302);
        assert_eq!(
            call(&service, &world, &world.admit_body()).action,
            Action::Allow
        );
        let held_attempt = world.attempt_id;
        let held_request = world.request_id;
        world.now = started + Duration::seconds(91);
        world.request_id = Uuid::from_u128(303);
        world.attempt_id = Uuid::from_u128(304);
        let blocked = call(&service, &world, &world.admit_body());
        assert_eq!(
            (blocked.action, blocked.reason, blocked.restriction_deadline),
            (
                Action::Skip,
                Reason::UnknownReset,
                Some(
                    world.deadline
                        + Duration::seconds(super::decide::RESULT_PUBLICATION_GRACE_SECONDS)
                )
            )
        );
        world.request_id = held_request;
        world.attempt_id = held_attempt;
        assert_eq!(
            call(&service, &world, &world.admit_body()).action,
            Action::Allow
        );
        world.request_id = Uuid::from_u128(305);
        world.attempt_id = Uuid::from_u128(306);
        assert_eq!(
            call(&service, &world, &world.admit_body()).reason,
            Reason::UnknownReset
        );
    }
    {
        let service = service_at(&path, 90, 1, 3);
        world.now = started + Duration::seconds(120);
        world.request_id = Uuid::from_u128(307);
        world.attempt_id = Uuid::from_u128(308);
        assert_eq!(
            call(&service, &world, &world.admit_body()).reason,
            Reason::UnknownReset
        );
    }
    {
        let service = service_at(&path, 90, 1, 3);
        world.now =
            world.deadline + Duration::seconds(super::decide::RESULT_PUBLICATION_GRACE_SECONDS);
        world.deadline = world.now + Duration::seconds(300);
        world.request_id = Uuid::from_u128(309);
        world.attempt_id = Uuid::from_u128(310);
        assert_eq!(
            call(&service, &world, &world.admit_body()).action,
            Action::Allow
        );
        let rows = document(&service).restrictions;
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].reset, Reset::Unknown);
        let recovery = rows[0].recovery.as_ref().expect("unknown recovery remains");
        assert!(
            recovery
                .inflight
                .iter()
                .all(|item| item.attempt_id != Uuid::from_u128(302)),
            "expired attempt still leased: {:?}",
            recovery.inflight
        );
        assert_eq!(
            recovery
                .inflight
                .iter()
                .map(|item| item.attempt_id)
                .collect::<Vec<_>>(),
            vec![Uuid::from_u128(310)]
        );
    }
}

#[test]
fn invalid_auth_fences_and_bodies_stop_redacted() {
    let (_dir, service) = harness(90, 1, 3);
    let world = World::goat();
    let secret_body = world.result_body(
        "explicit_rejection",
        Some(429),
        true,
        true,
        false,
        "provider_rejected",
        "sk-policy-secret",
        None,
    );
    let mut facts = Snapshot(world.facts());
    let denied = service.handle("nope", &secret_body, &mut facts);
    assert_eq!(denied.status, 401);
    let denied_text = String::from_utf8(denied.bytes).unwrap();
    assert_eq!(denied.decision.unwrap().reason, Reason::Unauthorized);
    assert!(!denied_text.contains("sk-policy-secret"));
    assert!(!denied_text.contains("nope"));

    let mut bad_version = world.common("admit");
    bad_version["protocolVersion"] = json!(2);
    assert_eq!(
        call(&service, &world, &serde_json::to_vec(&bad_version).unwrap()).reason,
        Reason::Malformed
    );
    let mut numeric = world.common("admit");
    numeric["processGeneration"] = json!(4);
    numeric["registrationEpoch"] = json!(1);
    assert_eq!(
        call(&service, &world, &serde_json::to_vec(&numeric).unwrap()).reason,
        Reason::Malformed
    );
    let mut http_code = world.common("result");
    http_code["sent"] = json!(true);
    http_code["status"] = json!(429);
    http_code["bodyComplete"] = json!(true);
    http_code["streamStarted"] = json!(false);
    http_code["outcome"] = json!("explicit_rejection");
    http_code["errorCode"] = json!("http_429");
    http_code["responseBody"] = json!("nope");
    assert_eq!(
        call(&service, &world, &serde_json::to_vec(&http_code).unwrap()).reason,
        Reason::Malformed
    );

    let huge = vec![b'{'; super::wire::MAX_REQUEST_BYTES + 1];
    let mut over_facts = Snapshot(world.facts());
    let oversize = service.handle(TOKEN, &huge, &mut over_facts);
    assert_eq!(oversize.status, 413);

    let mut fat = world.common("result");
    fat["sent"] = json!(true);
    fat["status"] = json!(429);
    fat["bodyComplete"] = json!(true);
    fat["streamStarted"] = json!(false);
    fat["outcome"] = json!("explicit_rejection");
    fat["errorCode"] = json!("provider_rejected");
    fat["responseBody"] = json!("x".repeat(super::wire::MAX_EVIDENCE_BODY_BYTES + 1));
    let mut fat_facts = Snapshot(world.facts());
    let fat_reply = service.handle(TOKEN, &serde_json::to_vec(&fat).unwrap(), &mut fat_facts);
    assert_eq!(fat_reply.decision.unwrap().reason, Reason::Oversize);
    assert!(
        !String::from_utf8(fat_reply.bytes)
            .unwrap()
            .contains(&"x".repeat(80))
    );

    let mut fenced = world.clone();
    fenced.generation = 99;
    assert_eq!(
        call(&service, &fenced, &world.admit_body()).reason,
        Reason::ProjectionFence
    );
    let mut attempt = world.clone();
    attempt.attempt_id = Uuid::from_u128(77);
    assert_eq!(
        call(&service, &attempt, &world.admit_body()).reason,
        Reason::IdentityFence
    );
    let mut model = world.clone();
    model.public_model = "other-public".into();
    assert_eq!(
        call(&service, &model, &world.admit_body()).reason,
        Reason::ModelFence
    );
    let mut namespaced = world.common("admit");
    namespaced["providerId"] = json!("compat-cred-a");
    assert_eq!(
        call(&service, &world, &serde_json::to_vec(&namespaced).unwrap()).reason,
        Reason::IdentityFence
    );
    let mut ungated = world.clone();
    ungated.granted = false;
    assert_eq!(
        call(&service, &ungated, &ungated.admit_body()).reason,
        Reason::NotGranted
    );
    assert!(document(&service).restrictions.is_empty());

    let mut ready_facts = Snapshot(world.ready_facts());
    let ready = service.handle(TOKEN, &world.ready_body(), &mut ready_facts);
    let report = ready.ready.unwrap();
    assert!(report.policy_ready);
    assert_eq!(report.process_generation, world.generation);
    assert_eq!(report.projection_digest, world.digest);
    let ready_json: Value = serde_json::from_slice(&ready.bytes).unwrap();
    let mut keys: Vec<_> = ready_json.as_object().unwrap().keys().cloned().collect();
    keys.sort();
    assert_eq!(
        keys,
        vec![
            "endpointPins",
            "policyReady",
            "processGeneration",
            "projectionDigest",
            "projectionRevision",
            "protocolVersion",
            "reason",
            "unavailable",
        ]
    );
}

#[test]
fn frozen_endpoint_pin_membership_keeps_restriction_authority() {
    let allowed = crate::cpa_projection::NativeEndpointPin {
        protocol: "chat_completions".into(),
        endpoint_id: "ep-allowed".into(),
        origin: "https://example.test".into(),
        endpoint_fingerprint: "ab".repeat(32),
        http_method: "POST".into(),
    };
    let other = crate::cpa_projection::NativeEndpointPin {
        protocol: "chat_completions".into(),
        endpoint_id: "ep-other".into(),
        origin: "https://example.test".into(),
        endpoint_fingerprint: "cd".repeat(32),
        http_method: "POST".into(),
    };
    let (_dir, service) = harness(90, 1, 3);
    let mut world = World::goat();
    let mut facts = PinFacts {
        facts: RefCell::new(world.facts()),
        allow_pins: Some(vec![allowed.clone()]),
        frozen: RefCell::new(std::collections::HashMap::new()),
        records: Cell::new(0),
        authority: Cell::new(true),
    };

    let admitted = handle_pins(&service, &mut facts, &world, &world.admit_body());
    let admitted_decision = admitted.decision.expect("allow");
    assert_eq!(admitted_decision.action, Action::Allow);
    assert_eq!(admitted_decision.endpoint_pins, Some(vec![allowed.clone()]));
    let admitted_json: Value = serde_json::from_slice(&admitted.bytes).unwrap();
    assert_eq!(admitted_json["endpointPins"][0]["endpointId"], "ep-allowed");
    assert_eq!(admitted_json["endpointPins"][0]["httpMethod"], "POST");

    let denied = handle_pins(
        &service,
        &mut facts,
        &world,
        &with_endpoint_pin(&definite(&world, "not a provider body"), Some(&other)),
    );
    let denied_decision = denied.decision.expect("local pin stop");
    assert_eq!(
        (denied_decision.action, denied_decision.reason),
        (Action::Stop, Reason::IdentityFence)
    );
    assert!(denied_decision.endpoint_pins.is_none());
    let denied_json: Value = serde_json::from_slice(&denied.bytes).unwrap();
    assert!(denied_json["endpointPins"].is_null());
    assert_eq!(facts.records.get(), 0);
    assert!(document(&service).restrictions.is_empty());
    assert!(
        document(&service)
            .attempts
            .iter()
            .any(|row| row.attempt_id == world.attempt_id)
    );

    facts.allow_pins = None;
    let mut second = world.clone();
    second.credential_id = "cred-b".into();
    second.auth_id = "auth-b".into();
    second.binding = "bind-b".into();
    second.attempt_id = Uuid::from_u128(90);
    second.request_id = Uuid::from_u128(10_090);
    let second_admit = handle_pins(&service, &mut facts, &second, &second.admit_body());
    assert_eq!(
        second_admit.decision.expect("second credential").action,
        Action::Allow
    );
    let second_json: Value = serde_json::from_slice(&second_admit.bytes).unwrap();
    assert!(second_json["endpointPins"].is_null());
    let invented = handle_pins(
        &service,
        &mut facts,
        &second,
        &with_endpoint_pin(&definite(&second, "local only"), Some(&allowed)),
    );
    assert_eq!(
        invented.decision.expect("null allow rejects a pin").reason,
        Reason::IdentityFence
    );
    assert_eq!(facts.records.get(), 0);
    assert!(
        document(&service)
            .attempts
            .iter()
            .any(|row| row.attempt_id == second.attempt_id)
    );

    let counted = handle_pins(
        &service,
        &mut facts,
        &second,
        &second.result_body("success", Some(200), true, true, false, "none", "ok", None),
    );
    assert_eq!(
        counted.decision.expect("null pin").reason,
        Reason::Completed
    );
    assert_eq!(facts.records.get(), 1);
    assert!(document(&service).restrictions.is_empty());
    assert!(
        document(&service)
            .attempts
            .iter()
            .all(|row| row.attempt_id != second.attempt_id)
    );

    let restricted = handle_pins(
        &service,
        &mut facts,
        &world,
        &with_endpoint_pin(
            &definite(&world, &goat_message("weekly", "2026-10-18T00:00:00Z")),
            Some(&allowed),
        ),
    );
    assert_eq!(
        restricted.decision.expect("member pin").reason,
        Reason::ExplicitRejection
    );
    assert_eq!(facts.records.get(), 2);
    let saved = document(&service).restrictions;
    assert_eq!(saved.len(), 1);
    assert!(matches!(saved[0].reset, Reset::Known { .. }));
    assert!(
        document(&service)
            .attempts
            .iter()
            .all(|row| row.attempt_id != world.attempt_id)
    );

    let mut third = world.clone();
    third.credential_id = "cred-c".into();
    third.auth_id = "auth-c".into();
    third.binding = "bind-c".into();
    third.attempt_id = Uuid::from_u128(91);
    third.request_id = Uuid::from_u128(10_091);
    assert_eq!(
        handle_pins(&service, &mut facts, &third, &third.admit_body())
            .decision
            .expect("third admit")
            .action,
        Action::Allow
    );
    facts.authority.set(false);
    let untouched = handle_pins(
        &service,
        &mut facts,
        &third,
        &definite(&third, &goat_message("weekly", "2026-11-01T00:00:00Z")),
    );
    assert_eq!(
        untouched.decision.expect("authority lost").reason,
        Reason::ProviderRejected
    );
    assert_eq!(facts.records.get(), 3);
    assert_eq!(document(&service).restrictions, saved);
    assert!(
        document(&service)
            .attempts
            .iter()
            .all(|row| row.attempt_id != third.attempt_id)
    );
}

struct PinFacts {
    facts: RefCell<PolicyFacts>,
    allow_pins: Option<Vec<crate::cpa_projection::NativeEndpointPin>>,
    frozen: RefCell<
        std::collections::HashMap<Uuid, Option<Vec<crate::cpa_projection::NativeEndpointPin>>>,
    >,
    records: Cell<u32>,
    authority: Cell<bool>,
}

impl CurrentFacts for PinFacts {
    fn revalidate(&mut self, _tx: &Transaction<'_>) -> Result<PolicyFacts, PolicyFault> {
        Ok(self.facts.borrow().clone())
    }

    fn freeze_admitted_attempt(
        &mut self,
        _tx: &Transaction<'_>,
        identity: &super::wire::AttemptIdentity,
        _allowed_at: chrono::DateTime<Utc>,
    ) -> Result<Option<Vec<crate::cpa_projection::NativeEndpointPin>>, PolicyFault> {
        let pins = self.allow_pins.clone();
        self.frozen
            .borrow_mut()
            .insert(identity.attempt_id, pins.clone());
        Ok(pins)
    }

    fn frozen_endpoint_pins(
        &self,
        attempt_id: Uuid,
    ) -> Option<Option<Vec<crate::cpa_projection::NativeEndpointPin>>> {
        self.frozen.borrow().get(&attempt_id).cloned()
    }

    fn retains_quota_restriction_authority(&self) -> bool {
        self.authority.get()
    }

    fn record_admitted_result(
        &mut self,
        _tx: &Transaction<'_>,
        _decision: &Decision,
        _identity: &super::wire::AttemptIdentity,
        _result: &super::wire::ResultBody,
        _raw_body: &str,
    ) -> Result<(), PolicyFault> {
        self.records.set(self.records.get() + 1);
        Ok(())
    }
}

fn handle_pins(
    service: &PolicyService<SqliteStore>,
    facts: &mut PinFacts,
    world: &World,
    body: &[u8],
) -> super::wire::PolicyReply {
    *facts.facts.borrow_mut() = world.facts();
    service.handle(TOKEN, body, facts)
}

fn with_endpoint_pin(
    body: &[u8],
    pin: Option<&crate::cpa_projection::NativeEndpointPin>,
) -> Vec<u8> {
    let mut value: Value = serde_json::from_slice(body).unwrap();
    value["endpointPin"] = match pin {
        Some(pin) => serde_json::to_value(pin).unwrap(),
        None => Value::Null,
    };
    serde_json::to_vec(&value).unwrap()
}

#[test]
fn cross_language_fixture_uses_the_go_body() {
    let fixture = policy_fixture();
    for key in [
        "ready",
        "admit",
        "success",
        "explicitTrustedLimit",
        "ordinary429",
        "bodyLost",
        "cancelled",
        "nullStatus",
    ] {
        assert!(
            fixture.get(key).and_then(Value::as_object).is_some(),
            "runtime/cpa/testdata/policy-v1.identity.json missing {key}"
        );
    }
    assert!(super::wire::parse_request(&serde_json::to_vec(&fixture["admit"]).unwrap()).is_ok());
    assert!(super::wire::parse_request(&serde_json::to_vec(&fixture["success"]).unwrap()).is_ok());
    let mut unpaired = fixture["admit"].clone();
    unpaired.as_object_mut().unwrap().remove("generationKind");
    assert!(super::wire::parse_request(&serde_json::to_vec(&unpaired).unwrap()).is_err());
    let mut missing_pin = fixture["success"].clone();
    missing_pin.as_object_mut().unwrap().remove("endpointPin");
    assert!(super::wire::parse_request(&serde_json::to_vec(&missing_pin).unwrap()).is_err());
    let mut admit_fields_on_result = fixture["success"].clone();
    admit_fields_on_result["callableProtocol"] = json!("chat_completions");
    admit_fields_on_result["generationKind"] = json!("execute");
    assert!(
        super::wire::parse_request(&serde_json::to_vec(&admit_fields_on_result).unwrap()).is_err()
    );
    let trusted_provider = fixture["explicitTrustedLimit"]["providerId"]
        .as_str()
        .unwrap_or("");
    assert!(
        trusted_provider == "command-code" || trusted_provider == "opencode",
        "explicitTrustedLimit.providerId is {trusted_provider}; trusted quota must use command-code or opencode"
    );

    let (_dir, service) = harness(90, 1, 3);
    let ready_body = &fixture["ready"];
    let ready_world = world_from_projection(ready_body);
    let mut ready_facts = Snapshot(ready_world.ready_facts());
    let ready = service.handle(
        TOKEN,
        &serde_json::to_vec(ready_body).unwrap(),
        &mut ready_facts,
    );
    let report = ready.ready.unwrap();
    assert!(report.policy_ready);
    assert_eq!(report.process_generation, ready_world.generation);
    assert!(
        !String::from_utf8(ready.bytes)
            .unwrap()
            .contains("\"action\"")
    );

    post_fixture_result(&service, &fixture["success"], Reason::Completed);
    assert!(document(&service).restrictions.is_empty());

    post_fixture_result(
        &service,
        &fixture["explicitTrustedLimit"],
        Reason::ExplicitRejection,
    );
    assert_eq!(document(&service).restrictions.len(), 1);
    assert!(
        document(&service).restrictions.iter().any(|row| {
            row.window == Window::FiveHours && row.source == EvidenceSource::GoatPlan
        })
    );
    let trusted_world = world_from_fixture_attempt(&fixture["explicitTrustedLimit"]);
    assert_eq!(
        call(
            &service,
            &trusted_world,
            &serde_json::to_vec(&fixture["explicitTrustedLimit"]).unwrap()
        )
        .reason,
        Reason::Uncorrelated
    );
    assert_eq!(document(&service).restrictions.len(), 1);

    post_fixture_result(&service, &fixture["ordinary429"], Reason::ProviderRejected);
    post_fixture_result(&service, &fixture["bodyLost"], Reason::Uncertain);
    post_fixture_result(&service, &fixture["cancelled"], Reason::Cancelled);
    post_fixture_result(&service, &fixture["nullStatus"], Reason::Uncertain);
    assert_eq!(document(&service).restrictions.len(), 1);

    let admit = &fixture["admit"];
    let admit_world = world_from_fixture_attempt(admit);
    let admit_bytes = serde_json::to_vec(admit).unwrap();
    assert_eq!(
        call(&service, &admit_world, &admit_bytes).action,
        Action::Allow
    );
    let before = raw_settings(&service);
    let mut denial = Denial;
    let failed = service.handle(TOKEN, &admit_bytes, &mut denial);
    assert!(failed.decision.unwrap().unavailable);
    assert_eq!(raw_settings(&service), before);
}

fn post_fixture_result(service: &PolicyService<SqliteStore>, body: &Value, reason: Reason) {
    let world = world_from_fixture_attempt(body);
    let admit = admit_from_result(body);
    assert_eq!(
        call(service, &world, &serde_json::to_vec(&admit).unwrap()).action,
        Action::Allow
    );
    let decision = call(service, &world, &serde_json::to_vec(body).unwrap());
    assert_eq!(decision.reason, reason, "{body}");
    assert_ne!(decision.action, Action::Allow);
    assert!(
        document(service)
            .attempts
            .iter()
            .all(|row| row.attempt_id != world.attempt_id)
    );
}

fn admit_from_result(body: &Value) -> Value {
    let mut admit = body.clone();
    admit["operation"] = json!("admit");
    let object = admit.as_object_mut().unwrap();
    for key in [
        "sent",
        "status",
        "bodyComplete",
        "streamStarted",
        "outcome",
        "errorCode",
        "responseBody",
        "headers",
        "reportedUsage",
        "observation",
        "endpointPin",
    ] {
        object.remove(key);
    }
    admit["callableProtocol"] = json!("chat_completions");
    admit["generationKind"] = json!("execute");
    admit
}

fn world_from_projection(body: &Value) -> World {
    let mut world = World::goat();
    world.generation = body["processGeneration"].as_str().unwrap().parse().unwrap();
    world.revision = body["projectionRevision"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();
    hex::decode_to_slice(
        body["projectionDigest"].as_str().unwrap(),
        &mut world.digest,
    )
    .unwrap();
    world
}

fn policy_fixture() -> Value {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../runtime/cpa/testdata/policy-v1.identity.json");
    let bytes =
        std::fs::read(&path).unwrap_or_else(|error| panic!("read {}: {error}", path.display()));
    serde_json::from_slice(&bytes)
        .unwrap_or_else(|error| panic!("json {}: {error}", path.display()))
}

fn restriction_row(
    subject: Subject,
    model: Option<&str>,
    window: Window,
    reset: Reset,
    source: EvidenceSource,
    observation_id: &str,
) -> Restriction {
    let now = Utc.with_ymd_and_hms(2026, 10, 4, 12, 0, 0).unwrap();
    Restriction {
        scope: super::store::Scope {
            subject,
            public_model: model.map(str::to_string),
        },
        window,
        reset,
        observed_at: now,
        observation_id: observation_id.into(),
        source,
        recovery: matches!(reset, Reset::Unknown).then(super::store::UnknownRecovery::default),
    }
}

#[test]
fn scoped_restriction_evidence_reports_known_unknown_reset_expired_and_malformed() {
    let now = Utc.with_ymd_and_hms(2026, 10, 4, 12, 0, 0).unwrap();
    let later = now + Duration::hours(5);
    let earlier = now - Duration::hours(1);
    let credential = Subject::Credential {
        credential_id: "cred".into(),
        credential_version: 3,
        provider_id: "opencode".into(),
        binding_id: "bind".into(),
    };
    let mut unknown_reset = restriction_row(
        credential.clone(),
        Some("public"),
        Window::Week,
        Reset::Unknown,
        EvidenceSource::GoUsage,
        "obs-unknown",
    );
    unknown_reset.recovery = Some(super::store::UnknownRecovery {
        inflight: vec![super::store::InflightAdmit {
            request_id: Uuid::nil(),
            attempt_id: Uuid::nil(),
            at: now,
        }],
        ..super::store::UnknownRecovery::default()
    });
    let document = PolicyDocument {
        restrictions: vec![
            restriction_row(
                credential.clone(),
                Some("public"),
                Window::FiveHours,
                Reset::Known { at: later },
                EvidenceSource::GoatPlan,
                "obs-known",
            ),
            unknown_reset,
            restriction_row(
                credential.clone(),
                Some("public"),
                Window::Month,
                Reset::Known { at: earlier },
                EvidenceSource::GoLimit,
                "obs-expired",
            ),
            restriction_row(
                Subject::Credential {
                    credential_id: "cred".into(),
                    credential_version: 2,
                    provider_id: "opencode".into(),
                    binding_id: "bind".into(),
                },
                Some("public"),
                Window::FiveHours,
                Reset::Known { at: later },
                EvidenceSource::GoatPlan,
                "obs-old-version",
            ),
            restriction_row(
                Subject::Pool {
                    pool_id: "pool".into(),
                    pool_version: 8,
                },
                Some("public"),
                Window::Free,
                Reset::Known { at: later },
                EvidenceSource::GoUsage,
                "obs-pool",
            ),
        ],
        attempts: Vec::new(),
    };
    let before = document.clone();
    let subject = super::RestrictionSubject {
        credential_id: "cred".into(),
        credential_version: 3,
        provider_id: "opencode".into(),
        binding_id: "bind".into(),
        memberships: vec![PoolMembership {
            pool_id: "pool".into(),
            pool_version: 8,
            public_models: vec!["public".into()],
            all_models: false,
        }],
        public_model: "public".into(),
    };
    let view = super::scoped_restriction_evidence(Ok(&document), &subject, now).unwrap();
    assert_eq!(document, before);
    let super::ScopedQuotaView::Evidence(rows) = view else {
        panic!("matching restrictions are evidence, not unknown quota");
    };
    assert_eq!(rows.len(), 4);
    assert!(rows.iter().any(|row| {
        row.observation_id == "obs-known"
            && row.applicable
            && matches!(row.reset, super::ResetEvidence::Known { at } if at == later)
            && row.window == Window::FiveHours
            && row.source == EvidenceSource::GoatPlan
    }));
    assert!(rows.iter().any(|row| {
        row.observation_id == "obs-unknown"
            && row.applicable
            && matches!(row.reset, super::ResetEvidence::UnknownReset)
    }));
    assert!(rows.iter().any(|row| {
        row.observation_id == "obs-expired"
            && !row.applicable
            && matches!(row.reset, super::ResetEvidence::Expired { at } if at == earlier)
    }));
    assert!(
        rows.iter()
            .any(|row| row.observation_id == "obs-pool" && row.applicable)
    );
    assert!(
        rows.iter()
            .all(|row| row.observation_id != "obs-old-version")
    );
    let old_version = super::RestrictionSubject {
        credential_version: 2,
        memberships: Vec::new(),
        ..subject.clone()
    };
    let old = super::scoped_restriction_evidence(Ok(&document), &old_version, now).unwrap();
    let super::ScopedQuotaView::Evidence(old_rows) = old else {
        panic!("the applied route version keeps its own restriction");
    };
    assert!(
        old_rows
            .iter()
            .any(|row| row.observation_id == "obs-old-version")
    );
    let other_model = super::RestrictionSubject {
        public_model: "other".into(),
        memberships: Vec::new(),
        ..subject
    };
    assert_eq!(
        super::scoped_restriction_evidence(Ok(&document), &other_model, now).unwrap(),
        super::ScopedQuotaView::Unknown
    );
    assert_eq!(
        super::scoped_restriction_evidence(Err(PolicyFault::Malformed), &other_model, now).unwrap(),
        super::ScopedQuotaView::Malformed
    );
    assert!(matches!(
        super::scoped_restriction_evidence(Err(PolicyFault::Unavailable), &other_model, now),
        Err(PolicyFault::Unavailable)
    ));
    assert_eq!(document, before);
}

#[test]
fn scoped_restriction_evidence_matches_admission_applies_without_an_attempt() {
    let now = Utc.with_ymd_and_hms(2026, 10, 4, 12, 0, 0).unwrap();
    let row = restriction_row(
        Subject::Credential {
            credential_id: "cred".into(),
            credential_version: 3,
            provider_id: "opencode".into(),
            binding_id: "bind".into(),
        },
        Some("public"),
        Window::Week,
        Reset::Known {
            at: now + Duration::hours(1),
        },
        EvidenceSource::GoUsage,
        "obs-shared",
    );
    let subject = super::RestrictionSubject {
        credential_id: "cred".into(),
        credential_version: 3,
        provider_id: "opencode".into(),
        binding_id: "bind".into(),
        memberships: Vec::new(),
        public_model: "public".into(),
    };
    let current = super::wire::CurrentCredential {
        credential_id: "cred".into(),
        credential_version: 3,
        provider_id: "opencode".into(),
        binding_id: "bind".into(),
        material_revision: "material".into(),
        registration_epoch: 1,
        auth_id: "auth".into(),
        memberships: Vec::new(),
        scopes: Vec::new(),
        granted: true,
        enabled: true,
        deleted: false,
        rebound: false,
    };
    let attempt = super::wire::AttemptIdentity {
        request_id: Uuid::nil(),
        attempt_id: Uuid::nil(),
        auth_id: "auth".into(),
        credential_id: "cred".into(),
        credential_version: 3,
        provider_id: "opencode".into(),
        public_model: "public".into(),
        upstream_model: "upstream".into(),
        binding_id: "bind".into(),
        material_revision: "material".into(),
        registration_epoch: 1,
        kind: SendKind::Accepted,
    };
    let mut facts = PolicyFacts {
        applied: super::wire::AppliedProjection {
            process_generation: 1,
            revision: 1,
            digest: [0; 32],
        },
        attempt: Some(attempt),
        current: Some(current),
        deadline_at: now + Duration::minutes(1),
        now,
        caller_stop: None,
        validated_pin: false,
    };
    assert!(super::decide::applies(&row, &facts));
    assert!(super::restriction_matches(
        &row,
        "cred",
        3,
        "opencode",
        "bind",
        &[],
        "public",
    ));
    facts.attempt = None;
    assert!(!super::decide::applies(&row, &facts));
    let document = PolicyDocument {
        restrictions: vec![row],
        attempts: Vec::new(),
    };
    let view = super::scoped_restriction_evidence(Ok(&document), &subject, now).unwrap();
    let super::ScopedQuotaView::Evidence(rows) = view else {
        panic!("the read helper does not need an attempt id");
    };
    assert_eq!(rows.len(), 1);
    assert!(rows[0].applicable);
}

fn world_from_fixture_attempt(admit: &Value) -> World {
    let mut world = World::goat();
    world.generation = admit
        .get("processGeneration")
        .unwrap()
        .as_str()
        .unwrap()
        .parse()
        .unwrap();
    world.revision = admit
        .get("projectionRevision")
        .unwrap()
        .as_str()
        .unwrap()
        .parse()
        .unwrap();
    let digest = admit.get("projectionDigest").unwrap().as_str().unwrap();
    hex::decode_to_slice(digest, &mut world.digest).unwrap();
    world.request_id = Uuid::parse_str(admit.get("requestId").unwrap().as_str().unwrap()).unwrap();
    world.attempt_id = Uuid::parse_str(admit.get("attemptId").unwrap().as_str().unwrap()).unwrap();
    world.auth_id = admit.get("authId").unwrap().as_str().unwrap().to_string();
    world.credential_id = admit
        .get("credentialId")
        .unwrap()
        .as_str()
        .unwrap()
        .to_string();
    world.version = admit
        .get("credentialVersion")
        .unwrap()
        .as_str()
        .unwrap()
        .parse()
        .unwrap();
    world.provider = admit
        .get("providerId")
        .unwrap()
        .as_str()
        .unwrap()
        .to_string();
    world.public_model = admit
        .get("publicModel")
        .unwrap()
        .as_str()
        .unwrap()
        .to_string();
    world.upstream_model = admit
        .get("upstreamModel")
        .unwrap()
        .as_str()
        .unwrap()
        .to_string();
    world.epoch = admit
        .get("registrationEpoch")
        .unwrap()
        .as_str()
        .unwrap()
        .parse()
        .unwrap();
    world.material = admit
        .get("materialRevision")
        .unwrap()
        .as_str()
        .unwrap()
        .to_string();
    world.kind = match admit.get("kind").unwrap().as_str().unwrap() {
        "validated" => SendKind::Validated,
        _ => SendKind::Accepted,
    };
    world
}

#[test]
fn cross_model_admit_sees_a_concurrent_result_commit() {
    let dir = scratch();
    let entered = Arc::new((Mutex::new(false), Condvar::new()));
    let release = Arc::new((Mutex::new(false), Condvar::new()));
    let store = PausingStore {
        inner: SqliteStore::open(dir.0.join("policy.sqlite")),
        seen: AtomicU64::new(0),
        entered: Arc::clone(&entered),
        release: Arc::clone(&release),
    };
    let service = Arc::new(PolicyService::new(TOKEN, OpportunityPolicy::default(), store).unwrap());
    let world = World::go();
    let mut other = world.clone();
    other.public_model = "beta".into();
    other.upstream_model = "beta-upstream".into();
    other.attempt_id = Uuid::from_u128(81);
    {
        let mut facts = Snapshot(world.facts());
        assert_eq!(
            service
                .handle(TOKEN, &world.admit_body(), &mut facts)
                .decision
                .unwrap()
                .action,
            Action::Allow
        );
    }
    let result_body = definite(&world, "Weekly usage limit reached. Resets in 4 days.");
    let finished = Arc::new(AtomicBool::new(false));
    let service_result = Arc::clone(&service);
    let world_result = world.clone();
    let result_thread = std::thread::spawn(move || {
        let mut facts = Snapshot(world_result.facts());
        service_result.handle(TOKEN, &result_body, &mut facts)
    });
    {
        let (lock, cv) = &*entered;
        let mut flag = lock.lock().unwrap();
        let started = std::time::Instant::now();
        while !*flag {
            let wait = cv.wait_timeout(flag, StdDuration::from_secs(5)).unwrap();
            flag = wait.0;
            if wait.1.timed_out() || started.elapsed() > StdDuration::from_secs(5) {
                panic!("result did not reach the settings transaction");
            }
        }
    }
    let finished_flag = Arc::clone(&finished);
    let service_admit = Arc::clone(&service);
    let admit_thread = std::thread::spawn(move || {
        let mut facts = Snapshot(other.facts());
        let decision = service_admit
            .handle(TOKEN, &other.admit_body(), &mut facts)
            .decision
            .unwrap();
        finished_flag.store(true, Ordering::SeqCst);
        decision
    });
    std::thread::sleep(StdDuration::from_millis(100));
    assert!(!finished.load(Ordering::SeqCst));
    {
        let (lock, cv) = &*release;
        *lock.lock().unwrap() = true;
        cv.notify_one();
    }
    let result = result_thread.join().unwrap().decision.unwrap();
    let admit = admit_thread.join().unwrap();
    assert_eq!(result.action, Action::Skip);
    assert_eq!(
        (admit.action, admit.reason),
        (Action::Skip, Reason::Restricted)
    );
}

struct PausingStore {
    inner: SqliteStore,
    seen: AtomicU64,
    entered: Arc<(Mutex<bool>, Condvar)>,
    release: Arc<(Mutex<bool>, Condvar)>,
}

impl PolicyStore for PausingStore {
    fn read(
        &self,
        read: &mut dyn for<'tx> FnMut(
            &Transaction<'tx>,
            &PolicyDocument,
        ) -> Result<(), PolicyFault>,
    ) -> Result<(), PolicyFault> {
        self.inner.read(read)
    }

    fn update(
        &self,
        mutate: &mut dyn for<'tx> FnMut(
            &Transaction<'tx>,
            &mut PolicyDocument,
        ) -> Result<(), PolicyFault>,
    ) -> Result<(), PolicyFault> {
        if self.seen.fetch_add(1, Ordering::SeqCst) == 1 {
            {
                let (lock, cv) = &*self.entered;
                *lock.lock().unwrap() = true;
                cv.notify_one();
            }
            let (lock, cv) = &*self.release;
            let mut ready = lock.lock().unwrap();
            let started = std::time::Instant::now();
            while !*ready {
                let wait = cv.wait_timeout(ready, StdDuration::from_secs(5)).unwrap();
                ready = wait.0;
                if wait.1.timed_out() || started.elapsed() > StdDuration::from_secs(5) {
                    return Err(PolicyFault::Unavailable);
                }
            }
        }
        self.inner.update(mutate)
    }
}

fn conn_execute(service: &PolicyService<SqliteStore>, sql: &str) {
    service
        .store
        .conn
        .lock()
        .execute(sql, [SETTINGS_KEY])
        .unwrap();
}

#[test]
fn allow_freezes_once_and_only_a_consuming_result_records() {
    let (_dir, service) = harness(90, 1, 3);
    let mut denied = World::go();
    denied.deleted = true;
    denied.request_id = Uuid::from_u128(11);
    denied.attempt_id = Uuid::from_u128(12);
    let mut facts = Counting {
        facts: denied.facts(),
        freezes: Cell::new(0),
        records: Cell::new(0),
    };
    let denied_decision = service
        .handle(TOKEN, &denied.admit_body(), &mut facts)
        .decision
        .expect("deleted admit");
    assert_eq!(denied_decision.action, Action::Skip);
    assert_eq!(denied_decision.reason, Reason::Deleted);
    assert!(!denied_decision.unavailable);
    assert_eq!(facts.freezes.get(), 0);
    assert_eq!(facts.records.get(), 0);

    let world = World::go();
    facts.facts = world.facts();
    let allowed = service
        .handle(TOKEN, &world.admit_body(), &mut facts)
        .decision
        .expect("allow");
    assert_eq!(allowed.action, Action::Allow);
    assert_eq!(allowed.reason, Reason::Eligible);
    assert_eq!(facts.freezes.get(), 1);
    assert_eq!(facts.records.get(), 0);

    let mut forged: Value = serde_json::from_slice(&world.result_body(
        "explicit_rejection",
        Some(429),
        true,
        true,
        false,
        "provider_rejected",
        "provider rejected this attempt",
        None,
    ))
    .unwrap();
    forged["providerId"] = json!(COMMAND_CODE_PROVIDER_ID);
    let fenced = service
        .handle(TOKEN, &serde_json::to_vec(&forged).unwrap(), &mut facts)
        .decision
        .expect("identity fence");
    assert_eq!(fenced.reason, Reason::IdentityFence);
    assert!(!fenced.unavailable);
    assert_eq!(facts.freezes.get(), 1);
    assert_eq!(facts.records.get(), 0);

    let consumed = service
        .handle(
            TOKEN,
            &definite(&world, "provider rejected this attempt"),
            &mut facts,
        )
        .decision
        .expect("consuming result");
    assert!(!consumed.unavailable);
    assert_ne!(consumed.reason, Reason::Unavailable);
    assert_eq!(facts.freezes.get(), 1);
    assert_eq!(facts.records.get(), 1);
}

fn goat_at(credential: &str, auth: &str, binding: &str, attempt: u128) -> World {
    let mut world = World::goat();
    world.credential_id = credential.into();
    world.auth_id = auth.into();
    world.binding = binding.into();
    world.attempt_id = Uuid::from_u128(attempt);
    world.request_id = Uuid::from_u128(attempt + 10_000);
    world
}

fn publish(
    service: &PolicyService<SqliteStore>,
    world: &World,
    body: &[u8],
) -> (Decision, Observed) {
    let mut observed = Observed::new(world.facts());
    let decision = service
        .handle(TOKEN, body, &mut observed)
        .decision
        .expect("decision");
    assert!(!decision.unavailable);
    (decision, observed)
}

fn credential_restrictions(
    service: &PolicyService<SqliteStore>,
    credential_id: &str,
) -> Vec<Restriction> {
    document(service)
        .restrictions
        .into_iter()
        .filter(|row| {
            matches!(
                &row.scope.subject,
                Subject::Credential {
                    credential_id: id,
                    ..
                } if id == credential_id
            )
        })
        .collect()
}

fn pool_restrictions(service: &PolicyService<SqliteStore>, pool_id: &str) -> Vec<Restriction> {
    document(service)
        .restrictions
        .into_iter()
        .filter(|row| {
            matches!(
                &row.scope.subject,
                Subject::Pool { pool_id: id, .. } if id == pool_id
            )
        })
        .collect()
}

fn known_at(row: &Restriction) -> chrono::DateTime<Utc> {
    match row.reset {
        Reset::Known { at } => at,
        Reset::Unknown => panic!("known deadline missing"),
    }
}

#[test]
fn revoked_grant_records_once_and_does_not_change_a_deadline() {
    let (_dir, service) = harness(90, 1, 3);
    let early = goat_message("weekly", "2026-10-18T00:00:00Z");
    let later = goat_message("weekly", "2026-10-25T00:00:00Z");
    let writer = goat_at("cred-held", "auth-held", "bind-held", 601);
    let mut inflight = writer.clone();
    inflight.attempt_id = Uuid::from_u128(602);
    inflight.request_id = Uuid::from_u128(1602);
    assert_eq!(
        call(&service, &writer, &writer.admit_body()).action,
        Action::Allow
    );
    assert_eq!(
        call(&service, &inflight, &inflight.admit_body()).action,
        Action::Allow
    );
    let trusted = call(&service, &writer, &definite(&writer, &early));
    assert_eq!(
        (trusted.action, trusted.reason),
        (Action::Skip, Reason::ExplicitRejection)
    );
    let held = credential_restrictions(&service, "cred-held");
    assert_eq!(held.len(), 1);
    let held_at = known_at(&held[0]);
    let held_observation = held[0].observation_id.clone();

    inflight.granted = false;
    inflight.observation_seq.set(40);
    let current = inflight.facts().current.expect("current");
    assert!(!current.granted);
    assert_eq!(current.auth_id, inflight.auth_id);
    assert_eq!(current.credential_version, inflight.version);
    assert_eq!(current.binding_id, inflight.binding);
    let bytes = definite(&inflight, &later);
    let (decision, mut observed) = publish(&service, &inflight, &bytes);
    assert_eq!(
        (decision.action, decision.reason),
        (Action::Skip, Reason::ProviderRejected)
    );
    observed.assert_admitted(&inflight);
    let held = credential_restrictions(&service, "cred-held");
    assert_eq!(held.len(), 1);
    assert_eq!(known_at(&held[0]), held_at);
    assert_eq!(held[0].observation_id, held_observation);
    assert!(
        document(&service)
            .attempts
            .iter()
            .all(|row| row.attempt_id != inflight.attempt_id)
    );

    let replay = service
        .handle(TOKEN, &bytes, &mut observed)
        .decision
        .expect("replay");
    assert_eq!(replay.reason, Reason::Uncorrelated);
    assert_eq!(observed.records.get(), 1);
    assert_eq!(credential_restrictions(&service, "cred-held").len(), 1);

    let mut fresh = goat_at("cred-revoked", "auth-revoked", "bind-revoked", 603);
    assert_eq!(
        call(&service, &fresh, &fresh.admit_body()).action,
        Action::Allow
    );
    fresh.granted = false;
    let (created, observed) = publish(&service, &fresh, &definite(&fresh, &later));
    assert_eq!(created.reason, Reason::ProviderRejected);
    observed.assert_admitted(&fresh);
    assert!(credential_restrictions(&service, "cred-revoked").is_empty());
}

#[test]
fn narrowed_model_or_pool_records_once_without_a_new_restriction() {
    let (_dir, service) = harness(90, 1, 3);
    let early = goat_message("weekly", "2026-10-18T00:00:00Z");
    let later = goat_message("weekly", "2026-10-25T00:00:00Z");

    let writer = goat_at("cred-all", "auth-all", "bind-all", 611);
    let mut inflight = writer.clone();
    inflight.attempt_id = Uuid::from_u128(612);
    inflight.request_id = Uuid::from_u128(1612);
    assert_eq!(
        call(&service, &writer, &writer.admit_body()).action,
        Action::Allow
    );
    assert_eq!(
        call(&service, &inflight, &inflight.admit_body()).action,
        Action::Allow
    );
    call(&service, &writer, &definite(&writer, &early));
    let held_at = known_at(&credential_restrictions(&service, "cred-all")[0]);
    let held_observation = credential_restrictions(&service, "cred-all")[0]
        .observation_id
        .clone();
    inflight.credential_model("beta");
    inflight.observation_seq.set(70);
    let (decision, observed) = publish(&service, &inflight, &definite(&inflight, &later));
    assert_eq!(decision.reason, Reason::ProviderRejected);
    observed.assert_admitted(&inflight);
    let held = credential_restrictions(&service, "cred-all");
    assert_eq!(held.len(), 1);
    assert_eq!(held[0].scope.public_model, None);
    assert_eq!(known_at(&held[0]), held_at);
    assert_eq!(held[0].observation_id, held_observation);

    let mut alpha = goat_at("cred-alpha", "auth-alpha", "bind-alpha", 613);
    alpha.credential_model("alpha");
    assert_eq!(
        call(&service, &alpha, &alpha.admit_body()).action,
        Action::Allow
    );
    alpha.credential_model("beta");
    let (decision, observed) = publish(&service, &alpha, &definite(&alpha, &later));
    assert_eq!(decision.reason, Reason::ProviderRejected);
    observed.assert_admitted(&alpha);
    assert!(credential_restrictions(&service, "cred-alpha").is_empty());

    let mut member_gone = goat_at("cred-member", "auth-member", "bind-member", 614);
    member_gone.memberships = vec![member("pool-member", 1, &["alpha"])];
    member_gone.pool_scope("pool-member", 1, "alpha");
    assert_eq!(
        call(&service, &member_gone, &member_gone.admit_body()).action,
        Action::Allow
    );
    member_gone.memberships.clear();
    let (decision, observed) = publish(&service, &member_gone, &definite(&member_gone, &later));
    assert_eq!(decision.reason, Reason::ProviderRejected);
    observed.assert_admitted(&member_gone);
    assert!(pool_restrictions(&service, "pool-member").is_empty());
    assert!(credential_restrictions(&service, "cred-member").is_empty());

    let mut scope_gone = goat_at("cred-declared", "auth-declared", "bind-declared", 615);
    scope_gone.memberships = vec![member("pool-declared", 1, &["alpha"])];
    scope_gone.pool_scope("pool-declared", 1, "alpha");
    assert_eq!(
        call(&service, &scope_gone, &scope_gone.admit_body()).action,
        Action::Allow
    );
    scope_gone.scopes = vec![DeclaredScope {
        subject: DeclaredSubject::Credential,
        public_model: None,
    }];
    let (decision, observed) = publish(&service, &scope_gone, &definite(&scope_gone, &later));
    assert_eq!(decision.reason, Reason::ProviderRejected);
    observed.assert_admitted(&scope_gone);
    assert!(pool_restrictions(&service, "pool-declared").is_empty());

    let mut narrowed = goat_at("cred-narrow", "auth-narrow", "bind-narrow", 616);
    narrowed.memberships = vec![member("pool-narrow", 1, &["alpha", "beta"])];
    narrowed.pool_scope("pool-narrow", 1, "alpha");
    assert_eq!(
        call(&service, &narrowed, &narrowed.admit_body()).action,
        Action::Allow
    );
    narrowed.pool_scope("pool-narrow", 1, "beta");
    let (decision, observed) = publish(&service, &narrowed, &definite(&narrowed, &later));
    assert_eq!(decision.reason, Reason::ProviderRejected);
    observed.assert_admitted(&narrowed);
    assert!(pool_restrictions(&service, "pool-narrow").is_empty());
}

#[test]
fn caller_revocation_and_cancel_still_record_a_trusted_limit() {
    let (_dir, service) = harness(90, 1, 3);
    let body = goat_message("weekly", "2026-10-19T00:00:00Z");

    let revoked = goat_at("cred-caller", "auth-caller", "bind-caller", 621);
    assert_eq!(
        call(&service, &revoked, &revoked.admit_body()).action,
        Action::Allow
    );
    let mut facts = revoked.facts();
    facts.caller_stop = Some(Reason::Unauthorized);
    let mut observed = Observed::new(facts);
    let decision = service
        .handle(TOKEN, &definite(&revoked, &body), &mut observed)
        .decision
        .expect("caller revocation");
    assert!(!decision.unavailable);
    assert_eq!(
        (decision.action, decision.reason),
        (Action::Skip, Reason::ExplicitRejection)
    );
    observed.assert_admitted(&revoked);
    let rows = credential_restrictions(&service, "cred-caller");
    assert_eq!(rows.len(), 1);
    assert!(known_at(&rows[0]) > revoked.now);

    let cancelled = goat_at("cred-cancel", "auth-cancel", "bind-cancel", 622);
    assert_eq!(
        call(&service, &cancelled, &cancelled.admit_body()).action,
        Action::Allow
    );
    let (decision, observed) = publish(
        &service,
        &cancelled,
        &cancelled.result_body(
            "cancelled",
            Some(499),
            true,
            true,
            true,
            "cancelled",
            &body,
            None,
        ),
    );
    assert_eq!(
        (decision.action, decision.reason),
        (Action::Stop, Reason::Cancelled)
    );
    observed.assert_admitted(&cancelled);
    assert_eq!(credential_restrictions(&service, "cred-cancel").len(), 1);
}

#[test]
fn ordinary_refresh_extends_a_deadline_and_replaced_auth_does_not() {
    let (_dir, service) = harness(90, 1, 3);
    let early = goat_message("weekly", "2026-10-18T00:00:00Z");
    let later = goat_message("weekly", "2026-10-25T00:00:00Z");
    let later_at = chrono::DateTime::parse_from_rfc3339("2026-10-25T00:00:00Z")
        .unwrap()
        .with_timezone(&Utc);

    let refreshed = goat_at("cred-refresh", "auth-refresh", "bind-refresh", 631);
    let mut refresh_inflight = refreshed.clone();
    refresh_inflight.attempt_id = Uuid::from_u128(632);
    refresh_inflight.request_id = Uuid::from_u128(1632);
    assert_eq!(
        call(&service, &refreshed, &refreshed.admit_body()).action,
        Action::Allow
    );
    assert_eq!(
        call(&service, &refresh_inflight, &refresh_inflight.admit_body()).action,
        Action::Allow
    );
    let trusted = call(&service, &refreshed, &definite(&refreshed, &early));
    assert_eq!(trusted.reason, Reason::ExplicitRejection);
    let early_at = known_at(&credential_restrictions(&service, "cred-refresh")[0]);

    refresh_inflight.live_material = Some("oauth-refresh".into());
    refresh_inflight.observation_seq.set(90);
    let current = refresh_inflight.facts().current.expect("refresh current");
    assert_ne!(current.material_revision, refresh_inflight.material);
    assert_eq!(current.registration_epoch, refresh_inflight.epoch);
    assert_eq!(current.auth_id, refresh_inflight.auth_id);
    assert_eq!(current.credential_version, refresh_inflight.version);
    assert_eq!(current.binding_id, refresh_inflight.binding);
    assert_eq!(
        known_at(&credential_restrictions(&service, "cred-refresh")[0]),
        early_at
    );
    let (decision, observed) = publish(
        &service,
        &refresh_inflight,
        &definite(&refresh_inflight, &later),
    );
    assert_eq!(decision.reason, Reason::ExplicitRejection);
    observed.assert_admitted(&refresh_inflight);
    let refreshed_row = credential_restrictions(&service, "cred-refresh");
    assert_eq!(refreshed_row.len(), 1);
    assert_eq!(known_at(&refreshed_row[0]), later_at);
    assert_eq!(credential_version(&refreshed_row[0]), Some(3));

    let replaced = goat_at("cred-rotated", "auth-rotated", "bind-rotated", 633);
    let mut replaced_inflight = replaced.clone();
    replaced_inflight.attempt_id = Uuid::from_u128(634);
    replaced_inflight.request_id = Uuid::from_u128(1634);
    assert_eq!(
        call(&service, &replaced, &replaced.admit_body()).action,
        Action::Allow
    );
    assert_eq!(
        call(
            &service,
            &replaced_inflight,
            &replaced_inflight.admit_body()
        )
        .action,
        Action::Allow
    );
    call(&service, &replaced, &definite(&replaced, &early));
    let saved = credential_restrictions(&service, "cred-rotated");
    let saved_at = known_at(&saved[0]);
    let saved_observation = saved[0].observation_id.clone();

    replaced_inflight.live_auth_id = Some("auth-rotated-next".into());
    replaced_inflight.observation_seq.set(110);
    let current = replaced_inflight.facts().current.expect("replaced current");
    assert_eq!(current.material_revision, replaced_inflight.material);
    assert_eq!(current.registration_epoch, replaced_inflight.epoch);
    assert_ne!(current.auth_id, replaced_inflight.auth_id);
    assert_eq!(current.credential_id, replaced_inflight.credential_id);
    assert_eq!(current.credential_version, replaced_inflight.version);
    assert_eq!(current.binding_id, replaced_inflight.binding);
    assert!(current.granted);
    let (decision, observed) = publish(
        &service,
        &replaced_inflight,
        &definite(&replaced_inflight, &later),
    );
    assert_eq!(decision.reason, Reason::ProviderRejected);
    observed.assert_admitted(&replaced_inflight);
    let rows = credential_restrictions(&service, "cred-rotated");
    assert_eq!(rows.len(), 1);
    assert_eq!(known_at(&rows[0]), saved_at);
    assert_eq!(rows[0].observation_id, saved_observation);
    assert!(
        document(&service)
            .attempts
            .iter()
            .all(|row| row.attempt_id != replaced_inflight.attempt_id)
    );

    let replaced_epoch = goat_at("cred-epoch", "auth-epoch", "bind-epoch", 635);
    let mut epoch_inflight = replaced_epoch.clone();
    epoch_inflight.attempt_id = Uuid::from_u128(636);
    epoch_inflight.request_id = Uuid::from_u128(1636);
    assert_eq!(
        call(&service, &replaced_epoch, &replaced_epoch.admit_body()).action,
        Action::Allow
    );
    assert_eq!(
        call(&service, &epoch_inflight, &epoch_inflight.admit_body()).action,
        Action::Allow
    );
    call(
        &service,
        &replaced_epoch,
        &definite(&replaced_epoch, &early),
    );
    let epoch_saved = credential_restrictions(&service, "cred-epoch");
    let epoch_at = known_at(&epoch_saved[0]);
    let epoch_observation = epoch_saved[0].observation_id.clone();

    epoch_inflight.live_epoch = Some(90);
    epoch_inflight.observation_seq.set(120);
    let current = epoch_inflight.facts().current.expect("epoch current");
    assert_eq!(current.material_revision, epoch_inflight.material);
    assert_ne!(current.registration_epoch, epoch_inflight.epoch);
    assert_eq!(current.auth_id, epoch_inflight.auth_id);
    assert_eq!(current.credential_version, epoch_inflight.version);
    assert_eq!(current.binding_id, epoch_inflight.binding);
    assert!(current.granted);
    let (decision, observed) = publish(
        &service,
        &epoch_inflight,
        &definite(&epoch_inflight, &later),
    );
    assert_eq!(decision.reason, Reason::ProviderRejected);
    observed.assert_admitted(&epoch_inflight);
    let epoch_rows = credential_restrictions(&service, "cred-epoch");
    assert_eq!(epoch_rows.len(), 1);
    assert_eq!(known_at(&epoch_rows[0]), epoch_at);
    assert_eq!(epoch_rows[0].observation_id, epoch_observation);
    assert!(
        document(&service)
            .attempts
            .iter()
            .all(|row| row.attempt_id != epoch_inflight.attempt_id)
    );
}
