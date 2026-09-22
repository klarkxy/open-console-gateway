use super::{replace_http_catalog_on, replace_http_destination_on};
use crate::db::Database;
use crate::destination_projection::load_runtime;
use ocg_domain::catalog::UpstreamProtocolKind;
use ocg_domain::connection::{LegacyConnectionKind, connection_id_for_legacy};
use ocg_domain::credential::{
    RouteSpec, assigned_endpoints_for_routes, credential_id_for_legacy_account,
};
use ocg_domain::destination::{
    AdapterKind, AuthScheme, CatalogModel, ModelResolution, Protocol, destination_id_for_builtin,
    http_configured_routes, sealed_capabilities,
};
use ocg_domain::dynamic::{
    DynamicAuthKind, DynamicModelMapping, DynamicModelUpstreamOverride, DynamicProviderDefinition,
};
use ocg_domain::ids::OPENCODE_ZEN_FREE_PROVIDER_ID;
use rusqlite::params;
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
        "ocg-dest-cmd-{label}-{}",
        uuid::Uuid::new_v4().simple()
    ));
    std::fs::create_dir_all(&dir).expect("test data dir");
    Opened {
        db: Some(Database::open(dir.clone()).expect("database should open")),
        dir,
    }
}

struct Seed {
    destination_id: String,
    credential_id: String,
    account_id: String,
    connection_id: ocg_domain::connection::ConnectionId,
}

fn mapping(public_model: &str, upstream_model: &str) -> DynamicModelMapping {
    DynamicModelMapping {
        public_model: public_model.into(),
        upstream_model: upstream_model.into(),
        upstream_override: None,
    }
}

fn catalog_row(
    public_model: &str,
    upstream_model: &str,
    protocol: Protocol,
    enabled: bool,
) -> CatalogModel {
    CatalogModel {
        public_model: public_model.into(),
        upstream_model: upstream_model.into(),
        protocols: vec![protocol],
        preferred: Some(protocol),
        enabled,
        upstream_override: None,
    }
}

fn definition(
    name: &str,
    endpoint: &str,
    protocol: Protocol,
    auth: DynamicAuthKind,
    mappings: Vec<DynamicModelMapping>,
) -> DynamicProviderDefinition {
    DynamicProviderDefinition {
        preset_id: None,
        id: "unused".into(),
        name: name.into(),
        endpoint_url: endpoint.into(),
        upstream_protocol: protocol,
        auth_kind: auth,
        mappings,
    }
}

#[allow(clippy::too_many_arguments)]
fn insert_destination(
    db: &Database,
    destination_id: &str,
    legacy_kind: &str,
    legacy_id: &str,
    name: &str,
    endpoint: &str,
    protocol: Protocol,
    auth: AuthScheme,
    resolution: ModelResolution,
    origin: Option<&str>,
    preset_id: Option<&str>,
    observer: bool,
    enabled: bool,
) {
    let mut capabilities = sealed_capabilities(AdapterKind::Http);
    capabilities.observer = observer;
    db.conn
        .execute(
            "INSERT INTO destinations (
                id, legacy_kind, legacy_id, adapter, name, brand_family, base_url,
                protocols_json, auth_scheme, model_resolution, capabilities_json, plan_json,
                max_credentials, observer_credential_id, enabled, origin, preset_id, updated_at
             ) VALUES (?1, ?2, ?3, 'http', ?4, NULL, ?5, ?6, ?7, ?8, ?9, NULL, NULL, NULL, ?10, ?11, ?12, ?13)",
            params![
                destination_id,
                legacy_kind,
                legacy_id,
                name,
                endpoint,
                serde_json::to_string(&[protocol]).unwrap(),
                auth.as_str(),
                resolution.as_str(),
                serde_json::to_string(&capabilities).unwrap(),
                i64::from(enabled),
                origin,
                preset_id,
                "2026-01-01T00:00:00Z",
            ],
        )
        .unwrap();
}

#[allow(clippy::too_many_arguments)]
fn insert_credential(
    db: &Database,
    destination_id: &str,
    account_id: &str,
    connection_id: &str,
    key_cipher: &str,
    kind: &str,
    rank: i64,
    cooldown: Option<&str>,
    scope_json: &str,
) -> String {
    let credential_id = credential_id_for_legacy_account(account_id)
        .as_str()
        .to_string();
    db.conn
        .execute(
            "INSERT INTO credentials (
                id, legacy_account_id, destination_id, name, has_secret, enabled, routing_rank,
                scope_json, auth_state, key_cipher, provider_id, credential_kind, quota_scope,
                account_type, setup_step, verification_status, credential_version,
                authorization_connection_id, cooldown_generic_until, created_at, updated_at
             ) VALUES (
                ?1, ?2, ?3, ?2, ?4, 1, ?5, ?6, 'unknown', ?7, 'custom', ?8, 'key',
                'key', 'ready', 'verified', 3, ?9, ?10, '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z'
             )",
            params![
                credential_id,
                account_id,
                destination_id,
                i64::from(!key_cipher.is_empty()),
                rank,
                scope_json,
                key_cipher,
                kind,
                connection_id,
                cooldown,
            ],
        )
        .unwrap();
    credential_id
}

fn seed(
    db: &Database,
    legacy_kind: &str,
    legacy_id: &str,
    resolution: ModelResolution,
    models: &[CatalogModel],
) -> Seed {
    let destination_id = format!("dest-{legacy_id}");
    let account_id = format!("acct-{legacy_id}");
    let connection_id = connection_id_for_legacy(
        if legacy_kind == "dynamic" {
            LegacyConnectionKind::DynamicProvider
        } else {
            LegacyConnectionKind::CustomAccount
        },
        legacy_id,
    );
    insert_destination(
        db,
        &destination_id,
        legacy_kind,
        legacy_id,
        "Before",
        "https://before.example/v1",
        UpstreamProtocolKind::ChatCompletions,
        AuthScheme::Bearer,
        resolution,
        Some("custom"),
        Some("preset-keep"),
        false,
        true,
    );
    crate::db::destination_store::replace_destination_catalog(&db.conn, &destination_id, models)
        .unwrap();
    let credential_id = insert_credential(
        db,
        &destination_id,
        &account_id,
        connection_id.as_str(),
        "cipher",
        "api_key",
        0,
        Some("2099-01-01T00:00:00Z"),
        r#"{"kind":"only","models":["keep-me"]}"#,
    );
    Seed {
        destination_id,
        credential_id,
        account_id,
        connection_id,
    }
}

fn loaded_dest<'a>(
    projection: &'a crate::destination_projection::DestinationProjection,
    id: &str,
) -> &'a ocg_domain::destination::Destination {
    projection
        .destinations
        .iter()
        .find(|destination| destination.id == id)
        .expect("destination")
}

fn model_row(
    db: &Database,
    destination_id: &str,
    public_model: &str,
) -> (bool, Option<String>, String, String) {
    db.conn
        .query_row(
            "SELECT enabled, preferred, protocols_json, upstream_model
             FROM destination_models
             WHERE destination_id = ?1 AND public_model_key = ?2",
            params![destination_id, public_model.to_ascii_lowercase()],
            |row| {
                Ok((
                    row.get::<_, i64>(0)? != 0,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                ))
            },
        )
        .unwrap()
}

fn grant_values(db: &Database, credential_id: &str) -> Vec<(String, String)> {
    let mut stmt = db
        .conn
        .prepare(
            "SELECT kind, value FROM credential_grants
             WHERE credential_id = ?1 ORDER BY kind, value",
        )
        .unwrap();
    stmt.query_map([credential_id], |row| Ok((row.get(0)?, row.get(1)?)))
        .unwrap()
        .map(|row| row.unwrap())
        .collect()
}

fn apply(
    db: &Database,
    destination_id: &str,
    definition: &DynamicProviderDefinition,
    authorize: &[String],
) -> anyhow::Result<()> {
    replace_http_destination_on(db, destination_id, definition, authorize)
}

fn endpoint_grant_values(db: &Database, credential_id: &str) -> Vec<String> {
    grant_values(db, credential_id)
        .into_iter()
        .filter_map(|(kind, value)| (kind == "endpoint_id").then_some(value))
        .collect()
}

fn override_model(public_model: &str, endpoint_url: &str) -> CatalogModel {
    CatalogModel {
        public_model: public_model.into(),
        upstream_model: format!("up-{public_model}"),
        protocols: vec![Protocol::Messages],
        preferred: Some(Protocol::Messages),
        enabled: true,
        upstream_override: Some(DynamicModelUpstreamOverride {
            protocol: UpstreamProtocolKind::Messages,
            endpoint_url: endpoint_url.into(),
        }),
    }
}

#[test]
fn override_grants_follow_exact_urls_across_reorder_and_removal() {
    let opened = open_db("override-grant-remap");
    let db = opened.db();
    let override_a = "https://same.example/messages-a";
    let override_c = "https://same.example/messages-c";
    let seeded = seed(
        db,
        "dynamic",
        "override-grant-remap",
        ModelResolution::PublicAndUpstream,
        &[
            override_model("model-a", override_a),
            override_model("model-c", override_c),
        ],
    );
    let before = loaded_dest(&load_runtime(db).unwrap(), &seeded.destination_id).clone();
    let old_routes = http_configured_routes(&before);
    let old_a = assigned_endpoints_for_routes(&seeded.connection_id, &old_routes)
        .into_iter()
        .zip(old_routes.iter())
        .find(|(_, route)| route.url.as_deref() == Some(override_a))
        .unwrap()
        .0
        .id;
    db.conn
        .execute(
            "INSERT INTO credential_grants (credential_id, kind, value) VALUES (?1, 'endpoint_id', ?2), (?1, 'endpoint_id', 'unknown-stale-endpoint')",
            rusqlite::params![seeded.credential_id, old_a],
        )
        .unwrap();

    let reordered = vec![
        override_model("model-c", override_c),
        override_model("model-a", override_a),
    ];
    replace_http_catalog_on(&db.conn, &before, &reordered).unwrap();
    let after_reorder = loaded_dest(&load_runtime(db).unwrap(), &seeded.destination_id).clone();
    let routes_after_reorder = http_configured_routes(&after_reorder);
    let expected_a_after_reorder =
        assigned_endpoints_for_routes(&seeded.connection_id, &routes_after_reorder)
            .into_iter()
            .zip(routes_after_reorder.iter())
            .find(|(_, route)| route.url.as_deref() == Some(override_a))
            .unwrap()
            .0
            .id;
    assert_eq!(
        endpoint_grant_values(db, &seeded.credential_id),
        vec![expected_a_after_reorder]
    );

    let only_a = vec![override_model("model-a", override_a)];
    replace_http_catalog_on(&db.conn, &after_reorder, &only_a).unwrap();
    let after_removal = loaded_dest(&load_runtime(db).unwrap(), &seeded.destination_id).clone();
    let routes_after_removal = http_configured_routes(&after_removal);
    let expected_a_after_removal =
        assigned_endpoints_for_routes(&seeded.connection_id, &routes_after_removal)
            .into_iter()
            .zip(routes_after_removal.iter())
            .find(|(_, route)| route.url.as_deref() == Some(override_a))
            .unwrap()
            .0
            .id;
    assert_eq!(
        endpoint_grant_values(db, &seeded.credential_id),
        vec![expected_a_after_removal]
    );
}

#[test]
fn dynamic_and_custom_same_mapping_update_preserve_controls() {
    for (kind, resolution, legacy) in [
        (
            "dynamic",
            ModelResolution::PublicAndUpstream,
            "11111111-1111-1111-1111-111111111111",
        ),
        (
            "custom_account",
            ModelResolution::PublicOnly,
            "22222222-2222-2222-2222-222222222222",
        ),
    ] {
        let opened = open_db(kind);
        let db = opened.db();
        let seeded = seed(
            db,
            kind,
            legacy,
            resolution,
            &[catalog_row(
                "keep",
                "up-keep",
                UpstreamProtocolKind::ChatCompletions,
                false,
            )],
        );
        apply(
            db,
            &seeded.destination_id,
            &definition(
                "After",
                "https://after.example/v1",
                UpstreamProtocolKind::ChatCompletions,
                DynamicAuthKind::Bearer,
                vec![mapping("keep", "up-keep")],
            ),
            &[],
        )
        .unwrap();
        let projection = load_runtime(db).unwrap();
        let dest = loaded_dest(&projection, &seeded.destination_id);
        assert_eq!(dest.name, "After");
        assert_eq!(dest.base_url.as_deref(), Some("https://after.example/v1"));
        assert_eq!(dest.model_resolution, resolution);
        assert!(dest.enabled);
        let (enabled, preferred, protocols, upstream) =
            model_row(db, &seeded.destination_id, "keep");
        assert!(!enabled, "{kind} must not reenable");
        assert_eq!(preferred.as_deref(), Some("chat_completions"));
        assert_eq!(
            protocols,
            serde_json::to_string(&[UpstreamProtocolKind::ChatCompletions]).unwrap()
        );
        assert_eq!(upstream, "up-keep");
        let origin: Option<String> = db
            .conn
            .query_row(
                "SELECT origin, preset_id FROM destinations WHERE id = ?1",
                [&seeded.destination_id],
                |row| {
                    Ok((
                        row.get::<_, Option<String>>(0)?,
                        row.get::<_, Option<String>>(1)?,
                    ))
                },
            )
            .unwrap()
            .0;
        assert_eq!(origin.as_deref(), Some("custom"));
        let _ = kind;
    }
}

#[test]
fn rename_does_not_reenable_or_clear_probe_evidence() {
    let opened = open_db("rename");
    let db = opened.db();
    let seeded = seed(
        db,
        "dynamic",
        "rename-id",
        ModelResolution::PublicAndUpstream,
        &[catalog_row(
            "keep",
            "up-keep",
            UpstreamProtocolKind::ChatCompletions,
            false,
        )],
    );
    db.conn
        .execute(
            "INSERT INTO provider_contract_model_protocols (
                scope_kind, scope_id, model_id, protocol, source
             ) VALUES ('custom_endpoint', ?1, 'keep', 'chat_completions', 'probe_confirmed')",
            [&seeded.account_id],
        )
        .unwrap();
    apply(
        db,
        &seeded.destination_id,
        &definition(
            "Renamed",
            "https://before.example/v1",
            UpstreamProtocolKind::ChatCompletions,
            DynamicAuthKind::Bearer,
            vec![mapping("keep", "up-keep")],
        ),
        &[],
    )
    .unwrap();
    let (enabled, _, _, _) = model_row(db, &seeded.destination_id, "keep");
    assert!(!enabled);
    let evidence: i64 = db
        .conn
        .query_row(
            "SELECT COUNT(*) FROM provider_contract_model_protocols WHERE scope_id = ?1",
            [&seeded.account_id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(evidence, 1);
}

#[test]
fn url_change_does_not_grant_without_authorize() {
    let opened = open_db("no-grant");
    let db = opened.db();
    let seeded = seed(
        db,
        "custom_account",
        "no-grant",
        ModelResolution::PublicOnly,
        &[catalog_row(
            "keep",
            "up-keep",
            UpstreamProtocolKind::ChatCompletions,
            true,
        )],
    );
    apply(
        db,
        &seeded.destination_id,
        &definition(
            "After",
            "https://after.example/v1",
            UpstreamProtocolKind::ChatCompletions,
            DynamicAuthKind::Bearer,
            vec![mapping("keep", "up-keep")],
        ),
        &[],
    )
    .unwrap();
    assert!(grant_values(db, &seeded.credential_id).is_empty());
}

#[test]
fn authorize_unions_full_routes_from_stored_connection_id() {
    let opened = open_db("grants");
    let db = opened.db();
    let seeded = seed(
        db,
        "dynamic",
        "grant-id",
        ModelResolution::PublicAndUpstream,
        &[catalog_row(
            "keep",
            "up-keep",
            UpstreamProtocolKind::ChatCompletions,
            true,
        )],
    );
    db.conn
        .execute(
            "INSERT INTO credential_grants (credential_id, kind, value) VALUES (?1, 'origin', 'https://kept.example')",
            [&seeded.credential_id],
        )
        .unwrap();
    let override_url = "https://alt.example/messages";
    apply(
        db,
        &seeded.destination_id,
        &definition(
            "After",
            "https://after.example/v1",
            UpstreamProtocolKind::ChatCompletions,
            DynamicAuthKind::Bearer,
            vec![DynamicModelMapping {
                public_model: "keep".into(),
                upstream_model: "up-keep".into(),
                upstream_override: Some(DynamicModelUpstreamOverride {
                    protocol: UpstreamProtocolKind::Messages,
                    endpoint_url: override_url.into(),
                }),
            }],
        ),
        std::slice::from_ref(&seeded.credential_id),
    )
    .unwrap();
    let assigned = assigned_endpoints_for_routes(
        &seeded.connection_id,
        &[
            RouteSpec {
                operation: ocg_domain::connection::EndpointOperation::from(
                    UpstreamProtocolKind::ChatCompletions,
                ),
                url: Some("https://after.example/v1".into()),
            },
            RouteSpec {
                operation: ocg_domain::connection::EndpointOperation::from(
                    UpstreamProtocolKind::Messages,
                ),
                url: Some(override_url.into()),
            },
        ],
    );
    let grants = grant_values(db, &seeded.credential_id);
    for endpoint in &assigned {
        assert!(
            grants
                .iter()
                .any(|(kind, value)| kind == "endpoint_id" && value == &endpoint.id),
            "missing endpoint grant {}",
            endpoint.id
        );
    }
    assert!(
        grants
            .iter()
            .any(|(kind, value)| kind == "origin" && value == "https://kept.example")
    );
}

#[test]
fn invalid_or_duplicate_credential_rejects_before_mutation() {
    let opened = open_db("reject");
    let db = opened.db();
    let seeded = seed(
        db,
        "dynamic",
        "reject-id",
        ModelResolution::PublicAndUpstream,
        &[catalog_row(
            "keep",
            "up-keep",
            UpstreamProtocolKind::ChatCompletions,
            false,
        )],
    );
    let before = loaded_dest(&load_runtime(db).unwrap(), &seeded.destination_id).clone();
    let next = definition(
        "ShouldNotStick",
        "https://after.example/v1",
        UpstreamProtocolKind::Responses,
        DynamicAuthKind::Bearer,
        vec![mapping("keep", "up-keep")],
    );
    let missing = apply(
        db,
        &seeded.destination_id,
        &next,
        &["missing-credential".into()],
    );
    assert!(missing.is_err());
    let duplicate = apply(
        db,
        &seeded.destination_id,
        &next,
        &[seeded.credential_id.clone(), seeded.credential_id.clone()],
    );
    assert!(duplicate.is_err());
    let snapshot = load_runtime(db).unwrap();
    let after = loaded_dest(&snapshot, &seeded.destination_id);
    assert_eq!(after.name, before.name);
    assert_eq!(after.base_url, before.base_url);
    let (enabled, _, _, _) = model_row(db, &seeded.destination_id, "keep");
    assert!(!enabled);
}

#[test]
fn caller_transaction_rollback_discards_successful_write() {
    let opened = open_db("rollback");
    let db = opened.db();
    let seeded = seed(
        db,
        "custom_account",
        "rollback-id",
        ModelResolution::PublicOnly,
        &[catalog_row(
            "keep",
            "up-keep",
            UpstreamProtocolKind::ChatCompletions,
            true,
        )],
    );
    let tx = db.conn.unchecked_transaction().unwrap();
    apply(
        db,
        &seeded.destination_id,
        &definition(
            "Rolled",
            "https://after.example/v1",
            UpstreamProtocolKind::ChatCompletions,
            DynamicAuthKind::Bearer,
            vec![mapping("keep", "up-keep")],
        ),
        &[],
    )
    .unwrap();
    tx.rollback().unwrap();
    let name: String = db
        .conn
        .query_row(
            "SELECT name FROM destinations WHERE id = ?1",
            [&seeded.destination_id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(name, "Before");
}

#[test]
fn keyed_to_none_clears_secret_and_bumps_version() {
    let opened = open_db("to-none");
    let db = opened.db();
    let seeded = seed(
        db,
        "dynamic",
        "none-id",
        ModelResolution::PublicAndUpstream,
        &[catalog_row(
            "keep",
            "up-keep",
            UpstreamProtocolKind::ChatCompletions,
            true,
        )],
    );
    apply(
        db,
        &seeded.destination_id,
        &definition(
            "None",
            "https://before.example/v1",
            UpstreamProtocolKind::ChatCompletions,
            DynamicAuthKind::None,
            vec![mapping("keep", "up-keep")],
        ),
        &[],
    )
    .unwrap();
    let (key, kind, secret, version, max_credentials): (String, String, i64, i64, Option<i64>) = db
        .conn
        .query_row(
            "SELECT c.key_cipher, c.credential_kind, c.has_secret, c.credential_version, d.max_credentials
             FROM credentials c JOIN destinations d ON d.id = c.destination_id
             WHERE c.id = ?1",
            [&seeded.credential_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?)),
        )
        .unwrap();
    assert!(key.is_empty());
    assert_eq!(kind, "none");
    assert_eq!(secret, 0);
    assert_eq!(version, 4);
    assert_eq!(max_credentials, Some(1));
}

#[test]
fn none_to_keyed_without_secret_stays_draft() {
    let opened = open_db("to-keyed");
    let db = opened.db();
    let destination_id = "dest-draft";
    insert_destination(
        db,
        destination_id,
        "dynamic",
        "draft-id",
        "Draft",
        "https://before.example/v1",
        UpstreamProtocolKind::ChatCompletions,
        AuthScheme::None,
        ModelResolution::PublicAndUpstream,
        None,
        None,
        false,
        true,
    );
    crate::db::destination_store::replace_destination_catalog(
        &db.conn,
        destination_id,
        &[catalog_row(
            "keep",
            "up-keep",
            UpstreamProtocolKind::ChatCompletions,
            true,
        )],
    )
    .unwrap();
    let connection_id = connection_id_for_legacy(LegacyConnectionKind::DynamicProvider, "draft-id");
    let credential_id = insert_credential(
        db,
        destination_id,
        "acct-draft",
        connection_id.as_str(),
        "",
        "none",
        0,
        None,
        r#"{"kind":"all"}"#,
    );
    apply(
        db,
        destination_id,
        &definition(
            "Keyed",
            "https://before.example/v1",
            UpstreamProtocolKind::ChatCompletions,
            DynamicAuthKind::Bearer,
            vec![mapping("keep", "up-keep")],
        ),
        &[],
    )
    .unwrap();
    let (key, kind, secret, version): (String, String, i64, i64) = db
        .conn
        .query_row(
            "SELECT key_cipher, credential_kind, has_secret, credential_version
             FROM credentials WHERE id = ?1",
            [&credential_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .unwrap();
    assert!(key.is_empty());
    assert_eq!(kind, "api_key");
    assert_eq!(secret, 0);
    assert_eq!(version, 3);
}

#[test]
fn none_auth_rejects_more_than_one_credential() {
    let opened = open_db("none-two");
    let db = opened.db();
    let seeded = seed(
        db,
        "dynamic",
        "two-id",
        ModelResolution::PublicAndUpstream,
        &[catalog_row(
            "keep",
            "up-keep",
            UpstreamProtocolKind::ChatCompletions,
            true,
        )],
    );
    insert_credential(
        db,
        &seeded.destination_id,
        "acct-two-b",
        seeded.connection_id.as_str(),
        "cipher-b",
        "api_key",
        1,
        None,
        r#"{"kind":"all"}"#,
    );
    let error = apply(
        db,
        &seeded.destination_id,
        &definition(
            "None",
            "https://before.example/v1",
            UpstreamProtocolKind::ChatCompletions,
            DynamicAuthKind::None,
            vec![mapping("keep", "up-keep")],
        ),
        &[],
    )
    .unwrap_err();
    assert!(error.to_string().contains("at most one credential"));
}

#[test]
fn non_http_destination_is_rejected() {
    let opened = open_db("non-http");
    let db = opened.db();
    let zen_id = destination_id_for_builtin(OPENCODE_ZEN_FREE_PROVIDER_ID);
    let error = apply(
        db,
        &zen_id,
        &definition(
            "Nope",
            "https://before.example/v1",
            UpstreamProtocolKind::ChatCompletions,
            DynamicAuthKind::Bearer,
            vec![mapping("keep", "up-keep")],
        ),
        &[],
    )
    .unwrap_err();
    assert!(error.to_string().contains("configurable HTTP"));
}

#[test]
fn observer_http_destination_is_rejected() {
    let opened = open_db("observer");
    let db = opened.db();
    insert_destination(
        db,
        "dest-observer",
        "platform_parent",
        "parent-id",
        "Parent",
        "https://site.example",
        UpstreamProtocolKind::ChatCompletions,
        AuthScheme::Bearer,
        ModelResolution::PublicOnly,
        None,
        None,
        true,
        true,
    );
    crate::db::destination_store::replace_destination_catalog(
        &db.conn,
        "dest-observer",
        &[catalog_row(
            "keep",
            "up-keep",
            UpstreamProtocolKind::ChatCompletions,
            true,
        )],
    )
    .unwrap();
    let error = apply(
        db,
        "dest-observer",
        &definition(
            "Nope",
            "https://site.example",
            UpstreamProtocolKind::ChatCompletions,
            DynamicAuthKind::Bearer,
            vec![mapping("keep", "up-keep")],
        ),
        &[],
    )
    .unwrap_err();
    assert!(error.to_string().contains("observer"));
}

#[test]
fn protocol_change_updates_preferred_without_enabling() {
    let opened = open_db("protocol");
    let db = opened.db();
    let seeded = seed(
        db,
        "custom_account",
        "proto-id",
        ModelResolution::PublicOnly,
        &[catalog_row(
            "keep",
            "up-keep",
            UpstreamProtocolKind::ChatCompletions,
            false,
        )],
    );
    apply(
        db,
        &seeded.destination_id,
        &definition(
            "Before",
            "https://before.example/v1",
            UpstreamProtocolKind::Messages,
            DynamicAuthKind::Bearer,
            vec![mapping("keep", "up-keep")],
        ),
        &[],
    )
    .unwrap();
    let (enabled, preferred, protocols, _) = model_row(db, &seeded.destination_id, "keep");
    assert!(!enabled);
    assert_eq!(preferred.as_deref(), Some("messages"));
    assert_eq!(
        protocols,
        serde_json::to_string(&[UpstreamProtocolKind::Messages]).unwrap()
    );
}

#[test]
fn new_mapping_is_enabled() {
    let opened = open_db("new-map");
    let db = opened.db();
    let seeded = seed(
        db,
        "dynamic",
        "new-map",
        ModelResolution::PublicAndUpstream,
        &[catalog_row(
            "keep",
            "up-keep",
            UpstreamProtocolKind::ChatCompletions,
            false,
        )],
    );
    apply(
        db,
        &seeded.destination_id,
        &definition(
            "Before",
            "https://before.example/v1",
            UpstreamProtocolKind::ChatCompletions,
            DynamicAuthKind::Bearer,
            vec![mapping("fresh", "up-fresh")],
        ),
        &[],
    )
    .unwrap();
    let (enabled, preferred, _, upstream) = model_row(db, &seeded.destination_id, "fresh");
    assert!(enabled);
    assert_eq!(preferred.as_deref(), Some("chat_completions"));
    assert_eq!(upstream, "up-fresh");
    let leftover: i64 = db
        .conn
        .query_row(
            "SELECT COUNT(*) FROM destination_models WHERE public_model_key = 'keep'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(leftover, 0);
}

#[test]
fn cooldown_and_scope_survive_substantive_edit() {
    let opened = open_db("cooldown");
    let db = opened.db();
    let seeded = seed(
        db,
        "custom_account",
        "cool-id",
        ModelResolution::PublicOnly,
        &[catalog_row(
            "keep",
            "up-keep",
            UpstreamProtocolKind::ChatCompletions,
            true,
        )],
    );
    apply(
        db,
        &seeded.destination_id,
        &definition(
            "After",
            "https://after.example/v1",
            UpstreamProtocolKind::ChatCompletions,
            DynamicAuthKind::Bearer,
            vec![mapping("keep", "up-keep")],
        ),
        &[],
    )
    .unwrap();
    let (scope, cooldown, verification): (String, Option<String>, String) = db
        .conn
        .query_row(
            "SELECT scope_json, cooldown_generic_until, verification_status
             FROM credentials WHERE id = ?1",
            [&seeded.credential_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert_eq!(scope, r#"{"kind":"only","models":["keep-me"]}"#);
    assert_eq!(cooldown.as_deref(), Some("2099-01-01T00:00:00Z"));
    assert_eq!(verification, "pending");
}

#[test]
fn missing_authorization_connection_id_rejects_authorize() {
    let opened = open_db("missing-conn");
    let db = opened.db();
    let seeded = seed(
        db,
        "dynamic",
        "missing-conn",
        ModelResolution::PublicAndUpstream,
        &[catalog_row(
            "keep",
            "up-keep",
            UpstreamProtocolKind::ChatCompletions,
            true,
        )],
    );
    db.conn
        .execute(
            "UPDATE credentials SET authorization_connection_id = NULL WHERE id = ?1",
            [&seeded.credential_id],
        )
        .unwrap();
    let error = apply(
        db,
        &seeded.destination_id,
        &definition(
            "After",
            "https://after.example/v1",
            UpstreamProtocolKind::ChatCompletions,
            DynamicAuthKind::Bearer,
            vec![mapping("keep", "up-keep")],
        ),
        std::slice::from_ref(&seeded.credential_id),
    )
    .unwrap_err();
    assert!(error.to_string().contains("authorization_connection_id"));
    let name: String = db
        .conn
        .query_row(
            "SELECT name FROM destinations WHERE id = ?1",
            [&seeded.destination_id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(name, "Before");
}

#[test]
fn url_change_clears_probe_evidence() {
    let opened = open_db("probe");
    let db = opened.db();
    let seeded = seed(
        db,
        "dynamic",
        "probe-id",
        ModelResolution::PublicAndUpstream,
        &[catalog_row(
            "keep",
            "up-keep",
            UpstreamProtocolKind::ChatCompletions,
            true,
        )],
    );
    db.conn
        .execute(
            "INSERT INTO provider_contract_model_protocols (
                scope_kind, scope_id, model_id, protocol, source
             ) VALUES ('custom_endpoint', ?1, 'keep', 'chat_completions', 'probe_confirmed')",
            [&seeded.account_id],
        )
        .unwrap();
    apply(
        db,
        &seeded.destination_id,
        &definition(
            "After",
            "https://after.example/v1",
            UpstreamProtocolKind::ChatCompletions,
            DynamicAuthKind::Bearer,
            vec![mapping("keep", "up-keep")],
        ),
        &[],
    )
    .unwrap();
    let evidence: i64 = db
        .conn
        .query_row(
            "SELECT COUNT(*) FROM provider_contract_model_protocols WHERE scope_id = ?1",
            [&seeded.account_id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(evidence, 0);
}
