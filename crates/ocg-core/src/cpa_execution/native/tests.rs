use super::super::store::{OAuthPresence, OAuthStamp, Record};
use super::*;
use crate::cpa::CpaOAuthProvider;
use crate::cpa_projection::{CredentialRouteSet, NormalizedRoute};
use crate::db::Database;
use crate::db::native_binding::{self, BoundNative};
use crate::provider::CPA_PROVIDER_ID;
use ocg_domain::credential::ModelScope;
use rusqlite::params;
use serde_json::json;
use std::path::PathBuf;

struct Opened {
    db: Option<Database>,
    dir: PathBuf,
}

impl Opened {
    fn db(&self) -> &Database {
        self.db.as_ref().expect("database")
    }
}

impl Drop for Opened {
    fn drop(&mut self) {
        self.db.take();
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn open_db(label: &str) -> Opened {
    let dir = std::env::temp_dir().join(format!(
        "ocg-native-exec-{label}-{}",
        uuid::Uuid::new_v4().simple()
    ));
    std::fs::create_dir_all(&dir).expect("test data dir");
    Opened {
        db: Some(Database::open(dir.clone()).expect("database")),
        dir,
    }
}

fn bind_codex(db: &Database) -> BoundNative {
    native_binding::bind_native_account(&db.conn, "codex", "codex.json").expect("bind")
}

fn stamp_for(bound: &BoundNative, material: &str, epoch: u64) -> OAuthStamp {
    OAuthStamp {
        relative_path: "codex.json".into(),
        auth_id: "codex.json".into(),
        credential_id: bound.credential_id.clone(),
        credential_version: bound.credential_version,
        material_revision: material.into(),
        provider_id: CPA_PROVIDER_ID.into(),
        native_provider: "codex".into(),
        registration_epoch: epoch,
        models: vec!["gpt-5".into()],
        presence: OAuthPresence::Present,
        recovery: String::new(),
        raw_provider_label: "codex".into(),
        native_mode: String::new(),
        reported_base: String::new(),
    }
}

fn save_record(db: &Database, record: &Record) {
    super::super::store::save(&db.conn, record).expect("save record");
}

fn load_record(db: &Database) -> Record {
    super::super::store::load(&db.conn)
        .expect("record")
        .expect("saved record")
}

fn discovered(
    bound: &BoundNative,
    material: &str,
    epoch: u64,
    version: u64,
) -> DiscoveredNativeRef {
    DiscoveredNativeRef {
        native_provider: "codex".into(),
        relative_path: "codex.json".into(),
        auth_id: "codex.json".into(),
        credential_id: bound.credential_id.clone(),
        credential_version: version,
        material_revision: material.into(),
        registration_epoch: epoch,
        models: vec!["gpt-5".into()],
        bound: true,
        disabled: Some(false),
        status: Some("active".into()),
        raw_provider_label: "codex".into(),
        native_mode: String::new(),
        reported_base: String::new(),
    }
}

#[test]
fn sealed_routes_keep_a_full_pin_for_each_native_executor() {
    let protocols = ["chat_completions", "responses", "messages"];
    let fingerprint = crate::cpa_projection::endpoint_fingerprint;
    let codex_url = "https://chatgpt.com/backend-api/codex/responses";
    let codex = sealed_routes(CpaOAuthProvider::Codex, "model-a");
    assert_eq!(
        codex
            .iter()
            .map(|route| route.protocol.as_str())
            .collect::<Vec<_>>(),
        protocols
    );
    assert!(codex.iter().all(|route| {
        route.origin == "https://chatgpt.com"
            && !route.origin.contains(":443")
            && route.endpoint_fingerprint == fingerprint(codex_url)
            && route.endpoint_id == codex[0].endpoint_id
            && route.protocol != "generate_content"
            && route.native_targets.len() == 2
    }));
    assert_eq!(
        sealed_route(CpaOAuthProvider::Codex, "model-a")
            .expect("preferred hop")
            .protocol,
        "chat_completions"
    );
    assert!(sealed_routes(CpaOAuthProvider::Codex, "  ").is_empty());

    let anthropic_url = "https://api.anthropic.com/v1/messages?beta=true";
    let count_url = "https://api.anthropic.com/v1/messages/count_tokens?beta=true";
    let anthropic = sealed_routes(CpaOAuthProvider::Anthropic, "model-a");
    assert_eq!(anthropic.len(), 3);
    assert!(anthropic.iter().all(|route| {
        route.origin == "https://api.anthropic.com"
            && route.endpoint_fingerprint == fingerprint(anthropic_url)
            && route.native_targets.iter().any(|target| {
                target.pin.endpoint_fingerprint == fingerprint(count_url)
                    && target.pin.endpoint_id != route.endpoint_id
            })
    }));

    assert!(sealed_routes(CpaOAuthProvider::Kimi, "model-a").is_empty());
    assert!(sealed_routes(CpaOAuthProvider::Xai, "model-a").is_empty());

    let daily = "https://daily-cloudcode-pa.googleapis.com/v1internal:generateContent";
    let ordinary = sealed_routes(CpaOAuthProvider::Antigravity, "model-a");
    assert_eq!(
        ordinary
            .iter()
            .map(|route| route.protocol.as_str())
            .collect::<Vec<_>>(),
        protocols
    );
    let production = fingerprint("https://cloudcode-pa.googleapis.com/v1internal:generateContent");
    assert!(ordinary.iter().all(|route| {
        route.origin == "https://daily-cloudcode-pa.googleapis.com"
            && route.endpoint_fingerprint == fingerprint(daily)
            && route.endpoint_fingerprint != production
            && route.protocol != "generate_content"
            && route.native_targets.len() == 3
    }));
    let ids = ordinary[0]
        .native_targets
        .iter()
        .map(|target| target.pin.endpoint_id.as_str())
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(ids.len(), 3);
    assert_eq!(
        sealed_route(CpaOAuthProvider::Antigravity, "model-a")
            .expect("preferred hop")
            .protocol,
        "chat_completions"
    );
    let stream_url =
        "https://daily-cloudcode-pa.googleapis.com/v1internal:streamGenerateContent?alt=sse";
    let streamed = sealed_routes(CpaOAuthProvider::Antigravity, "claude-sonnet");
    assert!(streamed.iter().all(|route| {
        route.endpoint_fingerprint == fingerprint(stream_url) && route.native_targets.len() == 2
    }));
}

#[test]
fn authorize_requires_the_applied_pin_scope_and_grants() {
    let opened = open_db("authorize");
    let db = opened.db();
    let bound = bind_codex(db);
    let sealed = sealed_route(CpaOAuthProvider::Codex, "gpt-5").expect("codex pin");
    assert!(
        native_binding::write_initial_grants(
            &db.conn,
            &bound.account_id,
            &[sealed.endpoint_id.clone()],
            &[sealed.origin.clone()],
        )
        .expect("grants")
    );
    let scope = serde_json::to_string(&ModelScope::All).unwrap();
    db.conn
        .execute(
            "UPDATE credentials SET scope_json = ?2 WHERE id = ?1",
            params![bound.credential_id, scope],
        )
        .unwrap();
    let row = native_binding::load_owned_credential(&db.conn, &bound.credential_id)
        .unwrap()
        .unwrap();
    let route = NormalizedRoute {
        public_model: "gpt-5".into(),
        upstream_model: "gpt-5".into(),
        protocol: sealed.protocol.clone(),
        endpoint_id: sealed.endpoint_id.clone(),
        origin: sealed.origin.clone(),
        endpoint_fingerprint: sealed.endpoint_fingerprint.clone(),
        validation_only: false,
        native_targets: Vec::new(),
    };
    let set = CredentialRouteSet {
        auth_id: "codex.json".into(),
        credential_id: bound.credential_id.clone(),
        credential_version: bound.credential_version,
        binding_id: row.binding_id.clone(),
        material_fingerprint: "material-1".into(),
        routing_rank: row.routing_rank,
        routes: vec![route],
        fingerprint: "route-set".into(),
    };
    let mut record = Record::empty();
    record.child_generation = 4;
    record.oauth = vec![stamp_for(&bound, "material-1", 1)];
    record.applied_routes = vec![set];
    save_record(db, &record);
    let query = SelectedRouteQuery {
        credential_id: bound.credential_id.clone(),
        credential_version: bound.credential_version,
        public_model: "gpt-5".into(),
        protocol: sealed.protocol.clone(),
        admission: AdmissionUse::Client,
    };
    let tx = db.conn.unchecked_transaction().unwrap();
    let allowed =
        authorize_selected_route(&tx, &record, RoutePlane::Applied, &query).expect("client route");
    assert_eq!(allowed.endpoint_id, sealed.endpoint_id);
    assert_eq!(allowed.origin, "https://chatgpt.com");
    assert!(!allowed.validation_only);
    let validated = SelectedRouteQuery {
        credential_id: query.credential_id.clone(),
        credential_version: query.credential_version,
        public_model: query.public_model.clone(),
        protocol: query.protocol.clone(),
        admission: AdmissionUse::Validated,
    };
    let pinned = authorize_selected_route(&tx, &record, RoutePlane::Desired, &validated)
        .expect_err("desired plane is not a validated pin");
    assert!(
        pinned
            .to_string()
            .contains("validated protocol pin is not enforced")
    );
    drop(tx);

    let mut validation_only = record.applied_routes[0].routes[0].clone();
    validation_only.validation_only = true;
    record.applied_routes[0].routes = vec![validation_only];
    let tx = db.conn.unchecked_transaction().unwrap();
    let client_only = authorize_selected_route(&tx, &record, RoutePlane::Applied, &query)
        .expect_err("validation route");
    assert!(client_only.to_string().contains("validation only"));
    drop(tx);

    record.applied_routes[0].routes[0].validation_only = false;
    record.applied_routes[0].routes[0].endpoint_id.clear();
    let tx = db.conn.unchecked_transaction().unwrap();
    let empty_pin =
        authorize_selected_route(&tx, &record, RoutePlane::Applied, &query).expect_err("empty pin");
    assert!(
        empty_pin
            .to_string()
            .contains("selected native route is empty")
    );
    drop(tx);

    let empty_scope = serde_json::to_string(&ModelScope::Only { models: Vec::new() }).unwrap();
    db.conn
        .execute(
            "UPDATE credentials SET scope_json = ?2 WHERE id = ?1",
            params![bound.credential_id, empty_scope],
        )
        .unwrap();
    record.applied_routes[0].routes[0].endpoint_id = sealed.endpoint_id.clone();
    let tx = db.conn.unchecked_transaction().unwrap();
    let scoped = authorize_selected_route(&tx, &record, RoutePlane::Applied, &query)
        .expect_err("empty scope");
    assert!(
        scoped
            .to_string()
            .contains("selected native scope is empty")
    );
    drop(tx);

    db.conn
        .execute(
            "UPDATE credentials SET scope_json = ?2, enabled = 0 WHERE id = ?1",
            params![bound.credential_id, scope],
        )
        .unwrap();
    let tx = db.conn.unchecked_transaction().unwrap();
    let disabled =
        authorize_selected_route(&tx, &record, RoutePlane::Applied, &query).expect_err("disabled");
    assert!(
        disabled
            .to_string()
            .contains("selected native credential is not current")
    );
    drop(tx);

    db.conn
        .execute(
            "UPDATE credentials SET enabled = 1 WHERE id = ?1",
            [&bound.credential_id],
        )
        .unwrap();
    record.oauth[0].presence = OAuthPresence::Absent;
    let tx = db.conn.unchecked_transaction().unwrap();
    let absent = authorize_selected_route(&tx, &record, RoutePlane::Applied, &query)
        .expect_err("absent stamp");
    assert!(
        absent
            .to_string()
            .contains("selected native reference is absent")
    );
}

#[test]
fn malformed_discovery_does_not_clear_and_a_late_snapshot_does_not_revive() {
    let opened = open_db("fence-snapshot");
    let db = opened.db();
    let bound = bind_codex(db);
    let mut record = Record::empty();
    record.child_generation = 3;
    record.oauth = vec![stamp_for(&bound, "material-old", 1)];
    save_record(db, &record);
    let lease = capture_native_lease(&db.conn, 3, 11).unwrap();
    let malformed = discovery_from_ready_body(
        r#"{"authRefs":[{"relativePath":"../codex.json","provider":"codex"}]}"#,
    );
    assert!(matches!(malformed, DiscoverySnapshot::Malformed));
    let report = reconcile_owned_discovery(&db.conn, &malformed, &lease).unwrap();
    assert!(!report.changed);
    let kept = load_record(db);
    assert_eq!(kept.oauth[0].presence, OAuthPresence::Present);
    assert_eq!(kept.oauth[0].material_revision, "material-old");

    let incomplete = discovery_from_ready_body(r#"{"status":"ok"}"#);
    assert!(matches!(incomplete, DiscoverySnapshot::Incomplete));
    reconcile_owned_discovery(&db.conn, &incomplete, &lease).unwrap();
    assert_eq!(load_record(db).oauth[0].presence, OAuthPresence::Present);

    let fenced = fence_mapped_target(&db.conn, "codex.json", "").unwrap();
    assert!(fenced.credential_version > bound.credential_version);
    let after_fence = load_record(db);
    assert_eq!(after_fence.oauth[0].presence, OAuthPresence::Absent);
    let late = DiscoverySnapshot::Complete(vec![discovered(
        &bound,
        "material-new",
        1,
        bound.credential_version,
    )]);
    let current = capture_native_lease(&db.conn, 3, 11).unwrap();
    let revived = reconcile_owned_discovery(&db.conn, &late, &current).unwrap();
    assert!(!revived.changed);
    let still = load_record(db);
    assert_eq!(still.oauth[0].presence, OAuthPresence::Absent);
    assert_eq!(still.oauth[0].material_revision, "material-old");
    assert_ne!(still.oauth[0].credential_version, bound.credential_version);
}

#[test]
fn epoch_change_does_not_copy_material_and_empty_refs_fence_omissions() {
    let opened = open_db("epoch");
    let db = opened.db();
    let bound = bind_codex(db);
    let mut record = Record::empty();
    record.child_generation = 2;
    record.oauth = vec![stamp_for(&bound, "material-old", 4)];
    save_record(db, &record);
    let lease = capture_native_lease(&db.conn, 2, 1).unwrap();
    let changed = DiscoverySnapshot::Complete(vec![discovered(
        &bound,
        "material-new",
        9,
        bound.credential_version,
    )]);
    let report = reconcile_owned_discovery(&db.conn, &changed, &lease).unwrap();
    assert!(
        report
            .epoch_refused
            .iter()
            .any(|id| id == &bound.credential_id)
    );
    let pending = load_record(db);
    assert_eq!(pending.oauth[0].presence, OAuthPresence::Pending);
    assert_eq!(pending.oauth[0].material_revision, "material-old");
    assert_eq!(pending.oauth[0].registration_epoch, 4);
    assert!(pending.oauth[0].credential_version > bound.credential_version);
    let fenced_version = pending.oauth[0].credential_version;
    let stale = reconcile_owned_discovery(&db.conn, &changed, &lease);
    assert!(matches!(
        stale,
        Err(crate::cpa_execution::ExecutionError::ApplyConflict(_))
    ));
    let late = DiscoverySnapshot::Complete(vec![discovered(
        &bound,
        "material-late",
        9,
        bound.credential_version,
    )]);
    let late_lease = capture_native_lease(&db.conn, 2, 1).unwrap();
    let late_report = reconcile_owned_discovery(&db.conn, &late, &late_lease).unwrap();
    assert!(!late_report.changed);
    let still = load_record(db);
    assert_eq!(still.oauth[0].presence, OAuthPresence::Pending);
    assert_eq!(still.oauth[0].material_revision, "material-old");
    assert_eq!(still.oauth[0].registration_epoch, 4);
    assert_eq!(still.oauth[0].credential_version, fenced_version);
    let fresh = DiscoverySnapshot::Complete(vec![discovered(
        &bound,
        "material-fresh",
        9,
        fenced_version,
    )]);
    let fresh_lease = capture_native_lease(&db.conn, 2, 1).unwrap();
    let recovered = reconcile_owned_discovery(&db.conn, &fresh, &fresh_lease).unwrap();
    assert!(recovered.changed);
    let ready = load_record(db);
    assert_eq!(ready.oauth[0].presence, OAuthPresence::Present);
    assert_eq!(ready.oauth[0].material_revision, "material-fresh");
    assert_eq!(ready.oauth[0].registration_epoch, 9);
    assert_eq!(ready.oauth[0].credential_version, fenced_version);

    let mut present = pending;
    present.oauth[0].presence = OAuthPresence::Present;
    save_record(db, &present);
    let lease = capture_native_lease(&db.conn, 2, 1).unwrap();
    let empty = discovery_from_ready_value(&json!({"authRefs": []}));
    assert!(matches!(empty, DiscoverySnapshot::Complete(ref refs) if refs.is_empty()));
    let fenced = reconcile_owned_discovery(&db.conn, &empty, &lease).unwrap();
    assert!(fenced.fenced.iter().any(|id| id == &bound.credential_id));
    let omitted = load_record(db);
    assert_eq!(omitted.oauth[0].presence, OAuthPresence::Absent);
    assert!(omitted.oauth[0].credential_version > bound.credential_version);
}

#[test]
fn ordered_discovery_keeps_needs_apply_after_a_new_inactive_bind() {
    let opened = open_db("apply-aggregate");
    let db = opened.db();
    let bound = bind_codex(db);
    let mut record = Record::empty();
    record.child_generation = 2;
    record.oauth = vec![stamp_for(&bound, "material-old", 4)];
    save_record(db, &record);
    let account_id = native_binding::account_id_for("anthropic", "claude.json").unwrap();
    let credential_id =
        ocg_domain::credential::credential_id_for_legacy_account(&account_id).to_string();
    let snapshot = DiscoverySnapshot::Complete(vec![
        discovered(&bound, "material-old", 9, bound.credential_version),
        DiscoveredNativeRef {
            native_provider: "anthropic".into(),
            relative_path: "claude.json".into(),
            auth_id: "claude.json".into(),
            credential_id,
            credential_version: 1,
            material_revision: "material".into(),
            registration_epoch: 1,
            models: Vec::new(),
            bound: true,
            disabled: Some(true),
            status: Some("active".into()),
            raw_provider_label: "claude".into(),
            native_mode: String::new(),
            reported_base: String::new(),
        },
    ]);
    let lease = capture_native_lease(&db.conn, 2, 1).unwrap();
    let report = reconcile_owned_discovery(&db.conn, &snapshot, &lease).unwrap();
    assert!(report.needs_apply);
    assert!(report.changed);
    assert!(
        report
            .epoch_refused
            .iter()
            .any(|id| id == &bound.credential_id)
    );
    let stored = load_record(db);
    assert_eq!(stored.oauth.len(), 2);
    assert_eq!(stored.oauth[0].native_provider, "codex");
    assert_eq!(stored.oauth[0].presence, OAuthPresence::Pending);
    assert_eq!(stored.oauth[0].material_revision, "material-old");
    assert_eq!(stored.oauth[0].registration_epoch, 4);
    assert!(stored.oauth[0].credential_version > bound.credential_version);
    assert_eq!(stored.oauth[1].native_provider, "anthropic");
    assert_eq!(stored.oauth[1].relative_path, "claude.json");
    assert_eq!(stored.oauth[1].presence, OAuthPresence::Pending);
    let enabled: i64 = db
        .conn
        .query_row(
            "SELECT enabled FROM credentials WHERE id = ?1",
            [&stored.oauth[1].credential_id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(enabled, 0);
}

#[test]
fn one_invalid_ref_is_malformed_and_an_unbound_ref_stays_pending() {
    let bad = discovery_from_ready_value(&json!({
        "authRefs": [
            {"relativePath": "codex.json", "provider": "codex", "credentialId": "cid", "credentialVersion": "1", "materialRevision": "m"},
            {"relativePath": "a/b.json", "provider": "kimi"}
        ]
    }));
    assert!(matches!(bad, DiscoverySnapshot::Malformed));
    let unbound = discovery_from_ready_value(&json!({
        "authRefs": [{
            "relativePath": "codex.json",
            "provider": "cpa",
            "providerId": "cpa",
            "rawProviderLabel": "codex",
            "effectiveSubtype": "codex",
            "effectiveMode": "",
            "effectiveGenerationBase": "",
            "models": []
        }]
    }));
    match unbound {
        DiscoverySnapshot::Complete(refs) => {
            assert_eq!(refs.len(), 1);
            assert!(!refs[0].bound);
            let stamp = stamp_from_discovered(&refs[0]);
            assert_eq!(stamp.presence, OAuthPresence::Pending);
            assert_eq!(stamp.provider_id, CPA_PROVIDER_ID);
            assert_eq!(stamp.native_provider, "codex");
            assert_eq!(stamp.raw_provider_label, "codex");
            assert!(stamp.native_mode.is_empty());
            assert_eq!(stamp.reported_base, "https://chatgpt.com/backend-api/codex");
            assert!(stamp.credential_id.is_empty());
        }
        other => panic!("unbound ref must stay in the snapshot: {other:?}"),
    }
}

#[test]
fn callback_refresh_does_not_replace_the_stored_stamp() {
    let opened = open_db("callback-refresh");
    let db = opened.db();
    let bound = bind_codex(db);
    let mut record = Record::empty();
    record.oauth.push(stamp_for(&bound, "material-old", 4));
    record.oauth.push(OAuthStamp {
        native_provider: "kimi".into(),
        relative_path: "kimi.json".into(),
        auth_id: "kimi.json".into(),
        ..record.oauth[0].clone()
    });
    save_record(db, &record);
    let captured = super::super::identity::CapturedAttempt {
        request_id: uuid::Uuid::nil(),
        attempt_id: uuid::Uuid::nil(),
        auth_id: "codex.json".into(),
        credential_id: bound.credential_id.clone(),
        credential_version: bound.credential_version,
        provider_id: CPA_PROVIDER_ID.into(),
        public_model: "gpt-5".into(),
        upstream_model: "gpt-5".into(),
        registration_epoch: 9,
        material_revision: "material-new".into(),
        kind: crate::cpa_policy::SendKind::Accepted,
        callable_protocol: String::new(),
        generation_kind: String::new(),
    };
    let tx = db.conn.unchecked_transaction().expect("transaction");
    let mut live = record.clone();
    advance_refresh_on_tx(&tx, &mut live, &captured).expect("stored stamp stays");
    assert_eq!(live.oauth, record.oauth);
    assert_eq!(live.oauth[0].presence, OAuthPresence::Present);
    assert_eq!(live.oauth[0].material_revision, "material-old");
    assert_eq!(live.oauth[0].registration_epoch, 4);
    drop(tx);
    assert_eq!(load_record(db).oauth, record.oauth);
}

#[test]
fn same_epoch_refresh_writes_material_only_when_the_capability_is_verified() {
    let opened = open_db("refresh-fence");
    let db = opened.db();
    let bound = bind_codex(db);
    let sealed = sealed_route(CpaOAuthProvider::Codex, "gpt-5").expect("codex pin");
    assert!(
        native_binding::write_initial_grants(
            &db.conn,
            &bound.account_id,
            &[sealed.endpoint_id.clone()],
            &[sealed.origin.clone()],
        )
        .expect("grants")
    );
    let scope = serde_json::to_string(&ModelScope::All).unwrap();
    db.conn
        .execute(
            "UPDATE credentials SET scope_json = ?2 WHERE id = ?1",
            params![bound.credential_id, scope],
        )
        .unwrap();
    let row = native_binding::load_owned_credential(&db.conn, &bound.credential_id)
        .unwrap()
        .unwrap();
    let route = NormalizedRoute {
        public_model: "gpt-5".into(),
        upstream_model: "gpt-5".into(),
        protocol: sealed.protocol,
        endpoint_id: sealed.endpoint_id,
        origin: sealed.origin,
        endpoint_fingerprint: sealed.endpoint_fingerprint,
        validation_only: false,
        native_targets: Vec::new(),
    };
    let set = CredentialRouteSet {
        auth_id: "codex.json".into(),
        credential_id: bound.credential_id.clone(),
        credential_version: bound.credential_version,
        binding_id: row.binding_id.clone(),
        material_fingerprint: "material-old".into(),
        routing_rank: row.routing_rank,
        routes: vec![route],
        fingerprint: "route-set".into(),
    };
    let auth = super::super::store::AuthStamp {
        auth_id: "codex.json".into(),
        credential_id: bound.credential_id.clone(),
        credential_version: bound.credential_version,
        binding_id: row.binding_id,
        material_revision: "material-old".into(),
        provider_id: CPA_PROVIDER_ID.into(),
        registration_epoch: 4,
    };
    let mut record = Record::empty();
    record.child_generation = 6;
    record.applied_generation = 6;
    record.oauth = vec![stamp_for(&bound, "material-old", 4)];
    record.applied_auth = vec![auth.clone()];
    record.desired_auth = vec![auth];
    record.applied_routes = vec![set.clone()];
    record.desired_routes = vec![set];
    save_record(db, &record);
    let changed = super::super::identity::CapturedAttempt {
        request_id: uuid::Uuid::nil(),
        attempt_id: uuid::Uuid::nil(),
        auth_id: "codex.json".into(),
        credential_id: bound.credential_id.clone(),
        credential_version: bound.credential_version,
        provider_id: CPA_PROVIDER_ID.into(),
        public_model: "gpt-5".into(),
        upstream_model: "gpt-5".into(),
        registration_epoch: 4,
        material_revision: "material-new".into(),
        kind: crate::cpa_policy::SendKind::Accepted,
        callable_protocol: String::new(),
        generation_kind: String::new(),
    };
    let tx = db.conn.unchecked_transaction().unwrap();
    let mut live = record.clone();
    advance_refresh_on_tx(&tx, &mut live, &changed).unwrap();
    assert_eq!(live.oauth[0].material_revision, "material-old");
    drop(tx);

    record.host_capabilities = vec!["native-refresh-registration-fence-v1".into()];
    let tx = db.conn.unchecked_transaction().unwrap();
    let mut live = record.clone();
    advance_refresh_on_tx(&tx, &mut live, &changed).unwrap();
    assert_eq!(live.oauth[0].material_revision, "material-new");
    assert_eq!(live.oauth[0].registration_epoch, 4);
    assert_eq!(live.oauth[0].credential_version, bound.credential_version);
    assert_eq!(live.oauth[0].presence, OAuthPresence::Present);
    assert_eq!(live.applied_auth[0].material_revision, "material-new");
    assert_ne!(live.applied_routes[0].fingerprint, "route-set");
    assert_eq!(live.applied_routes[0].material_fingerprint, "material-new");
    super::super::store::save(&tx, &live).unwrap();
    tx.commit().unwrap();
    let saved = load_record(db);
    assert_eq!(saved.oauth[0].material_revision, "material-new");
    assert_eq!(saved.oauth[0].registration_epoch, 4);

    let mut pending = saved;
    pending.oauth[0].material_revision = "material-new".into();
    pending.host_capabilities.clear();
    let epoch_attempt = super::super::identity::CapturedAttempt {
        registration_epoch: 9,
        material_revision: "material-epoch".into(),
        ..changed
    };
    let tx = db.conn.unchecked_transaction().unwrap();
    let mut live = pending.clone();
    advance_refresh_on_tx(&tx, &mut live, &epoch_attempt).unwrap();
    assert_eq!(live.oauth[0].presence, OAuthPresence::Pending);
    assert_eq!(live.oauth[0].material_revision, "material-new");
    assert_eq!(live.oauth[0].registration_epoch, 4);
    drop(tx);
    assert_eq!(load_record(db).oauth[0].presence, OAuthPresence::Present);
}

#[test]
fn auth_ref_disabled_and_status_gate_presence() {
    let inactive = discovery_from_ready_value(&json!({
        "authRefs": [{
            "relativePath": "codex.json",
            "provider": "cpa",
            "providerId": "cpa",
            "rawProviderLabel": "codex",
            "effectiveSubtype": "codex",
            "effectiveMode": "",
            "effectiveGenerationBase": "https://chatgpt.com/backend-api/codex",
            "credentialId": "cid",
            "credentialVersion": "2",
            "materialRevision": "m",
            "registrationEpoch": "1"
        }]
    }));
    match inactive {
        DiscoverySnapshot::Complete(refs) => {
            assert!(refs[0].bound);
            assert_eq!(refs[0].native_provider, "codex");
            assert_eq!(refs[0].disabled, None);
            assert_eq!(refs[0].status, None);
            assert_eq!(
                stamp_from_discovered(&refs[0]).presence,
                OAuthPresence::Pending
            );
        }
        other => panic!("missing authority fields stay observable: {other:?}"),
    }
    let disabled = discovery_from_ready_value(&json!({
        "authRefs": [{
            "relativePath": "codex.json",
            "provider": "cpa",
            "providerId": "cpa",
            "rawProviderLabel": "codex",
            "effectiveSubtype": "codex",
            "effectiveMode": "",
            "effectiveGenerationBase": "https://chatgpt.com/backend-api/codex",
            "credentialId": "cid",
            "credentialVersion": "2",
            "materialRevision": "m",
            "disabled": true,
            "status": "active"
        }]
    }));
    match disabled {
        DiscoverySnapshot::Complete(refs) => {
            assert_eq!(
                stamp_from_discovered(&refs[0]).presence,
                OAuthPresence::Pending
            );
        }
        other => panic!("disabled ref is not authority: {other:?}"),
    }
    let active = discovery_from_ready_value(&json!({
        "authRefs": [{
            "relativePath": "codex.json",
            "provider": "cpa",
            "providerId": "cpa",
            "rawProviderLabel": "codex",
            "effectiveSubtype": "codex",
            "effectiveMode": "",
            "effectiveGenerationBase": "https://chatgpt.com/backend-api/codex",
            "credentialId": "cid",
            "credentialVersion": "2",
            "materialRevision": "m",
            "disabled": false,
            "status": "active"
        }]
    }));
    match active {
        DiscoverySnapshot::Complete(refs) => {
            assert_eq!(refs[0].native_provider, "codex");
            assert_eq!(
                stamp_from_discovered(&refs[0]).presence,
                OAuthPresence::Present
            );
        }
        other => panic!("explicit active ref is present: {other:?}"),
    }
    assert!(matches!(
        discovery_from_ready_value(&json!({
            "authRefs": [{"relativePath": "codex.json", "provider": "codex", "disabled": "no"}]
        })),
        DiscoverySnapshot::Malformed
    ));
    assert!(matches!(
        discovery_from_ready_value(&json!({
            "authRefs": [{"relativePath": "codex.json", "provider": "codex", "status": 1}]
        })),
        DiscoverySnapshot::Malformed
    ));
    match discovery_from_ready_value(&json!({
        "authRefs": [{"relativePath": "codex.json", "provider": "codex", "status": "  "}]
    })) {
        DiscoverySnapshot::Complete(refs) => assert_eq!(refs[0].status, None),
        other => panic!("blank status is not malformed: {other:?}"),
    }
}

#[test]
fn owned_enabled_preference_survives_discovery() {
    let opened = open_db("enabled-pref");
    let db = opened.db();
    let bound = bind_codex(db);
    let mut record = Record::empty();
    record.child_generation = 2;
    record.oauth = vec![stamp_for(&bound, "material-old", 4)];
    save_record(db, &record);
    db.conn
        .execute(
            "UPDATE credentials SET enabled = 0 WHERE id = ?1",
            [&bound.credential_id],
        )
        .unwrap();
    let lease = capture_native_lease(&db.conn, 2, 1).unwrap();
    let active = DiscoverySnapshot::Complete(vec![discovered(
        &bound,
        "material-new",
        4,
        bound.credential_version,
    )]);
    reconcile_owned_discovery(&db.conn, &active, &lease).unwrap();
    let pending = load_record(db);
    assert_eq!(pending.oauth[0].presence, OAuthPresence::Pending);
    assert_eq!(pending.oauth[0].material_revision, "material-old");
    let enabled: i64 = db
        .conn
        .query_row(
            "SELECT enabled FROM credentials WHERE id = ?1",
            [&bound.credential_id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(enabled, 0);
    assert!(sync_owned_enabled(&db.conn, "codex.json", "", true).unwrap());
    let enabled: i64 = db
        .conn
        .query_row(
            "SELECT enabled FROM credentials WHERE id = ?1",
            [&bound.credential_id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(enabled, 1);
    assert_eq!(load_record(db).oauth[0].presence, OAuthPresence::Pending);
    let lease = capture_native_lease(&db.conn, 2, 1).unwrap();
    reconcile_owned_discovery(&db.conn, &active, &lease).unwrap();
    let present = load_record(db);
    assert_eq!(present.oauth[0].presence, OAuthPresence::Present);
    assert_eq!(present.oauth[0].material_revision, "material-new");
    assert!(sync_owned_enabled(&db.conn, "codex.json", "", false).unwrap());
    let lease = capture_native_lease(&db.conn, 2, 1).unwrap();
    let again = reconcile_owned_discovery(&db.conn, &active, &lease).unwrap();
    assert!(!again.changed);
    assert_eq!(load_record(db).oauth[0].presence, OAuthPresence::Pending);
    let enabled: i64 = db
        .conn
        .query_row(
            "SELECT enabled FROM credentials WHERE id = ?1",
            [&bound.credential_id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(enabled, 0);

    let fresh = open_db("enabled-first");
    let created = DiscoveredNativeRef {
        native_provider: "codex".into(),
        relative_path: "new.json".into(),
        auth_id: String::new(),
        credential_id: String::new(),
        credential_version: 1,
        material_revision: "material".into(),
        registration_epoch: 1,
        models: vec!["gpt-5".into()],
        bound: true,
        disabled: None,
        status: None,
        raw_provider_label: "codex".into(),
        native_mode: String::new(),
        reported_base: String::new(),
    };
    let lease = capture_native_lease(&fresh.db().conn, 0, 1).unwrap();
    reconcile_owned_discovery(
        &fresh.db().conn,
        &DiscoverySnapshot::Complete(vec![created]),
        &lease,
    )
    .unwrap();
    let stored = load_record(fresh.db());
    assert_eq!(stored.oauth[0].presence, OAuthPresence::Pending);
    let enabled: i64 = fresh
        .db()
        .conn
        .query_row(
            "SELECT enabled FROM credentials WHERE id = ?1",
            [&stored.oauth[0].credential_id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(enabled, 0);
}

#[test]
fn claude_label_keeps_the_raw_fact_and_uses_the_anthropic_provider() {
    let claude = discovery_from_ready_value(&json!({
        "authRefs": [{
            "relativePath": "claude.json",
            "provider": "cpa",
            "providerId": "cpa",
            "rawProviderLabel": "claude",
            "effectiveSubtype": "anthropic",
            "effectiveMode": "claude",
            "effectiveGenerationBase": "",
            "credentialId": "cid",
            "credentialVersion": "1",
            "materialRevision": "m"
        }]
    }));
    match claude {
        DiscoverySnapshot::Complete(refs) => {
            assert_eq!(refs[0].native_provider, "anthropic");
            assert_eq!(refs[0].raw_provider_label, "claude");
            assert!(refs[0].native_mode.is_empty());
            assert_eq!(refs[0].reported_base, "https://api.anthropic.com");
            assert!(refs[0].bound);
            let stamp = stamp_from_discovered(&refs[0]);
            assert_eq!(stamp.provider_id, CPA_PROVIDER_ID);
            assert_eq!(stamp.native_provider, "anthropic");
            assert_eq!(stamp.raw_provider_label, "claude");
            assert_eq!(stamp.reported_base, "https://api.anthropic.com");
        }
        other => panic!("claude label is a complete ref: {other:?}"),
    }
    let alias = discovery_from_ready_value(&json!({
        "authRefs": [{
            "relativePath": "claude.json",
            "provider": "codex",
            "type": "Claude",
            "baseUrl": "https://api.anthropic.com"
        }]
    }));
    match alias {
        DiscoverySnapshot::Complete(refs) => {
            assert!(refs[0].native_provider.is_empty());
            assert!(refs[0].raw_provider_label.is_empty());
            assert!(!refs[0].bound);
        }
        other => panic!("old aliases do not grant: {other:?}"),
    }
    let conflict = discovery_from_ready_value(&json!({
        "authRefs": [{
            "relativePath": "mixed.json",
            "provider": "cpa",
            "rawProviderLabel": "codex",
            "effectiveSubtype": "anthropic",
            "effectiveMode": "",
            "effectiveGenerationBase": ""
        }]
    }));
    match conflict {
        DiscoverySnapshot::Complete(refs) => {
            assert!(refs[0].native_provider.is_empty());
            assert_eq!(refs[0].raw_provider_label, "codex");
            assert!(!refs[0].bound);
        }
        other => panic!("conflicting labels stay in the snapshot: {other:?}"),
    }
    let kimi_conflict = discovery_from_ready_value(&json!({
        "authRefs": [{
            "relativePath": "kimi.json",
            "rawProviderLabel": "kimi.com",
            "effectiveSubtype": "kimi.ai",
            "effectiveMode": "ai",
            "effectiveGenerationBase": "https://api.kimi.ai/coding"
        }]
    }));
    match kimi_conflict {
        DiscoverySnapshot::Complete(refs) => {
            assert!(refs[0].native_provider.is_empty());
            assert_eq!(refs[0].raw_provider_label, "kimi.com");
            assert!(!refs[0].bound);
        }
        other => panic!("kimi host conflict stays unavailable: {other:?}"),
    }
    let malformed = discovery_from_ready_value(&json!({
        "authRefs": [{"relativePath": "codex.json", "effectiveSubtype": 1}]
    }));
    assert!(matches!(malformed, DiscoverySnapshot::Malformed));
    let malformed_base = discovery_from_ready_value(&json!({
        "authRefs": [{"relativePath": "codex.json", "effectiveGenerationBase": 1}]
    }));
    assert!(matches!(malformed_base, DiscoverySnapshot::Malformed));
    let xai_cli = discovery_from_ready_value(&json!({
        "authRefs": [{
            "relativePath": "xai.json",
            "provider": "cpa",
            "providerId": "cpa",
            "rawProviderLabel": "xai",
            "effectiveSubtype": "xai",
            "effectiveMode": "cli",
            "effectiveAuthKind": "oauth",
            "effectiveGenerationBase": "https://api.x.ai/v1"
        }]
    }));
    match xai_cli {
        DiscoverySnapshot::Complete(refs) => {
            assert_eq!(refs[0].native_provider, "xai");
            assert_eq!(refs[0].native_mode, "cli");
            assert_eq!(refs[0].reported_base, "https://cli-chat-proxy.grok.com/v1");
            let facts = crate::cpa_projection::NativeAuthorityFacts {
                raw_label: refs[0].raw_provider_label.clone(),
                provider: refs[0].native_provider.clone(),
                mode: refs[0].native_mode.clone(),
                reported_base: refs[0].reported_base.clone(),
            };
            let connection = ocg_domain::connection::connection_id_for_legacy(
                ocg_domain::connection::LegacyConnectionKind::BuiltinProvider,
                crate::db::native_binding::OWNED_NATIVE_LEGACY_ID,
            );
            let outcome = crate::cpa_projection::native_targets_for(
                &facts,
                "grok-3",
                "responses",
                crate::cpa_projection::NativeSourceOperation::Execute,
                &connection,
            );
            let crate::cpa_projection::NativeTargetOutcome::Network(targets) = outcome else {
                panic!("declared xai cli grants X1");
            };
            assert_eq!(
                targets[0].pin.endpoint_fingerprint,
                crate::cpa_projection::endpoint_fingerprint(
                    "https://cli-chat-proxy.grok.com/v1/responses"
                )
            );
            let (ids, origins) =
                crate::cpa_projection::default_grant_ids(&facts, &["grok-3".into()], &connection)
                    .expect("xai cli grants");
            assert!(ids.contains(&targets[0].pin.endpoint_id));
            assert!(origins.contains(&"https://cli-chat-proxy.grok.com".to_string()));
            assert!(origins.contains(&"https://api.x.ai".to_string()));
        }
        other => panic!("xai cli fact: {other:?}"),
    }
    let xai_alias = discovery_from_ready_value(&json!({
        "authRefs": [{
            "relativePath": "xai.json",
            "provider": "xai",
            "authKind": "oauth",
            "usingApi": false
        }]
    }));
    match xai_alias {
        DiscoverySnapshot::Complete(refs) => {
            assert!(refs[0].native_provider.is_empty());
            assert!(refs[0].native_mode.is_empty());
            assert!(!refs[0].bound);
        }
        other => panic!("old xai aliases do not become cli: {other:?}"),
    }
}

#[test]
fn first_bind_writes_exact_default_grants_and_leaves_existing_grants() {
    let opened = open_db("exact-grants");
    let db = opened.db();
    let account_id = native_binding::account_id_for("codex", "fresh-codex.json").unwrap();
    let credential_id =
        ocg_domain::credential::credential_id_for_legacy_account(&account_id).to_string();
    let body = json!({
        "authRefs": [{
            "relativePath": "fresh-codex.json",
            "provider": "cpa",
            "providerId": "cpa",
            "rawProviderLabel": "codex",
            "effectiveSubtype": "codex",
            "effectiveMode": "",
            "effectiveGenerationBase": "",
            "credentialId": credential_id,
            "credentialVersion": "1",
            "materialRevision": "material",
            "models": ["gpt-5"],
            "disabled": false,
            "status": "active"
        }]
    });
    let snapshot = discovery_from_ready_value(&body);
    let lease = capture_native_lease(&db.conn, 0, 1).unwrap();
    reconcile_owned_discovery(&db.conn, &snapshot, &lease).unwrap();
    let stored = load_record(db);
    assert_eq!(stored.oauth[0].provider_id, CPA_PROVIDER_ID);
    assert_eq!(stored.oauth[0].native_provider, "codex");
    assert_eq!(
        stored.oauth[0].reported_base,
        "https://chatgpt.com/backend-api/codex"
    );
    let row = native_binding::load_owned_credential(&db.conn, &stored.oauth[0].credential_id)
        .unwrap()
        .unwrap();
    let routes = sealed_routes(CpaOAuthProvider::Codex, "gpt-5");
    let mut ids = Vec::new();
    let mut origins = Vec::new();
    for route in &routes {
        for target in &route.native_targets {
            if !ids.contains(&target.pin.endpoint_id) {
                ids.push(target.pin.endpoint_id.clone());
            }
            if !origins.contains(&target.pin.origin) {
                origins.push(target.pin.origin.clone());
            }
        }
    }
    assert_eq!(row.allowed_endpoint_ids, ids);
    assert_eq!(row.allowed_origins, origins);
    assert!(ids.len() > 1);
    let lease = capture_native_lease(&db.conn, 0, 1).unwrap();
    reconcile_owned_discovery(&db.conn, &snapshot, &lease).unwrap();
    let again = native_binding::load_owned_credential(&db.conn, &stored.oauth[0].credential_id)
        .unwrap()
        .unwrap();
    assert_eq!(again.allowed_endpoint_ids, ids);
    assert_eq!(again.allowed_origins, origins);

    let narrow = open_db("narrow-grants");
    let narrow_db = narrow.db();
    let bound = bind_codex(narrow_db);
    assert!(
        native_binding::write_initial_grants(
            &narrow_db.conn,
            &bound.account_id,
            &["legacy-endpoint".to_string()],
            &["https://chatgpt.com".to_string()],
        )
        .unwrap()
    );
    let discovered = discovered(&bound, "material-1", 1, bound.credential_version);
    let lease = capture_native_lease(&narrow_db.conn, 0, 1).unwrap();
    reconcile_owned_discovery(
        &narrow_db.conn,
        &DiscoverySnapshot::Complete(vec![discovered]),
        &lease,
    )
    .unwrap();
    let kept = native_binding::load_owned_credential(&narrow_db.conn, &bound.credential_id)
        .unwrap()
        .unwrap();
    assert_eq!(
        kept.allowed_endpoint_ids,
        vec!["legacy-endpoint".to_string()]
    );

    let kimi = open_db("kimi-gap");
    let kimi_db = kimi.db();
    let kimi_account = native_binding::account_id_for("kimi", "kimi.json").unwrap();
    let kimi_credential =
        ocg_domain::credential::credential_id_for_legacy_account(&kimi_account).to_string();
    let kimi_snapshot = discovery_from_ready_value(&json!({
        "authRefs": [{
            "relativePath": "kimi.json",
            "provider": "kimi",
            "type": "kimi",
            "credentialId": kimi_credential,
            "credentialVersion": "1",
            "materialRevision": "material",
            "models": ["kimi-for-coding"],
            "disabled": false,
            "status": "active"
        }]
    }));
    match &kimi_snapshot {
        DiscoverySnapshot::Complete(refs) => {
            assert!(refs[0].native_provider.is_empty());
            assert!(refs[0].native_mode.is_empty());
            assert!(!refs[0].bound);
        }
        other => panic!("bare kimi is not a native provider: {other:?}"),
    }
    let lease = capture_native_lease(&kimi_db.conn, 0, 1).unwrap();
    reconcile_owned_discovery(&kimi_db.conn, &kimi_snapshot, &lease).unwrap();
    let stored_kimi = super::super::store::load(&kimi_db.conn).unwrap();
    assert!(
        stored_kimi
            .map(|record| record.oauth.is_empty())
            .unwrap_or(true)
    );
    assert!(
        native_binding::load_owned_credential(&kimi_db.conn, &kimi_credential)
            .unwrap()
            .is_none()
    );

    let proven = open_db("kimi-com-grants");
    let proven_db = proven.db();
    let proven_account = native_binding::account_id_for("kimi", "kimi-com.json").unwrap();
    let proven_credential =
        ocg_domain::credential::credential_id_for_legacy_account(&proven_account).to_string();
    let proven_snapshot = discovery_from_ready_value(&json!({
        "authRefs": [{
            "relativePath": "kimi-com.json",
            "provider": "cpa",
            "providerId": "cpa",
            "rawProviderLabel": "kimi.com",
            "effectiveSubtype": "kimi.com",
            "effectiveMode": "",
            "effectiveGenerationBase": "https://api.kimi.com/coding/v1",
            "credentialId": proven_credential,
            "credentialVersion": "1",
            "materialRevision": "material",
            "models": ["kimi-for-coding"],
            "disabled": false,
            "status": "active"
        }]
    }));
    let lease = capture_native_lease(&proven_db.conn, 0, 1).unwrap();
    reconcile_owned_discovery(&proven_db.conn, &proven_snapshot, &lease).unwrap();
    let proven_row = load_record(proven_db);
    assert_eq!(proven_row.oauth[0].provider_id, CPA_PROVIDER_ID);
    assert_eq!(proven_row.oauth[0].native_provider, "kimi");
    assert_eq!(proven_row.oauth[0].native_mode, "com");
    assert_eq!(
        proven_row.oauth[0].reported_base,
        "https://api.kimi.com/coding/v1"
    );
    let proven_grants =
        native_binding::load_owned_credential(&proven_db.conn, &proven_row.oauth[0].credential_id)
            .unwrap()
            .unwrap();
    let facts = crate::cpa_projection::NativeAuthorityFacts {
        raw_label: proven_row.oauth[0].raw_provider_label.clone(),
        provider: proven_row.oauth[0].native_provider.clone(),
        mode: proven_row.oauth[0].native_mode.clone(),
        reported_base: proven_row.oauth[0].reported_base.clone(),
    };
    let connection = ocg_domain::connection::connection_id_for_legacy(
        ocg_domain::connection::LegacyConnectionKind::BuiltinProvider,
        native_binding::OWNED_NATIVE_LEGACY_ID,
    );
    let (ids, origins) =
        crate::cpa_projection::default_grant_ids(&facts, &["kimi-for-coding".into()], &connection)
            .expect("kimi.com grants");
    assert_eq!(proven_grants.allowed_endpoint_ids, ids);
    assert_eq!(proven_grants.allowed_origins, origins);
    assert_eq!(origins, vec!["https://api.kimi.com".to_string()]);
    assert!(!ids.is_empty());
}
