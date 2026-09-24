use super::*;
use crate::db::Database;
use ocg_domain::destination::{
    AdapterKind, AuthScheme, Destination, HttpProtocolRoute, LegacyDestinationRef, ModelResolution,
    Protocol, destination_id_for_dynamic, sealed_capabilities,
};
use rusqlite::Connection;
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
        "ocg-http-routes-{label}-{}",
        uuid::Uuid::new_v4().simple()
    ));
    std::fs::create_dir_all(&dir).expect("test data dir");
    Opened {
        db: Some(Database::open(dir.clone()).expect("database should open")),
        dir,
    }
}

fn route(protocol: Protocol, url: &str, auth: AuthScheme) -> HttpProtocolRoute {
    HttpProtocolRoute {
        protocol,
        endpoint_url: url.to_string(),
        auth_scheme: auth,
    }
}

fn http_destination(protocol_routes: Vec<HttpProtocolRoute>) -> Destination {
    let first = protocol_routes
        .first()
        .expect("explicit routes for storage tests")
        .clone();
    Destination {
        id: destination_id_for_dynamic("lab-provider"),
        legacy: LegacyDestinationRef::Dynamic("lab-provider".to_string()),
        adapter: AdapterKind::Http,
        name: "Lab".to_string(),
        brand_family: None,
        base_url: Some(first.endpoint_url.clone()),
        protocols: protocol_routes.iter().map(|route| route.protocol).collect(),
        protocol_routes,
        auth_scheme: first.auth_scheme,
        model_resolution: ModelResolution::PublicAndUpstream,
        catalog: Vec::new(),
        capabilities: sealed_capabilities(AdapterKind::Http),
        plan: None,
        max_credentials: None,
        observer_credential_id: None,
        enabled: true,
    }
}

#[test]
fn ensure_storage_on_adds_nullable_column_idempotently() {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch("CREATE TABLE destinations (id TEXT PRIMARY KEY);")
        .unwrap();
    ensure_storage_on(&conn).unwrap();
    ensure_storage_on(&conn).unwrap();
    let mut stmt = conn.prepare("PRAGMA table_info(destinations)").unwrap();
    let columns = stmt
        .query_map([], |row| {
            Ok((row.get::<_, String>(1)?, row.get::<_, i64>(3)?))
        })
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap();
    let notnull = columns
        .iter()
        .find(|(name, _)| name == PROTOCOL_ROUTES_COLUMN)
        .map(|(_, notnull)| *notnull)
        .expect("protocol_routes_json column");
    assert_eq!(notnull, 0);
}

#[test]
fn decode_treats_null_and_empty_array_as_legacy() {
    assert!(decode_protocol_routes_json(None).unwrap().is_empty());
    assert!(decode_protocol_routes_json(Some("")).unwrap().is_empty());
    assert!(
        decode_protocol_routes_json(Some("null"))
            .unwrap()
            .is_empty()
    );
    assert!(decode_protocol_routes_json(Some("[]")).unwrap().is_empty());
}

#[test]
fn decode_refuses_unknown_protocol_and_duplicates() {
    let unknown = decode_protocol_routes_json(Some(
        r#"[{"protocol":"soap","endpoint_url":"https://lab.example/v1","auth_scheme":"bearer"}]"#,
    ));
    assert!(unknown.is_err());
    let duplicate = decode_protocol_routes_json(Some(
        r#"[{"protocol":"chat_completions","endpoint_url":"https://lab.example/v1","auth_scheme":"bearer"},{"protocol":"chat_completions","endpoint_url":"https://lab.example/other","auth_scheme":"bearer"}]"#,
    ));
    assert!(duplicate.is_err());
}

#[test]
fn sqlite_null_and_json_roundtrip_without_dropping_routes() {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch("CREATE TABLE destinations (id TEXT PRIMARY KEY);")
        .unwrap();
    ensure_storage_on(&conn).unwrap();
    conn.execute(
        "INSERT INTO destinations (id, protocol_routes_json) VALUES ('legacy', NULL)",
        [],
    )
    .unwrap();
    let raw: Option<String> = conn
        .query_row(
            "SELECT protocol_routes_json FROM destinations WHERE id = 'legacy'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert!(raw.is_none());
    assert!(
        decode_protocol_routes_json(raw.as_deref())
            .unwrap()
            .is_empty()
    );

    let routes = vec![
        route(
            Protocol::ChatCompletions,
            "https://lab.example/v1",
            AuthScheme::Bearer,
        ),
        route(
            Protocol::Messages,
            "https://lab.example/anthropic/v1/messages",
            AuthScheme::XApiKey,
        ),
    ];
    conn.execute(
        "INSERT INTO destinations (id, protocol_routes_json) VALUES ('explicit', ?1)",
        [encode_protocol_routes_json(&routes).unwrap()],
    )
    .unwrap();
    let stored: Option<String> = conn
        .query_row(
            "SELECT protocol_routes_json FROM destinations WHERE id = 'explicit'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        decode_protocol_routes_json(stored.as_deref()).unwrap(),
        routes
    );
}

#[test]
fn destination_store_and_projection_roundtrip_explicit_routes() {
    let opened = open_db("roundtrip");
    let db = opened.db();
    ensure_storage_on(&db.conn).unwrap();
    let destination = http_destination(vec![
        route(
            Protocol::ChatCompletions,
            "https://lab.example/v1",
            AuthScheme::Bearer,
        ),
        route(
            Protocol::Responses,
            "https://lab.example/v1/responses",
            AuthScheme::Bearer,
        ),
    ]);
    crate::db::destination_store::insert_destination_row(&db.conn, &destination).unwrap();
    assert_eq!(
        crate::db::destination_store::load_protocol_routes(&db.conn, &destination.id).unwrap(),
        destination.protocol_routes
    );
    let loaded = crate::destination_projection::load_persisted(db).unwrap();
    let found = loaded
        .destinations
        .iter()
        .find(|row| row.id == destination.id)
        .expect("inserted destination");
    assert_eq!(found.protocol_routes, destination.protocol_routes);
    assert_eq!(found.protocols, destination.protocols);
    assert_eq!(found.base_url, destination.base_url);
    assert_eq!(found.auth_scheme, destination.auth_scheme);
    assert_eq!(found.model_resolution, destination.model_resolution);
}

#[test]
fn load_refuses_first_route_mismatch_instead_of_dropping() {
    let opened = open_db("mismatch");
    let db = opened.db();
    ensure_storage_on(&db.conn).unwrap();
    let mut destination = http_destination(vec![route(
        Protocol::ChatCompletions,
        "https://lab.example/v1",
        AuthScheme::Bearer,
    )]);
    crate::db::destination_store::insert_destination_row(&db.conn, &destination).unwrap();
    destination.protocol_routes[0].endpoint_url = "https://other.example/v1".to_string();
    db.conn
        .execute(
            "UPDATE destinations SET protocol_routes_json = ?2 WHERE id = ?1",
            rusqlite::params![
                destination.id,
                encode_protocol_routes_json(&destination.protocol_routes).unwrap(),
            ],
        )
        .unwrap();
    assert!(crate::destination_projection::load_persisted(db).is_err());
}

#[test]
fn encode_omits_empty_list_as_null() {
    assert!(encode_protocol_routes_json(&[]).unwrap().is_none());
}
