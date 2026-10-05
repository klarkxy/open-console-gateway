//! Behavior checks for the read-only routing facade.
//!
//! Cargo is not run from this leaf. The parent module does not declare
//! `explain` yet, so this file is not part of the crate until primary adds
//! `pub(crate) mod explain`.

use super::super::ExecutionError;
use super::super::store::{self, AuthStamp, OAuthPresence, OAuthStamp, Record};
use super::{
    CONVERSATION_BINDING_NOT_EVALUATED, CPA_SELECTION_NOT_EVALUATED, HistoricalPlacement,
    MaterialFact, OwnedRoutingFacts, QUOTA_TRIAL_NOT_EVALUATED, QueryResolutionKind,
    RouteExclusion, RouteFact, RoutePlane, RoutePosture, RoutingProductChannel, Spelling,
    explain_owned_routes,
};
use crate::cpa_policy::{
    AdmittedAttempt, EvidenceSource, PolicyDocument, Reset, ResetEvidence, Restriction,
    SETTINGS_KEY as POLICY_KEY, Scope, ScopedQuotaView, SendKind, Subject, Window,
};
use crate::cpa_projection::{
    CredentialRouteSet, NATIVE_ENDPOINT_PIN_CAPABILITY, NativeAuthorityFacts, NormalizedRoute,
    native_route_targets,
};
use crate::cpa_runtime::{
    CpaRuntimeError, CpaRuntimeLogTail, CpaRuntimeProcessHost, CpaRuntimeProcessSpec,
    CpaRuntimeSecret,
};
use crate::crypto::{KeyCipher, StaticKeyCipher};
use crate::db::Database;
use crate::db::destination_store::{insert_destination_row, replace_destination_catalog};
use crate::db::native_binding::{self, NativeModelInsert, OWNED_NATIVE_LEGACY_ID};
use crate::models::RoutingMode;
use crate::state::{CoreState, CoreStateInner};
use chrono::{DateTime, Utc};
use ocg_domain::catalog::UpstreamProtocolKind;
use ocg_domain::connection::{LegacyConnectionKind, connection_id_for_legacy};
use ocg_domain::credential::{QuotaPolicyMode, QuotaSubject, RelationConfidence};
use ocg_domain::destination::{
    AdapterKind, AuthScheme, CatalogModel, Destination, LegacyDestinationRef, ModelResolution,
    sealed_capabilities,
};
use ocg_domain::ids::{
    COMMAND_CODE_PROVIDER_ID, CPA_PROVIDER_ID, MINIMAX_PROVIDER_ID, OPENCODE_ZEN_FREE_PROVIDER_ID,
};
use rusqlite::{Connection, params};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use uuid::Uuid;

const CIPHER: &str = "cipher-must-stay-in-row";
const MODEL: &str = "codex-test";
const PROTOCOL: &str = "chat_completions";
const DESIRED_DIGEST: &str = "abababababababababababababababababababababababababababababababab";
const APPLIED_DIGEST: &str = "cdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcd";
const OWNED_LISTENER: &str = "http://127.0.0.1:9";

#[derive(Clone)]
struct OpenOptions {
    apply_status: &'static str,
    applied_revision: u64,
    policy_ready: bool,
    unavailable: bool,
    native_validation_only: bool,
    capabilities: Vec<String>,
    face_exclusions: bool,
    historical_remote: bool,
}

struct World {
    dir: PathBuf,
    state: Option<CoreState>,
}

struct Pins {
    endpoint_id: String,
    origin: String,
    endpoint_fingerprint: String,
    targets: Vec<crate::cpa_projection::NativeDispatchTarget>,
}

impl Drop for World {
    fn drop(&mut self) {
        self.state.take();
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

impl World {
    fn state(&self) -> &CoreState {
        self.state.as_ref().expect("state")
    }

    fn explain(&self) -> OwnedRoutingFacts {
        explain_owned_routes(self.state(), MODEL, PROTOCOL, clock()).expect("routing facts")
    }
}

fn clock() -> DateTime<Utc> {
    instant("2026-10-04T12:00:00Z")
}

fn future() -> DateTime<Utc> {
    instant("2026-10-05T00:00:00Z")
}

fn past() -> DateTime<Utc> {
    instant("2026-10-03T00:00:00Z")
}

fn instant(raw: &str) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(raw)
        .expect("clock")
        .with_timezone(&Utc)
}

fn standard_options() -> OpenOptions {
    OpenOptions {
        apply_status: "apply_failed",
        applied_revision: 4,
        policy_ready: false,
        unavailable: false,
        native_validation_only: false,
        capabilities: vec![NATIVE_ENDPOINT_PIN_CAPABILITY.to_string()],
        face_exclusions: false,
        historical_remote: false,
    }
}

fn open_world(options: OpenOptions) -> World {
    open_world_edited(options, |_, _| {}, |_, _| {})
}

fn open_world_edited(
    options: OpenOptions,
    edit_conn: impl FnOnce(&Connection, &Pins),
    edit_record: impl FnOnce(&mut Record, &Pins),
) -> World {
    let dir = std::env::temp_dir().join(format!("ocg-explain-{}", Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let db = Database::open(dir.clone()).unwrap();
    let pins = canonical_pins();
    seed(&db.conn, &pins, options.historical_remote);
    edit_conn(&db.conn, &pins);
    let mut record = build_record(&options, &pins);
    edit_record(&mut record, &pins);
    store::save(&db.conn, &record).unwrap();
    let cipher: Arc<dyn KeyCipher + Send + Sync> =
        Arc::new(StaticKeyCipher::new("synthetic-cpa-execution"));
    let state = Arc::new(CoreStateInner::new(db, dir.clone(), cipher).unwrap());
    World {
        dir,
        state: Some(state),
    }
}

fn canonical_pins() -> Pins {
    let facts = NativeAuthorityFacts {
        raw_label: String::new(),
        provider: "codex".into(),
        mode: String::new(),
        reported_base: String::new(),
    };
    let connection = connection_id_for_legacy(
        LegacyConnectionKind::BuiltinProvider,
        OWNED_NATIVE_LEGACY_ID,
    );
    let (primary, targets) = native_route_targets(&facts, MODEL, PROTOCOL, &connection)
        .expect("canonical codex chat targets");
    Pins {
        endpoint_id: primary.endpoint_id,
        origin: primary.origin,
        endpoint_fingerprint: primary.endpoint_fingerprint,
        targets,
    }
}

fn seed(conn: &Connection, pins: &Pins, historical_remote: bool) {
    insert_lane(
        conn,
        "dest-zen",
        "Zen Dest",
        AdapterKind::Zen,
        AuthScheme::None,
        true,
        Some("https://zen.example"),
        "cred-zen",
        "acct-zen",
        "Zen Public",
        OPENCODE_ZEN_FREE_PROVIDER_ID,
        true,
        "",
        "bind-zen",
        "",
    );
    insert_lane(
        conn,
        "dest-goat",
        "Goat Dest",
        AdapterKind::Goat,
        AuthScheme::Bearer,
        true,
        Some("https://goat.example"),
        "cred-goat",
        "acct-goat",
        "Goat Public",
        COMMAND_CODE_PROVIDER_ID,
        true,
        CIPHER,
        "bind-goat",
        "",
    );
    insert_lane(
        conn,
        "dest-http",
        "Http Dest",
        AdapterKind::Http,
        AuthScheme::ApiKey,
        true,
        Some("https://http.example"),
        "cred-http",
        "acct-http",
        "Http Public",
        "custom-http",
        true,
        CIPHER,
        "bind-http",
        "",
    );
    let connection = connection_id_for_legacy(
        LegacyConnectionKind::BuiltinProvider,
        OWNED_NATIVE_LEGACY_ID,
    );
    insert_lane(
        conn,
        "dest-native",
        "Native Dest",
        AdapterKind::Cpa,
        AuthScheme::Bearer,
        true,
        Some(OWNED_LISTENER),
        "cred-native",
        "acct-native",
        "Native Public",
        CPA_PROVIDER_ID,
        true,
        "",
        "bind-native",
        connection.as_str(),
    );
    insert_lane(
        conn,
        "dest-held",
        "Held Dest",
        AdapterKind::Http,
        AuthScheme::Bearer,
        false,
        Some("https://held.example"),
        "cred-held",
        "acct-held",
        "Held Label",
        "custom-http",
        false,
        "",
        "bind-held",
        "",
    );
    insert_lane(
        conn,
        "dest-http",
        "Http Dest",
        AdapterKind::Http,
        AuthScheme::ApiKey,
        true,
        Some("https://http.example"),
        "cred-stale",
        "acct-stale",
        "Stale Public",
        "custom-http",
        true,
        "",
        "bind-stale",
        "",
    );
    insert_lane(
        conn,
        "dest-native",
        "Native Dest",
        AdapterKind::Cpa,
        AuthScheme::Bearer,
        true,
        Some(OWNED_LISTENER),
        "cred-ungranted",
        "acct-ungranted",
        "Ungranted Public",
        CPA_PROVIDER_ID,
        true,
        "",
        "bind-ungranted",
        connection.as_str(),
    );
    insert_lane(
        conn,
        "dest-native",
        "Native Dest",
        AdapterKind::Cpa,
        AuthScheme::Bearer,
        true,
        Some(OWNED_LISTENER),
        "cred-pending",
        "acct-pending",
        "Pending Public",
        CPA_PROVIDER_ID,
        true,
        "",
        "bind-pending",
        connection.as_str(),
    );
    replace_destination_catalog(
        conn,
        "dest-native",
        &[CatalogModel {
            public_model: MODEL.into(),
            upstream_model: MODEL.into(),
            protocols: vec![UpstreamProtocolKind::ChatCompletions],
            preferred: Some(UpstreamProtocolKind::ChatCompletions),
            enabled: true,
            upstream_override: None,
        }],
    )
    .unwrap();
    grant(conn, "cred-native", "endpoint_id", &pins.endpoint_id);
    grant(conn, "cred-native", "origin", &pins.origin);
    conn.execute(
        "INSERT INTO quota_pools (
            id, subject_kind, subject_ref, relation_confidence, policy_mode, created_at
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![
            "pool-held",
            QuotaSubject::Credential.as_str(),
            "acct-held",
            RelationConfidence::Unknown.as_str(),
            QuotaPolicyMode::AuthoritativeLimit.as_str(),
            "2026-01-01T00:00:00Z",
        ],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO quota_pool_members (pool_id, account_id) VALUES (?1, ?2)",
        params!["pool-held", "acct-held"],
    )
    .unwrap();
    if historical_remote {
        insert_lane(
            conn,
            "dest-remote",
            "Remote Dest",
            AdapterKind::Cpa,
            AuthScheme::Bearer,
            true,
            Some("https://historical.example"),
            "cred-remote",
            "acct-remote",
            "Remote Public",
            CPA_PROVIDER_ID,
            true,
            "",
            "bind-remote",
            connection.as_str(),
        );
        replace_destination_catalog(
            conn,
            "dest-remote",
            &[CatalogModel {
                public_model: MODEL.into(),
                upstream_model: MODEL.into(),
                protocols: vec![UpstreamProtocolKind::ChatCompletions],
                preferred: Some(UpstreamProtocolKind::ChatCompletions),
                enabled: true,
                upstream_override: None,
            }],
        )
        .unwrap();
    }
}

#[allow(clippy::too_many_arguments)]
fn insert_lane(
    conn: &Connection,
    destination_id: &str,
    destination_name: &str,
    adapter: AdapterKind,
    auth: AuthScheme,
    destination_enabled: bool,
    base_url: Option<&str>,
    credential_id: &str,
    legacy_account_id: &str,
    account_label: &str,
    provider_id: &str,
    credential_enabled: bool,
    cipher: &str,
    binding_id: &str,
    connection_id: &str,
) {
    if conn
        .query_row(
            "SELECT COUNT(*) FROM destinations WHERE id = ?1",
            [destination_id],
            |row| row.get::<_, i64>(0),
        )
        .unwrap()
        == 0
    {
        insert_destination_row(
            conn,
            &Destination {
                id: destination_id.into(),
                legacy: LegacyDestinationRef::CustomAccount(destination_id.into()),
                adapter,
                name: destination_name.into(),
                brand_family: None,
                base_url: base_url.map(str::to_string),
                protocols: vec![UpstreamProtocolKind::ChatCompletions],
                protocol_routes: Vec::new(),
                auth_scheme: auth,
                model_resolution: ModelResolution::AdapterDefined,
                catalog: Vec::new(),
                capabilities: sealed_capabilities(adapter),
                plan: None,
                max_credentials: None,
                observer_credential_id: None,
                enabled: destination_enabled,
            },
        )
        .unwrap();
    }
    conn.execute(
        "INSERT INTO credentials (
            id, legacy_account_id, destination_id, name, has_secret, enabled, routing_rank,
            scope_json, auth_state, key_cipher, provider_id, credential_kind, quota_scope,
            account_type, setup_step, verification_status, credential_version,
            authorization_connection_id, created_at, updated_at
         ) VALUES (
            ?1, ?2, ?3, ?4, ?5, ?6, 99, '{\"kind\":\"all\"}', 'unknown', ?7, ?8, 'key', 'key',
            'key', 'ready', 'verified', 3, ?9, '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z'
         )",
        params![
            credential_id,
            legacy_account_id,
            destination_id,
            account_label,
            i64::from(!cipher.is_empty()),
            i64::from(credential_enabled),
            cipher,
            provider_id,
            connection_id,
        ],
    )
    .unwrap();
    conn.execute(
        "UPDATE credentials SET binding_id = ?2, binding_enabled = 1 WHERE id = ?1",
        params![credential_id, binding_id],
    )
    .unwrap();
}

fn grant(conn: &Connection, credential_id: &str, kind: &str, value: &str) {
    conn.execute(
        "INSERT INTO credential_grants (credential_id, kind, value) VALUES (?1, ?2, ?3)",
        params![credential_id, kind, value],
    )
    .unwrap();
}

fn build_record(options: &OpenOptions, pins: &Pins) -> Record {
    let mut native = pinned_route(pins, MODEL, MODEL);
    native.validation_only = options.native_validation_only;
    let mut desired = blank_route("desired-model", "desired-model");
    desired.validation_only = false;
    let mut held = blank_route("held-model", "held-model");
    held.validation_only = true;
    let mut record = Record::empty();
    record.child_generation = 11;
    record.applied_generation = 11;
    record.desired_revision = 5;
    record.applied_revision = options.applied_revision;
    record.desired_digest = DESIRED_DIGEST.into();
    record.applied_digest = APPLIED_DIGEST.into();
    record.apply_status = options.apply_status.into();
    record.policy_ready = options.policy_ready;
    record.unavailable = options.unavailable;
    record.host_capabilities = options.capabilities.clone();
    record.owned_origin = OWNED_LISTENER.into();
    record.desired_auth = vec![stamp(
        "native-auth",
        "cred-native",
        3,
        "bind-native",
        CPA_PROVIDER_ID,
    )];
    record.desired_routes = vec![route_set(
        "native-auth",
        "cred-native",
        3,
        "bind-native",
        2,
        vec![desired],
    )];
    record.applied_auth = vec![
        stamp(
            "zen-auth",
            "cred-zen",
            3,
            "bind-zen",
            OPENCODE_ZEN_FREE_PROVIDER_ID,
        ),
        stamp(
            "native-auth",
            "cred-native",
            3,
            "bind-native",
            CPA_PROVIDER_ID,
        ),
        stamp("http-auth", "cred-http", 3, "bind-http", "custom-http"),
        stamp(
            "goat-auth",
            "cred-goat",
            3,
            "bind-goat",
            COMMAND_CODE_PROVIDER_ID,
        ),
        stamp("held-auth", "cred-held", 3, "bind-held", "custom-http"),
    ];
    record.applied_routes = vec![
        route_set(
            "zen-auth",
            "cred-zen",
            3,
            "bind-zen",
            1,
            vec![blank_route("zen-public", "")],
        ),
        route_set(
            "native-auth",
            "cred-native",
            3,
            "bind-native",
            2,
            vec![native],
        ),
        route_set(
            "http-auth",
            "cred-http",
            3,
            "bind-http",
            3,
            vec![blank_route("http-public", "")],
        ),
        route_set(
            "goat-auth",
            "cred-goat",
            3,
            "bind-goat",
            7,
            vec![blank_route("goat-public", "")],
        ),
        route_set("held-auth", "cred-held", 3, "bind-held", 8, vec![held]),
    ];
    record.oauth = vec![oauth("cred-native", "native-auth", OAuthPresence::Present)];
    if options.historical_remote {
        record.applied_auth.push(stamp(
            "remote-auth",
            "cred-remote",
            3,
            "bind-remote",
            CPA_PROVIDER_ID,
        ));
        record.applied_routes.push(route_set(
            "remote-auth",
            "cred-remote",
            3,
            "bind-remote",
            14,
            vec![pinned_route(pins, MODEL, MODEL)],
        ));
        record
            .oauth
            .push(oauth("cred-remote", "remote-auth", OAuthPresence::Present));
    }
    if options.face_exclusions {
        record.applied_auth.extend([
            stamp("stale-auth", "cred-stale", 3, "bind-stale", "custom-http"),
            stamp(
                "ungranted-auth",
                "cred-ungranted",
                3,
                "bind-ungranted",
                CPA_PROVIDER_ID,
            ),
            stamp(
                "pending-auth",
                "cred-pending",
                3,
                "bind-pending",
                CPA_PROVIDER_ID,
            ),
        ]);
        record.applied_routes.extend([
            route_set(
                "stale-auth",
                "cred-stale",
                2,
                "bind-stale",
                11,
                vec![blank_route("stale-public", "")],
            ),
            route_set(
                "ungranted-auth",
                "cred-ungranted",
                3,
                "bind-ungranted",
                12,
                vec![pinned_route(pins, MODEL, MODEL)],
            ),
            route_set(
                "pending-auth",
                "cred-pending",
                3,
                "bind-pending",
                13,
                vec![pinned_route(pins, MODEL, MODEL)],
            ),
        ]);
        record.oauth.extend([
            oauth("cred-ungranted", "ungranted-auth", OAuthPresence::Present),
            oauth("cred-pending", "pending-auth", OAuthPresence::Pending),
        ]);
    }
    record
}

fn pinned_route(pins: &Pins, public_model: &str, upstream: &str) -> NormalizedRoute {
    NormalizedRoute {
        public_model: public_model.into(),
        upstream_model: upstream.into(),
        protocol: PROTOCOL.into(),
        endpoint_id: pins.endpoint_id.clone(),
        origin: pins.origin.clone(),
        endpoint_fingerprint: pins.endpoint_fingerprint.clone(),
        validation_only: false,
        native_targets: pins.targets.clone(),
    }
}

fn blank_route(public_model: &str, upstream: &str) -> NormalizedRoute {
    NormalizedRoute {
        public_model: public_model.into(),
        upstream_model: upstream.into(),
        protocol: PROTOCOL.into(),
        endpoint_id: String::new(),
        origin: String::new(),
        endpoint_fingerprint: String::new(),
        validation_only: false,
        native_targets: Vec::new(),
    }
}

fn route_set(
    auth_id: &str,
    credential_id: &str,
    version: u64,
    binding_id: &str,
    rank: u32,
    routes: Vec<NormalizedRoute>,
) -> CredentialRouteSet {
    CredentialRouteSet {
        auth_id: auth_id.into(),
        credential_id: credential_id.into(),
        credential_version: version,
        binding_id: binding_id.into(),
        material_fingerprint: "route-material-fingerprint".into(),
        routing_rank: rank,
        routes,
        fingerprint: "set-fingerprint-absent".into(),
    }
}

fn stamp(
    auth_id: &str,
    credential_id: &str,
    version: u64,
    binding_id: &str,
    provider_id: &str,
) -> AuthStamp {
    AuthStamp {
        auth_id: auth_id.into(),
        credential_id: credential_id.into(),
        credential_version: version,
        binding_id: binding_id.into(),
        material_revision: "stamp-material-absent".into(),
        provider_id: provider_id.into(),
        registration_epoch: 4,
    }
}

fn oauth(credential_id: &str, auth_id: &str, presence: OAuthPresence) -> OAuthStamp {
    OAuthStamp {
        relative_path: "codex/token.json".into(),
        auth_id: auth_id.into(),
        credential_id: credential_id.into(),
        credential_version: 3,
        material_revision: "matrev-do-not-surface".into(),
        provider_id: CPA_PROVIDER_ID.into(),
        native_provider: "codex".into(),
        registration_epoch: 4,
        models: vec![MODEL.into()],
        presence,
        recovery: String::new(),
        raw_provider_label: String::new(),
        native_mode: String::new(),
        reported_base: String::new(),
    }
}

fn route<'a>(
    facts: &'a OwnedRoutingFacts,
    plane: RoutePlane,
    credential_id: &str,
) -> &'a RouteFact {
    let rows = match plane {
        RoutePlane::Desired => &facts.desired,
        RoutePlane::Applied => &facts.applied,
    };
    rows.iter()
        .find(|item| item.credential_id == credential_id)
        .unwrap_or_else(|| panic!("missing {credential_id} on {plane:?}"))
}

fn assert_pending(facts: &OwnedRoutingFacts) {
    assert!(facts.first_pick.is_none());
    assert_eq!(
        facts.conversation_binding,
        CONVERSATION_BINDING_NOT_EVALUATED
    );
    assert_eq!(
        facts.uncertainties,
        vec![
            CPA_SELECTION_NOT_EVALUATED.to_string(),
            QUOTA_TRIAL_NOT_EVALUATED.to_string(),
        ]
    );
    assert_eq!(facts.requested_model, MODEL);
    assert_eq!(facts.callable_protocol, PROTOCOL);
    assert_eq!(facts.evaluated_at, clock());
    for row in facts.desired.iter().chain(facts.applied.iter()) {
        assert!(row.caller_pending);
        assert!(row.send_pending);
        assert!(!row.migration_required);
        assert_ne!(row.historical_placement, HistoricalPlacement::Remote);
        if row.plane == RoutePlane::Desired {
            assert!(!row.client_configuration_eligible);
        }
    }
    let rendered = format!("{facts:?}");
    for marker in [
        "token.json",
        "matrev-do-not-surface",
        "route-material-fingerprint",
        "cipher-must-stay-in-row",
        "set-fingerprint-absent",
        "stamp-material-absent",
        "attempt-material-do-not-surface",
    ] {
        assert!(
            !rendered.contains(marker),
            "{marker} leaked into {rendered}"
        );
    }
}

fn assert_not_ready(facts: &OwnedRoutingFacts) {
    assert!(!facts.runtime.verified_ready);
    assert!(!facts.runtime.tuple_aligned);
    assert!(!facts.runtime.pin_capabilities_ready);
    assert!(!facts.runtime.origin_verified);
}

fn write_policy(world: &World, json: &str) {
    let db = world.state().db.lock();
    db.conn
        .execute(
            "INSERT OR REPLACE INTO settings (key, value) VALUES (?1, ?2)",
            params![POLICY_KEY, json],
        )
        .unwrap();
}

fn settings_snapshot(world: &World) -> Vec<(String, String)> {
    let db = world.state().db.lock();
    let mut statement = db
        .conn
        .prepare("SELECT key, value FROM settings ORDER BY key")
        .unwrap();
    statement
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
        .unwrap()
        .map(|row| row.unwrap())
        .collect()
}

fn cipher_snapshot(world: &World) -> Vec<(String, String)> {
    let db = world.state().db.lock();
    let mut statement = db
        .conn
        .prepare("SELECT id, key_cipher FROM credentials ORDER BY id")
        .unwrap();
    statement
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
        .unwrap()
        .map(|row| row.unwrap())
        .collect()
}

fn log_count(world: &World) -> i64 {
    let db = world.state().db.lock();
    if !crate::db::table_exists(&db.conn, "forward_logs").unwrap() {
        return 0;
    }
    db.conn
        .query_row("SELECT COUNT(*) FROM forward_logs", [], |row| row.get(0))
        .unwrap()
}

fn execution_value(world: &World) -> String {
    let db = world.state().db.lock();
    db.conn
        .query_row(
            "SELECT value FROM settings WHERE key = ?1",
            [store::SETTINGS_KEY],
            |row| row.get(0),
        )
        .unwrap()
}

fn column_rank(world: &World, credential_id: &str) -> i64 {
    let db = world.state().db.lock();
    db.conn
        .query_row(
            "SELECT routing_rank FROM credentials WHERE id = ?1",
            [credential_id],
            |row| row.get(0),
        )
        .unwrap()
}

#[test]
fn applied_native_authority_is_plane_local_and_not_a_pick() {
    let world = open_world(standard_options());
    let facts = world.explain();
    assert_pending(&facts);
    assert_not_ready(&facts);
    assert!(!facts.runtime.poisoned);
    assert!(!facts.runtime.state_changed);
    assert!(!facts.runtime.stopped);
    assert!(!facts.runtime.policy_malformed);
    assert!(!facts.runtime.unavailable);
    assert!(!facts.runtime.policy_ready);
    assert!(!facts.runtime.owned_running);
    assert!(!facts.runtime.owned_running_before);
    assert!(!facts.runtime.owned_running_after);
    assert_ne!(facts.projection.runtime_child_generation, 0);
    assert!(facts.resolution.known);
    assert_eq!(facts.resolution.kind, Some(QueryResolutionKind::PinnedRaw));
    assert!(facts.resolution.alias.is_none());
    assert!(!facts.resolution.ambiguous);
    assert!(facts.resolution.mappings.iter().any(|mapping| {
        mapping.destination_id == "dest-native"
            && mapping.upstream_model == MODEL
            && mapping.provider_id == CPA_PROVIDER_ID
            && mapping.routeable
            && !mapping.migration_required
            && mapping.adapter_kind == AdapterKind::Cpa.as_str()
    }));
    let native = route(&facts, RoutePlane::Applied, "cred-native");
    assert_eq!(native.adapter_kind, AdapterKind::Cpa.as_str());
    assert_eq!(native.historical_placement, HistoricalPlacement::OwnedPool);
    assert!(!native.migration_required);
    let http = route(&facts, RoutePlane::Applied, "cred-http");
    assert_eq!(http.adapter_kind, AdapterKind::Http.as_str());
    assert_eq!(
        http.historical_placement,
        HistoricalPlacement::NotApplicable
    );
    assert!(!http.migration_required);
    assert_eq!(native.posture, RoutePosture::Client);
    assert_eq!(native.material, MaterialFact::NativePresent);
    assert!(native.grants_cover);
    assert!(!native.native_operations.is_empty());
    assert!(native.capability_listed);
    assert!(!native.secret_recheck_pending);
    assert!(native.client_configuration_eligible);
    assert_eq!(native.spelling, Spelling::Same);
    assert_eq!(native.routing_rank, 2);
    assert_eq!(native.legacy_account_id, "acct-native");
    assert_ne!(native.legacy_account_id, native.credential_id);
    assert_eq!(native.channel, RoutingProductChannel::Go);
    assert_eq!(native.public_model, MODEL);
    assert_eq!(native.upstream_model, MODEL);
    assert_eq!(native.protocol, PROTOCOL);
    assert!(!native.endpoint_id.is_empty());
    assert!(native.exclusions.is_empty());
    assert!(matches!(native.quota, ScopedQuotaView::Unknown));
    let desired = route(&facts, RoutePlane::Desired, "cred-native");
    assert_eq!(desired.public_model, "desired-model");
    assert_eq!(desired.posture, RoutePosture::Excluded);
    assert!(desired.exclusions.contains(&RouteExclusion::Model));
    assert!(!desired.client_configuration_eligible);
    assert!(
        facts
            .applied
            .iter()
            .all(|row| row.public_model != "desired-model")
    );
    assert!(facts.desired.iter().all(|row| row.public_model != MODEL));
    assert_eq!(
        facts
            .applied
            .iter()
            .map(|row| row.credential_id.as_str())
            .collect::<Vec<_>>(),
        vec![
            "cred-zen",
            "cred-native",
            "cred-http",
            "cred-goat",
            "cred-held"
        ]
    );
}

#[test]
fn failed_apply_keeps_applied_client_route_distinct_from_desired() {
    let world = open_world(standard_options());
    let facts = world.explain();
    assert_pending(&facts);
    assert_eq!(facts.projection.apply_status, "apply_failed");
    assert_eq!(facts.runtime.apply_status, "apply_failed");
    assert!(!facts.projection.desired_running);
    assert_eq!(facts.projection.desired.generation, 11);
    assert_eq!(facts.projection.desired.revision, 5);
    assert_eq!(facts.projection.desired.digest, DESIRED_DIGEST);
    assert_eq!(facts.projection.applied.generation, 11);
    assert_eq!(facts.projection.applied.revision, 4);
    assert_eq!(facts.projection.applied.digest, APPLIED_DIGEST);
    let native = route(&facts, RoutePlane::Applied, "cred-native");
    assert_eq!(native.posture, RoutePosture::Client);
    assert!(native.client_configuration_eligible);
    let desired = route(&facts, RoutePlane::Desired, "cred-native");
    assert_ne!(desired.public_model, native.public_model);
    assert!(!desired.client_configuration_eligible);

    let mut validation = standard_options();
    validation.apply_status = "applied";
    validation.native_validation_only = true;
    let validation_world = open_world(validation);
    let validation_facts = validation_world.explain();
    assert_pending(&validation_facts);
    assert_eq!(validation_facts.runtime.apply_status, "applied");
    assert_not_ready(&validation_facts);
    let row = route(&validation_facts, RoutePlane::Applied, "cred-native");
    assert!(row.validation_only);
    assert_eq!(row.posture, RoutePosture::ValidationOnly);
    assert!(row.exclusions.is_empty());
    assert!(!row.client_configuration_eligible);
}

#[test]
fn excluded_applied_quota_survives_when_desired_omits_the_route() {
    let world = open_world(standard_options());
    let document = PolicyDocument {
        restrictions: vec![Restriction {
            scope: Scope {
                subject: Subject::Pool {
                    pool_id: "pool-held".into(),
                    pool_version: 1,
                },
                public_model: Some("held-model".into()),
            },
            window: Window::Week,
            reset: Reset::Known { at: future() },
            observed_at: clock(),
            observation_id: "obs-held-known".into(),
            source: EvidenceSource::GoUsage,
            recovery: None,
        }],
        attempts: Vec::new(),
    };
    write_policy(&world, &document.to_json().unwrap());
    let facts = world.explain();
    assert_pending(&facts);
    assert!(
        facts
            .desired
            .iter()
            .all(|row| row.credential_id != "cred-held" && row.public_model != "held-model")
    );
    let held = route(&facts, RoutePlane::Applied, "cred-held");
    assert_eq!(held.posture, RoutePosture::Excluded);
    assert!(held.validation_only);
    assert!(held.exclusions.contains(&RouteExclusion::Disabled));
    assert!(held.exclusions.contains(&RouteExclusion::Model));
    assert_eq!(held.legacy_account_id, "acct-held");
    assert_eq!(held.account_label, "Held Label");
    assert_eq!(held.destination_label, "Held Dest");
    assert_eq!(held.routing_rank, 8);
    assert_eq!(held.channel, RoutingProductChannel::Go);
    assert!(held.known_restriction_blocks);
    assert!(!held.trial_pending);
    assert!(!held.client_configuration_eligible);
    match &held.quota {
        ScopedQuotaView::Evidence(rows) => {
            assert_eq!(rows.len(), 1);
            let row = &rows[0];
            assert!(row.applicable);
            assert_eq!(row.observation_id, "obs-held-known");
            assert_eq!(row.window, Window::Week);
            assert_eq!(row.source, EvidenceSource::GoUsage);
            assert_eq!(row.scope.public_model.as_deref(), Some("held-model"));
            match &row.scope.subject {
                Subject::Pool {
                    pool_id,
                    pool_version,
                } => {
                    assert_eq!(pool_id, "pool-held");
                    assert_eq!(*pool_version, 1);
                }
                other => panic!("pool scope lost: {other:?}"),
            }
            match row.reset {
                ResetEvidence::Known { at } => assert_eq!(at, future()),
                other => panic!("reset lost: {other:?}"),
            }
        }
        other => panic!("quota lost: {other:?}"),
    }
}

#[test]
fn channels_labels_and_ranks_follow_stored_rows() {
    let world = open_world(standard_options());
    let facts = world.explain();
    assert_pending(&facts);
    let zen = route(&facts, RoutePlane::Applied, "cred-zen");
    let goat = route(&facts, RoutePlane::Applied, "cred-goat");
    let http = route(&facts, RoutePlane::Applied, "cred-http");
    let native = route(&facts, RoutePlane::Applied, "cred-native");
    assert_eq!(zen.channel, RoutingProductChannel::Free);
    assert_eq!(zen.material, MaterialFact::HttpNone);
    assert!(!zen.secret_recheck_pending);
    assert_eq!(goat.channel, RoutingProductChannel::Go);
    assert_eq!(http.channel, RoutingProductChannel::Go);
    assert_eq!(native.channel, RoutingProductChannel::Go);
    assert_eq!(goat.material, MaterialFact::KeyedUnchecked);
    assert_eq!(http.material, MaterialFact::KeyedUnchecked);
    assert!(goat.secret_recheck_pending);
    assert!(http.secret_recheck_pending);
    assert!(!native.secret_recheck_pending);
    for (row, account, destination, rank, legacy) in [
        (zen, "Zen Public", "Zen Dest", 1, "acct-zen"),
        (goat, "Goat Public", "Goat Dest", 7, "acct-goat"),
        (http, "Http Public", "Http Dest", 3, "acct-http"),
        (native, "Native Public", "Native Dest", 2, "acct-native"),
    ] {
        assert_eq!(row.account_label, account);
        assert_eq!(row.destination_label, destination);
        assert_eq!(row.routing_rank, rank);
        assert_eq!(row.legacy_account_id, legacy);
        assert_ne!(row.legacy_account_id, row.credential_id);
        assert_eq!(column_rank(&world, &row.credential_id), 99);
        assert!(row.exclusions.contains(&RouteExclusion::Model) || row.exclusions.is_empty());
    }
    assert!(zen.exclusions.contains(&RouteExclusion::Model));
    assert_eq!(native.posture, RoutePosture::Client);
}

#[test]
fn quota_known_expired_unknown_reset_and_malformed_stay_distinct() {
    let world = open_world(standard_options());
    let revision = world.state().settings_revision();
    let execution = execution_value(&world);
    let ciphers = cipher_snapshot(&world);
    let logs = log_count(&world);

    let open = world.explain();
    assert_pending(&open);
    assert!(!open.runtime.policy_malformed);
    let native = route(&open, RoutePlane::Applied, "cred-native");
    assert!(matches!(native.quota, ScopedQuotaView::Unknown));
    assert!(!native.known_restriction_blocks);
    assert!(!native.trial_pending);
    assert!(native.client_configuration_eligible);

    let mut known = credential_restriction(
        "obs-native-known",
        Window::FiveHours,
        EvidenceSource::GoLimit,
        Reset::Known { at: future() },
    );
    known.attempts.push(AdmittedAttempt {
        request_id: Uuid::parse_str("11111111-1111-4111-8111-111111111111").unwrap(),
        attempt_id: Uuid::parse_str("22222222-2222-4222-8222-222222222222").unwrap(),
        credential_id: "cred-native".into(),
        credential_version: 3,
        provider_id: CPA_PROVIDER_ID.into(),
        binding_id: "bind-native".into(),
        public_model: MODEL.into(),
        upstream_model: MODEL.into(),
        auth_id: "native-auth".into(),
        material_revision: "attempt-material-do-not-surface".into(),
        registration_epoch: 4,
        process_generation: 11,
        projection_revision: 4,
        projection_digest: [0xab; 32],
        kind: SendKind::Accepted,
        admitted_at: clock(),
        retain_until: future(),
        scopes: Vec::new(),
    });
    write_policy(&world, &known.to_json().unwrap());
    let known_facts = world.explain();
    assert_pending(&known_facts);
    let known_native = route(&known_facts, RoutePlane::Applied, "cred-native");
    assert!(known_native.known_restriction_blocks);
    assert!(!known_native.trial_pending);
    assert!(!known_native.client_configuration_eligible);
    match &known_native.quota {
        ScopedQuotaView::Evidence(rows) => {
            assert_eq!(rows.len(), 1);
            assert!(rows[0].applicable);
            assert_eq!(rows[0].observation_id, "obs-native-known");
            assert!(matches!(rows[0].reset, ResetEvidence::Known { .. }));
        }
        other => panic!("known quota lost: {other:?}"),
    }

    let expired = credential_restriction(
        "obs-native-expired",
        Window::Week,
        EvidenceSource::GoatPlan,
        Reset::Known { at: past() },
    );
    write_policy(&world, &expired.to_json().unwrap());
    let expired_facts = world.explain();
    let expired_native = route(&expired_facts, RoutePlane::Applied, "cred-native");
    assert!(!expired_native.known_restriction_blocks);
    assert!(!expired_native.trial_pending);
    assert!(expired_native.client_configuration_eligible);
    match &expired_native.quota {
        ScopedQuotaView::Evidence(rows) => {
            assert_eq!(rows.len(), 1);
            assert!(!rows[0].applicable);
            assert_eq!(rows[0].observation_id, "obs-native-expired");
            assert!(matches!(rows[0].reset, ResetEvidence::Expired { .. }));
        }
        other => panic!("expired quota lost: {other:?}"),
    }

    write_policy(
        &world,
        r#"{
            "version": 1,
            "restrictions": [{
                "scope": {
                    "subject": {
                        "kind": "credential",
                        "credential_id": "cred-native",
                        "credential_version": 3,
                        "provider_id": "cpa",
                        "binding_id": "bind-native"
                    },
                    "public_model": "codex-test"
                },
                "window": "five_hours",
                "reset": {"kind": "unknown"},
                "observed_at": "2026-10-04T12:00:00Z",
                "observation_id": "obs-native-unknown",
                "source": "goat_plan",
                "recovery": {}
            }],
            "attempts": []
        }"#,
    );
    let unknown_facts = world.explain();
    let unknown_native = route(&unknown_facts, RoutePlane::Applied, "cred-native");
    assert!(!unknown_native.known_restriction_blocks);
    assert!(unknown_native.trial_pending);
    assert!(!unknown_native.client_configuration_eligible);
    match &unknown_native.quota {
        ScopedQuotaView::Evidence(rows) => {
            assert_eq!(rows.len(), 1);
            assert!(rows[0].applicable);
            assert_eq!(rows[0].observation_id, "obs-native-unknown");
            assert_eq!(rows[0].reset, ResetEvidence::UnknownReset);
        }
        other => panic!("unknown reset lost: {other:?}"),
    }

    write_policy(&world, "{");
    let malformed = explain_owned_routes(world.state(), MODEL, PROTOCOL, clock())
        .expect("malformed policy remains a fact");
    assert!(malformed.runtime.policy_malformed);
    assert_pending(&malformed);
    for row in malformed.desired.iter().chain(malformed.applied.iter()) {
        assert!(matches!(row.quota, ScopedQuotaView::Malformed));
        assert!(!row.client_configuration_eligible);
    }
    assert_eq!(world.state().settings_revision(), revision);
    assert_eq!(execution_value(&world), execution);
    assert_eq!(cipher_snapshot(&world), ciphers);
    assert_eq!(log_count(&world), logs);
}

#[test]
fn stopped_incoherent_and_missing_capabilities_are_not_ready() {
    let mut stopped = standard_options();
    stopped.apply_status = "stopped";
    let world = open_world(stopped);
    let facts = world.explain();
    assert_pending(&facts);
    assert!(facts.runtime.stopped);
    assert_not_ready(&facts);
    assert_eq!(facts.runtime.apply_status, "stopped");
    let native = route(&facts, RoutePlane::Applied, "cred-native");
    assert!(native.capability_listed);
    assert_eq!(native.posture, RoutePosture::Client);
    assert!(native.caller_pending && native.send_pending);
    assert!(native.client_configuration_eligible);

    let mut applied = standard_options();
    applied.apply_status = "applied";
    applied.policy_ready = true;
    let applied_world = open_world(applied);
    let applied_facts = applied_world.explain();
    assert_eq!(applied_facts.runtime.apply_status, "applied");
    assert!(applied_facts.runtime.policy_ready);
    assert!(!applied_facts.runtime.stopped);
    assert_not_ready(&applied_facts);
    assert!(route(&applied_facts, RoutePlane::Applied, "cred-native").capability_listed);
}

#[test]
fn record_runtime_disagreement_is_state_changed() {
    let world = open_world(standard_options());
    let revision = world.state().settings_revision();
    let ciphers = cipher_snapshot(&world);
    let logs = log_count(&world);
    let mut changed = build_record(&standard_options(), &canonical_pins());
    changed.applied_revision = 9;
    {
        let db = world.state().db.lock();
        store::save(&db.conn, &changed).unwrap();
    }
    let facts = world.explain();
    assert_pending(&facts);
    assert!(facts.runtime.state_changed);
    assert!(!facts.runtime.unavailable);
    assert!(!facts.runtime.verified_ready);
    assert!(!facts.runtime.tuple_aligned);
    assert_eq!(facts.projection.applied.revision, 9);
    let native = route(&facts, RoutePlane::Applied, "cred-native");
    assert_eq!(native.posture, RoutePosture::Client);
    assert!(!native.client_configuration_eligible);
    assert!(
        facts
            .applied
            .iter()
            .chain(facts.desired.iter())
            .all(|row| !row.client_configuration_eligible)
    );
    assert_eq!(world.state().settings_revision(), revision);
    assert_eq!(cipher_snapshot(&world), ciphers);
    assert_eq!(log_count(&world), logs);
}

#[test]
fn unreadable_record_returns_unavailable_without_rewriting_settings() {
    let world = open_world(standard_options());
    let revision = world.state().settings_revision();
    let ciphers = cipher_snapshot(&world);
    let logs = log_count(&world);
    {
        let db = world.state().db.lock();
        db.conn
            .execute(
                "UPDATE settings SET value = ?2 WHERE key = ?1",
                params![store::SETTINGS_KEY, "{"],
            )
            .unwrap();
    }
    let error = explain_owned_routes(world.state(), MODEL, PROTOCOL, clock())
        .expect_err("unreadable record");
    assert!(matches!(
        error,
        ExecutionError::Unavailable(message) if message == "CPA execution record could not be read"
    ));
    assert_eq!(execution_value(&world), "{");
    assert_eq!(world.state().settings_revision(), revision);
    assert_eq!(cipher_snapshot(&world), ciphers);
    assert_eq!(log_count(&world), logs);
}

#[test]
fn repeated_reads_do_not_mutate_settings_logs_or_ciphertext() {
    let world = open_world(standard_options());
    let revision = world.state().settings_revision();
    let settings = settings_snapshot(&world);
    let ciphers = cipher_snapshot(&world);
    let logs = log_count(&world);
    assert!(
        ciphers
            .iter()
            .any(|(id, cipher)| id == "cred-goat" && cipher == CIPHER)
    );
    let first = world.explain();
    let second = world.explain();
    assert_eq!(first, second);
    assert_pending(&first);
    assert_eq!(settings_snapshot(&world), settings);
    assert_eq!(cipher_snapshot(&world), ciphers);
    assert_eq!(log_count(&world), logs);
    assert_eq!(world.state().settings_revision(), revision);
}

#[test]
fn stale_grant_and_native_presence_exclusions_stay_structured() {
    let mut options = standard_options();
    options.face_exclusions = true;
    let world = open_world(options);
    let facts = world.explain();
    assert_pending(&facts);
    let stale = route(&facts, RoutePlane::Applied, "cred-stale");
    assert_eq!(stale.posture, RoutePosture::Excluded);
    assert!(stale.exclusions.contains(&RouteExclusion::Version));
    assert_eq!(stale.credential_version, 2);
    assert_eq!(stale.current_version, Some(3));
    assert_eq!(stale.channel, RoutingProductChannel::Go);
    let ungranted = route(&facts, RoutePlane::Applied, "cred-ungranted");
    assert_eq!(ungranted.posture, RoutePosture::Excluded);
    assert!(ungranted.exclusions.contains(&RouteExclusion::NotGranted));
    assert!(!ungranted.grants_cover);
    assert_eq!(ungranted.channel, RoutingProductChannel::Go);
    assert_eq!(ungranted.legacy_account_id, "acct-ungranted");
    assert!(!ungranted.endpoint_id.is_empty());
    let pending = route(&facts, RoutePlane::Applied, "cred-pending");
    assert_eq!(pending.posture, RoutePosture::Excluded);
    assert_eq!(pending.material, MaterialFact::Unproven);
    assert!(pending.exclusions.contains(&RouteExclusion::NativePresence));
    assert_ne!(pending.posture, RoutePosture::Client);
    let native = route(&facts, RoutePlane::Applied, "cred-native");
    assert_eq!(native.posture, RoutePosture::Client);
    assert!(native.grants_cover);
}

struct FlagHost {
    running: AtomicBool,
    hits: AtomicUsize,
    flip: bool,
}

impl CpaRuntimeProcessHost for FlagHost {
    fn start_owned(&self, _: &CpaRuntimeProcessSpec) -> Result<(), CpaRuntimeError> {
        Ok(())
    }

    fn stop_owned(&self) -> Result<(), CpaRuntimeError> {
        Ok(())
    }

    fn owned_running(&self) -> bool {
        if self.flip {
            return self.hits.fetch_add(1, Ordering::SeqCst) == 0;
        }
        self.running.load(Ordering::SeqCst)
    }

    fn logs(&self) -> CpaRuntimeLogTail {
        CpaRuntimeLogTail {
            stdout: String::new(),
            stderr: String::new(),
        }
    }

    fn add_log_secret(&self, _: &CpaRuntimeSecret) {}
}

fn install_host(world: &World, running: bool, flip: bool) {
    world.state().cpa_runtime.set_host(Arc::new(FlagHost {
        running: AtomicBool::new(running),
        hits: AtomicUsize::new(0),
        flip,
    }));
}

fn insert_catalog_lane(
    world: &World,
    destination_id: &str,
    adapter: AdapterKind,
    provider_id: &str,
    credential_id: &str,
    public_model: &str,
    upstream: &str,
    enabled: bool,
    draft: bool,
) {
    let db = world.state().db.lock();
    insert_lane(
        &db.conn,
        destination_id,
        destination_id,
        adapter,
        AuthScheme::ApiKey,
        true,
        Some("https://catalog.example"),
        credential_id,
        &format!("acct-{credential_id}"),
        destination_id,
        provider_id,
        true,
        "",
        &format!("bind-{credential_id}"),
        "",
    );
    replace_destination_catalog(
        &db.conn,
        destination_id,
        &[CatalogModel {
            public_model: public_model.into(),
            upstream_model: upstream.into(),
            protocols: vec![UpstreamProtocolKind::ChatCompletions],
            preferred: Some(UpstreamProtocolKind::ChatCompletions),
            enabled,
            upstream_override: None,
        }],
    )
    .unwrap();
    db.conn
        .execute(
            "UPDATE destinations SET onboarding_draft = ?2 WHERE id = ?1",
            params![destination_id, i64::from(draft)],
        )
        .unwrap();
}

#[test]
fn accepted_record_with_exited_or_missing_host_is_not_running() {
    let mut options = standard_options();
    options.apply_status = "applied";
    options.policy_ready = true;
    options.capabilities = vec![
        NATIVE_ENDPOINT_PIN_CAPABILITY.to_string(),
        "validated-protocol-pin-v1".to_string(),
        "validation-only-routes-v1".to_string(),
        "absolute-request-deadline-v1".to_string(),
    ];
    let missing = open_world(options.clone());
    let stored = execution_value(&missing);
    let missing_facts = missing.explain();
    assert_eq!(missing_facts.runtime.apply_status, "applied");
    assert!(missing_facts.runtime.pin_capabilities_ready);
    assert!(!missing_facts.runtime.verified_ready);
    assert!(!missing_facts.runtime.tuple_aligned);
    assert!(!missing_facts.runtime.owned_running);
    assert!(!missing_facts.runtime.owned_running_before);
    assert!(!missing_facts.runtime.owned_running_after);
    assert!(!missing_facts.runtime.state_changed);
    assert!(!missing_facts.runtime.unavailable);
    assert!(
        route(&missing_facts, RoutePlane::Applied, "cred-native").client_configuration_eligible
    );
    assert_eq!(execution_value(&missing), stored);

    let exited = open_world(options);
    install_host(&exited, false, false);
    let exited_facts = exited.explain();
    assert!(!exited_facts.runtime.owned_running_before);
    assert!(!exited_facts.runtime.owned_running_after);
    assert!(!exited_facts.runtime.owned_running);
    assert!(exited_facts.runtime.pin_capabilities_ready);
    assert!(!exited_facts.runtime.verified_ready);
    assert!(route(&exited_facts, RoutePlane::Applied, "cred-native").client_configuration_eligible);
}

#[test]
fn owned_running_requires_both_live_observations() {
    let world = open_world(standard_options());
    install_host(&world, true, true);
    let facts = world.explain();
    assert!(facts.runtime.owned_running_before);
    assert!(!facts.runtime.owned_running_after);
    assert!(!facts.runtime.owned_running);
    assert!(!facts.runtime.state_changed);
    assert!(!facts.runtime.unavailable);
    assert!(route(&facts, RoutePlane::Applied, "cred-native").client_configuration_eligible);
}

#[test]
fn record_only_capability_list_disagreement_suppresses_eligibility() {
    let world = open_world(standard_options());
    let before = execution_value(&world);
    let mut changed = build_record(&standard_options(), &canonical_pins());
    changed
        .host_capabilities
        .push("record-only-capability".into());
    {
        let db = world.state().db.lock();
        store::save(&db.conn, &changed).unwrap();
    }
    let facts = world.explain();
    assert!(facts.runtime.state_changed);
    assert!(!facts.runtime.unavailable);
    assert!(!facts.runtime.verified_ready);
    let native = route(&facts, RoutePlane::Applied, "cred-native");
    assert_eq!(native.posture, RoutePosture::Client);
    assert!(!native.client_configuration_eligible);
    assert!(
        facts
            .applied
            .iter()
            .all(|row| !row.client_configuration_eligible)
    );
    assert_ne!(execution_value(&world), before);
    assert!(execution_value(&world).contains("record-only-capability"));
}

#[test]
fn catalog_resolution_distinguishes_alias_pin_disabled_and_unknown() {
    let world = open_world(standard_options());
    insert_catalog_lane(
        &world,
        "dest-alias",
        AdapterKind::Minimax,
        MINIMAX_PROVIDER_ID,
        "cred-alias",
        "my-facade-alias",
        "vendor/facade-upstream",
        true,
        false,
    );
    insert_catalog_lane(
        &world,
        "dest-minimax",
        AdapterKind::Minimax,
        MINIMAX_PROVIDER_ID,
        "cred-minimax",
        "MiniMax-M3",
        "MiniMax-M3",
        true,
        false,
    );
    insert_catalog_lane(
        &world,
        "dest-raw",
        AdapterKind::Http,
        "custom-http",
        "cred-raw",
        "vendor/ocg-facade-raw",
        "vendor/ocg-facade-raw",
        true,
        false,
    );
    insert_catalog_lane(
        &world,
        "dest-off",
        AdapterKind::Http,
        "custom-http",
        "cred-off",
        "ocg-facade-disabled",
        "ocg-facade-disabled",
        false,
        false,
    );
    insert_catalog_lane(
        &world,
        "dest-draft",
        AdapterKind::Http,
        "custom-http",
        "cred-draft",
        "ocg-facade-draft",
        "ocg-facade-draft",
        true,
        true,
    );
    {
        let db = world.state().db.lock();
        replace_destination_catalog(
            &db.conn,
            "dest-native",
            &[
                CatalogModel {
                    public_model: MODEL.into(),
                    upstream_model: MODEL.into(),
                    protocols: vec![UpstreamProtocolKind::ChatCompletions],
                    preferred: Some(UpstreamProtocolKind::ChatCompletions),
                    enabled: true,
                    upstream_override: None,
                },
                CatalogModel {
                    public_model: "ocg-native-scoped".into(),
                    upstream_model: "ocg-native-scoped".into(),
                    protocols: vec![UpstreamProtocolKind::ChatCompletions],
                    preferred: Some(UpstreamProtocolKind::ChatCompletions),
                    enabled: true,
                    upstream_override: None,
                },
            ],
        )
        .unwrap();
        db.conn
            .execute(
                "INSERT INTO provider_model_catalogs (
                    provider_id, models_json, refreshed_at, source_url
                 ) VALUES ('cpa', ?1, '2026-01-01T00:00:00Z', 'memory')
                 ON CONFLICT(provider_id) DO UPDATE SET models_json = excluded.models_json",
                params![r#"["retired-global-cpa-only"]"#],
            )
            .unwrap();
    }

    let alias = explain_owned_routes(world.state(), "my-facade-alias", PROTOCOL, clock()).unwrap();
    assert!(alias.resolution.known);
    assert_eq!(alias.resolution.kind, Some(QueryResolutionKind::Alias));
    assert_eq!(alias.resolution.alias.as_deref(), Some("my-facade-alias"));
    assert!(!alias.resolution.ambiguous);
    assert!(alias.resolution.mappings.iter().any(|mapping| {
        mapping.destination_id == "dest-alias"
            && mapping.upstream_model == "vendor/facade-upstream"
            && !mapping.routeable
            && !mapping.migration_required
            && mapping.adapter_kind == AdapterKind::Minimax.as_str()
    }));
    assert!(
        alias
            .applied
            .iter()
            .chain(alias.desired.iter())
            .all(|row| row.upstream_model != "vendor/facade-upstream")
    );

    {
        let db = world.state().db.lock();
        db.conn
            .execute(
                "DELETE FROM destination_models
                 WHERE destination_id != 'dest-minimax'
                   AND (upstream_model = 'MiniMax-M3' OR public_model = 'MiniMax-M3'
                        OR public_model = 'minimax-m3')",
                [],
            )
            .unwrap();
    }
    let shared = explain_owned_routes(world.state(), "minimax-m3", PROTOCOL, clock()).unwrap();
    assert!(shared.resolution.known);
    assert!(!shared.resolution.ambiguous);
    assert_eq!(shared.resolution.kind, Some(QueryResolutionKind::Alias));
    assert_eq!(shared.resolution.alias.as_deref(), Some("minimax-m3"));
    assert!(shared.resolution.mappings.iter().any(|mapping| {
        mapping.provider_id == MINIMAX_PROVIDER_ID
            && mapping.upstream_model == "MiniMax-M3"
            && mapping.destination_id == "dest-minimax"
            && !mapping.routeable
            && !mapping.migration_required
    }));
    assert!(
        shared
            .applied
            .iter()
            .all(|row| row.upstream_model != "MiniMax-M3")
    );

    let pinned =
        explain_owned_routes(world.state(), "vendor/ocg-facade-raw", PROTOCOL, clock()).unwrap();
    assert!(pinned.resolution.known);
    assert_eq!(pinned.resolution.kind, Some(QueryResolutionKind::PinnedRaw));
    assert!(pinned.resolution.alias.is_none());
    assert!(pinned.resolution.mappings.iter().any(|mapping| {
        mapping.destination_id == "dest-raw" && !mapping.routeable && !mapping.migration_required
    }));

    let native =
        explain_owned_routes(world.state(), "ocg-native-scoped", PROTOCOL, clock()).unwrap();
    assert!(native.resolution.known);
    assert_eq!(native.resolution.kind, Some(QueryResolutionKind::PinnedRaw));
    assert!(native.resolution.mappings.iter().any(|mapping| {
        mapping.destination_id == "dest-native"
            && mapping.adapter_kind == AdapterKind::Cpa.as_str()
            && !mapping.routeable
            && !mapping.migration_required
    }));
    assert!(
        native.applied.is_empty()
            || native
                .applied
                .iter()
                .all(|row| row.public_model != "ocg-native-scoped")
    );

    let disabled =
        explain_owned_routes(world.state(), "ocg-facade-disabled", PROTOCOL, clock()).unwrap();
    assert!(disabled.resolution.known);
    assert!(
        disabled
            .resolution
            .mappings
            .iter()
            .all(|mapping| !mapping.routeable)
    );

    let draft = explain_owned_routes(world.state(), "ocg-facade-draft", PROTOCOL, clock()).unwrap();
    assert!(draft.resolution.known);
    assert!(draft.resolution.mappings.iter().any(|mapping| {
        mapping.destination_id == "dest-draft" && !mapping.routeable && !mapping.migration_required
    }));

    let unknown =
        explain_owned_routes(world.state(), "retired-global-cpa-only", PROTOCOL, clock()).unwrap();
    assert!(!unknown.resolution.known);
    assert!(unknown.resolution.kind.is_none());
    assert!(unknown.resolution.mappings.is_empty());
    assert!(!unknown.resolution.ambiguous);
}

fn filled_route(public_model: &str, upstream: &str, validation_only: bool) -> NormalizedRoute {
    let mut route = blank_route(public_model, upstream);
    route.endpoint_id = format!("endpoint-{public_model}");
    route.origin = "https://catalog.example".into();
    route.endpoint_fingerprint = format!("fingerprint-{public_model}");
    route.validation_only = validation_only;
    route
}

fn push_plane(
    record: &mut Record,
    plane: RoutePlane,
    auth_id: &str,
    credential_id: &str,
    binding_id: &str,
    rank: u32,
    route: NormalizedRoute,
) {
    let auth = stamp(auth_id, credential_id, 3, binding_id, "custom-http");
    let set = route_set(auth_id, credential_id, 3, binding_id, rank, vec![route]);
    match plane {
        RoutePlane::Desired => {
            record.desired_auth.push(auth);
            record.desired_routes.push(set);
        }
        RoutePlane::Applied => {
            record.applied_auth.push(auth);
            record.applied_routes.push(set);
        }
    }
}

fn pins_for(upstream: &str) -> Pins {
    let facts = NativeAuthorityFacts {
        raw_label: String::new(),
        provider: "codex".into(),
        mode: String::new(),
        reported_base: String::new(),
    };
    let connection = connection_id_for_legacy(
        LegacyConnectionKind::BuiltinProvider,
        OWNED_NATIVE_LEGACY_ID,
    );
    let (primary, targets) =
        native_route_targets(&facts, upstream, PROTOCOL, &connection).expect("collision pins");
    Pins {
        endpoint_id: primary.endpoint_id,
        origin: primary.origin,
        endpoint_fingerprint: primary.endpoint_fingerprint,
        targets,
    }
}

fn open_upstream_collision(public_model: &str, upstream: &str) -> World {
    let pins = pins_for(upstream);
    let endpoint_id = pins.endpoint_id.clone();
    let origin = pins.origin.clone();
    let applied = pinned_route(&pins, public_model, upstream);
    let connection = connection_id_for_legacy(
        LegacyConnectionKind::BuiltinProvider,
        OWNED_NATIVE_LEGACY_ID,
    );
    open_world_edited(
        standard_options(),
        move |conn, _| {
            insert_lane(
                conn,
                "dest-collision",
                "Collision",
                AdapterKind::Cpa,
                AuthScheme::Bearer,
                true,
                Some(OWNED_LISTENER),
                "cred-collision",
                "acct-collision",
                "Collision",
                CPA_PROVIDER_ID,
                true,
                "",
                "bind-collision",
                connection.as_str(),
            );
            replace_destination_catalog(
                conn,
                "dest-collision",
                &[
                    CatalogModel {
                        public_model: "ocg-collision-foo".into(),
                        upstream_model: "ocg-collision-bar".into(),
                        protocols: vec![UpstreamProtocolKind::ChatCompletions],
                        preferred: Some(UpstreamProtocolKind::ChatCompletions),
                        enabled: true,
                        upstream_override: None,
                    },
                    CatalogModel {
                        public_model: "ocg-collision-baz".into(),
                        upstream_model: "ocg-collision-foo".into(),
                        protocols: vec![UpstreamProtocolKind::ChatCompletions],
                        preferred: Some(UpstreamProtocolKind::ChatCompletions),
                        enabled: true,
                        upstream_override: None,
                    },
                ],
            )
            .unwrap();
            grant(conn, "cred-collision", "endpoint_id", &endpoint_id);
            grant(conn, "cred-collision", "origin", &origin);
        },
        move |record, _| {
            record.applied_auth.push(stamp(
                "collision-auth",
                "cred-collision",
                3,
                "bind-collision",
                CPA_PROVIDER_ID,
            ));
            record.applied_routes.push(route_set(
                "collision-auth",
                "cred-collision",
                3,
                "bind-collision",
                21,
                vec![applied],
            ));
            record.oauth.push(oauth(
                "cred-collision",
                "collision-auth",
                OAuthPresence::Present,
            ));
        },
    )
}

#[test]
fn same_destination_alias_route_does_not_route_a_different_raw_upstream() {
    let alias = open_upstream_collision("ocg-collision-foo", "ocg-collision-bar");
    let alias_facts =
        explain_owned_routes(alias.state(), "ocg-collision-foo", PROTOCOL, clock()).unwrap();
    assert!(!alias_facts.runtime.state_changed);
    assert!(alias_facts.resolution.known);
    assert!(!alias_facts.resolution.ambiguous);
    assert_eq!(
        alias_facts.resolution.kind,
        Some(QueryResolutionKind::PinnedRaw)
    );
    assert!(alias_facts.resolution.alias.is_none());
    assert!(alias_facts.resolution.mappings.iter().any(|mapping| {
        mapping.destination_id == "dest-collision"
            && mapping.provider_id == CPA_PROVIDER_ID
            && mapping.upstream_model == "ocg-collision-foo"
            && !mapping.routeable
            && !mapping.migration_required
    }));
    assert!(
        alias_facts
            .resolution
            .mappings
            .iter()
            .all(|mapping| mapping.upstream_model != "ocg-collision-bar")
    );
    let alias_route = route(&alias_facts, RoutePlane::Applied, "cred-collision");
    assert_eq!(alias_route.public_model, "ocg-collision-foo");
    assert_eq!(alias_route.upstream_model, "ocg-collision-bar");
    assert_eq!(alias_route.destination_id, "dest-collision");
    assert_eq!(alias_route.provider_id, CPA_PROVIDER_ID);
    assert_eq!(alias_route.posture, RoutePosture::Client);
    assert!(alias_route.client_configuration_eligible);
    assert!(alias_route.exclusions.is_empty());

    let actual = open_upstream_collision("ocg-collision-foo", "ocg-collision-foo");
    let actual_facts =
        explain_owned_routes(actual.state(), "ocg-collision-foo", PROTOCOL, clock()).unwrap();
    assert!(!actual_facts.runtime.state_changed);
    assert!(actual_facts.resolution.known);
    assert!(!actual_facts.resolution.ambiguous);
    assert_eq!(
        actual_facts.resolution.kind,
        Some(QueryResolutionKind::PinnedRaw)
    );
    assert!(actual_facts.resolution.alias.is_none());
    assert!(actual_facts.resolution.mappings.iter().any(|mapping| {
        mapping.destination_id == "dest-collision"
            && mapping.provider_id == CPA_PROVIDER_ID
            && mapping.upstream_model == "ocg-collision-foo"
            && mapping.routeable
            && !mapping.migration_required
    }));
    let actual_route = route(&actual_facts, RoutePlane::Applied, "cred-collision");
    assert_eq!(actual_route.public_model, "ocg-collision-foo");
    assert_eq!(actual_route.upstream_model, "ocg-collision-foo");
    assert_eq!(actual_route.destination_id, "dest-collision");
    assert_eq!(actual_route.provider_id, CPA_PROVIDER_ID);
    assert_eq!(actual_route.posture, RoutePosture::Client);
    assert!(actual_route.client_configuration_eligible);
    assert!(actual_route.exclusions.is_empty());
}

#[test]
fn known_catalog_without_applied_route_is_not_routeable() {
    let world = open_world(standard_options());
    insert_catalog_lane(
        &world,
        "dest-unrouted",
        AdapterKind::Http,
        "custom-http",
        "cred-unrouted",
        "ocg-known-unrouted",
        "ocg-known-unrouted",
        true,
        false,
    );
    let facts =
        explain_owned_routes(world.state(), "ocg-known-unrouted", PROTOCOL, clock()).unwrap();
    assert!(!facts.runtime.state_changed);
    assert!(facts.resolution.known);
    assert!(!facts.resolution.ambiguous);
    assert_eq!(facts.resolution.kind, Some(QueryResolutionKind::PinnedRaw));
    assert!(facts.resolution.mappings.iter().any(|mapping| {
        mapping.destination_id == "dest-unrouted"
            && mapping.provider_id == "custom-http"
            && !mapping.routeable
            && !mapping.migration_required
    }));
    assert!(
        facts
            .applied
            .iter()
            .chain(facts.desired.iter())
            .all(|row| row.destination_id != "dest-unrouted")
    );
}

#[test]
fn desired_only_catalog_stays_known_and_not_routeable() {
    let world = open_world_edited(
        standard_options(),
        |conn, _| {
            insert_lane(
                conn,
                "dest-desired-only",
                "Desired Only",
                AdapterKind::Http,
                AuthScheme::ApiKey,
                true,
                Some("https://catalog.example"),
                "cred-desired-only",
                "acct-desired-only",
                "Desired Only",
                "custom-http",
                true,
                "",
                "bind-desired-only",
                "",
            );
            replace_destination_catalog(
                conn,
                "dest-desired-only",
                &[CatalogModel {
                    public_model: "ocg-desired-only".into(),
                    upstream_model: "ocg-desired-only".into(),
                    protocols: vec![UpstreamProtocolKind::ChatCompletions],
                    preferred: Some(UpstreamProtocolKind::ChatCompletions),
                    enabled: true,
                    upstream_override: None,
                }],
            )
            .unwrap();
        },
        |record, _| {
            push_plane(
                record,
                RoutePlane::Desired,
                "desired-only-auth",
                "cred-desired-only",
                "bind-desired-only",
                16,
                filled_route("ocg-desired-only", "ocg-desired-only", false),
            );
        },
    );
    let facts = explain_owned_routes(world.state(), "ocg-desired-only", PROTOCOL, clock()).unwrap();
    assert!(!facts.runtime.state_changed);
    assert!(facts.resolution.known);
    assert!(
        facts
            .resolution
            .mappings
            .iter()
            .any(|mapping| { mapping.destination_id == "dest-desired-only" && !mapping.routeable })
    );
    let desired = route(&facts, RoutePlane::Desired, "cred-desired-only");
    assert!(!desired.validation_only);
    assert!(!desired.client_configuration_eligible);
    assert!(
        facts
            .applied
            .iter()
            .all(|row| row.credential_id != "cred-desired-only")
    );
}

#[test]
fn applied_validation_only_is_not_undone_by_desired_client_route() {
    let world = open_world_edited(
        standard_options(),
        |conn, _| {
            insert_lane(
                conn,
                "dest-validate",
                "Validate",
                AdapterKind::Http,
                AuthScheme::ApiKey,
                true,
                Some("https://catalog.example"),
                "cred-validate",
                "acct-validate",
                "Validate",
                "custom-http",
                true,
                "",
                "bind-cred-validate",
                "",
            );
            replace_destination_catalog(
                conn,
                "dest-validate",
                &[CatalogModel {
                    public_model: "ocg-facade-validate".into(),
                    upstream_model: "ocg-facade-validate".into(),
                    protocols: vec![UpstreamProtocolKind::ChatCompletions],
                    preferred: Some(UpstreamProtocolKind::ChatCompletions),
                    enabled: true,
                    upstream_override: None,
                }],
            )
            .unwrap();
        },
        |record, _| {
            push_plane(
                record,
                RoutePlane::Applied,
                "validate-auth",
                "cred-validate",
                "bind-cred-validate",
                15,
                filled_route("ocg-facade-validate", "ocg-facade-validate", true),
            );
            push_plane(
                record,
                RoutePlane::Desired,
                "validate-auth",
                "cred-validate",
                "bind-cred-validate",
                15,
                filled_route("ocg-facade-validate", "ocg-facade-validate", false),
            );
        },
    );
    let facts =
        explain_owned_routes(world.state(), "ocg-facade-validate", PROTOCOL, clock()).unwrap();
    assert!(!facts.runtime.state_changed);
    assert!(facts.resolution.known);
    assert!(facts.resolution.mappings.iter().any(|mapping| {
        mapping.destination_id == "dest-validate"
            && !mapping.routeable
            && !mapping.migration_required
    }));
    let applied = route(&facts, RoutePlane::Applied, "cred-validate");
    assert!(applied.validation_only);
    assert!(!applied.client_configuration_eligible);
    let desired = route(&facts, RoutePlane::Desired, "cred-validate");
    assert!(!desired.validation_only);
    assert!(!desired.client_configuration_eligible);
}

#[test]
fn omitted_remote_catalog_keeps_migration_and_is_not_routeable() {
    let world = open_world(standard_options());
    let revision = world.state().settings_revision();
    {
        let db = world.state().db.lock();
        insert_lane(
            &db.conn,
            "dest-omitted-remote",
            "Omitted Remote",
            AdapterKind::Cpa,
            AuthScheme::Bearer,
            true,
            Some("https://omitted.example"),
            "cred-omitted-remote",
            "acct-omitted-remote",
            "Omitted Remote",
            CPA_PROVIDER_ID,
            true,
            "",
            "bind-omitted-remote",
            "",
        );
        replace_destination_catalog(
            &db.conn,
            "dest-omitted-remote",
            &[CatalogModel {
                public_model: "ocg-omitted-remote".into(),
                upstream_model: "ocg-omitted-remote".into(),
                protocols: vec![UpstreamProtocolKind::ChatCompletions],
                preferred: Some(UpstreamProtocolKind::ChatCompletions),
                enabled: true,
                upstream_override: None,
            }],
        )
        .unwrap();
    }
    let facts =
        explain_owned_routes(world.state(), "ocg-omitted-remote", PROTOCOL, clock()).unwrap();
    assert!(!facts.runtime.state_changed);
    assert!(facts.resolution.known);
    assert!(!facts.resolution.ambiguous);
    assert!(facts.resolution.mappings.iter().any(|mapping| {
        mapping.destination_id == "dest-omitted-remote"
            && mapping.provider_id == CPA_PROVIDER_ID
            && !mapping.routeable
            && mapping.migration_required
            && mapping.adapter_kind == AdapterKind::Cpa.as_str()
    }));
    assert!(
        facts
            .applied
            .iter()
            .chain(facts.desired.iter())
            .all(|row| row.destination_id != "dest-omitted-remote")
    );
    let native = route(&facts, RoutePlane::Applied, "cred-native");
    assert!(!native.migration_required);
    assert_eq!(native.historical_placement, HistoricalPlacement::OwnedPool);
    assert_eq!(world.state().settings_revision(), revision);
    let base: String = {
        let db = world.state().db.lock();
        db.conn
            .query_row(
                "SELECT base_url FROM destinations WHERE id = 'dest-omitted-remote'",
                [],
                |row| row.get(0),
            )
            .unwrap()
    };
    assert_eq!(base, "https://omitted.example");
}

#[test]
fn real_owned_native_storage_is_one_mapping() {
    let world = open_world(standard_options());
    {
        let db = world.state().db.lock();
        let destination_id = native_binding::ensure_owned_destination(&db.conn).unwrap();
        native_binding::bind_native_account(&db.conn, "codex", "owned-facade.json").unwrap();
        let inserted = native_binding::insert_native_models_if_new(
            &db.conn,
            &destination_id,
            &[NativeModelInsert {
                public_model: "ocg-owned-native-pin".into(),
                upstream_model: "ocg-owned-native-pin".into(),
                protocols: vec!["chat_completions".into()],
            }],
        )
        .unwrap();
        assert_eq!(inserted, 1);
    }
    let facts =
        explain_owned_routes(world.state(), "ocg-owned-native-pin", PROTOCOL, clock()).unwrap();
    assert!(facts.resolution.known);
    assert!(!facts.resolution.ambiguous);
    assert_eq!(facts.resolution.kind, Some(QueryResolutionKind::PinnedRaw));
    assert!(facts.resolution.alias.is_none());
    assert_eq!(facts.resolution.mappings.len(), 1);
    let mapping = &facts.resolution.mappings[0];
    assert_eq!(mapping.provider_id, CPA_PROVIDER_ID);
    assert_ne!(mapping.provider_id, OWNED_NATIVE_LEGACY_ID);
    assert!(!mapping.routeable);
    assert!(!mapping.migration_required);
    assert_eq!(mapping.adapter_kind, AdapterKind::Cpa.as_str());
}

#[test]
fn historical_remote_is_excluded_without_rewriting_native_or_http() {
    let mut options = standard_options();
    options.historical_remote = true;
    let world = open_world(options);
    let settings = settings_snapshot(&world);
    let ciphers = cipher_snapshot(&world);
    let logs = log_count(&world);
    let revision = world.state().settings_revision();
    let base: String = {
        let db = world.state().db.lock();
        db.conn
            .query_row(
                "SELECT base_url FROM destinations WHERE id = 'dest-remote'",
                [],
                |row| row.get(0),
            )
            .unwrap()
    };
    let facts = world.explain();
    let remote = route(&facts, RoutePlane::Applied, "cred-remote");
    assert_eq!(remote.historical_placement, HistoricalPlacement::Remote);
    assert!(remote.migration_required);
    assert!(!remote.client_configuration_eligible);
    assert!(facts.resolution.mappings.iter().any(|mapping| {
        mapping.destination_id == "dest-remote" && mapping.migration_required && !mapping.routeable
    }));
    let native = route(&facts, RoutePlane::Applied, "cred-native");
    assert_eq!(native.historical_placement, HistoricalPlacement::OwnedPool);
    assert!(!native.migration_required);
    assert!(native.client_configuration_eligible);
    assert_eq!(native.posture, RoutePosture::Client);
    let http = route(&facts, RoutePlane::Applied, "cred-http");
    assert_eq!(
        http.historical_placement,
        HistoricalPlacement::NotApplicable
    );
    assert!(!http.migration_required);
    assert_eq!(http.adapter_kind, AdapterKind::Http.as_str());
    assert_eq!(settings_snapshot(&world), settings);
    assert_eq!(cipher_snapshot(&world), ciphers);
    assert_eq!(log_count(&world), logs);
    assert_eq!(world.state().settings_revision(), revision);
    let base_after: String = {
        let db = world.state().db.lock();
        db.conn
            .query_row(
                "SELECT base_url FROM destinations WHERE id = 'dest-remote'",
                [],
                |row| row.get(0),
            )
            .unwrap()
    };
    assert_eq!(base_after, base);
    assert_eq!(base, "https://historical.example");
}

fn credential_restriction(
    observation_id: &str,
    window: Window,
    source: EvidenceSource,
    reset: Reset,
) -> PolicyDocument {
    PolicyDocument {
        restrictions: vec![Restriction {
            scope: Scope {
                subject: Subject::Credential {
                    credential_id: "cred-native".into(),
                    credential_version: 3,
                    provider_id: CPA_PROVIDER_ID.into(),
                    binding_id: "bind-native".into(),
                },
                public_model: Some(MODEL.into()),
            },
            window,
            reset,
            observed_at: clock(),
            observation_id: observation_id.into(),
            source,
            recovery: None,
        }],
        attempts: Vec::new(),
    }
}

#[test]
fn captured_response_meta_stays_with_the_read_after_config_mutation() {
    let world = open_world(standard_options());
    let before_revision = world.state().settings_revision();
    let before_generation = world.state().process_generation();
    let before_pricing = world.state().pricing_snapshot().revision.clone();
    let before_mode = world.state().config().routing_mode;
    let before_sticky = world.state().config().conversation_sticky;
    let facts = world.explain();
    assert_eq!(world.state().settings_revision(), before_revision);
    assert_eq!(facts.captured.settings_revision, before_revision);
    assert_eq!(facts.captured.process_generation, before_generation);
    assert_eq!(facts.captured.pricing_revision, before_pricing);
    assert_eq!(facts.captured.routing_mode, before_mode);
    assert_eq!(facts.captured.conversation_sticky, before_sticky);

    let mut config = world.state().config();
    if config.gateway_key.trim().is_empty() {
        config.gateway_key = "routing-explain-gateway-key".into();
    }
    config.routing_mode = if before_mode == RoutingMode::RoundRobin {
        RoutingMode::StrictPriority
    } else {
        RoutingMode::RoundRobin
    };
    config.conversation_sticky = !before_sticky;
    world.state().set_config(config).unwrap();
    assert!(world.state().settings_revision() > before_revision);
    assert_ne!(world.state().config().routing_mode, before_mode);
    assert_eq!(facts.captured.settings_revision, before_revision);
    assert_eq!(facts.captured.process_generation, before_generation);
    assert_eq!(facts.captured.pricing_revision, before_pricing);
    assert_eq!(facts.captured.routing_mode, before_mode);
    assert_eq!(facts.captured.conversation_sticky, before_sticky);
}

#[test]
fn control_scalar_drift_is_revision_or_generation() {
    assert!(!super::control_scalars_drifted(3, 3, 8, 8));
    assert!(super::control_scalars_drifted(3, 4, 8, 8));
    assert!(super::control_scalars_drifted(3, 3, 8, 9));
    assert!(super::control_scalars_drifted(3, 4, 8, 9));
}
