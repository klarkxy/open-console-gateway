use super::super::store::{AuthStamp, OAuthPresence, OAuthStamp, Record};
use super::{
    ConfigAuthorityQuery, ConfigExclusion, ConfigPlane, MaterialPosture, NativeGrantDisposition,
    ProductChannel, StaticPosture, configuration_authority_on,
};
use crate::cpa_policy::Reason;
use crate::cpa_projection::{
    CredentialRouteSet, NATIVE_ENDPOINT_PIN_CAPABILITY, NativeAuthorityFacts, NormalizedRoute,
    native_route_targets,
};
use ocg_domain::ids::{COMMAND_CODE_PROVIDER_ID, CPA_PROVIDER_ID, OPENCODE_ZEN_FREE_PROVIDER_ID};
use rusqlite::{Connection, params};

const SCOPE_ALL: &str = r#"{"kind":"all"}"#;

fn schema(conn: &Connection) {
    conn.execute_batch(
        "CREATE TABLE destinations (
            id TEXT PRIMARY KEY,
            legacy_kind TEXT NOT NULL,
            legacy_id TEXT NOT NULL,
            adapter TEXT NOT NULL,
            name TEXT NOT NULL,
            brand_family TEXT,
            base_url TEXT,
            protocols_json TEXT NOT NULL,
            auth_scheme TEXT NOT NULL,
            model_resolution TEXT NOT NULL,
            capabilities_json TEXT NOT NULL,
            plan_json TEXT,
            max_credentials INTEGER,
            observer_credential_id TEXT,
            enabled INTEGER NOT NULL,
            onboarding_draft INTEGER NOT NULL
        );
        CREATE TABLE destination_models (
            destination_id TEXT NOT NULL,
            public_model TEXT NOT NULL,
            upstream_model TEXT NOT NULL,
            protocols_json TEXT NOT NULL,
            preferred TEXT,
            enabled INTEGER NOT NULL,
            upstream_override TEXT
        );
        CREATE TABLE credentials (
            id TEXT PRIMARY KEY,
            provider_id TEXT NOT NULL,
            destination_id TEXT NOT NULL,
            binding_id TEXT NOT NULL,
            credential_version INTEGER NOT NULL,
            enabled INTEGER NOT NULL,
            binding_enabled INTEGER NOT NULL,
            setup_step TEXT NOT NULL,
            account_type TEXT NOT NULL,
            key_cipher TEXT NOT NULL,
            scope_json TEXT NOT NULL,
            authorization_connection_id TEXT NOT NULL
        );
        CREATE TABLE credential_grants (
            credential_id TEXT NOT NULL,
            kind TEXT NOT NULL,
            value TEXT NOT NULL
        );",
    )
    .unwrap();
}

fn capabilities() -> String {
    serde_json::json!({
        "testable": true,
        "discoverable_models": false,
        "official_balance_probe": [],
        "observer": false,
        "managed_signup": false,
        "external_integration": false,
        "billing_tier_required": false,
        "redirect_policy": "no_follow",
        "identity_headers": false
    })
    .to_string()
}

fn insert_destination(
    conn: &Connection,
    id: &str,
    adapter: &str,
    legacy_id: &str,
    auth_scheme: &str,
    enabled: i64,
) {
    conn.execute(
        "INSERT INTO destinations (
            id, legacy_kind, legacy_id, adapter, name, brand_family, base_url,
            protocols_json, auth_scheme, model_resolution, capabilities_json, plan_json,
            max_credentials, observer_credential_id, enabled, onboarding_draft
         ) VALUES (
            ?1, 'builtin', ?2, ?3, ?1, NULL, NULL,
            '[\"chat_completions\"]', ?4, 'adapter_defined', ?5, NULL,
            NULL, NULL, ?6, 0
         )",
        params![id, legacy_id, adapter, auth_scheme, capabilities(), enabled],
    )
    .unwrap();
}

fn insert_model(conn: &Connection, destination_id: &str, model: &str) {
    conn.execute(
        "INSERT INTO destination_models (
            destination_id, public_model, upstream_model, protocols_json, preferred, enabled,
            upstream_override
         ) VALUES (?1, ?2, ?2, '[\"chat_completions\"]', NULL, 1, NULL)",
        params![destination_id, model],
    )
    .unwrap();
}

fn insert_credential(
    conn: &Connection,
    id: &str,
    provider_id: &str,
    destination_id: &str,
    binding_id: &str,
    version: i64,
    key_cipher: &str,
    scope_json: &str,
) {
    insert_linked_credential(
        conn,
        id,
        provider_id,
        destination_id,
        binding_id,
        version,
        key_cipher,
        scope_json,
        "",
    );
}

fn insert_linked_credential(
    conn: &Connection,
    id: &str,
    provider_id: &str,
    destination_id: &str,
    binding_id: &str,
    version: i64,
    key_cipher: &str,
    scope_json: &str,
    connection_id: &str,
) {
    conn.execute(
        "INSERT INTO credentials (
            id, provider_id, destination_id, binding_id, credential_version, enabled,
            binding_enabled, setup_step, account_type, key_cipher, scope_json,
            authorization_connection_id
         ) VALUES (?1, ?2, ?3, ?4, ?5, 1, 1, 'ready', 'key', ?6, ?7, ?8)",
        params![
            id,
            provider_id,
            destination_id,
            binding_id,
            version,
            key_cipher,
            scope_json,
            connection_id
        ],
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

fn route(model: &str, protocol: &str, endpoint_id: &str, origin: &str) -> NormalizedRoute {
    NormalizedRoute {
        public_model: model.into(),
        upstream_model: model.into(),
        protocol: protocol.into(),
        endpoint_id: endpoint_id.into(),
        origin: origin.into(),
        endpoint_fingerprint: "ab".repeat(32),
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
        material_fingerprint: "cd".repeat(32),
        routing_rank: rank,
        routes,
        fingerprint: "ef".repeat(32),
    }
}

fn auth_stamp(
    auth_id: &str,
    credential_id: &str,
    version: u64,
    binding_id: &str,
    provider_id: &str,
    epoch: u64,
) -> AuthStamp {
    AuthStamp {
        auth_id: auth_id.into(),
        credential_id: credential_id.into(),
        credential_version: version,
        binding_id: binding_id.into(),
        material_revision: "rev".into(),
        provider_id: provider_id.into(),
        registration_epoch: epoch,
    }
}

fn query(model: &str, protocol: &str) -> ConfigAuthorityQuery {
    ConfigAuthorityQuery {
        public_model: model.into(),
        callable_protocol: protocol.into(),
    }
}

fn open() -> Connection {
    let conn = Connection::open_in_memory().unwrap();
    schema(&conn);
    conn
}

#[test]
fn http_none_keyed_unchecked_and_go_free_channels() {
    let conn = open();
    insert_destination(
        &conn,
        "zen-dest",
        "zen",
        OPENCODE_ZEN_FREE_PROVIDER_ID,
        "none",
        1,
    );
    insert_destination(
        &conn,
        "goat-dest",
        "goat",
        COMMAND_CODE_PROVIDER_ID,
        "bearer",
        1,
    );
    insert_destination(&conn, "http-dest", "http", "custom-http", "api_key", 1);
    insert_destination(&conn, "bad-none-dest", "http", "custom-http", "none", 1);
    insert_credential(
        &conn,
        "zen-cid",
        OPENCODE_ZEN_FREE_PROVIDER_ID,
        "zen-dest",
        "bind-zen",
        3,
        "",
        SCOPE_ALL,
    );
    insert_credential(
        &conn,
        "goat-cid",
        COMMAND_CODE_PROVIDER_ID,
        "goat-dest",
        "bind-goat",
        3,
        "sealed-ciphertext",
        SCOPE_ALL,
    );
    insert_credential(
        &conn,
        "http-cid",
        "custom-http",
        "http-dest",
        "bind-http",
        3,
        "another-ciphertext",
        SCOPE_ALL,
    );
    insert_credential(
        &conn,
        "bad-none",
        "custom-http",
        "bad-none-dest",
        "bind-bad",
        3,
        "not-empty-cipher",
        SCOPE_ALL,
    );
    let mut record = Record::empty();
    record.applied_auth = vec![
        auth_stamp(
            "auth-zen",
            "zen-cid",
            3,
            "bind-zen",
            OPENCODE_ZEN_FREE_PROVIDER_ID,
            1,
        ),
        auth_stamp(
            "auth-goat",
            "goat-cid",
            3,
            "bind-goat",
            COMMAND_CODE_PROVIDER_ID,
            1,
        ),
        auth_stamp("auth-http", "http-cid", 3, "bind-http", "custom-http", 1),
        auth_stamp("auth-bad", "bad-none", 3, "bind-bad", "custom-http", 1),
    ];
    record.applied_routes = vec![
        route_set(
            "auth-zen",
            "zen-cid",
            3,
            "bind-zen",
            1,
            vec![route(
                "zen-model",
                "chat_completions",
                "ep-zen",
                "https://opencode.ai",
            )],
        ),
        route_set(
            "auth-goat",
            "goat-cid",
            3,
            "bind-goat",
            2,
            vec![route(
                "goat-model",
                "chat_completions",
                "ep-goat",
                "https://goat.example",
            )],
        ),
        route_set(
            "auth-http",
            "http-cid",
            3,
            "bind-http",
            3,
            vec![route(
                "http-model",
                "chat_completions",
                "ep-http",
                "https://http.example",
            )],
        ),
        route_set(
            "auth-bad",
            "bad-none",
            3,
            "bind-bad",
            4,
            vec![route(
                "bad-model",
                "chat_completions",
                "ep-bad",
                "https://bad.example",
            )],
        ),
    ];
    let tx = conn.unchecked_transaction().unwrap();
    let proofs = configuration_authority_on(
        &tx,
        &record,
        ConfigPlane::Applied,
        &query("unasked", "chat_completions"),
    )
    .unwrap();
    let zen = proofs
        .iter()
        .find(|proof| proof.credential_id == "zen-cid")
        .unwrap();
    let goat = proofs
        .iter()
        .find(|proof| proof.credential_id == "goat-cid")
        .unwrap();
    let http = proofs
        .iter()
        .find(|proof| proof.credential_id == "http-cid")
        .unwrap();
    let bad = proofs
        .iter()
        .find(|proof| proof.credential_id == "bad-none")
        .unwrap();
    assert_eq!(zen.channel, ProductChannel::Free);
    assert_eq!(zen.material, MaterialPosture::HttpNone);
    assert!(!zen.secret_recheck_pending);
    assert!(zen.native_operations.is_empty());
    assert_eq!(goat.channel, ProductChannel::Go);
    assert_eq!(goat.material, MaterialPosture::KeyedUnchecked);
    assert!(goat.secret_recheck_pending);
    assert!(goat.caller_pending && goat.send_pending);
    assert_eq!(http.channel, ProductChannel::Go);
    assert_eq!(http.material, MaterialPosture::KeyedUnchecked);
    assert_eq!(bad.material, MaterialPosture::Unproven);
    assert!(bad.exclusions.contains(&ConfigExclusion::Material));
    assert_ne!(bad.material, MaterialPosture::HttpNone);
    let rendered = format!("{goat:?}");
    assert!(!rendered.contains("sealed-ciphertext"));
    assert!(!rendered.contains("cdcdcd"));
}

#[test]
fn applied_route_keeps_structured_authority_when_absent_from_desired() {
    let conn = open();
    insert_destination(&conn, "old-dest", "http", "custom-http", "bearer", 1);
    insert_credential(
        &conn,
        "old-cid",
        "custom-http",
        "old-dest",
        "bind-old",
        4,
        "old-ciphertext",
        r#"{"kind":"only","models":["other"]}"#,
    );
    let retired = route_set(
        "auth-old",
        "old-cid",
        3,
        "bind-old",
        7,
        vec![route(
            "asked",
            "chat_completions",
            "ep-retired",
            "https://retired.example",
        )],
    );
    let mut record = Record::empty();
    record.applied_auth = vec![auth_stamp(
        "auth-old",
        "old-cid",
        3,
        "bind-old",
        "custom-http",
        2,
    )];
    record.applied_routes = vec![retired];
    let tx = conn.unchecked_transaction().unwrap();
    let asked = query("asked", "chat_completions");
    let applied = configuration_authority_on(&tx, &record, ConfigPlane::Applied, &asked).unwrap();
    let desired = configuration_authority_on(&tx, &record, ConfigPlane::Desired, &asked).unwrap();
    assert!(desired.is_empty());
    assert_eq!(applied.len(), 1);
    let proof = &applied[0];
    assert_eq!(proof.credential_id, "old-cid");
    assert_eq!(proof.credential_version, 3);
    assert_eq!(proof.current_version, Some(4));
    assert_eq!(proof.endpoint_id, "ep-retired");
    assert_eq!(proof.origin, "https://retired.example");
    assert_eq!(proof.public_model, "asked");
    assert_eq!(proof.routing_rank, 7);
    assert_eq!(proof.registration_epoch, Some(2));
    assert!(proof.exclusions.contains(&ConfigExclusion::Version));
    assert!(proof.exclusions.contains(&ConfigExclusion::Scope));
    assert_eq!(proof.posture, StaticPosture::Excluded);
    assert_eq!(proof.material, MaterialPosture::KeyedUnchecked);
    assert_eq!(proof.channel, ProductChannel::Go);
    assert!(proof.native_operations.is_empty());
}

#[test]
fn desired_stamp_does_not_prove_the_applied_plane() {
    let conn = open();
    insert_destination(
        &conn,
        "plane-dest",
        "goat",
        COMMAND_CODE_PROVIDER_ID,
        "bearer",
        1,
    );
    insert_credential(
        &conn,
        "plane-cid",
        COMMAND_CODE_PROVIDER_ID,
        "plane-dest",
        "bind-plane",
        3,
        "plane-ciphertext",
        SCOPE_ALL,
    );
    let set = route_set(
        "auth-plane",
        "plane-cid",
        3,
        "bind-plane",
        1,
        vec![route(
            "plane-model",
            "chat_completions",
            "ep-plane",
            "https://plane.example",
        )],
    );
    let mut record = Record::empty();
    record.desired_auth = vec![auth_stamp(
        "auth-plane",
        "plane-cid",
        3,
        "bind-plane",
        COMMAND_CODE_PROVIDER_ID,
        5,
    )];
    record.desired_routes = vec![set.clone()];
    record.applied_routes = vec![set];
    let tx = conn.unchecked_transaction().unwrap();
    let asked = query("plane-model", "chat_completions");
    let desired = configuration_authority_on(&tx, &record, ConfigPlane::Desired, &asked).unwrap();
    let applied = configuration_authority_on(&tx, &record, ConfigPlane::Applied, &asked).unwrap();
    assert!(!desired[0].exclusions.contains(&ConfigExclusion::Identity));
    assert_eq!(desired[0].registration_epoch, Some(5));
    assert!(applied[0].exclusions.contains(&ConfigExclusion::Identity));
    assert_eq!(applied[0].registration_epoch, None);
    assert_eq!(applied[0].endpoint_id, "ep-plane");
    assert_eq!(applied[0].posture, StaticPosture::Excluded);
}

#[test]
fn empty_native_targets_are_not_local_count() {
    let conn = open();
    insert_destination(&conn, "cpa-dest", "cpa", CPA_PROVIDER_ID, "bearer", 1);
    insert_model(&conn, "cpa-dest", "codex-test");
    insert_credential(
        &conn,
        "native-cid",
        CPA_PROVIDER_ID,
        "cpa-dest",
        "bind-native",
        3,
        "",
        SCOPE_ALL,
    );
    let mut record = Record::empty();
    record.host_capabilities = vec![NATIVE_ENDPOINT_PIN_CAPABILITY.into()];
    record.applied_auth = vec![auth_stamp(
        "native-auth",
        "native-cid",
        3,
        "bind-native",
        CPA_PROVIDER_ID,
        4,
    )];
    record.oauth.push(oauth_stamp(OAuthPresence::Present));
    let mut matched = route(
        "codex-test",
        "chat_completions",
        "ep-empty",
        "https://chatgpt.com",
    );
    matched.native_targets.clear();
    let mut other = route("codex-test", "responses", "ep-other", "https://chatgpt.com");
    other.native_targets.clear();
    record.applied_routes = vec![route_set(
        "native-auth",
        "native-cid",
        3,
        "bind-native",
        1,
        vec![matched, other],
    )];
    let tx = conn.unchecked_transaction().unwrap();
    let proofs = configuration_authority_on(
        &tx,
        &record,
        ConfigPlane::Applied,
        &query("codex-test", "chat_completions"),
    )
    .unwrap();
    let chat = proofs
        .iter()
        .find(|proof| proof.protocol == "chat_completions")
        .unwrap();
    let responses = proofs
        .iter()
        .find(|proof| proof.protocol == "responses")
        .unwrap();
    assert_eq!(chat.material, MaterialPosture::NativePresent);
    assert_eq!(chat.channel, ProductChannel::Go);
    assert_eq!(chat.native_provider, "codex");
    assert!(chat.capability_listed);
    assert_eq!(
        disposition(chat, "execute"),
        NativeGrantDisposition::Unavailable
    );
    assert_eq!(
        disposition(chat, "stream"),
        NativeGrantDisposition::Unavailable
    );
    assert_eq!(
        disposition(chat, "count-tokens"),
        NativeGrantDisposition::LocalOnly
    );
    assert_eq!(
        disposition(responses, "count-tokens"),
        NativeGrantDisposition::Unavailable
    );
    assert!(responses.exclusions.contains(&ConfigExclusion::Protocol));
    let rendered = format!("{chat:?}");
    assert!(!rendered.contains("token.json"));
}

#[test]
fn pending_native_is_unproven_and_not_local_count() {
    let conn = open();
    insert_destination(&conn, "cpa-dest", "cpa", CPA_PROVIDER_ID, "bearer", 1);
    insert_credential(
        &conn,
        "native-cid",
        CPA_PROVIDER_ID,
        "cpa-dest",
        "bind-native",
        3,
        "",
        SCOPE_ALL,
    );
    let mut record = Record::empty();
    record.host_capabilities = vec![NATIVE_ENDPOINT_PIN_CAPABILITY.into()];
    record.applied_auth = vec![auth_stamp(
        "native-auth",
        "native-cid",
        3,
        "bind-native",
        CPA_PROVIDER_ID,
        4,
    )];
    record.oauth.push(oauth_stamp(OAuthPresence::Pending));
    record.applied_routes = vec![route_set(
        "native-auth",
        "native-cid",
        3,
        "bind-native",
        1,
        vec![route(
            "codex-test",
            "chat_completions",
            "ep-pending",
            "https://chatgpt.com",
        )],
    )];
    let tx = conn.unchecked_transaction().unwrap();
    let proof = &configuration_authority_on(
        &tx,
        &record,
        ConfigPlane::Applied,
        &query("codex-test", "chat_completions"),
    )
    .unwrap()[0];
    assert_eq!(proof.material, MaterialPosture::Unproven);
    assert!(proof.exclusions.contains(&ConfigExclusion::NativePresence));
    assert_eq!(
        disposition(proof, "count-tokens"),
        NativeGrantDisposition::Unavailable
    );
    assert_ne!(proof.posture, StaticPosture::Client);
}

#[test]
fn native_present_face_uses_only_the_selected_plane() {
    let facts = NativeAuthorityFacts {
        raw_label: String::new(),
        provider: "codex".into(),
        mode: String::new(),
        reported_base: String::new(),
    };
    let (primary, targets) = native_route_targets(
        &facts,
        "codex-test",
        "chat_completions",
        &super::owned_native_connection(),
    )
    .expect("canonical codex targets");
    let conn = open();
    insert_destination(&conn, "cpa-dest", "cpa", CPA_PROVIDER_ID, "bearer", 1);
    insert_model(&conn, "cpa-dest", "codex-test");
    insert_credential(
        &conn,
        "native-cid",
        CPA_PROVIDER_ID,
        "cpa-dest",
        "bind-native",
        3,
        "",
        SCOPE_ALL,
    );
    grant(&conn, "native-cid", "endpoint_id", &primary.endpoint_id);
    grant(&conn, "native-cid", "origin", &primary.origin);
    let mut native_route = route(
        "codex-test",
        "chat_completions",
        &primary.endpoint_id,
        &primary.origin,
    );
    native_route.endpoint_fingerprint = primary.endpoint_fingerprint.clone();
    native_route.native_targets = targets;
    let set = route_set(
        "native-auth",
        "native-cid",
        3,
        "bind-native",
        1,
        vec![native_route],
    );
    let mut record = Record::empty();
    record.host_capabilities = vec![NATIVE_ENDPOINT_PIN_CAPABILITY.into()];
    record.applied_auth = vec![auth_stamp(
        "native-auth",
        "native-cid",
        3,
        "bind-native",
        CPA_PROVIDER_ID,
        4,
    )];
    record.applied_routes = vec![set.clone()];
    record.desired_routes = vec![set];
    record.oauth.push(oauth_stamp(OAuthPresence::Present));
    let tx = conn.unchecked_transaction().unwrap();
    let row = super::load_admission(&tx, "native-cid").unwrap().unwrap();
    let stored = &record.applied_routes[0].routes[0];
    assert!(
        super::confirm_native_face(&tx, &record, &record.applied_auth, &row, stored)
            .unwrap()
            .is_none()
    );
    assert_eq!(
        super::confirm_native_face(&tx, &record, &record.desired_auth, &row, stored).unwrap(),
        Some(Reason::IdentityFence)
    );
    let asked = query("codex-test", "chat_completions");
    let applied = configuration_authority_on(&tx, &record, ConfigPlane::Applied, &asked).unwrap();
    assert_eq!(applied[0].posture, StaticPosture::Client);
    assert_eq!(applied[0].material, MaterialPosture::NativePresent);
    assert!(applied[0].grants_cover);
    assert!(applied[0].caller_pending && applied[0].send_pending);
    assert!(!applied[0].secret_recheck_pending);
    assert!(matches!(
        disposition(&applied[0], "execute"),
        NativeGrantDisposition::Granted { .. }
    ));
    assert_eq!(
        disposition(&applied[0], "count-tokens"),
        NativeGrantDisposition::LocalOnly
    );
    let desired = configuration_authority_on(&tx, &record, ConfigPlane::Desired, &asked).unwrap();
    assert!(desired[0].exclusions.contains(&ConfigExclusion::Identity));
    assert_ne!(desired[0].posture, StaticPosture::Client);
    let mut validation = record.clone();
    validation.applied_routes[0].routes[0].validation_only = true;
    let validation_proof =
        &configuration_authority_on(&tx, &validation, ConfigPlane::Applied, &asked).unwrap()[0];
    assert!(validation_proof.validation_only);
    assert_eq!(validation_proof.posture, StaticPosture::ValidationOnly);
}

fn oauth_stamp(presence: OAuthPresence) -> OAuthStamp {
    OAuthStamp {
        relative_path: "codex/token.json".into(),
        auth_id: "native-auth".into(),
        credential_id: "native-cid".into(),
        credential_version: 3,
        material_revision: "rev".into(),
        provider_id: CPA_PROVIDER_ID.into(),
        native_provider: "codex".into(),
        registration_epoch: 4,
        models: vec!["codex-test".into()],
        presence,
        recovery: String::new(),
        raw_provider_label: String::new(),
        native_mode: String::new(),
        reported_base: String::new(),
    }
}

fn goat_client_route(conn: &Connection, credential_id: &str) -> NormalizedRoute {
    let tx = conn.unchecked_transaction().unwrap();
    let row = super::load_admission(&tx, credential_id).unwrap().unwrap();
    let destination = super::load_destination(&tx, &row.destination_id)
        .unwrap()
        .unwrap();
    let model = super::catalog_row(&destination.catalog, "goat-model").unwrap();
    let protocol = super::protocol_kind("chat_completions").unwrap();
    let url = super::face_url(&destination, model, protocol, &row.provider_id).expect("goat face");
    let origin = super::face_origin(&url).unwrap();
    let fingerprint = crate::cpa_projection::endpoint_fingerprint(&url);
    let endpoint_id = super::endpoint_identity(
        &destination,
        model,
        protocol,
        &row.provider_id,
        &row.connection_id,
    )
    .expect("goat endpoint");
    grant(&tx, credential_id, "endpoint_id", &endpoint_id);
    grant(&tx, credential_id, "origin", &origin);
    tx.commit().unwrap();
    let mut built = route("goat-model", "chat_completions", &endpoint_id, &origin);
    built.endpoint_fingerprint = fingerprint;
    built
}

fn insert_goat(conn: &Connection, credential_id: &str, binding_id: &str) {
    insert_destination(
        conn,
        "goat-dest",
        "goat",
        COMMAND_CODE_PROVIDER_ID,
        "bearer",
        1,
    );
    insert_model(conn, "goat-dest", "goat-model");
    insert_linked_credential(
        conn,
        credential_id,
        COMMAND_CODE_PROVIDER_ID,
        "goat-dest",
        binding_id,
        3,
        "sealed-ciphertext",
        SCOPE_ALL,
        "goat-connection",
    );
}

fn codex_pins() -> (
    crate::cpa_projection::NativeEndpointPin,
    Vec<crate::cpa_projection::NativeDispatchTarget>,
) {
    let facts = NativeAuthorityFacts {
        raw_label: String::new(),
        provider: "codex".into(),
        mode: String::new(),
        reported_base: String::new(),
    };
    native_route_targets(
        &facts,
        "codex-test",
        "chat_completions",
        &super::owned_native_connection(),
    )
    .expect("canonical codex targets")
}

fn codex_route_with_grants(conn: &Connection, credential_id: &str) -> NormalizedRoute {
    let (primary, targets) = codex_pins();
    grant(conn, credential_id, "endpoint_id", &primary.endpoint_id);
    grant(conn, credential_id, "origin", &primary.origin);
    let mut built = route(
        "codex-test",
        "chat_completions",
        &primary.endpoint_id,
        &primary.origin,
    );
    built.endpoint_fingerprint = primary.endpoint_fingerprint;
    built.native_targets = targets;
    built
}

fn assert_only_identity(proof: &super::RouteAuthorityProof) {
    assert_eq!(proof.exclusions, vec![ConfigExclusion::Identity]);
    assert_eq!(proof.posture, StaticPosture::Excluded);
}

fn disposition(proof: &super::RouteAuthorityProof, kind: &str) -> NativeGrantDisposition {
    proof
        .native_operations
        .iter()
        .find(|fact| fact.generation_kind == kind)
        .unwrap()
        .disposition
        .clone()
}

#[test]
fn single_stamp_http_route_is_client() {
    let conn = open();
    insert_goat(&conn, "goat-cid", "bind-goat");
    let built = goat_client_route(&conn, "goat-cid");
    let mut record = Record::empty();
    record.applied_auth = vec![auth_stamp(
        "auth-goat",
        "goat-cid",
        3,
        "bind-goat",
        COMMAND_CODE_PROVIDER_ID,
        4,
    )];
    record.applied_routes = vec![route_set(
        "auth-goat",
        "goat-cid",
        3,
        "bind-goat",
        2,
        vec![built],
    )];
    let tx = conn.unchecked_transaction().unwrap();
    let proof = &configuration_authority_on(
        &tx,
        &record,
        ConfigPlane::Applied,
        &query("goat-model", "chat_completions"),
    )
    .unwrap()[0];
    assert!(proof.exclusions.is_empty());
    assert_eq!(proof.posture, StaticPosture::Client);
    assert_eq!(proof.channel, ProductChannel::Go);
    assert_eq!(proof.material, MaterialPosture::KeyedUnchecked);
    assert!(proof.secret_recheck_pending);
    assert!(proof.caller_pending && proof.send_pending);
    assert_eq!(proof.registration_epoch, Some(4));
    assert_eq!(proof.routing_rank, 2);
    assert_eq!(proof.public_model, "goat-model");
    assert!(!proof.endpoint_id.is_empty());
    assert!(proof.native_operations.is_empty());
}

#[test]
fn competing_auth_ids_exclude_desired_and_applied_http_and_native() {
    let conn = open();
    insert_goat(&conn, "goat-cid", "bind-goat");
    let goat = goat_client_route(&conn, "goat-cid");
    let goat_endpoint = goat.endpoint_id.clone();
    insert_destination(&conn, "cpa-dest", "cpa", CPA_PROVIDER_ID, "bearer", 1);
    insert_model(&conn, "cpa-dest", "codex-test");
    insert_credential(
        &conn,
        "native-cid",
        CPA_PROVIDER_ID,
        "cpa-dest",
        "bind-native",
        3,
        "",
        SCOPE_ALL,
    );
    let native = codex_route_with_grants(&conn, "native-cid");
    let native_endpoint = native.endpoint_id.clone();
    let goat_set = route_set("auth-a", "goat-cid", 3, "bind-goat", 2, vec![goat]);
    let native_set = route_set(
        "native-auth",
        "native-cid",
        3,
        "bind-native",
        6,
        vec![native],
    );
    let mut record = Record::empty();
    record.host_capabilities = vec![NATIVE_ENDPOINT_PIN_CAPABILITY.into()];
    record.oauth.push(oauth_stamp(OAuthPresence::Present));
    let goat_stamps = vec![
        auth_stamp(
            "auth-a",
            "goat-cid",
            3,
            "bind-goat",
            COMMAND_CODE_PROVIDER_ID,
            4,
        ),
        auth_stamp(
            "auth-b",
            "goat-cid",
            3,
            "bind-goat",
            COMMAND_CODE_PROVIDER_ID,
            11,
        ),
    ];
    let native_stamps = vec![
        auth_stamp(
            "native-auth",
            "native-cid",
            3,
            "bind-native",
            CPA_PROVIDER_ID,
            4,
        ),
        auth_stamp(
            "other-auth",
            "native-cid",
            3,
            "bind-native",
            CPA_PROVIDER_ID,
            11,
        ),
    ];
    record.desired_auth = [goat_stamps.clone(), native_stamps.clone()].concat();
    record.applied_auth = [goat_stamps, native_stamps].concat();
    record.desired_routes = vec![goat_set.clone(), native_set.clone()];
    record.applied_routes = vec![goat_set, native_set];
    let tx = conn.unchecked_transaction().unwrap();
    for plane in [ConfigPlane::Desired, ConfigPlane::Applied] {
        let http = configuration_authority_on(
            &tx,
            &record,
            plane,
            &query("goat-model", "chat_completions"),
        )
        .unwrap();
        assert_eq!(http.len(), 2);
        let goat_proof = http
            .iter()
            .find(|proof| proof.credential_id == "goat-cid")
            .unwrap();
        assert_only_identity(goat_proof);
        assert_eq!(goat_proof.registration_epoch, Some(4));
        assert_eq!(goat_proof.endpoint_id, goat_endpoint);
        assert_eq!(goat_proof.public_model, "goat-model");
        assert_eq!(goat_proof.credential_version, 3);
        assert_eq!(goat_proof.routing_rank, 2);
        assert_eq!(goat_proof.channel, ProductChannel::Go);
        assert_eq!(goat_proof.material, MaterialPosture::KeyedUnchecked);
        let held = http
            .iter()
            .find(|proof| proof.credential_id == "native-cid")
            .unwrap();
        assert!(held.exclusions.contains(&ConfigExclusion::Identity));
        assert_eq!(held.posture, StaticPosture::Excluded);
        assert_eq!(held.endpoint_id, native_endpoint);
        assert_eq!(held.credential_version, 3);
        assert_eq!(held.routing_rank, 6);

        let native_view = configuration_authority_on(
            &tx,
            &record,
            plane,
            &query("codex-test", "chat_completions"),
        )
        .unwrap();
        let native_proof = native_view
            .iter()
            .find(|proof| proof.credential_id == "native-cid")
            .unwrap();
        assert_only_identity(native_proof);
        assert_eq!(native_proof.registration_epoch, Some(4));
        assert!(native_proof.grants_cover);
        assert_eq!(native_proof.material, MaterialPosture::NativePresent);
        assert_eq!(native_proof.endpoint_id, native_endpoint);
        assert_eq!(native_proof.public_model, "codex-test");
        assert_eq!(native_proof.channel, ProductChannel::Go);
        assert_eq!(native_proof.routing_rank, 6);
    }
}

#[test]
fn same_auth_different_epochs_stay_excluded_on_the_applied_plane() {
    let conn = open();
    insert_goat(&conn, "goat-cid", "bind-goat");
    let built = goat_client_route(&conn, "goat-cid");
    let endpoint = built.endpoint_id.clone();
    let set = route_set("auth-goat", "goat-cid", 3, "bind-goat", 2, vec![built]);
    let mut record = Record::empty();
    record.desired_auth = vec![auth_stamp(
        "auth-goat",
        "goat-cid",
        3,
        "bind-goat",
        COMMAND_CODE_PROVIDER_ID,
        4,
    )];
    record.applied_auth = vec![
        auth_stamp(
            "auth-goat",
            "goat-cid",
            3,
            "bind-goat",
            COMMAND_CODE_PROVIDER_ID,
            4,
        ),
        auth_stamp(
            "auth-goat",
            "goat-cid",
            3,
            "bind-goat",
            COMMAND_CODE_PROVIDER_ID,
            8,
        ),
    ];
    record.desired_routes = vec![set.clone()];
    record.applied_routes = vec![set];
    let tx = conn.unchecked_transaction().unwrap();
    let asked = query("goat-model", "chat_completions");
    let desired =
        &configuration_authority_on(&tx, &record, ConfigPlane::Desired, &asked).unwrap()[0];
    assert!(desired.exclusions.is_empty());
    assert_eq!(desired.posture, StaticPosture::Client);
    assert_eq!(desired.registration_epoch, Some(4));
    let applied =
        &configuration_authority_on(&tx, &record, ConfigPlane::Applied, &asked).unwrap()[0];
    assert_only_identity(applied);
    assert_eq!(applied.registration_epoch, None);
    assert_eq!(applied.endpoint_id, endpoint);
    assert_eq!(applied.public_model, "goat-model");
    assert_eq!(applied.credential_version, 3);
    assert_eq!(applied.routing_rank, 2);
    assert_eq!(applied.channel, ProductChannel::Go);
    assert_eq!(applied.material, MaterialPosture::KeyedUnchecked);
}

#[test]
fn stale_route_stamp_does_not_borrow_another_current_native_stamp() {
    let conn = open();
    insert_destination(&conn, "cpa-dest", "cpa", CPA_PROVIDER_ID, "bearer", 1);
    insert_model(&conn, "cpa-dest", "codex-test");
    insert_credential(
        &conn,
        "native-cid",
        CPA_PROVIDER_ID,
        "cpa-dest",
        "bind-native",
        3,
        "",
        SCOPE_ALL,
    );
    let built = codex_route_with_grants(&conn, "native-cid");
    let endpoint = built.endpoint_id.clone();
    let set = route_set("stale-auth", "native-cid", 3, "bind-native", 6, vec![built]);
    let mut record = Record::empty();
    record.host_capabilities = vec![NATIVE_ENDPOINT_PIN_CAPABILITY.into()];
    let mut oauth = oauth_stamp(OAuthPresence::Present);
    oauth.auth_id = "current-auth".into();
    oauth.registration_epoch = 9;
    record.oauth.push(oauth);
    record.desired_auth = vec![auth_stamp(
        "current-auth",
        "native-cid",
        3,
        "bind-native",
        CPA_PROVIDER_ID,
        9,
    )];
    record.applied_auth = vec![
        auth_stamp(
            "stale-auth",
            "native-cid",
            3,
            "bind-native",
            CPA_PROVIDER_ID,
            1,
        ),
        auth_stamp(
            "current-auth",
            "native-cid",
            3,
            "bind-native",
            CPA_PROVIDER_ID,
            9,
        ),
    ];
    record.desired_routes = vec![set.clone()];
    record.applied_routes = vec![set];
    let tx = conn.unchecked_transaction().unwrap();
    let row = super::load_admission(&tx, "native-cid").unwrap().unwrap();
    let stored = &record.applied_routes[0].routes[0];
    assert!(
        super::confirm_native_face(&tx, &record, &record.applied_auth, &row, stored)
            .unwrap()
            .is_none()
    );
    assert!(
        super::confirm_native_face(&tx, &record, &record.desired_auth, &row, stored)
            .unwrap()
            .is_none()
    );
    let asked = query("codex-test", "chat_completions");
    let desired =
        &configuration_authority_on(&tx, &record, ConfigPlane::Desired, &asked).unwrap()[0];
    assert_only_identity(desired);
    assert_eq!(desired.registration_epoch, None);
    assert!(!desired.grants_cover);
    assert_eq!(desired.endpoint_id, endpoint);
    assert_eq!(desired.material, MaterialPosture::NativePresent);
    let applied =
        &configuration_authority_on(&tx, &record, ConfigPlane::Applied, &asked).unwrap()[0];
    assert_only_identity(applied);
    assert_eq!(applied.registration_epoch, Some(1));
    assert!(!applied.grants_cover);
    assert_eq!(applied.endpoint_id, endpoint);
    assert_eq!(applied.public_model, "codex-test");
    assert_eq!(applied.credential_version, 3);
    assert_eq!(applied.routing_rank, 6);
    assert_eq!(applied.channel, ProductChannel::Go);
    assert_eq!(applied.material, MaterialPosture::NativePresent);
}
