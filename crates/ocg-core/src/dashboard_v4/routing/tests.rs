//! Handler checks for `GET /routing/explain`.
//!
//! These fixtures are not a live Ready host. Handler fixtures leave
//! `verified_ready` false. The selected-mapping input sets that bit only on a
//! detached `RuntimeFacts` value and does not write it onto `CoreState`.
//! The execution record is inserted before `CoreState` opens so the in-memory
//! plane matches the database. Cargo is not run from this leaf.

use super::*;
use crate::cpa_execution::explain::{CapturedResponseMeta, OwnedProjectionFacts, PlaneTuple};
use crate::cpa_policy::{
    EvidenceSource, PolicyDocument, Reset, Restriction, SETTINGS_KEY, Scope, Subject, Window,
};
use crate::cpa_projection::NATIVE_ENDPOINT_PIN_CAPABILITY;
use crate::crypto::{KeyCipher, StaticKeyCipher};
use crate::db::Database;
use crate::db::destination_store::{insert_destination_row, replace_destination_catalog};
use crate::db::native_binding::OWNED_NATIVE_LEGACY_ID;
use crate::state::CoreStateInner;
use axum::Json;
use axum::extract::FromRequestParts;
use axum::http::{Request, StatusCode};
use axum::response::IntoResponse;
use chrono::{DateTime, Utc};
use ocg_domain::catalog::UpstreamProtocolKind;
use ocg_domain::connection::{EndpointOperation, LegacyConnectionKind, connection_id_for_legacy};
use ocg_domain::credential::{RouteSpec, assigned_endpoints_for_routes};
use ocg_domain::destination::{
    AdapterKind, AuthScheme, CatalogModel, Destination, LegacyDestinationRef, ModelResolution,
    sealed_capabilities,
};
use ocg_domain::ids::{COMMAND_CODE_PROVIDER_ID, CPA_PROVIDER_ID, OPENCODE_ZEN_FREE_PROVIDER_ID};
use ocg_domain::protocol::{ApiFormat, command_code_upstream_path};
use ocg_domain::provider::{COMMAND_CODE_GOAT_BASE_URL, builtin_provider};
use rusqlite::{Connection, OptionalExtension, params};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

const EXECUTION_KEY: &str = "cpa_execution_projection_v1";
const DESIRED_DIGEST: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const APPLIED_DIGEST: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
const MATERIAL: &str = "material-fingerprint-must-not-leak";
const SET_FINGERPRINT: &str = "set-fingerprint-must-not-leak";
const OAUTH_PATH: &str = "oauth/must-not-leak.json";
const CIPHER: &str = "sealed-ciphertext";
const STAMP_MATERIAL: &str = "stamp-material-must-not-leak";
const REPORTED_BASE: &str = "https://reported-base-must-not-leak.example";
const LEGACY_WIRE: &[&str] = &[
    "mapping_protocol_incompatible",
    "credential_disabled",
    "binding_disabled",
    "model_scope_denied",
    "goat_not_eligible",
    "goat_unverified",
    "candidate_materialization_failed",
    "production_route_unsupported",
    "account_disabled",
    "setup_not_ready",
    "channel_mismatch",
    "credential_missing",
    "auth_error",
    "cooling_down",
    "free_channel_unavailable",
    "quota_waiting",
    "quota_due",
    "quota_probing",
    "retry_exclusions_not_applied",
    "upstream_result_unknown",
];

#[derive(Debug)]
struct CountingCipher {
    inner: StaticKeyCipher,
    decrypts: Arc<AtomicUsize>,
}

impl KeyCipher for CountingCipher {
    fn encrypt(&self, plaintext: &str) -> anyhow::Result<String> {
        self.inner.encrypt(plaintext)
    }

    fn decrypt(&self, ciphertext: &str) -> anyhow::Result<String> {
        self.decrypts.fetch_add(1, Ordering::SeqCst);
        self.inner.decrypt(ciphertext)
    }
}

struct World {
    dir: PathBuf,
    state: Option<CoreState>,
    decrypts: Arc<AtomicUsize>,
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
}

struct Face {
    endpoint_id: String,
    origin: String,
    endpoint_fingerprint: String,
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

fn open_with(prepare: impl FnOnce(&Connection)) -> World {
    let dir = std::env::temp_dir().join(format!("ocg-routing-explain-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let db = Database::open(dir.clone()).unwrap();
    prepare(&db.conn);
    let decrypts = Arc::new(AtomicUsize::new(0));
    let cipher: Arc<dyn KeyCipher + Send + Sync> = Arc::new(CountingCipher {
        inner: StaticKeyCipher::new("routing-explain-test"),
        decrypts: Arc::clone(&decrypts),
    });
    let state = Arc::new(CoreStateInner::new(db, dir.clone(), cipher).unwrap());
    World {
        dir,
        state: Some(state),
        decrypts,
    }
}

fn goat_face() -> Face {
    let path = command_code_upstream_path(ApiFormat::ChatCompletions).expect("goat chat path");
    let joined = format!(
        "{}/{}",
        COMMAND_CODE_GOAT_BASE_URL.trim_end_matches('/'),
        path.trim_start_matches('/')
    );
    let rewritten = match crate::cpa_test_endpoints::rewrite_url(COMMAND_CODE_PROVIDER_ID, &joined)
    {
        Ok(Some(url)) => url,
        Ok(None) => joined,
        Err(error) => panic!("goat face rewrite failed: {error}"),
    };
    let parsed = reqwest::Url::parse(&rewritten).expect("goat face url");
    crate::custom_http::inspect_custom_url(&parsed).expect("goat face inspection");
    let url = parsed.as_str().to_string();
    let host = parsed.host_str().expect("goat host").to_ascii_lowercase();
    let port = parsed.port_or_known_default().expect("goat port");
    let origin = format!("{}://{host}:{port}", parsed.scheme().to_ascii_lowercase());
    let endpoint_fingerprint = crate::cpa_projection::endpoint_fingerprint(&url);
    let connection = serde_json::from_value(serde_json::Value::String("goat-connection".into()))
        .expect("goat connection");
    let plan = builtin_provider(COMMAND_CODE_PROVIDER_ID).expect("goat provider");
    let routes: Vec<RouteSpec> = plan
        .upstream_protocols
        .iter()
        .copied()
        .map(|protocol| RouteSpec {
            operation: EndpointOperation::from(protocol),
            url: None,
        })
        .collect();
    let assigned = assigned_endpoints_for_routes(&connection, &routes);
    let chat = EndpointOperation::from(UpstreamProtocolKind::ChatCompletions);
    let endpoint_id = routes
        .iter()
        .zip(assigned.iter())
        .find(|(route, _)| route.operation == chat && route.url.is_none())
        .map(|(_, endpoint)| endpoint.id.clone())
        .expect("goat chat endpoint");
    Face {
        endpoint_id,
        origin,
        endpoint_fingerprint,
    }
}

fn codex_targets() -> (crate::cpa_projection::NativeEndpointPin, serde_json::Value) {
    let facts = crate::cpa_projection::NativeAuthorityFacts {
        raw_label: String::new(),
        provider: "codex".into(),
        mode: String::new(),
        reported_base: String::new(),
    };
    let connection = connection_id_for_legacy(
        LegacyConnectionKind::BuiltinProvider,
        OWNED_NATIVE_LEGACY_ID,
    );
    let (pin, targets) = crate::cpa_projection::native_route_targets(
        &facts,
        "codex-test",
        "chat_completions",
        &connection,
    )
    .expect("canonical codex chat targets");
    let targets = serde_json::to_value(&targets).expect("native targets");
    (pin, targets)
}

fn chat_model(public_model: &str, upstream_model: &str, enabled: bool) -> CatalogModel {
    CatalogModel {
        public_model: public_model.into(),
        upstream_model: upstream_model.into(),
        protocols: vec![UpstreamProtocolKind::ChatCompletions],
        preferred: Some(UpstreamProtocolKind::ChatCompletions),
        enabled,
        upstream_override: None,
    }
}

fn insert_destination(
    conn: &Connection,
    id: &str,
    name: &str,
    legacy: LegacyDestinationRef,
    adapter: AdapterKind,
    auth: AuthScheme,
    base_url: Option<&str>,
) {
    let exists: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM destinations WHERE id = ?1",
            [id],
            |row| row.get(0),
        )
        .unwrap();
    if exists != 0 {
        return;
    }
    let (legacy_kind, legacy_id) = match &legacy {
        LegacyDestinationRef::Builtin(value) => ("builtin", value.as_str()),
        LegacyDestinationRef::Dynamic(value) => ("dynamic", value.as_str()),
        LegacyDestinationRef::CustomAccount(value) => ("custom_account", value.as_str()),
        LegacyDestinationRef::PlatformParent(value) => ("platform_parent", value.as_str()),
    };
    let seeded: Option<String> = conn
        .query_row(
            "SELECT id FROM destinations WHERE legacy_kind = ?1 AND legacy_id = ?2",
            params![legacy_kind, legacy_id],
            |row| row.get(0),
        )
        .optional()
        .unwrap();
    if let Some(seeded) = seeded.filter(|seeded| seeded != id) {
        conn.execute(
            "UPDATE destination_models SET destination_id = ?2 WHERE destination_id = ?1",
            params![seeded, id],
        )
        .unwrap();
        conn.execute(
            "UPDATE credentials SET destination_id = ?2 WHERE destination_id = ?1",
            params![seeded, id],
        )
        .unwrap();
        conn.execute("DELETE FROM destinations WHERE id = ?1", [&seeded])
            .unwrap();
    }
    insert_destination_row(
        conn,
        &Destination {
            id: id.into(),
            legacy,
            adapter,
            name: name.into(),
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
            enabled: true,
        },
    )
    .unwrap();
}

fn insert_credential(
    conn: &Connection,
    id: &str,
    legacy_account_id: &str,
    destination_id: &str,
    label: &str,
    provider_id: &str,
    cipher: &str,
    binding_id: &str,
    connection_id: &str,
) {
    conn.execute(
        "INSERT INTO credentials (
            id, legacy_account_id, destination_id, name, has_secret, enabled, routing_rank,
            scope_json, auth_state, key_cipher, provider_id, credential_kind, quota_scope,
            account_type, setup_step, verification_status, credential_version,
            authorization_connection_id, created_at, updated_at
         ) VALUES (
            ?1, ?2, ?3, ?4, ?5, 1, 99, '{\"kind\":\"all\"}', 'unknown', ?6, ?7, 'key', 'key',
            'key', 'ready', 'verified', 3, ?8, '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z'
         )",
        params![
            id,
            legacy_account_id,
            destination_id,
            label,
            i64::from(!cipher.is_empty()),
            cipher,
            provider_id,
            connection_id,
        ],
    )
    .unwrap();
    conn.execute(
        "UPDATE credentials SET binding_id = ?2, binding_enabled = 1 WHERE id = ?1",
        params![id, binding_id],
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

fn save_setting(conn: &Connection, key: &str, value: &str) {
    conn.execute(
        "INSERT OR REPLACE INTO settings (key, value) VALUES (?1, ?2)",
        params![key, value],
    )
    .unwrap();
}

fn auth_json(
    auth_id: &str,
    credential_id: &str,
    version: u64,
    binding_id: &str,
    provider_id: &str,
    epoch: u64,
) -> serde_json::Value {
    serde_json::json!({
        "authId": auth_id,
        "credentialId": credential_id,
        "credentialVersion": version.to_string(),
        "bindingId": binding_id,
        "materialRevision": STAMP_MATERIAL,
        "providerId": provider_id,
        "registrationEpoch": epoch.to_string(),
    })
}

fn route_json(
    public_model: &str,
    upstream_model: &str,
    endpoint_id: &str,
    origin: &str,
    endpoint_fingerprint: &str,
    validation_only: bool,
    native_targets: serde_json::Value,
) -> serde_json::Value {
    serde_json::json!({
        "publicModel": public_model,
        "upstreamModel": upstream_model,
        "protocol": "chat_completions",
        "endpointId": endpoint_id,
        "origin": origin,
        "endpointFingerprint": endpoint_fingerprint,
        "validationOnly": validation_only,
        "nativeTargets": native_targets,
    })
}

fn set_json(
    auth_id: &str,
    credential_id: &str,
    version: u64,
    binding_id: &str,
    rank: u32,
    routes: Vec<serde_json::Value>,
) -> serde_json::Value {
    serde_json::json!({
        "authId": auth_id,
        "credentialId": credential_id,
        "credentialVersion": version.to_string(),
        "bindingId": binding_id,
        "materialFingerprint": MATERIAL,
        "routingRank": rank,
        "routes": routes,
        "fingerprint": SET_FINGERPRINT,
    })
}

fn oauth_json(credential_id: &str, auth_id: &str, presence: &str) -> serde_json::Value {
    serde_json::json!({
        "relativePath": OAUTH_PATH,
        "authId": auth_id,
        "credentialId": credential_id,
        "credentialVersion": "3",
        "materialRevision": STAMP_MATERIAL,
        "providerId": CPA_PROVIDER_ID,
        "nativeProvider": "codex",
        "registrationEpoch": "4",
        "models": ["codex-test"],
        "presence": presence,
        "recovery": REPORTED_BASE,
        "rawProviderLabel": "",
        "nativeMode": "",
        "reportedBase": "",
    })
}

fn record_json(
    apply_status: &str,
    desired_auth: Vec<serde_json::Value>,
    desired_routes: Vec<serde_json::Value>,
    applied_auth: Vec<serde_json::Value>,
    applied_routes: Vec<serde_json::Value>,
    oauth: Vec<serde_json::Value>,
) -> serde_json::Value {
    serde_json::json!({
        "version": 1,
        "childGeneration": "11",
        "appliedGeneration": "11",
        "desiredRevision": "5",
        "appliedRevision": "4",
        "desiredDigest": DESIRED_DIGEST,
        "appliedDigest": APPLIED_DIGEST,
        "artifactSha256": "",
        "listenPort": 0,
        "applyStatus": apply_status,
        "desiredRunning": false,
        "publicOrigin": "",
        "ownedOrigin": "",
        "policyReady": false,
        "unavailable": false,
        "desiredAuth": desired_auth,
        "appliedAuth": applied_auth,
        "desiredRoutes": desired_routes,
        "appliedRoutes": applied_routes,
        "hostCapabilities": [NATIVE_ENDPOINT_PIN_CAPABILITY],
        "oauth": oauth,
    })
}

fn blank_route(public_model: &str, upstream_model: &str) -> serde_json::Value {
    route_json(
        public_model,
        upstream_model,
        "",
        "",
        "",
        false,
        serde_json::json!([]),
    )
}

fn face_route(public_model: &str, face: &Face, validation_only: bool) -> serde_json::Value {
    route_json(
        public_model,
        public_model,
        &face.endpoint_id,
        &face.origin,
        &face.endpoint_fingerprint,
        validation_only,
        serde_json::json!([]),
    )
}

fn client_world(apply_status: &str) -> World {
    let face = goat_face();
    let record = record_json(
        apply_status,
        vec![auth_json(
            "auth-goat",
            "cred-goat",
            3,
            "bind-goat",
            COMMAND_CODE_PROVIDER_ID,
            4,
        )],
        vec![set_json(
            "auth-goat",
            "cred-goat",
            3,
            "bind-goat",
            9,
            vec![blank_route("desired-only", "desired-only")],
        )],
        vec![
            auth_json(
                "auth-goat",
                "cred-goat",
                3,
                "bind-goat",
                COMMAND_CODE_PROVIDER_ID,
                4,
            ),
            auth_json(
                "auth-zen",
                "cred-zen",
                3,
                "bind-zen",
                OPENCODE_ZEN_FREE_PROVIDER_ID,
                4,
            ),
        ],
        vec![
            set_json(
                "auth-goat",
                "cred-goat",
                3,
                "bind-goat",
                9,
                vec![
                    face_route("goat-model", &face, false),
                    face_route("held-model", &face, true),
                ],
            ),
            set_json(
                "auth-zen",
                "cred-zen",
                3,
                "bind-zen",
                1,
                vec![blank_route("zen-public", "")],
            ),
        ],
        Vec::new(),
    );
    let fingerprint = face.endpoint_fingerprint.clone();
    let origin = face.origin.clone();
    let endpoint_id = face.endpoint_id.clone();
    open_with(move |conn| {
        insert_destination(
            conn,
            "dest-goat",
            "Goat Dest",
            LegacyDestinationRef::Builtin(COMMAND_CODE_PROVIDER_ID.into()),
            AdapterKind::Goat,
            AuthScheme::Bearer,
            None,
        );
        insert_credential(
            conn,
            "cred-goat",
            "acct-goat",
            "dest-goat",
            "Goat Public",
            COMMAND_CODE_PROVIDER_ID,
            CIPHER,
            "bind-goat",
            "goat-connection",
        );
        insert_destination(
            conn,
            "dest-zen",
            "Zen Dest",
            LegacyDestinationRef::Builtin(OPENCODE_ZEN_FREE_PROVIDER_ID.into()),
            AdapterKind::Zen,
            AuthScheme::None,
            None,
        );
        insert_credential(
            conn,
            "cred-zen",
            "acct-zen",
            "dest-zen",
            "Zen Public",
            OPENCODE_ZEN_FREE_PROVIDER_ID,
            "",
            "bind-zen",
            "",
        );
        replace_destination_catalog(
            conn,
            "dest-goat",
            &[
                chat_model("goat-model", "goat-model", true),
                chat_model("held-model", "held-model", true),
            ],
        )
        .unwrap();
        grant(conn, "cred-goat", "endpoint_id", &endpoint_id);
        grant(conn, "cred-goat", "origin", &origin);
        save_setting(conn, EXECUTION_KEY, &record.to_string());
        let _ = fingerprint;
    })
}

fn native_world() -> World {
    let (pin, targets) = codex_targets();
    let native_route = route_json(
        "codex-test",
        "codex-test",
        &pin.endpoint_id,
        &pin.origin,
        &pin.endpoint_fingerprint,
        false,
        targets.clone(),
    );
    let pending_route = native_route.clone();
    let remote_route = route_json(
        "remote-model",
        "codex-test",
        &pin.endpoint_id,
        &pin.origin,
        &pin.endpoint_fingerprint,
        false,
        targets,
    );
    let record = record_json(
        "apply_failed",
        Vec::new(),
        Vec::new(),
        vec![
            auth_json(
                "native-auth",
                "cred-native",
                3,
                "bind-native",
                CPA_PROVIDER_ID,
                4,
            ),
            auth_json(
                "pending-auth",
                "cred-pending",
                3,
                "bind-pending",
                CPA_PROVIDER_ID,
                4,
            ),
            auth_json(
                "remote-auth",
                "cred-remote",
                3,
                "bind-remote",
                CPA_PROVIDER_ID,
                4,
            ),
        ],
        vec![
            set_json(
                "native-auth",
                "cred-native",
                3,
                "bind-native",
                2,
                vec![native_route],
            ),
            set_json(
                "pending-auth",
                "cred-pending",
                3,
                "bind-pending",
                6,
                vec![pending_route],
            ),
            set_json(
                "remote-auth",
                "cred-remote",
                3,
                "bind-remote",
                14,
                vec![remote_route],
            ),
        ],
        vec![
            oauth_json("cred-native", "native-auth", "present"),
            oauth_json("cred-pending", "pending-auth", "pending"),
            oauth_json("cred-remote", "remote-auth", "present"),
        ],
    );
    let endpoint_id = pin.endpoint_id.clone();
    let origin = pin.origin.clone();
    let connection = connection_id_for_legacy(
        LegacyConnectionKind::BuiltinProvider,
        OWNED_NATIVE_LEGACY_ID,
    )
    .as_str()
    .to_string();
    open_with(move |conn| {
        insert_destination(
            conn,
            "dest-native",
            "Native Dest",
            LegacyDestinationRef::Builtin(OWNED_NATIVE_LEGACY_ID.into()),
            AdapterKind::Cpa,
            AuthScheme::Bearer,
            None,
        );
        insert_destination(
            conn,
            "dest-remote",
            "Remote Dest",
            LegacyDestinationRef::Builtin(CPA_PROVIDER_ID.into()),
            AdapterKind::Cpa,
            AuthScheme::Bearer,
            Some("https://legacy-remote.example"),
        );
        for (id, legacy, destination, label, binding, connection) in [
            (
                "cred-native",
                "acct-native",
                "dest-native",
                "Native Public",
                "bind-native",
                connection.as_str(),
            ),
            (
                "cred-pending",
                "acct-pending",
                "dest-native",
                "Pending Public",
                "bind-pending",
                connection.as_str(),
            ),
            (
                "cred-remote",
                "acct-remote",
                "dest-remote",
                "Remote Public",
                "bind-remote",
                connection.as_str(),
            ),
        ] {
            insert_credential(
                conn,
                id,
                legacy,
                destination,
                label,
                CPA_PROVIDER_ID,
                "",
                binding,
                connection,
            );
            grant(conn, id, "endpoint_id", &endpoint_id);
            grant(conn, id, "origin", &origin);
        }
        replace_destination_catalog(
            conn,
            "dest-native",
            &[chat_model("codex-test", "codex-test", true)],
        )
        .unwrap();
        replace_destination_catalog(
            conn,
            "dest-remote",
            &[chat_model("remote-model", "codex-test", true)],
        )
        .unwrap();
        save_setting(conn, EXECUTION_KEY, &record.to_string());
    })
}

fn omitted_remote_world() -> World {
    open_with(|conn| {
        insert_destination(
            conn,
            "dest-omitted-remote",
            "Omitted Remote",
            LegacyDestinationRef::CustomAccount("dest-omitted-remote".into()),
            AdapterKind::Cpa,
            AuthScheme::Bearer,
            Some("https://omitted.example"),
        );
        insert_credential(
            conn,
            "cred-omitted-remote",
            "acct-omitted-remote",
            "dest-omitted-remote",
            "Omitted Remote",
            CPA_PROVIDER_ID,
            "",
            "bind-omitted-remote",
            "",
        );
        replace_destination_catalog(
            conn,
            "dest-omitted-remote",
            &[chat_model("ocg-omitted-remote", "ocg-omitted-remote", true)],
        )
        .unwrap();
    })
}

fn alias_world() -> World {
    open_with(|conn| {
        for (id, name, provider, public_model, upstream, enabled) in [
            (
                "dest-alias",
                "Alias Dest",
                "custom-alias",
                "plain-alias",
                "vendor/raw-model",
                true,
            ),
            (
                "dest-same",
                "Same Dest",
                "custom-same",
                "same-name",
                "same-name",
                true,
            ),
            (
                "dest-disabled",
                "Disabled Dest",
                "custom-off",
                "disabled-alias",
                "vendor/disabled-raw",
                false,
            ),
            (
                "dest-pin-a",
                "Pin A",
                "custom-a",
                "pin-a",
                "vendor/shared-raw",
                true,
            ),
            (
                "dest-pin-b",
                "Pin B",
                "custom-b",
                "pin-b",
                "vendor/shared-raw",
                true,
            ),
        ] {
            insert_destination(
                conn,
                id,
                name,
                LegacyDestinationRef::CustomAccount(id.into()),
                AdapterKind::Http,
                AuthScheme::ApiKey,
                Some("https://http.example"),
            );
            insert_credential(
                conn,
                &format!("cred-{id}"),
                &format!("acct-{id}"),
                id,
                name,
                provider,
                CIPHER,
                &format!("bind-{id}"),
                &format!("conn-{id}"),
            );
            replace_destination_catalog(conn, id, &[chat_model(public_model, upstream, enabled)])
                .unwrap();
        }
    })
}

fn stale_world() -> World {
    let record = record_json(
        "apply_failed",
        Vec::new(),
        Vec::new(),
        vec![auth_json(
            "auth-stale",
            "cred-stale",
            2,
            "bind-stale",
            "custom-stale",
            4,
        )],
        vec![set_json(
            "auth-stale",
            "cred-stale",
            2,
            "bind-stale",
            11,
            vec![blank_route("stale-public", "stale-public")],
        )],
        Vec::new(),
    );
    open_with(move |conn| {
        insert_destination(
            conn,
            "dest-stale",
            "Stale Dest",
            LegacyDestinationRef::CustomAccount("dest-stale".into()),
            AdapterKind::Http,
            AuthScheme::Bearer,
            Some("https://stale.example"),
        );
        insert_credential(
            conn,
            "cred-stale",
            "acct-stale",
            "dest-stale",
            "Stale Public",
            "custom-stale",
            CIPHER,
            "bind-stale",
            "stale-connection",
        );
        replace_destination_catalog(
            conn,
            "dest-stale",
            &[chat_model("stale-public", "stale-public", true)],
        )
        .unwrap();
        save_setting(conn, EXECUTION_KEY, &record.to_string());
    })
}

fn explain_at(
    world: &World,
    model: &str,
    protocol: RoutingClientProtocol,
) -> Result<RoutingExplanation, V3ApiError> {
    explain_model_at(world.state(), model, protocol, clock())
}

fn setting(world: &World, key: &str) -> Option<String> {
    let db = world.state().db.lock();
    db.conn
        .query_row("SELECT value FROM settings WHERE key = ?1", [key], |row| {
            row.get(0)
        })
        .optional()
        .unwrap()
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
        .prepare("SELECT id, COALESCE(key_cipher, '') FROM credentials ORDER BY id")
        .unwrap();
    statement
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
        .unwrap()
        .map(|row| row.unwrap())
        .collect()
}

fn recovery_snapshot(world: &World) -> Vec<(String, String)> {
    let db = world.state().db.lock();
    let mut statement = db
        .conn
        .prepare("SELECT id, COALESCE(quota_recovery_json, '') FROM credentials ORDER BY id")
        .unwrap();
    statement
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
        .unwrap()
        .map(|row| row.unwrap())
        .collect()
}

fn grant_snapshot(world: &World) -> Vec<(String, String, String)> {
    let db = world.state().db.lock();
    let mut statement = db
        .conn
        .prepare("SELECT credential_id, kind, value FROM credential_grants ORDER BY rowid")
        .unwrap();
    statement
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
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

fn changes(world: &World) -> u64 {
    world.state().db.lock().conn.total_changes()
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

fn error_json(error: V3ApiError) -> (StatusCode, serde_json::Value) {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        let response = error.into_response();
        let status = response.status();
        let bytes = axum::body::to_bytes(response.into_body(), 64 * 1024)
            .await
            .unwrap();
        let value = serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null);
        (status, value)
    })
}

fn assert_redacted(body: &impl serde::Serialize) {
    let json = serde_json::to_string(body).unwrap();
    for marker in [
        MATERIAL,
        SET_FINGERPRINT,
        OAUTH_PATH,
        CIPHER,
        STAMP_MATERIAL,
        REPORTED_BASE,
    ] {
        assert!(!json.contains(marker), "{marker} leaked into {json}");
    }
}

fn assert_legacy_absent(body: &RoutingExplanation) {
    let json = serde_json::to_string(body).unwrap();
    for code in LEGACY_WIRE {
        assert!(
            !json.contains(&format!("\"{code}\"")),
            "{code} was emitted: {json}"
        );
    }
}

fn assert_not_sendable(body: &RoutingExplanation) {
    assert!(body.eligible.is_empty());
    assert!(body.expected_base_policy_first_pick.is_none());
    assert_eq!(
        body.conversation_binding,
        RoutingConversationBinding::NotEvaluated
    );
    assert!(!body.owned_projection.owned_running);
    assert!(!body.owned_projection.owned_running_before);
    assert!(!body.owned_projection.owned_running_after);
    assert!(!body.owned_projection.verified_ready);
    assert!(!body.owned_projection.policy_ready);
    assert!(!body.owned_projection.pin_capabilities_ready);
    assert!(!body.owned_projection.tuple_aligned);
    assert!(!body.owned_projection.origin_verified);
    assert!(!body.owned_projection.poisoned);
    assert!(!body.owned_projection.unavailable);
}

fn assert_stored_tuple(body: &RoutingExplanation, apply_status: &str) {
    assert_eq!(body.owned_projection.apply_status, apply_status);
    assert!(!body.owned_projection.desired_running);
    assert_eq!(body.owned_projection.desired.generation, 11);
    assert_eq!(body.owned_projection.desired.revision, 5);
    assert_eq!(body.owned_projection.desired.digest, DESIRED_DIGEST);
    assert_eq!(body.owned_projection.applied.generation, 11);
    assert_eq!(body.owned_projection.applied.revision, 4);
    assert_eq!(body.owned_projection.applied.digest, APPLIED_DIGEST);
    assert_ne!(body.owned_projection.runtime_child_generation, 0);
    assert_ne!(
        body.owned_projection.runtime_child_generation,
        body.owned_projection.desired.generation
    );
}

fn quiet_uncertainties(keyed: bool) -> Vec<RuntimeOnlyUncertainty> {
    let mut out = vec![RuntimeOnlyUncertainty::ConversationBindingNotEvaluated];
    if keyed {
        out.push(RuntimeOnlyUncertainty::CredentialRecheckPending);
    }
    out.push(RuntimeOnlyUncertainty::CpaSelectionNotEvaluated);
    out.push(RuntimeOnlyUncertainty::QuotaTrialNotEvaluated);
    out
}

fn authority<'a>(body: &'a RoutingExplanation, credential_id: &str) -> &'a RoutingRouteAuthority {
    body.exclusions
        .iter()
        .find_map(|item| {
            item.authority
                .as_ref()
                .filter(|authority| authority.credential_id == credential_id)
        })
        .unwrap_or_else(|| panic!("missing authority for {credential_id}"))
}

fn authority_for<'a>(
    body: &'a RoutingExplanation,
    credential_id: &str,
    public_model: &str,
) -> &'a RoutingRouteAuthority {
    body.exclusions
        .iter()
        .find_map(|item| {
            item.authority.as_ref().filter(|authority| {
                authority.credential_id == credential_id && authority.public_model == public_model
            })
        })
        .unwrap_or_else(|| panic!("missing authority for {credential_id} {public_model}"))
}

fn codes(body: &RoutingExplanation, credential_id: &str) -> Vec<RoutingExclusionCode> {
    body.exclusions
        .iter()
        .filter(|item| {
            item.authority
                .as_ref()
                .is_some_and(|authority| authority.credential_id == credential_id)
        })
        .map(|item| item.code)
        .collect()
}

fn assert_cold_codes(found: &[RoutingExclusionCode]) {
    for code in [
        RoutingExclusionCode::OwnedNotRunning,
        RoutingExclusionCode::OriginUnverified,
        RoutingExclusionCode::NotReady,
        RoutingExclusionCode::PolicyNotReady,
        RoutingExclusionCode::PinCapabilities,
        RoutingExclusionCode::TupleUnaligned,
    ] {
        assert!(found.contains(&code), "missing {code:?} in {found:?}");
    }
}

fn operation<'a>(authority: &'a RoutingRouteAuthority, kind: &str) -> &'a RoutingOperationFact {
    authority
        .native_operations
        .iter()
        .find(|fact| fact.generation_kind == kind)
        .unwrap_or_else(|| panic!("missing operation {kind}"))
}

fn write_policy(world: &World, json: &str) {
    let db = world.state().db.lock();
    save_setting(&db.conn, SETTINGS_KEY, json);
}

fn known_policy(reset_at: DateTime<Utc>, observation_id: &str) -> String {
    PolicyDocument {
        restrictions: vec![Restriction {
            scope: Scope {
                subject: Subject::Credential {
                    credential_id: "cred-goat".into(),
                    credential_version: 3,
                    provider_id: COMMAND_CODE_PROVIDER_ID.into(),
                    binding_id: "bind-goat".into(),
                },
                public_model: Some("goat-model".into()),
            },
            window: Window::FiveHours,
            reset: Reset::Known { at: reset_at },
            observed_at: clock(),
            observation_id: observation_id.into(),
            source: EvidenceSource::GoatPlan,
            recovery: None,
        }],
        attempts: Vec::new(),
    }
    .to_json()
    .unwrap()
}

fn unknown_policy() -> String {
    serde_json::json!({
        "version": 1,
        "restrictions": [{
            "scope": {
                "subject": {
                    "kind": "credential",
                    "credential_id": "cred-goat",
                    "credential_version": 3,
                    "provider_id": COMMAND_CODE_PROVIDER_ID,
                    "binding_id": "bind-goat"
                },
                "public_model": "goat-model"
            },
            "window": "five_hours",
            "reset": {"kind": "unknown"},
            "observed_at": "2026-10-04T12:00:00Z",
            "observation_id": "obs-unknown",
            "source": "goat_plan",
            "recovery": {}
        }],
        "attempts": []
    })
    .to_string()
}

async fn handler_explain(state: &CoreState, uri: &str) -> Result<RoutingExplanation, V3ApiError> {
    let mut parts = Request::builder().uri(uri).body(()).unwrap().into_parts().0;
    let query = RoutingExplainQuery::from_request_parts(&mut parts, state).await?;
    let Json(body) = explain(axum::extract::State(state.clone()), query).await?;
    Ok(body)
}

#[test]
fn parse_explain_query_defaults_protocol_and_rejects_blank_or_invalid() {
    let parsed = parse_explain_query(&ExplainQuery {
        model: Some(" glm-5.2 ".into()),
        client_protocol: None,
    })
    .unwrap();
    assert_eq!(parsed.0, "glm-5.2");
    assert_eq!(parsed.1, RoutingClientProtocol::ChatCompletions);

    let responses = parse_explain_query(&ExplainQuery {
        model: Some("glm-5.2".into()),
        client_protocol: Some("responses".into()),
    })
    .unwrap();
    assert_eq!(responses.1, RoutingClientProtocol::Responses);

    let messages = parse_explain_query(&ExplainQuery {
        model: Some("glm-5.2".into()),
        client_protocol: Some("messages".into()),
    })
    .unwrap();
    assert_eq!(messages.1, RoutingClientProtocol::Messages);

    let gemini = parse_explain_query(&ExplainQuery {
        model: Some("glm-5.2".into()),
        client_protocol: Some("gemini".into()),
    })
    .unwrap();
    assert_eq!(gemini.1, RoutingClientProtocol::Gemini);

    assert_eq!(
        parse_explain_query(&ExplainQuery {
            model: None,
            client_protocol: None,
        })
        .unwrap_err(),
        "model is required"
    );
    assert_eq!(
        parse_explain_query(&ExplainQuery {
            model: Some("   ".into()),
            client_protocol: None,
        })
        .unwrap_err(),
        "model is required"
    );
    assert_eq!(
        parse_explain_query(&ExplainQuery {
            model: Some("glm-5.2".into()),
            client_protocol: Some("   ".into()),
        })
        .unwrap_err(),
        "clientProtocol must be chat_completions, responses, messages, or gemini"
    );
    assert_eq!(
        parse_explain_query(&ExplainQuery {
            model: Some("glm-5.2".into()),
            client_protocol: Some("grpc".into()),
        })
        .unwrap_err(),
        "clientProtocol must be chat_completions, responses, messages, or gemini"
    );
}

#[test]
fn old_exclusion_and_uncertainty_values_still_deserialize() {
    for (raw, expected) in [
        ("account_disabled", RoutingExclusionCode::AccountDisabled),
        ("quota_waiting", RoutingExclusionCode::QuotaWaiting),
        ("quota_probing", RoutingExclusionCode::QuotaProbing),
        ("cooling_down", RoutingExclusionCode::CoolingDown),
        ("goat_not_eligible", RoutingExclusionCode::GoatNotEligible),
        ("identity", RoutingExclusionCode::Identity),
        ("quota_known_reset", RoutingExclusionCode::QuotaKnownReset),
        ("policy_malformed", RoutingExclusionCode::PolicyMalformed),
    ] {
        let parsed: RoutingExclusionCode =
            serde_json::from_value(serde_json::Value::String(raw.into())).unwrap();
        assert_eq!(parsed, expected);
    }
    for (raw, expected) in [
        (
            "upstream_result_unknown",
            RuntimeOnlyUncertainty::UpstreamResultUnknown,
        ),
        (
            "retry_exclusions_not_applied",
            RuntimeOnlyUncertainty::RetryExclusionsNotApplied,
        ),
        (
            "cpa_selection_not_evaluated",
            RuntimeOnlyUncertainty::CpaSelectionNotEvaluated,
        ),
        (
            "quota_trial_not_evaluated",
            RuntimeOnlyUncertainty::QuotaTrialNotEvaluated,
        ),
    ] {
        let parsed: RuntimeOnlyUncertainty =
            serde_json::from_value(serde_json::Value::String(raw.into())).unwrap();
        assert_eq!(parsed, expected);
    }
    let channel: RoutingChannel =
        serde_json::from_value(serde_json::Value::String("go".into())).unwrap();
    assert_eq!(channel, RoutingChannel::Go);
}

#[tokio::test]
async fn repeated_get_leaves_state_unchanged_and_does_not_pick() {
    let world = client_world("apply_failed");
    let state = world.state().clone();
    let revision = state.settings_revision();
    let generation = state.process_generation();
    let settings = settings_snapshot(&world);
    let ciphers = cipher_snapshot(&world);
    let recovery = recovery_snapshot(&world);
    let grants = grant_snapshot(&world);
    let logs = log_count(&world);
    let selector = format!("{:?}", state.routing);
    let decrypts = world.decrypts.load(Ordering::SeqCst);
    let writes = changes(&world);
    let uri = "/dashboard/api/v4/routing/explain?model=goat-model&clientProtocol=chat_completions";

    let first = handler_explain(&state, uri).await.unwrap();
    let second = handler_explain(&state, uri).await.unwrap();
    let third =
        explain_model(&state, "goat-model", RoutingClientProtocol::ChatCompletions).unwrap();
    let blank = handler_explain(
        &state,
        "/dashboard/api/v4/routing/explain?model=%20%20&clientProtocol=chat_completions",
    )
    .await
    .unwrap_err();

    for body in [&first, &second, &third] {
        assert!(body.expected_base_policy_first_pick.is_none());
        assert!(body.eligible.is_empty());
        assert!(!body.observed_at.is_empty());
        assert_eq!(body.revision, ControlRevision::from_state(&state));
        assert_redacted(body);
    }
    let mut comparable_first = first.clone();
    let mut comparable_second = second.clone();
    comparable_first.observed_at.clear();
    comparable_second.observed_at.clear();
    assert_eq!(comparable_first, comparable_second);
    let (status, error) = {
        let response = blank.into_response();
        let status = response.status();
        let bytes = axum::body::to_bytes(response.into_body(), 64 * 1024)
            .await
            .unwrap();
        let value = serde_json::from_slice::<serde_json::Value>(&bytes).unwrap();
        (status, value)
    };
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(error["message"], "model is required");
    assert_eq!(settings_snapshot(&world), settings);
    assert_eq!(cipher_snapshot(&world), ciphers);
    assert_eq!(recovery_snapshot(&world), recovery);
    assert_eq!(grant_snapshot(&world), grants);
    assert_eq!(log_count(&world), logs);
    assert_eq!(format!("{:?}", state.routing), selector);
    assert_eq!(state.settings_revision(), revision);
    assert_eq!(state.process_generation(), generation);
    assert_eq!(changes(&world), writes);
    assert_eq!(world.decrypts.load(Ordering::SeqCst), decrypts);
    assert_eq!(decrypts, 0);
}

#[test]
fn routing_modes_keep_first_pick_null() {
    let world = alias_world();
    for (mode, dto) in [
        (RoutingMode::StrictPriority, RoutingModeDto::StrictPriority),
        (RoutingMode::StickyGlobal, RoutingModeDto::StickyGlobal),
        (RoutingMode::RoundRobin, RoutingModeDto::RoundRobin),
    ] {
        let mut config = world.state().config();
        if config.gateway_key.trim().is_empty() {
            config.gateway_key = "routing-explain-gateway-key".into();
        }
        config.routing_mode = mode;
        config.conversation_sticky = true;
        world.state().set_config(config).unwrap();
        let revision = world.state().settings_revision();
        let generation = world.state().process_generation();
        let writes = changes(&world);
        let body = explain_at(
            &world,
            "plain-alias",
            RoutingClientProtocol::ChatCompletions,
        )
        .expect("known alias");
        assert!(body.expected_base_policy_first_pick.is_none());
        assert!(body.eligible.is_empty());
        assert_eq!(
            body.conversation_binding,
            RoutingConversationBinding::NotEvaluated
        );
        assert!(body.conversation_sticky);
        assert_eq!(body.routing_mode, dto);
        assert_eq!(world.state().settings_revision(), revision);
        assert_eq!(world.state().process_generation(), generation);
        assert_eq!(changes(&world), writes);
        assert_redacted(&body);
    }
}

#[test]
fn formats_keep_gemini_on_chat_and_mark_other_protocols() {
    let world = client_world("apply_failed");
    let chat = explain_at(&world, "goat-model", RoutingClientProtocol::ChatCompletions).unwrap();
    let gemini = explain_at(&world, "goat-model", RoutingClientProtocol::Gemini).unwrap();
    let responses = explain_at(&world, "goat-model", RoutingClientProtocol::Responses).unwrap();
    let messages = explain_at(&world, "goat-model", RoutingClientProtocol::Messages).unwrap();

    assert_eq!(gemini.client_protocol, RoutingClientProtocol::Gemini);
    assert_eq!(chat.client_protocol, RoutingClientProtocol::ChatCompletions);
    for body in [&chat, &gemini] {
        let goat = authority(body, "cred-goat");
        assert_eq!(goat.protocol, "chat_completions");
        assert!(!codes(body, "cred-goat").contains(&RoutingExclusionCode::Protocol));
        assert_redacted(body);
    }
    for body in [&responses, &messages] {
        assert!(codes(body, "cred-goat").contains(&RoutingExclusionCode::Protocol));
        assert_eq!(authority(body, "cred-goat").protocol, "chat_completions");
        assert_redacted(body);
    }
    assert_eq!(
        gemini.resolved.mappings[0].adapter_kind,
        AdapterKind::Goat.as_str()
    );
}

#[test]
fn alias_pinned_raw_unknown_and_ambiguous_use_catalog_facts() {
    let world = alias_world();
    let writes = changes(&world);
    let alias = explain_at(
        &world,
        "plain-alias",
        RoutingClientProtocol::ChatCompletions,
    )
    .unwrap();
    assert_eq!(alias.resolved.kind, RoutingResolvedKind::Alias);
    assert_eq!(alias.resolved.alias.as_deref(), Some("plain-alias"));
    assert!(alias.resolved.mappings.iter().any(|mapping| {
        mapping.destination_id == "dest-alias"
            && mapping.provider_id == "custom-alias"
            && mapping.upstream_model == "vendor/raw-model"
            && mapping.adapter_kind == AdapterKind::Http.as_str()
            && !mapping.routeable
    }));

    let pinned = explain_at(
        &world,
        "vendor/raw-model",
        RoutingClientProtocol::ChatCompletions,
    )
    .unwrap();
    assert_eq!(pinned.resolved.kind, RoutingResolvedKind::PinnedRaw);
    assert!(pinned.resolved.alias.is_none());
    assert_eq!(
        pinned.resolved.mappings[0].upstream_model,
        "vendor/raw-model"
    );

    let same = explain_at(&world, "same-name", RoutingClientProtocol::ChatCompletions).unwrap();
    assert_eq!(same.resolved.kind, RoutingResolvedKind::PinnedRaw);
    assert!(same.resolved.alias.is_none());
    assert_eq!(same.resolved.mappings[0].upstream_model, "same-name");

    let disabled = explain_at(
        &world,
        "disabled-alias",
        RoutingClientProtocol::ChatCompletions,
    )
    .unwrap();
    assert_eq!(disabled.resolved.kind, RoutingResolvedKind::Alias);
    assert!(
        disabled.resolved.mappings.iter().any(|mapping| {
            mapping.upstream_model == "vendor/disabled-raw" && !mapping.routeable
        })
    );

    let unknown = explain_at(
        &world,
        "missing-model",
        RoutingClientProtocol::ChatCompletions,
    )
    .unwrap_err();
    let ambiguous = explain_at(
        &world,
        "vendor/shared-raw",
        RoutingClientProtocol::ChatCompletions,
    )
    .unwrap_err();
    for error in [unknown, ambiguous] {
        let (status, body) = error_json(error);
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(body["message"], "model is unknown or ambiguous");
    }
    assert_eq!(changes(&world), writes);
    assert_eq!(world.decrypts.load(Ordering::SeqCst), 0);
    for body in [&alias, &pinned, &same, &disabled] {
        assert!(body.expected_base_policy_first_pick.is_none());
        assert_redacted(body);
    }
}

#[test]
fn failed_apply_keeps_structured_applied_routes_apart_from_desired() {
    let world = client_world("apply_failed");
    let body = explain_at(&world, "goat-model", RoutingClientProtocol::ChatCompletions).unwrap();
    assert_not_sendable(&body);
    assert_stored_tuple(&body, "apply_failed");
    assert!(!body.owned_projection.stopped);
    assert!(!body.owned_projection.state_changed);
    let desired: Vec<&str> = body
        .desired_routes
        .iter()
        .map(|route| route.authority.public_model.as_str())
        .collect();
    assert_eq!(desired, vec!["desired-only"]);
    assert!(
        body.exclusions
            .iter()
            .all(|item| item.authority.as_ref().unwrap().public_model != "desired-only")
    );
    let goat = authority(&body, "cred-goat");
    assert_eq!(goat.public_model, "goat-model");
    assert_eq!(goat.plane, RoutingRoutePlane::Applied);
    assert_eq!(goat.quota.state, RoutingQuotaState::Unknown);
    assert!(goat.quota.evidence.is_empty());
    assert!(body.resolved.mappings.iter().any(|mapping| {
        mapping.destination_id == "dest-goat"
            && mapping.provider_id == COMMAND_CODE_PROVIDER_ID
            && mapping.upstream_model == "goat-model"
            && mapping.adapter_kind == "goat"
            && mapping.routeable
    }));
    assert_eq!(body.runtime_only_uncertainty, quiet_uncertainties(true));
    assert_legacy_absent(&body);
    assert_redacted(&body);
    assert_eq!(column_rank(&world, "cred-goat"), 99);
}

#[test]
fn stored_rank_channel_and_legacy_ids_are_not_a_pick() {
    let world = client_world("apply_failed");
    let body = explain_at(&world, "goat-model", RoutingClientProtocol::ChatCompletions).unwrap();
    assert_not_sendable(&body);
    assert_eq!(
        body.exclusions[0].authority.as_ref().unwrap().routing_rank,
        9
    );
    assert_eq!(body.exclusions[0].account_id.as_deref(), Some("acct-goat"));
    let ranks: Vec<u32> = body
        .exclusions
        .iter()
        .map(|item| item.authority.as_ref().unwrap().routing_rank)
        .collect();
    let zen_at = ranks.iter().position(|rank| *rank == 1).unwrap();
    assert!(ranks[..zen_at].iter().all(|rank| *rank == 9));

    let goat = authority(&body, "cred-goat");
    assert_eq!(
        goat.posture,
        RoutingRoutePosture::Client,
        "goat fixture is not client: exclusions={:?} material={:?} endpoint={} origin={}",
        goat.exclusions,
        goat.material,
        goat.endpoint_id,
        goat.origin
    );
    assert!(goat.exclusions.is_empty(), "{:?}", goat.exclusions);
    assert!(goat.client_configuration_eligible);
    assert!(goat.caller_pending && goat.send_pending && goat.secret_recheck_pending);
    assert_eq!(goat.channel, RoutingChannel::Go);
    assert_eq!(goat.adapter_kind, "goat");
    assert_eq!(goat.material, RoutingMaterialFact::KeyedUnchecked);
    assert_eq!(goat.legacy_account_id, "acct-goat");
    assert_eq!(goat.registration_epoch, Some(4));
    assert_eq!(goat.current_version, Some(3));
    assert_eq!(goat.credential_version, 3);
    assert!(!goat.endpoint_fingerprint.is_empty());
    assert!(
        serde_json::to_string(&body)
            .unwrap()
            .contains(&goat.endpoint_fingerprint)
    );
    let goat_codes = codes(&body, "cred-goat");
    assert_cold_codes(&goat_codes);
    assert!(!goat_codes.contains(&RoutingExclusionCode::Stopped));
    assert!(!goat_codes.contains(&RoutingExclusionCode::StateChanged));

    let zen = authority(&body, "cred-zen");
    assert_eq!(zen.channel, RoutingChannel::Free);
    assert_eq!(zen.adapter_kind, "zen");
    assert_eq!(zen.material, RoutingMaterialFact::HttpNone);
    assert!(!zen.secret_recheck_pending);
    assert_eq!(zen.routing_rank, 1);
    assert_eq!(zen.legacy_account_id, "acct-zen");
    assert_eq!(column_rank(&world, "cred-goat"), 99);
    assert_eq!(column_rank(&world, "cred-zen"), 99);
    assert_legacy_absent(&body);
    assert_redacted(&body);
}

#[test]
fn validation_only_held_model_is_not_public_eligible() {
    let world = client_world("apply_failed");
    let body = explain_at(&world, "held-model", RoutingClientProtocol::ChatCompletions).unwrap();
    let held = authority_for(&body, "cred-goat", "held-model");
    assert!(held.validation_only);
    assert_eq!(held.posture, RoutingRoutePosture::ValidationOnly);
    assert!(!held.client_configuration_eligible);
    assert!(body.exclusions.iter().any(|item| {
        item.code == RoutingExclusionCode::ValidationOnly
            && item.authority.as_ref().is_some_and(|authority| {
                authority.credential_id == "cred-goat" && authority.public_model == "held-model"
            })
    }));
    assert!(body.eligible.is_empty());
    assert!(body.expected_base_policy_first_pick.is_none());
    assert_redacted(&body);
}

#[test]
fn stale_grant_keeps_version_identity_and_epoch() {
    let world = stale_world();
    let body = explain_at(
        &world,
        "stale-public",
        RoutingClientProtocol::ChatCompletions,
    )
    .unwrap();
    let stale = authority(&body, "cred-stale");
    let found = codes(&body, "cred-stale");
    assert!(found.contains(&RoutingExclusionCode::Version), "{found:?}");
    assert!(found.contains(&RoutingExclusionCode::Identity), "{found:?}");
    assert!(stale.exclusions.contains(&RoutingExclusionCode::Version));
    assert!(stale.exclusions.contains(&RoutingExclusionCode::Identity));
    assert_eq!(stale.registration_epoch, Some(4));
    assert_eq!(stale.current_version, Some(3));
    assert_eq!(stale.credential_version, 2);
    assert_eq!(stale.legacy_account_id, "acct-stale");
    assert!(!stale.client_configuration_eligible);
    assert!(body.eligible.is_empty());
    assert_redacted(&body);
}

#[test]
fn native_present_pending_remote_and_owned_pool_are_structured() {
    let world = native_world();
    let writes = changes(&world);
    let decrypts = world.decrypts.load(Ordering::SeqCst);
    let native_body =
        explain_at(&world, "codex-test", RoutingClientProtocol::ChatCompletions).unwrap();
    let remote_body = explain_at(
        &world,
        "remote-model",
        RoutingClientProtocol::ChatCompletions,
    )
    .unwrap();

    assert_not_sendable(&native_body);
    assert_not_sendable(&remote_body);
    assert_eq!(
        native_body.runtime_only_uncertainty,
        quiet_uncertainties(false)
    );
    assert_eq!(
        remote_body.runtime_only_uncertainty,
        quiet_uncertainties(false)
    );
    assert!(!native_body.owned_projection.state_changed);
    let native = authority(&native_body, "cred-native");
    assert_eq!(
        native.posture,
        RoutingRoutePosture::Client,
        "{:?}",
        native.exclusions
    );
    assert_eq!(native.material, RoutingMaterialFact::NativePresent);
    assert_eq!(native.channel, RoutingChannel::Go);
    assert_eq!(native.adapter_kind, "cpa");
    assert_eq!(
        native.historical_placement,
        RoutingHistoricalPlacement::OwnedPool
    );
    assert!(!native.migration_required);
    assert!(native_body.resolved.mappings.iter().any(|mapping| {
        mapping.destination_id == "dest-native"
            && mapping.adapter_kind == "cpa"
            && !mapping.migration_required
    }));
    assert!(!native.secret_recheck_pending);
    assert!(native.caller_pending && native.send_pending);
    assert!(native.client_configuration_eligible);
    assert!(native.capability_listed);
    assert!(native.grants_cover);
    assert!(native.native_operations.len() == 7);
    let kinds: Vec<&str> = native
        .native_operations
        .iter()
        .map(|fact| fact.generation_kind.as_str())
        .collect();
    assert_eq!(
        kinds,
        vec![
            "execute",
            "refresh-resend",
            "stream",
            "stream-refresh",
            "stream-bootstrap",
            "internal",
            "count-tokens",
        ]
    );
    assert!(matches!(
        operation(native, "count-tokens").disposition,
        RoutingGrantDisposition::LocalOnly
    ));
    match &operation(native, "execute").disposition {
        RoutingGrantDisposition::Granted { pins } => {
            assert!(!pins.is_empty());
            assert!(pins.iter().all(|pin| pin.http_method == "POST"));
            assert!(pins.iter().any(|pin| !pin.endpoint_fingerprint.is_empty()));
        }
        other => panic!("execute pins lost: {other:?}"),
    }

    let pending = authority(&native_body, "cred-pending");
    assert_eq!(pending.material, RoutingMaterialFact::Unproven);
    assert!(codes(&native_body, "cred-pending").contains(&RoutingExclusionCode::NativePresence));
    assert!(matches!(
        operation(pending, "count-tokens").disposition,
        RoutingGrantDisposition::Unavailable
    ));
    assert!(!matches!(
        operation(pending, "count-tokens").disposition,
        RoutingGrantDisposition::LocalOnly
    ));

    let remote = authority(&remote_body, "cred-remote");
    assert_eq!(
        remote.historical_placement,
        RoutingHistoricalPlacement::Remote
    );
    assert!(remote.migration_required);
    assert!(remote_body.resolved.mappings.iter().any(|mapping| {
        mapping.destination_id == "dest-remote" && mapping.migration_required && !mapping.routeable
    }));
    assert!(!remote.client_configuration_eligible);
    assert!(codes(&remote_body, "cred-remote").contains(&RoutingExclusionCode::MigrationRequired));
    assert!(remote_body.eligible.is_empty());
    assert_eq!(changes(&world), writes);
    assert_eq!(world.decrypts.load(Ordering::SeqCst), decrypts);
    assert_legacy_absent(&native_body);
    assert_redacted(&native_body);
    assert_redacted(&remote_body);
}

#[tokio::test]
async fn catalog_only_remote_mapping_keeps_migration_without_routes() {
    let world = omitted_remote_world();
    let state = world.state().clone();
    let revision = state.settings_revision();
    let generation = state.process_generation();
    let settings = settings_snapshot(&world);
    let ciphers = cipher_snapshot(&world);
    let recovery = recovery_snapshot(&world);
    let grants = grant_snapshot(&world);
    let logs = log_count(&world);
    let selector = format!("{:?}", state.routing);
    let decrypts = world.decrypts.load(Ordering::SeqCst);
    let writes = changes(&world);
    let uri = "/dashboard/api/v4/routing/explain?model=ocg-omitted-remote&clientProtocol=chat_completions";

    let first = handler_explain(&state, uri).await.unwrap();
    let second = handler_explain(&state, uri).await.unwrap();

    for body in [&first, &second] {
        assert_eq!(body.resolved.kind, RoutingResolvedKind::PinnedRaw);
        assert!(body.resolved.alias.is_none());
        assert!(body.expected_base_policy_first_pick.is_none());
        assert!(body.eligible.is_empty());
        assert!(body.exclusions.is_empty());
        assert!(body.desired_routes.is_empty());
        let mapping = body
            .resolved
            .mappings
            .iter()
            .find(|mapping| mapping.destination_id == "dest-omitted-remote")
            .expect("catalog mapping");
        assert_eq!(mapping.provider_id, CPA_PROVIDER_ID);
        assert_eq!(mapping.upstream_model, "ocg-omitted-remote");
        assert_eq!(mapping.adapter_kind, "cpa");
        assert!(!mapping.routeable);
        assert!(mapping.migration_required);
        let wire = serde_json::to_value(body).unwrap();
        assert_eq!(
            wire["resolved"]["mappings"]
                .as_array()
                .unwrap()
                .iter()
                .find(|item| item["destinationId"] == "dest-omitted-remote")
                .unwrap()["migrationRequired"],
            true
        );
        assert!(!wire.to_string().contains("omitted.example"));
        assert_redacted(body);
    }
    let mut comparable_first = first.clone();
    let mut comparable_second = second.clone();
    comparable_first.observed_at.clear();
    comparable_second.observed_at.clear();
    assert_eq!(comparable_first, comparable_second);
    assert_eq!(settings_snapshot(&world), settings);
    assert_eq!(cipher_snapshot(&world), ciphers);
    assert_eq!(recovery_snapshot(&world), recovery);
    assert_eq!(grant_snapshot(&world), grants);
    assert_eq!(log_count(&world), logs);
    assert_eq!(format!("{:?}", state.routing), selector);
    assert_eq!(state.settings_revision(), revision);
    assert_eq!(state.process_generation(), generation);
    assert_eq!(changes(&world), writes);
    assert_eq!(world.decrypts.load(Ordering::SeqCst), decrypts);
    assert_eq!(decrypts, 0);
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
fn quota_known_expired_unknown_malformed_and_recovery_stay_distinct() {
    let world = client_world("apply_failed");
    let missing = explain_at(&world, "goat-model", RoutingClientProtocol::ChatCompletions).unwrap();
    assert_eq!(
        authority(&missing, "cred-goat").quota.state,
        RoutingQuotaState::Unknown
    );
    assert!(!codes(&missing, "cred-goat").contains(&RoutingExclusionCode::QuotaKnownReset));
    assert!(!codes(&missing, "cred-goat").contains(&RoutingExclusionCode::QuotaUnknownReset));
    assert!(!codes(&missing, "cred-goat").contains(&RoutingExclusionCode::QuotaMalformed));
    assert!(setting(&world, SETTINGS_KEY).is_none());

    let known_json = known_policy(future(), "obs-known");
    write_policy(&world, &known_json);
    let writes = changes(&world);
    let known = explain_at(&world, "goat-model", RoutingClientProtocol::ChatCompletions).unwrap();
    let goat = authority(&known, "cred-goat");
    assert_eq!(goat.quota.state, RoutingQuotaState::Evidence);
    assert!(goat.known_restriction_blocks);
    assert!(!goat.trial_pending);
    assert!(!goat.client_configuration_eligible);
    let evidence = &goat.quota.evidence[0];
    assert!(evidence.applicable);
    assert_eq!(evidence.reset, "known");
    assert_eq!(evidence.window, "five_hours");
    assert_eq!(evidence.source, "goat_plan");
    assert_eq!(evidence.subject_kind, "credential");
    assert_eq!(evidence.credential_id.as_deref(), Some("cred-goat"));
    assert!(codes(&known, "cred-goat").contains(&RoutingExclusionCode::QuotaKnownReset));
    assert!(!codes(&known, "cred-zen").contains(&RoutingExclusionCode::QuotaKnownReset));
    assert_eq!(
        authority(&known, "cred-zen").quota.state,
        RoutingQuotaState::Unknown
    );
    assert!(
        known
            .desired_routes
            .iter()
            .any(|route| route.authority.public_model == "desired-only")
    );
    assert_eq!(
        setting(&world, SETTINGS_KEY).as_deref(),
        Some(known_json.as_str())
    );
    assert_eq!(changes(&world), writes);
    assert_eq!(known.runtime_only_uncertainty, quiet_uncertainties(true));

    let expired_json = known_policy(past(), "obs-expired");
    write_policy(&world, &expired_json);
    let expired = explain_at(&world, "goat-model", RoutingClientProtocol::ChatCompletions).unwrap();
    let expired_row = &authority(&expired, "cred-goat").quota.evidence[0];
    assert!(!expired_row.applicable);
    assert_eq!(expired_row.reset, "expired");
    assert!(expired_row.reset_at.is_some());
    assert!(!codes(&expired, "cred-goat").contains(&RoutingExclusionCode::QuotaKnownReset));
    assert_eq!(
        setting(&world, SETTINGS_KEY).as_deref(),
        Some(expired_json.as_str())
    );

    let unknown_json = unknown_policy();
    write_policy(&world, &unknown_json);
    let unknown = explain_at(&world, "goat-model", RoutingClientProtocol::ChatCompletions).unwrap();
    let unknown_auth = authority(&unknown, "cred-goat");
    assert!(unknown_auth.trial_pending);
    assert!(!unknown_auth.known_restriction_blocks);
    assert_eq!(unknown_auth.quota.evidence[0].reset, "unknown_reset");
    assert!(codes(&unknown, "cred-goat").contains(&RoutingExclusionCode::QuotaUnknownReset));
    let unknown_json_body = serde_json::to_string(&unknown).unwrap();
    assert!(!unknown_json_body.contains("inflight"));
    assert!(!unknown_json_body.contains("\"requests\""));
    assert_eq!(
        setting(&world, SETTINGS_KEY).as_deref(),
        Some(unknown_json.as_str())
    );

    write_policy(&world, "{");
    let malformed =
        explain_at(&world, "goat-model", RoutingClientProtocol::ChatCompletions).unwrap();
    assert!(malformed.owned_projection.policy_malformed);
    assert_eq!(
        authority(&malformed, "cred-goat").quota.state,
        RoutingQuotaState::Malformed
    );
    assert!(authority(&malformed, "cred-goat").quota.evidence.is_empty());
    assert!(codes(&malformed, "cred-goat").contains(&RoutingExclusionCode::QuotaMalformed));
    assert!(codes(&malformed, "cred-goat").contains(&RoutingExclusionCode::PolicyMalformed));
    assert_ne!(
        authority(&malformed, "cred-goat").quota.state,
        RoutingQuotaState::Unknown
    );
    assert_eq!(setting(&world, SETTINGS_KEY).as_deref(), Some("{"));

    {
        let db = world.state().db.lock();
        db.conn
            .execute(
                "UPDATE credentials SET quota_recovery_json = ?2 WHERE id = ?1",
                params!["cred-goat", r#"{"status":"quota_waiting_probe"}"#],
            )
            .unwrap();
    }
    let recovery_before = recovery_snapshot(&world);
    let probed = explain_at(&world, "goat-model", RoutingClientProtocol::ChatCompletions).unwrap();
    assert_eq!(recovery_snapshot(&world), recovery_before);
    assert_legacy_absent(&probed);
    assert_eq!(world.decrypts.load(Ordering::SeqCst), 0);
    for body in [&missing, &known, &expired, &unknown, &malformed, &probed] {
        assert!(body.eligible.is_empty());
        assert!(body.expected_base_policy_first_pick.is_none());
        assert_redacted(body);
    }
}

#[test]
fn stopped_exited_and_capability_disagreement_stay_distinct() {
    let stopped = client_world("stopped");
    let stopped_body = explain_at(
        &stopped,
        "goat-model",
        RoutingClientProtocol::ChatCompletions,
    )
    .unwrap();
    assert_eq!(stopped_body.owned_projection.apply_status, "stopped");
    assert!(stopped_body.owned_projection.stopped);
    assert!(codes(&stopped_body, "cred-goat").contains(&RoutingExclusionCode::Stopped));
    assert!(stopped_body.eligible.is_empty());
    assert_redacted(&stopped_body);

    let exited = client_world("applied");
    let record = setting(&exited, EXECUTION_KEY).unwrap();
    assert!(record.contains(NATIVE_ENDPOINT_PIN_CAPABILITY));
    let exited_body = explain_at(
        &exited,
        "goat-model",
        RoutingClientProtocol::ChatCompletions,
    )
    .unwrap();
    assert_not_sendable(&exited_body);
    assert_stored_tuple(&exited_body, "applied");
    assert!(!exited_body.owned_projection.stopped);
    assert!(!exited_body.owned_projection.state_changed);
    assert!(!codes(&exited_body, "cred-goat").contains(&RoutingExclusionCode::Stopped));
    let goat = authority(&exited_body, "cred-goat");
    assert!(goat.capability_listed);
    assert!(goat.client_configuration_eligible);
    assert!(exited_body.eligible.is_empty());
    assert_redacted(&exited_body);

    let disagreed = client_world("applied");
    {
        let db = disagreed.state().db.lock();
        let raw: String = db
            .conn
            .query_row(
                "SELECT value FROM settings WHERE key = ?1",
                [EXECUTION_KEY],
                |row| row.get(0),
            )
            .unwrap();
        let mut value: serde_json::Value = serde_json::from_str(&raw).unwrap();
        value["hostCapabilities"] =
            serde_json::json!([NATIVE_ENDPOINT_PIN_CAPABILITY, "capability-list-disagrees"]);
        save_setting(&db.conn, EXECUTION_KEY, &value.to_string());
    }
    let revision = disagreed.state().settings_revision();
    let writes = changes(&disagreed);
    let body = explain_at(
        &disagreed,
        "goat-model",
        RoutingClientProtocol::ChatCompletions,
    )
    .unwrap();
    assert!(body.owned_projection.state_changed);
    assert!(!body.owned_projection.unavailable);
    assert!(!body.owned_projection.verified_ready);
    assert!(codes(&body, "cred-goat").contains(&RoutingExclusionCode::StateChanged));
    assert!(
        body.runtime_only_uncertainty
            .contains(&RuntimeOnlyUncertainty::StateChangedAfterSnapshot)
    );
    assert!(!authority(&body, "cred-goat").client_configuration_eligible);
    assert!(authority(&body, "cred-goat").credential_id == "cred-goat");
    assert!(body.eligible.is_empty());
    assert_eq!(disagreed.state().settings_revision(), revision);
    assert_eq!(changes(&disagreed), writes);
    assert_redacted(&body);
}

/// Ready bits below are predicate inputs. They are not a product ready setter
/// and they are not a live CLI Ready host.
#[test]
fn selected_raw_collision_input_cannot_borrow_alias_authority() {
    let alias = collision_facts(
        collision_route("ocg-collision-bar", false),
        false,
        ready_input_runtime(false),
    );
    let discarded = explanation_from_facts(
        &alias,
        RoutingClientProtocol::ChatCompletions,
        QueryResolutionKind::PinnedRaw,
    );
    assert!(discarded.eligible.is_empty());
    assert!(discarded.expected_base_policy_first_pick.is_none());
    assert_eq!(
        discarded.conversation_binding,
        RoutingConversationBinding::NotEvaluated
    );
    assert_eq!(discarded.revision.revision, 41);
    assert_eq!(discarded.routing_mode, RoutingModeDto::StrictPriority);
    assert!(!discarded.conversation_sticky);
    let resolved = &discarded.resolved.mappings[0];
    assert_eq!(resolved.upstream_model, "ocg-collision-foo");
    assert!(!resolved.routeable);
    let model_exclusion = discarded
        .exclusions
        .iter()
        .find(|item| item.code == RoutingExclusionCode::Model)
        .expect("model exclusion");
    let authority = model_exclusion.authority.as_ref().expect("authority");
    assert_eq!(authority.public_model, "ocg-collision-foo");
    assert_eq!(authority.upstream_model, "ocg-collision-bar");
    assert_eq!(authority.destination_id, "dest-collision");
    assert_eq!(authority.provider_id, CPA_PROVIDER_ID);
    assert_eq!(
        model_exclusion.upstream_model.as_deref(),
        Some("ocg-collision-bar")
    );

    let trap = collision_facts(
        collision_route("ocg-collision-bar", false),
        true,
        ready_input_runtime(false),
    );
    let borrowed = explanation_from_facts(
        &trap,
        RoutingClientProtocol::ChatCompletions,
        QueryResolutionKind::PinnedRaw,
    );
    assert!(borrowed.eligible.is_empty());
    assert!(
        borrowed
            .exclusions
            .iter()
            .any(|item| item.code == RoutingExclusionCode::Model)
    );

    let matched = collision_facts(
        collision_route("ocg-collision-foo", false),
        true,
        ready_input_runtime(false),
    );
    let positive = explanation_from_facts(
        &matched,
        RoutingClientProtocol::ChatCompletions,
        QueryResolutionKind::PinnedRaw,
    );
    assert!(positive.exclusions.is_empty());
    assert_eq!(positive.eligible.len(), 1);
    let candidate = &positive.eligible[0];
    assert_eq!(candidate.resolved_model, "ocg-collision-foo");
    assert_eq!(candidate.provider_id, CPA_PROVIDER_ID);
    assert_eq!(candidate.routing_rank, 4);
    assert!(candidate.authority.caller_pending);
    assert!(candidate.authority.send_pending);
    assert!(!candidate.authority.secret_recheck_pending);
    assert_eq!(candidate.authority.quota.state, RoutingQuotaState::Unknown);
    assert!(positive.resolved.mappings[0].routeable);

    let held = collision_facts(
        collision_route("ocg-collision-foo", true),
        false,
        ready_input_runtime(false),
    );
    let validation = explanation_from_facts(
        &held,
        RoutingClientProtocol::ChatCompletions,
        QueryResolutionKind::PinnedRaw,
    );
    assert!(validation.eligible.is_empty());
    let held_codes: Vec<_> = validation.exclusions.iter().map(|item| item.code).collect();
    assert!(held_codes.contains(&RoutingExclusionCode::ValidationOnly));
    assert!(!held_codes.contains(&RoutingExclusionCode::Model));

    let drifted = collision_facts(
        collision_route("ocg-collision-foo", false),
        true,
        ready_input_runtime(true),
    );
    let changed = explanation_from_facts(
        &drifted,
        RoutingClientProtocol::ChatCompletions,
        QueryResolutionKind::PinnedRaw,
    );
    assert!(changed.eligible.is_empty());
    let changed_codes: Vec<_> = changed.exclusions.iter().map(|item| item.code).collect();
    assert!(changed_codes.contains(&RoutingExclusionCode::StateChanged));
    assert!(!changed_codes.contains(&RoutingExclusionCode::Model));
}

#[test]
fn captured_facts_keep_revision_and_mode_after_settings_mutation() {
    let world = alias_world();
    let state = world.state();
    let mut config = state.config();
    if config.gateway_key.trim().is_empty() {
        config.gateway_key = "routing-explain-gateway-key".into();
    }
    config.routing_mode = RoutingMode::StrictPriority;
    config.conversation_sticky = false;
    state.set_config(config).unwrap();
    let captured_pricing = state.pricing_snapshot().revision.clone();
    let facts = explain_owned_routes(state, "plain-alias", "chat_completions", clock()).unwrap();
    let captured_revision = facts.captured.settings_revision;
    let captured_generation = facts.captured.process_generation;
    assert_eq!(captured_revision, state.settings_revision());
    assert_eq!(captured_generation, state.process_generation());
    assert_eq!(facts.captured.pricing_revision, captured_pricing);
    assert_eq!(facts.captured.routing_mode, RoutingMode::StrictPriority);
    assert!(!facts.captured.conversation_sticky);
    let kind = facts.resolution.kind.expect("known alias");

    let mut mutated = state.config();
    mutated.routing_mode = RoutingMode::RoundRobin;
    mutated.conversation_sticky = true;
    state.set_config(mutated).unwrap();
    assert!(state.settings_revision() > captured_revision);

    let body = explanation_from_facts(&facts, RoutingClientProtocol::ChatCompletions, kind);
    assert_eq!(body.revision.revision, captured_revision);
    assert_eq!(body.revision.process_generation, captured_generation);
    assert_eq!(body.revision.pricing_revision, captured_pricing);
    assert_ne!(body.revision, ControlRevision::from_state(state));
    assert_eq!(body.routing_mode, RoutingModeDto::StrictPriority);
    assert!(!body.conversation_sticky);
    assert!(body.eligible.is_empty());

    let fresh = explain_at(
        &world,
        "plain-alias",
        RoutingClientProtocol::ChatCompletions,
    )
    .unwrap();
    assert_eq!(fresh.routing_mode, RoutingModeDto::RoundRobin);
    assert!(fresh.conversation_sticky);
    assert_eq!(fresh.revision, ControlRevision::from_state(state));
    assert!(fresh.eligible.is_empty());
}

fn collision_route(upstream: &str, validation_only: bool) -> RouteFact {
    RouteFact {
        plane: RoutePlane::Applied,
        credential_id: "cred-collision".into(),
        credential_version: 3,
        current_version: Some(3),
        provider_id: CPA_PROVIDER_ID.into(),
        binding_id: "bind-collision".into(),
        auth_id: "auth-collision".into(),
        registration_epoch: Some(1),
        routing_rank: 4,
        destination_id: "dest-collision".into(),
        legacy_account_id: "acct-collision".into(),
        account_label: "Collision".into(),
        destination_label: "Collision Dest".into(),
        public_model: "ocg-collision-foo".into(),
        upstream_model: upstream.into(),
        spelling: if upstream == "ocg-collision-foo" {
            Spelling::Same
        } else {
            Spelling::DistinctUpstream
        },
        protocol: "chat_completions".into(),
        endpoint_id: "endpoint-collision".into(),
        origin: "https://owned.example".into(),
        endpoint_fingerprint: "fingerprint-collision".into(),
        validation_only,
        channel: RoutingProductChannel::Go,
        material: MaterialFact::HttpNone,
        posture: RoutePosture::Client,
        exclusions: Vec::new(),
        credential_enabled: true,
        binding_enabled: true,
        destination_enabled: true,
        destination_draft: false,
        setup_step: "ready".into(),
        native_provider: String::new(),
        native_mode: String::new(),
        capability_listed: true,
        grants_cover: true,
        native_operations: Vec::new(),
        caller_pending: true,
        secret_recheck_pending: false,
        send_pending: true,
        quota: ScopedQuotaView::Unknown,
        known_restriction_blocks: false,
        trial_pending: false,
        client_configuration_eligible: !validation_only,
        adapter_kind: "cpa".into(),
        migration_required: false,
        historical_placement: HistoricalPlacement::NotApplicable,
    }
}

fn ready_input_runtime(state_changed: bool) -> RuntimeFacts {
    RuntimeFacts {
        poisoned: false,
        state_changed,
        stopped: false,
        origin_verified: true,
        verified_ready: true,
        policy_ready: true,
        policy_malformed: false,
        tuple_aligned: true,
        pin_capabilities_ready: true,
        apply_status: "current".into(),
        unavailable: false,
        owned_running_before: true,
        owned_running_after: true,
        owned_running: true,
    }
}

fn collision_facts(route: RouteFact, routeable: bool, runtime: RuntimeFacts) -> OwnedRoutingFacts {
    OwnedRoutingFacts {
        requested_model: "ocg-collision-foo".into(),
        callable_protocol: "chat_completions".into(),
        evaluated_at: clock(),
        projection: OwnedProjectionFacts {
            desired: PlaneTuple {
                generation: 0,
                revision: 0,
                digest: String::new(),
            },
            applied: PlaneTuple {
                generation: 0,
                revision: 0,
                digest: String::new(),
            },
            apply_status: "current".into(),
            desired_running: false,
            runtime_child_generation: 0,
        },
        runtime,
        resolution: QueryResolution {
            known: true,
            kind: Some(QueryResolutionKind::PinnedRaw),
            alias: None,
            ambiguous: false,
            mappings: vec![QueryMapping {
                destination_id: "dest-collision".into(),
                provider_id: CPA_PROVIDER_ID.into(),
                upstream_model: "ocg-collision-foo".into(),
                routeable,
                migration_required: false,
                adapter_kind: "cpa".into(),
            }],
        },
        desired: Vec::new(),
        applied: vec![route],
        first_pick: None,
        conversation_binding: "not_evaluated".into(),
        uncertainties: Vec::new(),
        captured: CapturedResponseMeta {
            settings_revision: 41,
            process_generation: 17,
            pricing_revision: "price-captured".into(),
            routing_mode: RoutingMode::StrictPriority,
            conversation_sticky: false,
        },
    }
}
