use super::*;
use crate::db::Database;
use crate::db::cpa;
use crate::provider::{CPA_ACCOUNT_ID, CPA_PROVIDER_ID};
use ocg_domain::catalog::{CredentialKind, QuotaScope};
use ocg_domain::credential::ModelScope;
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
        "ocg-native-bind-{label}-{}",
        uuid::Uuid::new_v4().simple()
    ));
    std::fs::create_dir_all(&dir).expect("test data dir");
    Opened {
        db: Some(Database::open(dir.clone()).expect("database")),
        dir,
    }
}

fn bind(db: &Database) -> BoundNative {
    bind_native_account(&db.conn, "codex", "codex.json").expect("first bind")
}

#[test]
fn first_bind_is_stable_and_refresh_preserves_controls() {
    let opened = open_db("bind");
    let db = opened.db();
    let first = bind(db);
    let again = bind(db);
    assert!(first.created);
    assert!(!again.created);
    assert_eq!(first.account_id, again.account_id);
    assert_eq!(first.credential_id, again.credential_id);
    assert_eq!(
        first.account_id,
        account_id_for("codex", "codex.json").unwrap()
    );
    assert_eq!(first.destination_id, owned_destination_id());
    assert_ne!(first.destination_id, cpa::destination_id());
    let before = load_owned_credential(&db.conn, &first.credential_id)
        .unwrap()
        .unwrap();
    assert_eq!(before.provider_id, CPA_PROVIDER_ID);
    assert_eq!(before.credential_kind, "none");
    assert!(before.key_empty);
    assert!(before.enabled);
    let empty_scope = serde_json::to_string(&ModelScope::Only { models: Vec::new() }).unwrap();
    db.conn
        .execute(
            "UPDATE credentials SET enabled = 0, scope_json = ?2 WHERE id = ?1",
            params![first.credential_id, empty_scope],
        )
        .unwrap();
    if crate::db::table_exists(&db.conn, "credential_bindings").unwrap() {
        db.conn
            .execute(
                "UPDATE credential_bindings
                 SET allowed_endpoint_ids = '[]', allowed_origins = '[]'
                 WHERE account_id = ?1",
                [&first.account_id],
            )
            .unwrap();
    } else {
        db.conn
            .execute(
                "DELETE FROM credential_grants WHERE credential_id = ?1",
                [&first.credential_id],
            )
            .unwrap();
        db.conn
            .execute(
                "UPDATE credentials SET grants_initialized = 1 WHERE legacy_account_id = ?1",
                [&first.account_id],
            )
            .unwrap();
    }
    let rank = before.routing_rank;
    let refreshed = bind(db);
    assert!(!refreshed.created);
    assert_eq!(refreshed.credential_version, first.credential_version);
    let after = load_owned_credential(&db.conn, &first.credential_id)
        .unwrap()
        .unwrap();
    assert!(!after.enabled);
    assert_eq!(after.routing_rank, rank);
    assert_eq!(after.scope_json, empty_scope);
    assert!(after.allowed_endpoint_ids.is_empty());
    assert!(after.allowed_origins.is_empty());
    assert!(
        !write_initial_grants(
            &db.conn,
            &first.account_id,
            &["endpoint".into()],
            &["https://chatgpt.com".into()],
        )
        .unwrap()
    );
    let still = load_owned_credential(&db.conn, &first.credential_id)
        .unwrap()
        .unwrap();
    assert!(still.allowed_endpoint_ids.is_empty());
}

#[test]
fn initial_grants_fill_only_an_empty_new_binding() {
    let opened = open_db("grants");
    let db = opened.db();
    let bound = bind(db);
    assert!(bound.created);
    assert!(
        write_initial_grants(
            &db.conn,
            &bound.account_id,
            &["endpoint-1".into()],
            &["https://chatgpt.com".into()],
        )
        .unwrap()
    );
    let row = load_owned_credential(&db.conn, &bound.credential_id)
        .unwrap()
        .unwrap();
    assert_eq!(row.allowed_endpoint_ids, vec!["endpoint-1".to_string()]);
    assert_eq!(row.allowed_origins, vec!["https://chatgpt.com".to_string()]);
}

#[test]
fn fence_bumps_version_and_auth_state_together() {
    let opened = open_db("fence");
    let db = opened.db();
    let bound = bind(db);
    let fenced = bump_auth_fence(&db.conn, &bound.credential_id).unwrap();
    assert_eq!(fenced.credential_version, bound.credential_version + 1);
    assert_eq!(fenced.auth_state_version, bound.auth_state_version + 1);
    let state: String = db
        .conn
        .query_row(
            "SELECT auth_state FROM credentials WHERE id = ?1",
            [&bound.credential_id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(state, "unknown");
}

#[test]
fn native_models_keep_disabled_history() {
    let opened = open_db("models");
    let db = opened.db();
    let destination_id = ensure_owned_destination(&db.conn).unwrap();
    let first = NativeModelInsert {
        public_model: "gpt-5".into(),
        upstream_model: "gpt-5".into(),
        protocols: vec!["responses".into()],
    };
    assert_eq!(
        insert_native_models_if_new(&db.conn, &destination_id, &[first.clone()]).unwrap(),
        1
    );
    db.conn
        .execute(
            "UPDATE destination_models SET enabled = 0
             WHERE destination_id = ?1 AND public_model_key = 'gpt-5'",
            [&destination_id],
        )
        .unwrap();
    let extra = NativeModelInsert {
        public_model: "gpt-5-codex".into(),
        upstream_model: "gpt-5-codex".into(),
        protocols: vec!["responses".into()],
    };
    assert_eq!(
        insert_native_models_if_new(&db.conn, &destination_id, &[first, extra]).unwrap(),
        1
    );
    let rows: Vec<(String, i64)> = {
        let mut statement = db
            .conn
            .prepare(
                "SELECT public_model, enabled FROM destination_models
                 WHERE destination_id = ?1 ORDER BY public_model",
            )
            .unwrap();
        statement
            .query_map([&destination_id], |row| Ok((row.get(0)?, row.get(1)?)))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap()
    };
    assert_eq!(
        rows,
        vec![("gpt-5".to_string(), 0), ("gpt-5-codex".to_string(), 1),]
    );
    let catalog: i64 = db
        .conn
        .query_row(
            "SELECT COUNT(*) FROM provider_model_catalogs WHERE provider_id = 'cpa'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(catalog, 0);
}

#[test]
fn retired_generate_content_protocols_are_replaced_without_flipping_enabled() {
    let opened = open_db("repair");
    let db = opened.db();
    let destination_id = ensure_owned_destination(&db.conn).unwrap();
    let retired = NativeModelInsert {
        public_model: "gemini-retired".into(),
        upstream_model: "gemini-retired".into(),
        protocols: vec!["generate_content".into()],
    };
    let kept = NativeModelInsert {
        public_model: "custom-mix".into(),
        upstream_model: "custom-mix".into(),
        protocols: vec!["chat_completions".into(), "generate_content".into()],
    };
    assert_eq!(
        insert_native_models_if_new(&db.conn, &destination_id, &[retired, kept]).unwrap(),
        2
    );
    db.conn
        .execute(
            "UPDATE destination_models SET enabled = 0
             WHERE destination_id = ?1 AND public_model_key = 'gemini-retired'",
            [&destination_id],
        )
        .unwrap();
    assert_eq!(
        repair_retired_generate_content_protocols(&db.conn, &destination_id).unwrap(),
        1
    );
    let rows: Vec<(String, String, String, i64)> = {
        let mut statement = db
            .conn
            .prepare(
                "SELECT public_model, protocols_json, preferred, enabled
                 FROM destination_models WHERE destination_id = ?1 ORDER BY public_model",
            )
            .unwrap();
        statement
            .query_map([&destination_id], |row| {
                Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
            })
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap()
    };
    assert_eq!(rows[0].0, "custom-mix");
    assert_eq!(rows[0].3, 1);
    assert!(rows[0].1.contains("generate_content"));
    assert_eq!(rows[1].0, "gemini-retired");
    assert_eq!(rows[1].2, "chat_completions");
    assert_eq!(rows[1].3, 0);
    assert!(rows[1].1.contains("responses"));
    assert!(rows[1].1.contains("messages"));
    assert!(!rows[1].1.contains("generate_content"));
}

#[test]
fn owned_destination_is_outside_the_remote_singleton() {
    let opened = open_db("remote");
    let db = opened.db();
    ensure_owned_destination(&db.conn).unwrap();
    assert!(!cpa::destination_present(&db.conn).unwrap());
    assert!(cpa::integration_on(&db.conn).unwrap().is_none());
    let mut account = crate::db::tests::account(CPA_ACCOUNT_ID);
    account.provider_id = CPA_PROVIDER_ID.to_string();
    account.credential_kind = CredentialKind::ApiKey;
    account.quota_scope = QuotaScope::Key;
    account.key_cipher = "synthetic-remote-key".into();
    db.upsert_cpa_integration(&account, "http://10.0.0.8:8317", "synthetic-management")
        .unwrap();
    assert!(cpa::destination_present(&db.conn).unwrap());
    let integration = cpa::integration_on(&db.conn).unwrap().unwrap();
    assert_eq!(integration.base_url, "http://10.0.0.8:8317");
    assert_ne!(
        integration.account_id,
        account_id_for("kimi", "kimi.json").unwrap()
    );
    let owned_observer: Option<String> = db
        .conn
        .query_row(
            "SELECT observer_credential_id FROM destinations WHERE id = ?1",
            [owned_destination_id()],
            |row| row.get(0),
        )
        .unwrap();
    assert!(owned_observer.is_none());
}

#[test]
fn unsafe_path_and_unknown_provider_are_rejected() {
    let error = account_id_for("codex", "nested/token.json").unwrap_err();
    assert!(error.to_string().contains("single relative"));
    assert!(account_id_for("openai", "token.json").is_err());
}
