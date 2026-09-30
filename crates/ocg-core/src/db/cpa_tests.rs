//! Behavioral tests split from the root db test module.

use super::tests::*;
use super::*;
use std::fs;

#[test]
pub(super) fn cpa_singleton_upsert_catalog_and_disconnect_are_idempotent_and_atomic() {
    let dir = temp_data_dir("cpa-singleton-lifecycle");
    let db = open_with_host_cipher(dir.clone()).unwrap();
    let now = Utc::now();
    let mut cpa_account = account(CPA_ACCOUNT_ID);
    cpa_account.provider_id = CPA_PROVIDER_ID.to_string();
    cpa_account.credential_kind = CredentialKind::ApiKey;
    cpa_account.quota_scope = QuotaScope::Key;
    cpa_account.name = CPA_ACCOUNT_NAME.to_string();
    cpa_account.key_cipher = test_host_cipher().encrypt("cpa-inference").unwrap();
    cpa_account.enabled = false;
    cpa_account.account_type = AccountType::Key;
    cpa_account.setup_step = AccountSetupStep::Ready;
    cpa_account.created_at = now;
    cpa_account.updated_at = now;
    let management_cipher = test_host_cipher().encrypt("cpa-management").unwrap();

    db.upsert_cpa_integration(&cpa_account, "http://127.0.0.1:8317", &management_cipher)
        .unwrap();
    cpa_account.enabled = true;
    db.upsert_cpa_integration(&cpa_account, "http://127.0.0.1:9317", &management_cipher)
        .unwrap();
    let record = db.cpa_integration().unwrap().unwrap();
    assert_eq!(record.account_id, CPA_ACCOUNT_ID);
    assert_eq!(record.base_url, "http://127.0.0.1:9317");
    assert_eq!(record.management_key_cipher, management_cipher);
    assert!(db.get_account(CPA_ACCOUNT_ID).unwrap().unwrap().enabled);
    assert!(!table_exists(&db.conn, "cpa_integration").unwrap());
    let dest_url: Option<String> = db
        .conn
        .query_row(
            "SELECT base_url FROM destinations
             WHERE adapter = 'cpa'
                OR (legacy_kind = 'builtin' AND legacy_id = 'cpa')",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(dest_url.as_deref(), Some("http://127.0.0.1:9317"));
    let observer_cipher: String = db
        .conn
        .query_row(
            "SELECT key_cipher FROM credentials WHERE id = ?1",
            [ocg_domain::credential::observer_credential_id_for_cpa().as_str()],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(observer_cipher, management_cipher);
    assert_ne!(
        db.get_account(CPA_ACCOUNT_ID).unwrap().unwrap().key_cipher,
        management_cipher
    );

    db.conn
        .execute(
            "UPDATE credentials SET auth_error = '401' WHERE legacy_account_id = ?1",
            [CPA_ACCOUNT_ID],
        )
        .unwrap();
    db.upsert_cpa_integration(&cpa_account, "http://127.0.0.1:9317", &management_cipher)
        .unwrap();
    assert_eq!(
        db.get_account(CPA_ACCOUNT_ID)
            .unwrap()
            .unwrap()
            .auth_error
            .as_deref(),
        Some("401"),
        "saving the same inference cipher must preserve an existing breaker"
    );
    cpa_account.key_cipher = test_host_cipher().encrypt("cpa-inference-fixed").unwrap();
    db.upsert_cpa_integration(&cpa_account, "http://127.0.0.1:9317", &management_cipher)
        .unwrap();
    assert!(
        db.get_account(CPA_ACCOUNT_ID)
            .unwrap()
            .unwrap()
            .auth_error
            .is_none(),
        "replacing the inference cipher must clear the stale 401 breaker"
    );

    db.replace_cpa_model_catalog(
        &[
            CpaCatalogModel {
                id: "gpt-5.6-sol".into(),
                owned_by: Some("openai".into()),
                enabled: true,
            },
            "unknown-cpa-model".into(),
        ],
        "http://127.0.0.1:9317",
        now,
    )
    .unwrap();
    let catalog = db.cpa_model_catalog().unwrap().unwrap();
    assert_eq!(catalog.models.len(), 2);
    assert_eq!(catalog.models[0].id, "gpt-5.6-sol");
    assert_eq!(catalog.models[0].owned_by.as_deref(), Some("openai"));
    assert!(catalog.models[1].owned_by.is_none());

    drop(db);
    let db = open_with_host_cipher(dir.clone()).unwrap();
    let reopened = db.cpa_integration().unwrap().unwrap();
    assert_eq!(reopened.account_id, CPA_ACCOUNT_ID);
    assert_eq!(reopened.base_url, "http://127.0.0.1:9317");
    assert_eq!(reopened.management_key_cipher, management_cipher);
    assert!(!table_exists(&db.conn, "cpa_integration").unwrap());

    db.delete_cpa_integration().unwrap();
    db.delete_cpa_integration().unwrap();
    assert!(db.cpa_integration().unwrap().is_none());
    assert!(db.cpa_model_catalog().unwrap().is_none());
    assert!(db.get_account(CPA_ACCOUNT_ID).unwrap().is_none());
    let leftover_dest: i64 = db
        .conn
        .query_row(
            "SELECT COUNT(*) FROM destinations
             WHERE adapter = 'cpa'
                OR (legacy_kind = 'builtin' AND legacy_id = 'cpa')",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(leftover_dest, 0);
    drop(db);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
pub(super) fn cpa_model_catalog_reads_legacy_id_arrays() {
    let dir = temp_data_dir("cpa-catalog-legacy-ids");
    let db = open_with_host_cipher(dir.clone()).unwrap();
    db.conn
        .execute(
            "INSERT INTO provider_model_catalogs
                 (provider_id, models_json, refreshed_at, source_url)
             VALUES (?1, ?2, ?3, ?4)",
            params![
                CPA_PROVIDER_ID,
                r#"["gpt-5","claude"]"#,
                Utc::now().to_rfc3339(),
                "http://127.0.0.1:8317",
            ],
        )
        .unwrap();
    let catalog = db.cpa_model_catalog().unwrap().unwrap();
    assert_eq!(
        catalog.models,
        [
            CpaCatalogModel {
                id: "gpt-5".into(),
                owned_by: None,
                enabled: true,
            },
            CpaCatalogModel {
                id: "claude".into(),
                owned_by: None,
                enabled: true,
            },
        ]
    );
    drop(db);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
pub(super) fn cpa_model_catalog_merge_refresh_keeps_selection_and_defaults_new_ids_off() {
    let previous = vec![
        CpaCatalogModel {
            id: "kept".into(),
            owned_by: None,
            enabled: true,
        },
        CpaCatalogModel {
            id: "off".into(),
            owned_by: None,
            enabled: false,
        },
        CpaCatalogModel {
            id: "gone".into(),
            owned_by: None,
            enabled: true,
        },
    ];
    let incoming = vec![
        CpaCatalogModel {
            id: "kept".into(),
            owned_by: Some("openai".into()),
            enabled: true,
        },
        CpaCatalogModel {
            id: "off".into(),
            owned_by: None,
            enabled: true,
        },
        CpaCatalogModel {
            id: "fresh".into(),
            owned_by: None,
            enabled: true,
        },
    ];
    assert_eq!(
        CpaCatalogModel::merge_refresh(incoming, &previous),
        [
            CpaCatalogModel {
                id: "kept".into(),
                owned_by: Some("openai".into()),
                enabled: true,
            },
            CpaCatalogModel {
                id: "off".into(),
                owned_by: None,
                enabled: false,
            },
            CpaCatalogModel {
                id: "fresh".into(),
                owned_by: None,
                enabled: false,
            },
        ]
    );
    assert!(
        CpaCatalogModel::enabled_ids(&CpaCatalogModel::merge_refresh(vec!["only".into()], &[]))
            .is_empty()
    );
}

#[test]
pub(super) fn cpa_model_catalog_reads_enabled_flag_and_defaults_missing_on() {
    let dir = temp_data_dir("cpa-catalog-enabled");
    let db = open_with_host_cipher(dir.clone()).unwrap();
    db.conn
        .execute(
            "INSERT INTO provider_model_catalogs
                 (provider_id, models_json, refreshed_at, source_url)
             VALUES (?1, ?2, ?3, ?4)",
            params![
                CPA_PROVIDER_ID,
                r#"[{"id":"legacy"},{"id":"off","enabled":false}]"#,
                Utc::now().to_rfc3339(),
                "http://127.0.0.1:8317",
            ],
        )
        .unwrap();
    let catalog = db.cpa_model_catalog().unwrap().unwrap();
    assert_eq!(
        catalog.models,
        [
            CpaCatalogModel {
                id: "legacy".into(),
                owned_by: None,
                enabled: true,
            },
            CpaCatalogModel {
                id: "off".into(),
                owned_by: None,
                enabled: false,
            },
        ]
    );
    drop(db);
    fs::remove_dir_all(dir).unwrap();
}
