//! Behavioral tests split from the root db test module.

use super::tests::*;
use super::*;
use std::collections::HashSet;
use std::fs;

#[test]
pub(super) fn platform_parent_link_refresh_survives_reopen_without_leftover_tables() {
    use crate::platform::{PlatformGroup, PlatformKind, PlatformSnapshot};

    let dir = temp_data_dir("v54-platform-reopen");
    let db = Database::open(dir.clone()).unwrap();
    assert!(!table_exists(&db.conn, "platform_accounts").unwrap());
    let mut custom = account("reopen-linked");
    custom.provider_id = CUSTOM_PROVIDER_ID.to_string();
    custom.credential_kind = CredentialKind::ApiKey;
    custom.quota_scope = QuotaScope::Key;
    custom.key_cipher = "reopen-key".into();
    db.create_account_with_contract(
        &custom,
        Some(&AccountCustomConfigInput {
            endpoint_url: "https://old.example/v1/chat/completions".into(),
            upstream_protocol: UpstreamProtocolKind::ChatCompletions,
        }),
        &[AccountModelCapabilityInput {
            public_model: "reopen-model".into(),
            upstream_model: "reopen-model".into(),
            protocol: UpstreamProtocolKind::ChatCompletions,
            source: None,
        }],
    )
    .unwrap();
    db.create_platform_account(
        "reopen-parent",
        PlatformKind::Sub2api,
        "Reopen Parent",
        "https://sub.example",
        Some("reopen-mgmt"),
    )
    .unwrap();
    db.link_platform_account("reopen-linked", "reopen-parent", &PlatformGroup::default())
        .unwrap();
    let token = db.platform_refresh_token("reopen-parent", None).unwrap();
    assert!(
        db.save_platform_refresh(
            "reopen-parent",
            None,
            &token,
            &PlatformSnapshot {
                observed_at: 9,
                ..PlatformSnapshot::default()
            },
        )
        .unwrap()
    );
    drop(db);

    let db = Database::open(dir.clone()).unwrap();
    assert!(!table_exists(&db.conn, "platform_accounts").unwrap());
    assert!(!table_exists(&db.conn, "platform_links").unwrap());
    let parent = db.platform_account("reopen-parent").unwrap().unwrap();
    assert_eq!(parent.kind, PlatformKind::Sub2api);
    assert_eq!(parent.base_url, "https://sub.example");
    assert_eq!(parent.name, "Reopen Parent");
    assert!(parent.has_user_credential);
    assert!(parent.snapshot.is_some());
    assert_eq!(
        db.platform_credential_cipher("reopen-parent")
            .unwrap()
            .as_deref(),
        Some("reopen-mgmt")
    );
    let links = db.list_platform_links().unwrap();
    assert_eq!(links.len(), 1);
    assert_eq!(links[0].account_id, "reopen-linked");
    db.unlink_platform_account("reopen-linked").unwrap();
    assert!(db.list_platform_links().unwrap().is_empty());
    db.delete_platform_account("reopen-parent").unwrap();
    assert!(db.platform_account("reopen-parent").unwrap().is_none());
    drop(db);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
pub(super) fn platform_link_lifecycle_and_refresh_races() {
    use crate::platform::{PlatformGroup, PlatformKind, PlatformSnapshot};
    let dir = temp_data_dir("platform-link");
    let mut db = Database::open(dir.clone()).unwrap();
    let mut key = account("platform-key");
    key.provider_id = CUSTOM_PROVIDER_ID.into();
    db.create_account_with_contract(
        &key,
        Some(&AccountCustomConfigInput {
            endpoint_url: "https://old.example/v1/chat/completions".into(),
            upstream_protocol: UpstreamProtocolKind::ChatCompletions,
        }),
        &[AccountModelCapabilityInput {
            public_model: "model-a".into(),
            upstream_model: "model-a".into(),
            protocol: UpstreamProtocolKind::ChatCompletions,
            source: None,
        }],
    )
    .unwrap();
    db.create_platform_account(
        "parent",
        PlatformKind::NewApi,
        "Parent",
        "https://new.example/v1",
        Some("obfuscated-test-credential"),
    )
    .unwrap();
    db.link_platform_account(&key.id, "parent", &PlatformGroup::default())
        .unwrap();
    assert_eq!(
        db.account_custom_config(&key.id)
            .unwrap()
            .unwrap()
            .endpoint_url,
        "https://new.example"
    );
    let linked_runtime = db
        .list_custom_account_runtimes()
        .unwrap()
        .into_iter()
        .find(|runtime| runtime.account_id == key.id)
        .expect("linked custom runtime");
    assert!(linked_runtime.protocol_passthrough);
    assert_eq!(linked_runtime.config.endpoint_url, "https://new.example");
    assert!(db.delete_platform_account("parent").is_err());
    let old = db.platform_refresh_token("parent", Some(&key.id)).unwrap();
    db.unlink_platform_account(&key.id).unwrap();
    db.link_platform_account(&key.id, "parent", &PlatformGroup::default())
        .unwrap();
    assert!(
        !db.save_platform_refresh("parent", Some(&key.id), &old, &PlatformSnapshot::default())
            .unwrap()
    );
    let old = db.platform_refresh_token("parent", Some(&key.id)).unwrap();
    db.update_platform_account("parent", "Parent", Some(None))
        .unwrap();
    assert!(
        !db.save_platform_refresh("parent", Some(&key.id), &old, &PlatformSnapshot::default())
            .unwrap()
    );
    assert!(
        !db.platform_account("parent")
            .unwrap()
            .unwrap()
            .has_user_credential
    );
    let current = db.platform_refresh_token("parent", Some(&key.id)).unwrap();
    assert!(
        db.save_platform_refresh(
            "parent",
            Some(&key.id),
            &current,
            &PlatformSnapshot::default()
        )
        .unwrap()
    );
    assert!(
        !db.save_platform_refresh(
            "parent",
            Some(&key.id),
            &current,
            &PlatformSnapshot::default()
        )
        .unwrap()
    );
    let untouched = db.platform_refresh_token("parent", Some(&key.id)).unwrap();
    platform::merge_platforms_on(&db.conn, &[], &[], &HashSet::new()).unwrap();
    assert_eq!(
        db.platform_refresh_token("parent", Some(&key.id)).unwrap(),
        untouched
    );
    assert!(db.list_platform_links().unwrap()[0].snapshot.is_some());
    db.unlink_platform_account(&key.id).unwrap();
    assert_eq!(
        db.account_custom_config(&key.id)
            .unwrap()
            .unwrap()
            .endpoint_url,
        "https://new.example"
    );
    db.link_platform_account(&key.id, "parent", &PlatformGroup::default())
        .unwrap();
    db.delete_account(&key.id).unwrap();
    assert!(db.list_platform_links().unwrap().is_empty());
    db.delete_platform_account("parent").unwrap();
    drop(db);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
pub(super) fn platform_link_failure_rolls_back_endpoint() {
    use crate::platform::{PlatformGroup, PlatformKind};
    let dir = temp_data_dir("platform-atomic");
    let db = Database::open(dir.clone()).unwrap();
    assert!(
        db.create_platform_account(
            "invalid",
            PlatformKind::Sub2api,
            "Invalid",
            "https://new.example/v1/messages",
            None
        )
        .is_err()
    );
    let mut key = account("platform-key");
    key.provider_id = CUSTOM_PROVIDER_ID.into();
    db.create_account_with_contract(
        &key,
        Some(&AccountCustomConfigInput {
            endpoint_url: "https://old.example/v1/chat/completions".into(),
            upstream_protocol: UpstreamProtocolKind::ChatCompletions,
        }),
        &[AccountModelCapabilityInput {
            public_model: "model-a".into(),
            upstream_model: "model-a".into(),
            protocol: UpstreamProtocolKind::ChatCompletions,
            source: None,
        }],
    )
    .unwrap();
    db.create_platform_account(
        "parent",
        PlatformKind::Sub2api,
        "Parent",
        "https://new.example",
        None,
    )
    .unwrap();
    db.conn.execute_batch("CREATE TRIGGER reject_platform BEFORE UPDATE ON credentials WHEN NEW.group_json IS NOT NULL AND OLD.group_json IS NULL BEGIN SELECT RAISE(ABORT,'injected failure'); END;").unwrap();
    assert!(
        db.link_platform_account(&key.id, "parent", &PlatformGroup::default())
            .is_err()
    );
    assert_eq!(
        db.account_custom_config(&key.id)
            .unwrap()
            .unwrap()
            .endpoint_url,
        "https://old.example/v1/chat/completions"
    );
    assert!(db.list_platform_links().unwrap().is_empty());
    drop(db);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
pub(super) fn platform_key_survives_failed_link_and_retry_links_without_a_second_key() {
    use crate::platform::{PlatformGroup, PlatformKind};
    let dir = temp_data_dir("platform-key-link-retry");
    let db = Database::open(dir.clone()).unwrap();
    let mut key = account("platform-key");
    key.provider_id = CUSTOM_PROVIDER_ID.into();
    db.create_account_with_contract(
        &key,
        Some(&AccountCustomConfigInput {
            endpoint_url: "https://old.example/v1/chat/completions".into(),
            upstream_protocol: UpstreamProtocolKind::ChatCompletions,
        }),
        &[AccountModelCapabilityInput {
            public_model: "model-a".into(),
            upstream_model: "model-a".into(),
            protocol: UpstreamProtocolKind::ChatCompletions,
            source: None,
        }],
    )
    .unwrap();
    db.create_platform_account(
        "parent",
        PlatformKind::NewApi,
        "Parent",
        "https://new.example",
        None,
    )
    .unwrap();
    db.conn
        .execute_batch(
            "CREATE TRIGGER reject_platform BEFORE UPDATE ON credentials WHEN NEW.group_json IS NOT NULL AND OLD.group_json IS NULL BEGIN SELECT RAISE(ABORT,'injected failure'); END;",
        )
        .unwrap();
    assert!(
        db.link_platform_account(&key.id, "parent", &PlatformGroup::default())
            .is_err()
    );
    let custom_ids: Vec<_> = db
        .list_accounts()
        .unwrap()
        .into_iter()
        .filter(|account| account.provider_id == CUSTOM_PROVIDER_ID)
        .map(|account| account.id)
        .collect();
    assert_eq!(custom_ids, ["platform-key".to_string()]);
    assert!(db.get_account("platform-key").unwrap().is_some());
    assert!(db.list_platform_links().unwrap().is_empty());

    db.conn
        .execute_batch("DROP TRIGGER reject_platform;")
        .unwrap();
    db.link_platform_account(&key.id, "parent", &PlatformGroup::default())
        .unwrap();
    let custom_ids: Vec<_> = db
        .list_accounts()
        .unwrap()
        .into_iter()
        .filter(|account| account.provider_id == CUSTOM_PROVIDER_ID)
        .map(|account| account.id)
        .collect();
    assert_eq!(custom_ids, ["platform-key".to_string()]);
    let links = db.list_platform_links().unwrap();
    assert_eq!(links.len(), 1);
    assert_eq!(links[0].account_id, "platform-key");
    assert_eq!(links[0].platform_account_id, "parent");
    drop(db);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
pub(super) fn platform_discovery_preserves_empty_protocol_controls_and_initializes_new_models() {
    let dir = temp_data_dir("platform-protocol-controls");
    let db = Database::open(dir.clone()).unwrap();
    seed_linked_platform_keys(&db, "parent-controls", &[("key-a", "model-a", "up-a")]);
    let id = ocg_domain::destination::destination_id_for_platform_account("parent-controls");
    let before = destination_store::load_destination_catalog(&db.conn, &id).unwrap();
    assert_eq!(before[0].protocols.len(), 3);
    db.conn.execute("UPDATE destination_models SET enabled = 0, protocols_json = '[]', preferred = NULL WHERE destination_id = ?1 AND public_model = 'model-a'", [&id]).unwrap();
    db.replace_account_model_capabilities(
        "key-a",
        &[
            custom_capability("model-a", "up-a"),
            custom_capability("model-b", "up-b"),
        ],
    )
    .unwrap();
    let catalog = destination_store::load_destination_catalog(&db.conn, &id).unwrap();
    let saved = catalog
        .iter()
        .find(|m| m.public_model == "model-a")
        .unwrap();
    assert!(!saved.enabled);
    assert!(saved.protocols.is_empty());
    assert!(saved.preferred.is_none());
    let new = catalog
        .iter()
        .find(|m| m.public_model == "model-b")
        .unwrap();
    assert_eq!(new.protocols.len(), 3);
    drop(db);
    fs::remove_dir_all(dir).unwrap();
}
