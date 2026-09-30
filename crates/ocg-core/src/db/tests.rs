use super::*;
use super::{migrations::V27MigrationFault, v27_test_hooks};
use crate::crypto::{KeyCipher, StaticKeyCipher};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::sync::Arc;

pub(super) const TEST_HOST_SECRET: &str = "ocg-db-v27-test-host";

pub(super) fn billing_open_fixture(dir: &Path) -> (Database, i64, crate::billing::CreditAttempt) {
    use crate::billing_types::{CreditBucket, CreditBucketKind, CreditConfiguration, CreditRate};
    let db = open_with_host_cipher(dir.to_path_buf()).unwrap();
    let mut draft = account("billing-open");
    draft.provider_id = CUSTOM_PROVIDER_ID.into();
    draft.key_cipher = fixture_account_key_cipher();
    let endpoint = "https://billing-open.example/v1/chat/completions";
    db.create_account_with_contract(
        &draft,
        Some(&AccountCustomConfigInput {
            endpoint_url: endpoint.into(),
            upstream_protocol: UpstreamProtocolKind::ChatCompletions,
        }),
        &[AccountModelCapabilityInput {
            public_model: "model".into(),
            upstream_model: "model".into(),
            protocol: UpstreamProtocolKind::ChatCompletions,
            source: None,
        }],
    )
    .unwrap();
    let now = Utc::now();
    billing::configure_on(
        &db.conn,
        &draft.id,
        CreditConfiguration {
            name: "Personal".into(),
            currency: "CNY".into(),
            credits_per_currency: 1.0,
            rates: vec![CreditRate {
                model: "model".into(),
                input_per_million: 10.0,
                output_per_million: 20.0,
                cache_read_per_million: None,
                cache_write_per_million: None,
            }],
            monthly: None,
            source_url: None,
        },
        Some(vec![CreditBucket {
            id: "initial".into(),
            kind: CreditBucketKind::Manual,
            label: "Current".into(),
            granted: 100.0,
            remaining: 75.0,
            starts_at: now,
            expires_at: None,
        }]),
        now,
    )
    .unwrap();
    let attempt = billing::capture_on(&db.conn, &draft.id, endpoint, "model", now)
        .unwrap()
        .unwrap();
    let log_id = db
        .log_forward(&forward_log(&draft.id, "streaming", 0.0))
        .unwrap();
    billing::attach_attempt_on(&db.conn, log_id, &attempt).unwrap();
    (db, log_id, attempt)
}

pub(super) fn assert_billing_open_state(
    db: &Database,
    remaining: f64,
    pending: u64,
    unpriced: u64,
) {
    let view = billing::read_view_on(&db.conn, "billing-open", Utc::now())
        .unwrap()
        .unwrap();
    assert_eq!(view.remaining, remaining);
    assert_eq!(view.pending_requests, pending);
    assert_eq!(view.unpriced_requests, unpriced);
}

pub(super) fn finish_billing_open_attempt(
    db: &Database,
    log_id: i64,
    attempt: &crate::billing::CreditAttempt,
) {
    for _ in 0..2 {
        let tx = db.conn.unchecked_transaction().unwrap();
        billing::settle_on(
            &tx,
            log_id,
            attempt,
            ocg_domain::billing::BillingTokens::new(1_000_000, 0, 0, 0),
            "success",
            Utc::now(),
        )
        .unwrap();
        tx.commit().unwrap();
        assert_billing_open_state(db, 65.0, 0, 0);
    }
}

// Run by the parent test in another process, including on Windows. The child
// remains open while the parent finalizes the original pending receipt.

#[test]
pub(super) fn authorized_builtin_protocol_edit_grants_only_selected_credentials_atomically() {
    use ocg_domain::connection::{
        EndpointOperation, LegacyConnectionKind, connection_id_for_legacy, endpoint_id_for,
    };
    use ocg_domain::credential::credential_id_for_legacy_account;

    let dir = temp_data_dir("authorized-protocol-grants");
    let db = open_with_host_cipher(dir.clone()).unwrap();
    let mut goat_a = account("authorized-goat-a");
    goat_a.provider_id = COMMAND_CODE_PROVIDER_ID.into();
    goat_a.key_cipher = fixture_account_key_cipher();
    let mut goat_b = account("authorized-goat-b");
    goat_b.provider_id = COMMAND_CODE_PROVIDER_ID.into();
    goat_b.key_cipher = fixture_account_key_cipher();
    let mut foreign = account("authorized-foreign");
    foreign.provider_id = OPENCODE_PROVIDER_ID.into();
    foreign.key_cipher = fixture_account_key_cipher();
    db.create_account(&goat_a).unwrap();
    db.create_account(&goat_b).unwrap();
    db.create_account(&foreign).unwrap();

    let goat_a_id = credential_id_for_legacy_account(&goat_a.id).to_string();
    let goat_b_id = credential_id_for_legacy_account(&goat_b.id).to_string();
    let foreign_id = credential_id_for_legacy_account(&foreign.id).to_string();
    let goat_connection = connection_id_for_legacy(
        LegacyConnectionKind::BuiltinProvider,
        COMMAND_CODE_PROVIDER_ID,
    );
    let responses_endpoint =
        endpoint_id_for(&goat_connection, EndpointOperation::ResponseCreate).to_string();
    for credential_id in [&goat_a_id, &goat_b_id] {
        db.conn
            .execute(
                "DELETE FROM credential_grants WHERE credential_id = ?1 AND kind = 'endpoint_id' AND value = ?2",
                params![credential_id, responses_endpoint],
            )
            .unwrap();
    }
    fn grants_for(db: &Database, credential_id: &str) -> Vec<(String, String)> {
        db.conn
            .prepare(
                "SELECT kind, value FROM credential_grants WHERE credential_id = ?1 ORDER BY kind, value",
            )
            .unwrap()
            .query_map([credential_id], |row| Ok((row.get(0)?, row.get(1)?)))
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap()
    }
    let before_a = grants_for(&db, &goat_a_id);
    let before_b = grants_for(&db, &goat_b_id);
    let before_foreign = grants_for(&db, &foreign_id);
    assert!(
        !before_a
            .iter()
            .any(|(_, value)| value == &responses_endpoint)
    );
    assert!(
        !before_b
            .iter()
            .any(|(_, value)| value == &responses_endpoint)
    );

    let scope = ContractScope::provider(COMMAND_CODE_PROVIDER_ID);
    let rows = [(
        COMMAND_CODE_GOAT_DEEPSEEK_V4_FLASH_UPSTREAM.into(),
        UpstreamProtocolKind::Responses,
        ProtocolOverrideState::ForceOn,
    )];
    let now = Utc::now();
    db.set_model_protocol_settings(&scope, &rows, &[], now)
        .unwrap();
    assert_eq!(grants_for(&db, &goat_a_id), before_a);
    assert_eq!(grants_for(&db, &goat_b_id), before_b);

    db.set_model_protocol_settings_authorized(
        &scope,
        &rows,
        &[],
        now,
        std::slice::from_ref(&goat_a_id),
    )
    .unwrap();
    let after_a = grants_for(&db, &goat_a_id);
    assert!(
        after_a
            .iter()
            .any(|(kind, value)| kind == "endpoint_id" && value == &responses_endpoint)
    );
    assert_eq!(
        after_a
            .iter()
            .filter(|(_, value)| value != &responses_endpoint)
            .cloned()
            .collect::<Vec<_>>(),
        before_a
    );
    assert_eq!(grants_for(&db, &goat_b_id), before_b);
    assert_eq!(grants_for(&db, &foreign_id), before_foreign);

    let contract_before_foreign = db.load_persisted_contracts().unwrap();
    let grants_before_foreign = [
        grants_for(&db, &goat_a_id),
        grants_for(&db, &goat_b_id),
        grants_for(&db, &foreign_id),
    ];
    assert!(
        db.set_model_protocol_settings_authorized(
            &scope,
            &rows,
            &[],
            now,
            &[goat_a_id.clone(), foreign_id.clone()],
        )
        .is_err()
    );
    assert_eq!(
        db.load_persisted_contracts().unwrap(),
        contract_before_foreign
    );
    assert_eq!(grants_for(&db, &goat_a_id), grants_before_foreign[0]);
    assert_eq!(grants_for(&db, &goat_b_id), grants_before_foreign[1]);
    assert_eq!(grants_for(&db, &foreign_id), grants_before_foreign[2]);

    let contract_before_duplicate = db.load_persisted_contracts().unwrap();
    let grants_before_duplicate = [
        grants_for(&db, &goat_a_id),
        grants_for(&db, &goat_b_id),
        grants_for(&db, &foreign_id),
    ];
    assert!(
        db.set_model_protocol_settings_authorized(
            &scope,
            &rows,
            &[],
            now,
            &[goat_a_id.clone(), goat_a_id.clone()],
        )
        .is_err()
    );
    assert_eq!(
        db.load_persisted_contracts().unwrap(),
        contract_before_duplicate
    );
    assert_eq!(grants_for(&db, &goat_a_id), grants_before_duplicate[0]);
    assert_eq!(grants_for(&db, &goat_b_id), grants_before_duplicate[1]);
    assert_eq!(grants_for(&db, &foreign_id), grants_before_duplicate[2]);

    drop(db);
    fs::remove_dir_all(dir).unwrap();
}

pub(super) fn rewind_identity_model_to_v44(conn: &Connection) {
    account_store::materialize_legacy_accounts_for_rewind(conn).unwrap();
    conn.execute_batch(
        "DROP TABLE IF EXISTS quota_pool_members;
         DROP TABLE IF EXISTS quota_pools;
         DROP TABLE IF EXISTS subscription_records;
         DROP TABLE IF EXISTS onboarding_tasks;
         DROP TABLE IF EXISTS legacy_identity_map;
         DROP TABLE IF EXISTS credential_bindings;
         DROP TABLE IF EXISTS credential_state;
         DROP TABLE IF EXISTS upstream_identities;
         UPDATE accounts SET identity_id = NULL;
         DELETE FROM schema_version;
         INSERT INTO schema_version(version) VALUES (44);",
    )
    .unwrap();
}

#[test]
pub(super) fn unlink_to_unchanged_empty_custom_source_does_not_duplicate_connection() {
    use crate::platform::{PlatformGroup, PlatformKind};

    let dir = temp_data_dir("platform-unlink-reuses-empty-source");
    let mut db = open_with_host_cipher(dir.clone()).unwrap();
    let mut key = account("site-key");
    key.provider_id = CUSTOM_PROVIDER_ID.into();
    key.key_cipher = fixture_account_key_cipher();
    db.create_account_with_contract(
        &key,
        Some(&AccountCustomConfigInput {
            endpoint_url: "https://site.example/chat".into(),
            upstream_protocol: UpstreamProtocolKind::ChatCompletions,
        }),
        &[AccountModelCapabilityInput {
            public_model: "site-model".into(),
            upstream_model: "site-model".into(),
            protocol: UpstreamProtocolKind::ChatCompletions,
            source: None,
        }],
    )
    .unwrap();
    let source_id = ocg_domain::destination::destination_id_for_custom_account(&key.id);
    db.create_platform_account(
        "site-parent",
        PlatformKind::NewApi,
        "Site Parent",
        "https://site.example/chat/v1",
        None,
    )
    .unwrap();
    db.link_platform_account(&key.id, "site-parent", &PlatformGroup::default())
        .unwrap();
    db.unlink_platform_account(&key.id).unwrap();
    let destination_id: String = db
        .conn
        .query_row(
            "SELECT destination_id FROM credentials WHERE legacy_account_id = ?1",
            [&key.id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(destination_id, source_id);
    db.delete_account(&key.id).unwrap();
    db.delete_platform_account("site-parent").unwrap();
    let remaining: i64 = db
        .conn
        .query_row(
            "SELECT COUNT(*) FROM destinations WHERE legacy_kind = 'custom_account'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(remaining, 1);
    drop(db);
    let reopened = open_with_host_cipher(dir.clone()).unwrap();
    let projected = crate::destination_projection::load_persisted(&reopened).unwrap();
    let custom = projected
        .destinations
        .iter()
        .filter(|row| {
            matches!(
                &row.legacy,
                ocg_domain::destination::LegacyDestinationRef::CustomAccount(_)
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(custom.len(), 1);
    assert_eq!(custom[0].id, source_id);
    drop(reopened);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
pub(super) fn shared_custom_link_and_unlink_preserve_sibling_connection() {
    use crate::platform::{PlatformGroup, PlatformKind};

    let dir = temp_data_dir("shared-custom-link-unlink");
    let db = open_with_host_cipher(dir.clone()).unwrap();
    let mut owner = account("shared-owner");
    owner.provider_id = CUSTOM_PROVIDER_ID.into();
    owner.key_cipher = fixture_account_key_cipher();
    db.create_account_with_contract(
        &owner,
        Some(&AccountCustomConfigInput {
            endpoint_url: "https://shared-source.example/v1/chat/completions".into(),
            upstream_protocol: UpstreamProtocolKind::ChatCompletions,
        }),
        &[AccountModelCapabilityInput {
            public_model: "shared-model".into(),
            upstream_model: "vendor/shared-model".into(),
            protocol: UpstreamProtocolKind::ChatCompletions,
            source: None,
        }],
    )
    .unwrap();
    db.replace_credential_id("shared-owner", "00000000-0000-4000-8000-00000000c0de")
        .unwrap();
    let source_destination = ocg_domain::destination::destination_id_for_custom_account(&owner.id);
    let mut sibling = account("shared-sibling");
    sibling.provider_id = CUSTOM_PROVIDER_ID.into();
    sibling.key_cipher = fixture_account_key_cipher();
    db.commit_onboarding_existing_account(
        &sibling,
        Some(&source_destination),
        &NewDashboardOperation {
            operation_id: uuid::Uuid::new_v4().to_string(),
            kind: "onboarding_commit".into(),
            payload_digest: "2".repeat(64),
            result_json: "{}".into(),
        },
    )
    .unwrap();
    db.create_platform_account(
        "shared-parent",
        PlatformKind::NewApi,
        "Shared Parent",
        "https://shared-source.example/v1",
        Some("obfuscated-test-credential"),
    )
    .unwrap();

    db.link_platform_account("shared-owner", "shared-parent", &PlatformGroup::default())
        .unwrap();
    let sibling_after_link = db.account_custom_config("shared-sibling").unwrap().unwrap();
    assert_eq!(
        sibling_after_link.endpoint_url,
        "https://shared-source.example/v1/chat/completions"
    );
    let sibling_destination: String = db
        .conn
        .query_row(
            "SELECT destination_id FROM credentials WHERE legacy_account_id = 'shared-sibling'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(sibling_destination, source_destination);

    db.unlink_platform_account("shared-owner").unwrap();
    let owner_destination: String = db
        .conn
        .query_row(
            "SELECT destination_id FROM credentials WHERE legacy_account_id = 'shared-owner'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_ne!(owner_destination, source_destination);
    let owner_legacy_id: String = db
        .conn
        .query_row(
            "SELECT legacy_id FROM destinations WHERE id = ?1",
            [&owner_destination],
            |row| row.get(0),
        )
        .unwrap();
    let owner_connection = ocg_domain::connection::connection_id_for_legacy(
        ocg_domain::connection::LegacyConnectionKind::CustomAccount,
        &owner_legacy_id,
    );
    let expected_endpoint = ocg_domain::connection::endpoint_id_for(
        &owner_connection,
        ocg_domain::connection::EndpointOperation::ChatCreate,
    );
    let owner_binding = db
        .list_inference_bindings()
        .unwrap()
        .into_iter()
        .find(|binding| binding.account_id == "shared-owner")
        .unwrap();
    assert_eq!(
        owner_binding.allowed_endpoint_ids,
        vec![expected_endpoint.to_string()]
    );
    let sibling_destination: String = db
        .conn
        .query_row(
            "SELECT destination_id FROM credentials WHERE legacy_account_id = 'shared-sibling'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(sibling_destination, source_destination);
    assert_eq!(
        db.account_custom_config("shared-sibling")
            .unwrap()
            .unwrap()
            .endpoint_url,
        "https://shared-source.example/v1/chat/completions"
    );
    assert_eq!(
        db.account_custom_config("shared-owner")
            .unwrap()
            .unwrap()
            .endpoint_url,
        "https://shared-source.example"
    );

    let binding_id = owner_binding.binding_id;
    db.update_credential_binding(&binding_id, None, None, Some(&[]), Some(&[]))
        .unwrap();
    db.link_platform_account("shared-owner", "shared-parent", &PlatformGroup::default())
        .unwrap();
    db.unlink_platform_account("shared-owner").unwrap();
    let revoked = db
        .list_inference_bindings()
        .unwrap()
        .into_iter()
        .find(|binding| binding.account_id == "shared-owner")
        .unwrap();
    assert!(revoked.allowed_endpoint_ids.is_empty());
    assert!(revoked.allowed_origins.is_empty());

    drop(db);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
pub(super) fn substantive_custom_destination_edit_invalidates_verification_and_auth_projection() {
    let dir = temp_data_dir("custom-edit-invalidates-verification");
    let db = open_with_host_cipher(dir.clone()).unwrap();
    let mut custom = account("custom-verified");
    custom.provider_id = CUSTOM_PROVIDER_ID.into();
    custom.key_cipher = fixture_account_key_cipher();
    db.create_account_with_contract(
        &custom,
        Some(&AccountCustomConfigInput {
            endpoint_url: "https://before.example/v1/chat/completions".into(),
            upstream_protocol: UpstreamProtocolKind::ChatCompletions,
        }),
        &[AccountModelCapabilityInput {
            public_model: "verified-model".into(),
            upstream_model: "vendor/verified-model".into(),
            protocol: UpstreamProtocolKind::ChatCompletions,
            source: None,
        }],
    )
    .unwrap();
    db.conn
        .execute(
            "UPDATE credentials SET verification_status = 'verified',
                 connection_verified_at = '2026-09-20T00:00:00Z',
                 cooldown_generic_until = '2026-09-21T00:00:00Z'
             WHERE legacy_account_id = 'custom-verified'",
            [],
        )
        .unwrap();
    account_store::sync_inference_credential_projection_on(&db.conn, "custom-verified").unwrap();
    let destination_id =
        ocg_domain::destination::destination_id_for_custom_account("custom-verified");
    db.replace_custom_destination(
        &destination_id,
        &ocg_domain::dynamic::DynamicProviderDefinition {
            preset_id: None,
            id: "custom-verified".into(),
            name: "After".into(),
            endpoint_url: "https://after.example/v1/chat/completions".into(),
            upstream_protocol: UpstreamProtocolKind::ChatCompletions,
            auth_kind: ocg_domain::dynamic::DynamicAuthKind::Bearer,
            mappings: vec![ocg_domain::dynamic::DynamicModelMapping {
                public_model: "verified-model".into(),
                upstream_model: "vendor/verified-model".into(),
                upstream_override: None,
            }],
        },
        &[],
    )
    .unwrap();
    let state: (String, Option<String>, String, Option<String>) = db
        .conn
        .query_row(
            "SELECT verification_status, connection_verified_at, auth_state,
                    cooldown_generic_until
             FROM credentials WHERE legacy_account_id = 'custom-verified'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .unwrap();
    assert_eq!(state.0, "pending");
    assert!(state.1.is_none());
    assert_eq!(state.2, "unknown");
    assert_eq!(state.3.as_deref(), Some("2026-09-21T00:00:00Z"));

    drop(db);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
pub(super) fn identity_repair_preserves_existing_shared_graph_and_ids() {
    use ocg_domain::credential::ModelScope;
    let dir = temp_data_dir("repair-preserves-shared-graph");
    let db = open_with_host_cipher(dir.clone()).unwrap();
    let mut first = account("repair-shared-a");
    first.key_cipher = fixture_account_key_cipher();
    db.create_account(&first).unwrap();
    let identity_id = db
        .list_identity_model()
        .unwrap()
        .accounts
        .into_iter()
        .find(|row| row.account.id == first.id)
        .unwrap()
        .identity_id;
    let mut second = account("repair-shared-b");
    second.key_cipher = fixture_account_key_cipher();
    db.create_account_for_identity(
        &identity_id,
        &second,
        &local_today(),
        ConnectionVerificationStatus::NotRequired,
        crate::db::identity::QuotaSharingJoin::Shared {
            source_credential_id: ocg_domain::credential::credential_id_for_legacy_account(
                "repair-shared-a",
            )
            .to_string(),
        },
        None,
    )
    .unwrap();
    let mut other = account("repair-unrelated");
    other.key_cipher = fixture_account_key_cipher();
    db.create_account(&other).unwrap();
    let credential_id = "00000000-0000-4000-8000-000000000091";
    let binding_id = "00000000-0000-4000-8000-000000000092";
    let pool_id = "00000000-0000-4000-8000-000000000093";
    let scope = ModelScope::Only {
        models: vec!["glm-5.1".into()],
    };
    db.conn
        .execute(
            "UPDATE credentials SET id=?2, credential_version=7, auth_state_version=7
             WHERE legacy_account_id=?1",
            params![second.id, credential_id],
        )
        .unwrap();
    db.conn
        .execute(
            "UPDATE credentials SET binding_id=?2, scope_json=?3, binding_enabled=0
             WHERE legacy_account_id=?1",
            params![
                second.id,
                binding_id,
                serde_json::to_string(&scope).unwrap()
            ],
        )
        .unwrap();
    let old_pool: String = db
        .conn
        .query_row(
            "SELECT pool_id FROM quota_pool_members WHERE account_id=?1",
            [&first.id],
            |row| row.get(0),
        )
        .unwrap();
    db.conn
        .execute(
            "INSERT INTO quota_pools SELECT ?2, subject_kind, subject_ref,
        relation_confidence, policy_mode, created_at FROM quota_pools WHERE id=?1",
            params![old_pool, pool_id],
        )
        .unwrap();
    db.conn
        .execute(
            "UPDATE quota_pool_members SET pool_id=?2 WHERE pool_id=?1",
            params![old_pool, pool_id],
        )
        .unwrap();
    db.conn
        .execute("DELETE FROM quota_pools WHERE id=?1", [&old_pool])
        .unwrap();
    db.conn
        .execute(
            "UPDATE credentials SET binding_id = NULL WHERE legacy_account_id=?1",
            [&other.id],
        )
        .unwrap();
    drop(db);

    let db = open_with_host_cipher(dir.clone()).unwrap();
    let snapshot = db.list_identity_model().unwrap();
    let repaired = snapshot
        .accounts
        .iter()
        .find(|row| row.account.id == second.id)
        .unwrap();
    assert_eq!(repaired.identity_id, identity_id);
    assert_eq!(repaired.credential_id, credential_id);
    assert_eq!(repaired.credential_version, 7);
    assert_eq!(repaired.binding_id, binding_id);
    assert!(!repaired.binding_enabled);
    assert_eq!(repaired.binding_model_scope, scope);
    assert_eq!(
        db.shared_pool_account_ids(&first.id).unwrap(),
        vec![first.id.clone(), second.id.clone()]
    );
    let pools: Vec<String> = db
        .conn
        .prepare("SELECT pool_id FROM quota_pool_members WHERE account_id=?1")
        .unwrap()
        .query_map([&second.id], |row| row.get(0))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    assert_eq!(pools, vec![pool_id]);
    let bindings: i64 = db
        .conn
        .query_row(
            "SELECT COUNT(*) FROM credentials
             WHERE legacy_account_id=?1 AND binding_id IS NOT NULL AND binding_id <> ''",
            [&second.id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(bindings, 1);
    assert!(
        snapshot
            .accounts
            .iter()
            .any(|row| row.account.id == other.id)
    );
    drop(db);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
pub(super) fn saved_grants_remain_revoked_across_reopen_and_rotation() {
    use ocg_domain::credential::credential_id_for_legacy_account;

    let dir = temp_data_dir("v46-grants-once");
    let db = open_with_host_cipher(dir.clone()).unwrap();
    let mut keyed = account("grant-go");
    keyed.key_cipher = fixture_account_key_cipher();
    db.create_account(&keyed).unwrap();
    let stored = db
        .list_identity_model()
        .unwrap()
        .accounts
        .into_iter()
        .find(|row| row.account.id == "grant-go")
        .unwrap();
    assert!(!stored.allowed_endpoint_ids.is_empty());
    assert!(stored.allowed_origins.is_empty());
    db.conn
        .execute(
            "DELETE FROM credential_grants WHERE credential_id = ?1",
            [credential_id_for_legacy_account("grant-go").as_str()],
        )
        .unwrap();
    db.conn
        .execute(
            "UPDATE credentials SET grants_initialized = 1 WHERE legacy_account_id='grant-go'",
            [],
        )
        .unwrap();
    drop(db);

    let db = open_with_host_cipher(dir.clone()).unwrap();
    let reopened = db
        .list_identity_model()
        .unwrap()
        .accounts
        .into_iter()
        .find(|row| row.account.id == "grant-go")
        .unwrap();
    assert!(reopened.allowed_endpoint_ids.is_empty());
    assert!(reopened.allowed_origins.is_empty());
    db.rotate_account_credential("grant-go", &fixture_account_key_cipher())
        .unwrap();
    let rotated = db
        .list_identity_model()
        .unwrap()
        .accounts
        .into_iter()
        .find(|row| row.account.id == "grant-go")
        .unwrap();
    assert!(rotated.allowed_endpoint_ids.is_empty());
    assert_eq!(
        rotated.credential_id,
        credential_id_for_legacy_account("grant-go").as_str()
    );
    assert!(rotated.credential_version >= 2);
    drop(db);
    fs::remove_dir_all(dir).unwrap();
}

pub(super) fn accounts_table_present(conn: &Connection) -> bool {
    table_exists(conn, "accounts").unwrap()
}

pub(super) fn leftover_custom_account(
    db: &Database,
    id: &str,
    key_cipher: &str,
    public_model: &str,
    upstream_model: &str,
) {
    let mut custom = account(id);
    custom.provider_id = CUSTOM_PROVIDER_ID.to_string();
    custom.enabled = false;
    custom.credential_kind = CredentialKind::ApiKey;
    custom.quota_scope = QuotaScope::Key;
    custom.key_cipher = key_cipher.into();
    db.create_account_with_contract(
        &custom,
        Some(&AccountCustomConfigInput {
            endpoint_url: "https://old.example/v1/chat/completions".into(),
            upstream_protocol: UpstreamProtocolKind::ChatCompletions,
        }),
        &[AccountModelCapabilityInput {
            public_model: public_model.into(),
            upstream_model: upstream_model.into(),
            protocol: UpstreamProtocolKind::ChatCompletions,
            source: Some("manual".into()),
        }],
    )
    .unwrap();
}

pub(super) fn insert_leftover_capability(
    db: &Database,
    account_id: &str,
    public_model: &str,
    upstream_model: &str,
) {
    db.conn
        .execute(
            "INSERT INTO account_model_capabilities (
                account_id, model_id, upstream_model, protocol, source
             ) VALUES (?1, ?2, ?3, 'chat_completions', 'manual')",
            rusqlite::params![account_id, public_model, upstream_model],
        )
        .unwrap();
}

pub(super) fn capability_pairs(db: &Database, account_id: &str) -> Vec<(String, String)> {
    // These scope tests compare model identities; v59 materializes the
    // platform passthrough protocols as several rows for each same identity.
    let mut pairs: Vec<_> = db
        .list_account_model_capabilities(account_id)
        .unwrap()
        .into_iter()
        .map(|row| (row.public_model, row.upstream_model))
        .collect();
    pairs.dedup();
    pairs
}

pub(super) fn parent_catalog_pairs(db: &Database, parent_id: &str) -> Vec<(String, String)> {
    use ocg_domain::destination::destination_id_for_platform_account;
    let dest_id = destination_id_for_platform_account(parent_id);
    let mut stmt = db
        .conn
        .prepare(
            "SELECT public_model, upstream_model FROM destination_models
             WHERE destination_id = ?1 ORDER BY rowid ASC",
        )
        .unwrap();
    stmt.query_map([dest_id], |row| Ok((row.get(0)?, row.get(1)?)))
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap()
}

pub(super) fn stored_model_scope(
    db: &Database,
    account_id: &str,
) -> ocg_domain::credential::ModelScope {
    db.list_identity_model()
        .unwrap()
        .accounts
        .into_iter()
        .find(|row| row.account.id == account_id)
        .unwrap()
        .binding_model_scope
}

pub(super) fn assert_key_cannot_serve(db: &Database, account_id: &str, model: &str) {
    use ocg_domain::credential::model_scope_allows;
    let runtime = db
        .list_custom_account_runtimes()
        .unwrap()
        .into_iter()
        .find(|runtime| runtime.account_id == account_id)
        .expect("custom runtime");
    assert!(
        runtime.capability_matching_public(model).is_none(),
        "{account_id} still declares {model}"
    );
    assert!(
        !model_scope_allows(&stored_model_scope(db, account_id), model),
        "{account_id} scope still admits {model}"
    );
}

pub(super) fn custom_capability(
    public_model: &str,
    upstream_model: &str,
) -> AccountModelCapabilityInput {
    AccountModelCapabilityInput {
        public_model: public_model.into(),
        upstream_model: upstream_model.into(),
        protocol: UpstreamProtocolKind::ChatCompletions,
        source: Some("manual".into()),
    }
}

pub(super) fn seed_linked_platform_keys(
    db: &Database,
    parent_id: &str,
    keys: &[(&str, &str, &str)],
) {
    use crate::platform::{PlatformGroup, PlatformKind};
    for (account_id, public_model, upstream_model) in keys {
        leftover_custom_account(
            db,
            account_id,
            &format!("cipher-{account_id}"),
            public_model,
            upstream_model,
        );
    }
    db.create_platform_account(
        parent_id,
        PlatformKind::NewApi,
        "Shared Parent",
        "https://platform.example/v1",
        Some("mgmt-cipher"),
    )
    .unwrap();
    for (account_id, _, _) in keys {
        db.link_platform_account(account_id, parent_id, &PlatformGroup::default())
            .unwrap();
    }
}

pub(super) fn rewind_linked_keys_to_v52_leftover_capabilities(
    db: &Database,
    parent_id: &str,
    leftovers: &[(&str, &str, &str)],
) {
    use ocg_domain::destination::destination_id_for_platform_account;
    let dest_id = destination_id_for_platform_account(parent_id);
    db.conn
        .execute_batch(
            "CREATE TABLE account_model_capabilities (
                account_id TEXT NOT NULL,
                model_id TEXT NOT NULL,
                upstream_model TEXT NOT NULL,
                protocol TEXT NOT NULL,
                verified_at TEXT,
                source TEXT NOT NULL DEFAULT 'manual',
                PRIMARY KEY (account_id, model_id, protocol)
             );",
        )
        .unwrap();
    for (account_id, public_model, upstream_model) in leftovers {
        insert_leftover_capability(db, account_id, public_model, upstream_model);
    }
    db.conn
        .execute(
            "DELETE FROM destination_models WHERE destination_id = ?1",
            [&dest_id],
        )
        .unwrap();
    db.conn
        .execute_batch(
            "DELETE FROM schema_version;
             INSERT INTO schema_version(version) VALUES (52);",
        )
        .unwrap();
}

pub(super) fn force_binding_scope_all(db: &Database, account_id: &str) {
    db.conn
        .execute(
            r#"UPDATE credentials SET scope_json = '{"kind":"all"}' WHERE legacy_account_id = ?1"#,
            [account_id],
        )
        .unwrap();
}

#[test]
pub(super) fn refresh_one_linked_key_does_not_replace_sibling_catalog() {
    let dir = temp_data_dir("refresh-one-key");
    let db = Database::open(dir.clone()).unwrap();
    seed_linked_platform_keys(
        &db,
        "parent-refresh",
        &[("key-a", "model-a", "up-a"), ("key-b", "model-b", "up-b")],
    );
    db.replace_account_model_capabilities("key-a", &[custom_capability("model-a", "up-a")])
        .unwrap();
    assert_eq!(
        parent_catalog_pairs(&db, "parent-refresh"),
        vec![
            ("model-a".into(), "up-a".into()),
            ("model-b".into(), "up-b".into()),
        ]
    );
    assert_eq!(
        capability_pairs(&db, "key-b"),
        vec![("model-b".into(), "up-b".into())]
    );
    assert_key_cannot_serve(&db, "key-a", "model-b");
    drop(db);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
pub(super) fn fetch_all_linked_keys_either_order_keeps_shared_catalog_and_scopes() {
    use ocg_domain::credential::ModelScope;

    fn apply_fetch_all(db: &Database, first: &str, second: &str) {
        let first_cap = if first == "key-a" {
            custom_capability("model-a", "up-a")
        } else {
            custom_capability("model-b", "up-b")
        };
        let second_cap = if second == "key-a" {
            custom_capability("model-a", "up-a")
        } else {
            custom_capability("model-b", "up-b")
        };
        db.replace_account_model_capabilities(first, &[first_cap])
            .unwrap();
        db.replace_account_model_capabilities(second, &[second_cap])
            .unwrap();
    }

    fn assert_shared(db: &Database) {
        let mut catalog = parent_catalog_pairs(db, "parent-fetch-all");
        catalog.sort();
        assert_eq!(
            catalog,
            vec![
                ("model-a".into(), "up-a".into()),
                ("model-b".into(), "up-b".into()),
            ]
        );
        assert_eq!(
            stored_model_scope(db, "key-a"),
            ModelScope::Only {
                models: vec!["model-a".into()]
            }
        );
        assert_eq!(
            stored_model_scope(db, "key-b"),
            ModelScope::Only {
                models: vec!["model-b".into()]
            }
        );
        assert_eq!(
            capability_pairs(db, "key-a"),
            vec![("model-a".into(), "up-a".into())]
        );
        assert_eq!(
            capability_pairs(db, "key-b"),
            vec![("model-b".into(), "up-b".into())]
        );
    }

    for (label, first, second) in [
        ("a-then-b", "key-a", "key-b"),
        ("b-then-a", "key-b", "key-a"),
    ] {
        let dir = temp_data_dir(&format!("fetch-all-{label}"));
        let db = Database::open(dir.clone()).unwrap();
        seed_linked_platform_keys(
            &db,
            "parent-fetch-all",
            &[("key-a", "model-a", "up-a"), ("key-b", "model-b", "up-b")],
        );
        apply_fetch_all(&db, first, second);
        assert_shared(&db);
        drop(db);
        fs::remove_dir_all(dir).unwrap();
    }
}

pub(super) fn rewind_identity_satellites_to_v56(conn: &Connection) {
    crate::db::identity_v57::rewind_identity_satellites_to_v56(conn).unwrap();
}

#[test]
pub(super) fn custom_config_and_capabilities_round_trip_without_leftover_tables() {
    let dir = temp_data_dir("v53-custom-roundtrip");
    let db = Database::open(dir.clone()).unwrap();
    assert!(!table_exists(&db.conn, "account_custom_configs").unwrap());
    let mut custom = account("custom-roundtrip");
    custom.provider_id = CUSTOM_PROVIDER_ID.to_string();
    custom.enabled = false;
    custom.credential_kind = CredentialKind::ApiKey;
    custom.quota_scope = QuotaScope::Key;
    db.create_account_with_contract(
        &custom,
        Some(&AccountCustomConfigInput {
            endpoint_url: "https://api.example.com/v1/chat/completions".into(),
            upstream_protocol: UpstreamProtocolKind::ChatCompletions,
        }),
        &[AccountModelCapabilityInput {
            public_model: "org/one".into(),
            upstream_model: "org/one-up".into(),
            protocol: UpstreamProtocolKind::ChatCompletions,
            source: Some("manual".into()),
        }],
    )
    .unwrap();
    db.upsert_account_custom_config(
        "custom-roundtrip",
        &AccountCustomConfigInput {
            endpoint_url: "https://api.example.net/v1/messages".into(),
            upstream_protocol: UpstreamProtocolKind::Messages,
        },
    )
    .unwrap();
    db.replace_account_model_capabilities(
        "custom-roundtrip",
        &[AccountModelCapabilityInput {
            public_model: "org/two".into(),
            upstream_model: "org/two-up".into(),
            protocol: UpstreamProtocolKind::Messages,
            source: Some("manual".into()),
        }],
    )
    .unwrap();
    let live_config = db
        .account_custom_config("custom-roundtrip")
        .unwrap()
        .unwrap();
    let live_caps = db
        .list_account_model_capabilities("custom-roundtrip")
        .unwrap();
    assert_eq!(
        live_config.endpoint_url,
        "https://api.example.net/v1/messages"
    );
    assert_eq!(
        live_config.upstream_protocol,
        UpstreamProtocolKind::Messages
    );
    assert_eq!(live_caps.len(), 1);
    assert_eq!(live_caps[0].public_model, "org/two");
    assert_eq!(live_caps[0].upstream_model, "org/two-up");
    drop(db);

    let db = Database::open(dir.clone()).unwrap();
    assert!(!table_exists(&db.conn, "account_custom_configs").unwrap());
    assert!(!table_exists(&db.conn, "account_model_capabilities").unwrap());
    let reopened = db
        .account_custom_config("custom-roundtrip")
        .unwrap()
        .unwrap();
    let reopened_caps = db
        .list_account_model_capabilities("custom-roundtrip")
        .unwrap();
    assert_eq!(reopened.endpoint_url, live_config.endpoint_url);
    assert_eq!(reopened.upstream_protocol, live_config.upstream_protocol);
    assert_eq!(reopened_caps.len(), 1);
    assert_eq!(reopened_caps[0].public_model, "org/two");
    assert_eq!(reopened_caps[0].upstream_model, "org/two-up");
    drop(db);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
pub(super) fn mutation_then_reopen_keeps_reconstructed_account_and_credential_secrets() {
    let dir = temp_data_dir("v52-mutate-reopen");
    let db = open_with_host_cipher(dir.clone()).unwrap();
    let mut keyed = account("mutate-go");
    keyed.key_cipher = fixture_account_key_cipher();
    keyed.notes = Some("before".into());
    db.create_account(&keyed).unwrap();
    db.update_account(
        "mutate-go",
        &AccountUpdate {
            name: Some("Mutated".into()),
            username: None,
            password: None,
            key: None,
            enabled: Some(false),
            referral_code: None,
            purchase_date: None,
            notes: Some("after".into()),
        },
        Some(&fixture_account_key_cipher()),
        None,
    )
    .unwrap();
    let live = db.get_account("mutate-go").unwrap().unwrap();
    drop(db);

    let db = open_with_host_cipher(dir.clone()).unwrap();
    assert!(!accounts_table_present(&db.conn));
    let reopened = db.get_account("mutate-go").unwrap().unwrap();
    assert_eq!(reopened.enabled, live.enabled);
    assert_eq!(reopened.name, live.name);
    assert_eq!(reopened.notes, live.notes);
    assert_eq!(reopened.key_cipher, live.key_cipher);
    let sql_cipher: String = db
        .conn
        .query_row(
            "SELECT key_cipher FROM credentials WHERE legacy_account_id = ?1",
            ["mutate-go"],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(sql_cipher, reopened.key_cipher);
    assert_fixture_account_cipher(&sql_cipher);
    drop(db);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
pub(super) fn second_identity_credential_is_independent_until_explicit_share() {
    use crate::models::{UpstreamChannel, UsageWindowKind, local_today};
    use crate::provider::ConnectionVerificationStatus;

    let dir = temp_data_dir("independent-second-key");
    let db = open_with_host_cipher(dir.clone()).unwrap();
    let mut first = account("indep-a");
    first.key_cipher = fixture_account_key_cipher();
    db.create_account(&first).unwrap();
    let identity_id: String = db
        .conn
        .query_row(
            "SELECT identity_id FROM credentials WHERE legacy_account_id = 'indep-a'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    let mut second = account("indep-b");
    second.key_cipher = fixture_account_key_cipher();
    db.create_account_for_identity(
        &identity_id,
        &second,
        &local_today(),
        ConnectionVerificationStatus::NotRequired,
        crate::db::identity::QuotaSharingJoin::Independent,
        None,
    )
    .unwrap();
    let members = db.shared_pool_account_ids("indep-a").unwrap();
    assert_eq!(members, vec!["indep-a".to_string()]);
    let until = Utc::now() + chrono::Duration::hours(2);
    db.set_account_rate_limit(
        "indep-a",
        until,
        "429 exhausted",
        Some(UsageWindowKind::FiveHours),
    )
    .unwrap();
    let sibling = db.get_account("indep-b").unwrap().expect("sibling");
    assert!(sibling.cooldown_5h_until.is_none());
    assert!(!sibling.is_cooling_for(UpstreamChannel::Go, Utc::now()));
    drop(db);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
pub(super) fn shared_pool_fanout_preserves_maxima_and_clear_still_propagates() {
    use crate::models::{UsageWindowKind, local_today};
    use crate::provider::ConnectionVerificationStatus;

    let dir = temp_data_dir("shared-pool-max-fanout");
    let db = open_with_host_cipher(dir.clone()).unwrap();
    let mut first = account("max-a");
    first.key_cipher = fixture_account_key_cipher();
    db.create_account(&first).unwrap();
    let identity_id: String = db
        .conn
        .query_row(
            "SELECT identity_id FROM credentials WHERE legacy_account_id = 'max-a'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    let source_credential: String = db
        .conn
        .query_row(
            "SELECT id FROM credentials WHERE legacy_account_id = 'max-a'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    let mut second = account("max-b");
    second.key_cipher = fixture_account_key_cipher();
    db.create_account_for_identity(
        &identity_id,
        &second,
        &local_today(),
        ConnectionVerificationStatus::NotRequired,
        crate::db::identity::QuotaSharingJoin::Shared {
            source_credential_id: source_credential,
        },
        None,
    )
    .unwrap();

    let two_hours = Utc::now() + chrono::Duration::hours(2);
    db.set_account_rate_limit(
        "max-a",
        two_hours,
        "429 two hours",
        Some(UsageWindowKind::FiveHours),
    )
    .unwrap();
    let one_hour = Utc::now() + chrono::Duration::hours(1);
    db.set_account_rate_limit(
        "max-b",
        one_hour,
        "429 one hour",
        Some(UsageWindowKind::FiveHours),
    )
    .unwrap();
    let stored_a = db.get_account("max-a").unwrap().unwrap();
    let stored_b = db.get_account("max-b").unwrap().unwrap();
    assert_eq!(stored_a.cooldown_5h_until, Some(two_hours));
    assert_eq!(stored_b.cooldown_5h_until, Some(two_hours));

    db.clear_account_cooldown("max-a").unwrap();
    let stored_a = db.get_account("max-a").unwrap().unwrap();
    let stored_b = db.get_account("max-b").unwrap().unwrap();
    assert!(stored_a.cooldown_5h_until.is_none());
    assert!(stored_b.cooldown_5h_until.is_none());
    drop(db);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
pub(super) fn rotate_increments_version_and_auth_state_version_together() {
    let dir = temp_data_dir("rotate-versions");
    let db = open_with_host_cipher(dir.clone()).unwrap();
    let mut keyed = account("rotate-go");
    keyed.key_cipher = fixture_account_key_cipher();
    keyed.auth_error = Some("stale-auth".into());
    keyed.last_error = Some("stale-limit".into());
    db.create_account(&keyed).unwrap();
    let before: (i64, i64) = db
        .conn
        .query_row(
            "SELECT COALESCE(credential_version, 1), COALESCE(auth_state_version, 1)
             FROM credentials WHERE legacy_account_id = 'rotate-go'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(before, (1, 1));

    let rotated = db
        .rotate_account_credential("rotate-go", "replacement-cipher")
        .unwrap();
    assert_eq!(rotated.version, 2);
    assert_eq!(rotated.auth_state_version, 2);
    let after: (i64, i64, Option<String>, Option<String>, String) = db
        .conn
        .query_row(
            "SELECT COALESCE(credential_version, 1), COALESCE(auth_state_version, 1),
                    auth_error, last_error, key_cipher
             FROM credentials
             WHERE legacy_account_id = 'rotate-go'",
            [],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                ))
            },
        )
        .unwrap();
    assert_eq!((after.0, after.1), (2, 2));
    assert!(after.2.is_none());
    assert!(after.3.is_none());
    assert_eq!(after.4, "replacement-cipher");

    db.conn
        .execute(
            "UPDATE credentials SET credential_version = NULL, auth_state_version = NULL
             WHERE legacy_account_id = 'rotate-go'",
            [],
        )
        .unwrap();
    let repaired = db
        .rotate_account_credential("rotate-go", "repaired-cipher")
        .unwrap();
    assert_eq!(repaired.version, 2);
    assert_eq!(repaired.auth_state_version, 2);
    assert_eq!(repaired.credential_id, rotated.credential_id);
    drop(db);
    fs::remove_dir_all(dir).unwrap();
}

pub(super) fn probe_row(
    scope: ContractScope,
    model_id: &str,
    now: DateTime<Utc>,
) -> PersistedModelProtocol {
    PersistedModelProtocol {
        scope,
        model_id: model_id.into(),
        protocol: UpstreamProtocolKind::ChatCompletions,
        source: ContractEvidenceSource::ProbeConfirmed,
        verified_at: Some(now),
        observed_at: Some(now),
        last_probe_result: Some(ProbeResultKind::Success),
        last_probe_at: Some(now),
        last_probe_error: None,
    }
}

pub(super) fn pre_v42_backup_paths(dir: &Path) -> Vec<PathBuf> {
    backup_paths_with_prefix(dir, PRE_V42_BACKUP_FILE_PREFIX)
}

pub(super) fn pre_v48_backup_paths(dir: &Path) -> Vec<PathBuf> {
    backup_paths_with_prefix(dir, PRE_V48_BACKUP_FILE_PREFIX)
}

pub(super) fn pre_v58_backup_paths(dir: &Path) -> Vec<PathBuf> {
    backup_paths_with_prefix(dir, PRE_V58_BACKUP_FILE_PREFIX)
}

pub(super) fn assert_leftover_dynamic_provider_storage_absent(conn: &Connection) {
    assert!(!table_exists(conn, "providers").unwrap());
    assert!(!table_exists(conn, "provider_models").unwrap());
}

pub(super) fn assert_leftover_identity_tables_absent(conn: &Connection) {
    for table in crate::db::identity_v57::IDENTITY_LEFTOVER_TABLES {
        assert!(!table_exists(conn, table).unwrap(), "{table}");
    }
}

pub(super) fn leftover_providers_ddl() -> &'static str {
    "CREATE TABLE providers (
            id TEXT PRIMARY KEY,
            origin TEXT NOT NULL CHECK(origin IN ('builtin','preset','custom')),
            adapter_kind TEXT NOT NULL,
            name TEXT NOT NULL,
            endpoint_url TEXT,
            upstream_protocol TEXT,
            auth_kind TEXT,
            preset_id TEXT,
            offering TEXT NOT NULL DEFAULT 'api' CHECK(offering IN ('plan','api')),
            display_family TEXT,
            endpoint_per_account INTEGER NOT NULL DEFAULT 0,
            created_at TEXT NOT NULL,
            updated_at TEXT NOT NULL,
            onboarding_draft INTEGER NOT NULL DEFAULT 0
         );
         CREATE TABLE provider_models (
            provider_id TEXT NOT NULL,
            public_model TEXT NOT NULL,
            public_model_key TEXT NOT NULL,
            upstream_model TEXT NOT NULL,
            upstream_override TEXT,
            PRIMARY KEY (provider_id, public_model_key)
         );"
}

pub(super) fn materialize_legacy_providers_for_rewind(conn: &Connection) {
    if table_exists(conn, "providers").unwrap() {
        return;
    }
    conn.execute_batch(leftover_providers_ddl())
        .expect("rewind leftover providers should recreate");
}

pub(super) fn assert_retired_dynamic_provider_tables_absent(conn: &Connection) {
    assert!(!table_exists(conn, "dynamic_providers").unwrap());
    assert!(!table_exists(conn, "dynamic_provider_models").unwrap());
    let leftover_index: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master
             WHERE type = 'index' AND name = 'idx_dynamic_provider_models_provider'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(leftover_index, 0);
}

pub(super) fn assert_v48_inert_columns_absent(conn: &Connection) {
    assert!(!table_has_column(conn, "accounts", "free_alias_enabled").unwrap());
    for column in [
        "chat_completions_enabled",
        "responses_enabled",
        "messages_enabled",
    ] {
        assert!(
            !table_has_column(conn, "provider_contract_scopes", column).unwrap(),
            "{column}"
        );
    }
}

pub(super) fn custom_platform_import_record(
    id: &str,
    public_model: &str,
    upstream_model: &str,
) -> AccountImportRecord {
    let mut custom = account(id);
    custom.provider_id = CUSTOM_PROVIDER_ID.to_string();
    custom.credential_kind = CredentialKind::ApiKey;
    custom.quota_scope = QuotaScope::Key;
    custom.key_cipher = format!("cipher-{id}");
    AccountImportRecord {
        account: custom,
        custom_config: Some(AccountCustomConfigInput {
            endpoint_url: "https://old.example/v1/chat/completions".into(),
            upstream_protocol: UpstreamProtocolKind::ChatCompletions,
        }),
        capabilities: vec![custom_capability(public_model, upstream_model)],
        verification_status: ConnectionVerificationStatus::NotRequired,
        connection_verified_at: None,
        ollama_billing_tier: None,
    }
}

pub(super) fn identity_snapshot_forcing_all(
    db: &Database,
    account_ids: &[&str],
) -> crate::db::identity::IdentityImportSnapshot {
    use crate::db::identity::{IdentityImportSnapshot, ImportedAccountIdentity, ImportedIdentity};
    use ocg_domain::credential::ModelScope;
    let model = db.list_identity_model().unwrap();
    let accounts = model
        .accounts
        .iter()
        .filter(|row| account_ids.contains(&row.account.id.as_str()))
        .map(|row| ImportedAccountIdentity {
            account_id: row.account.id.clone(),
            identity_id: row.identity_id.clone(),
            credential_id: row.credential_id.clone(),
            credential_version: row.credential_version,
            auth_state_version: row.auth_state_version,
            binding_id: row.binding_id.clone(),
            binding_enabled: row.binding_enabled,
            binding_model_scope: ModelScope::All,
            allowed_endpoint_ids: row.allowed_endpoint_ids.clone(),
            allowed_origins: row.allowed_origins.clone(),
        })
        .collect::<Vec<_>>();
    let identity_ids = accounts
        .iter()
        .map(|row| row.identity_id.clone())
        .collect::<HashSet<_>>();
    let identities = model
        .identities
        .into_iter()
        .filter(|identity| identity_ids.contains(&identity.id))
        .map(|identity| ImportedIdentity {
            id: identity.id,
            label: identity.label,
            identity_confidence: identity.identity_confidence,
            authority_site: identity.authority_site,
            authority_subject: identity.authority_subject,
            enabled: identity.enabled,
            notes: identity.notes,
        })
        .collect();
    IdentityImportSnapshot {
        identities,
        accounts,
        quota_pools: Vec::new(),
    }
}

pub(super) fn assert_restored_platform_catalog_and_scopes(db: &Database, parent_id: &str) {
    use crate::custom::eligible_custom_public_models;
    use ocg_domain::credential::ModelScope;
    let mut catalog = parent_catalog_pairs(db, parent_id);
    catalog.sort();
    assert_eq!(
        catalog,
        vec![
            ("model-a".into(), "up-a".into()),
            ("model-b".into(), "up-b".into()),
        ]
    );
    assert_eq!(
        stored_model_scope(db, "key-a"),
        ModelScope::Only {
            models: vec!["model-a".into()]
        }
    );
    assert_eq!(
        stored_model_scope(db, "key-b"),
        ModelScope::Only {
            models: vec!["model-b".into()]
        }
    );
    assert_eq!(
        capability_pairs(db, "key-a"),
        vec![("model-a".into(), "up-a".into())]
    );
    assert_eq!(
        capability_pairs(db, "key-b"),
        vec![("model-b".into(), "up-b".into())]
    );
    let mut public = eligible_custom_public_models(&db.list_custom_account_runtimes().unwrap());
    public.sort();
    assert_eq!(public, vec!["model-a".to_string(), "model-b".to_string()]);
    assert_key_cannot_serve(db, "key-a", "model-b");
    assert_key_cannot_serve(db, "key-b", "model-a");
}

pub(super) fn node_import_record(
    db: &Database,
    accounts: Vec<AccountImportRecord>,
    platform_accounts: Vec<crate::platform::PortablePlatformAccount>,
    platform_links: Vec<crate::platform::PortablePlatformLink>,
) -> NodeImportRecord {
    let mut account_order: Vec<String> = db
        .list_accounts()
        .unwrap()
        .into_iter()
        .map(|account| account.id)
        .collect();
    for record in &accounts {
        if !account_order.contains(&record.account.id) {
            account_order.push(record.account.id.clone());
        }
    }
    let config = crate::models::AppConfig {
        gateway_key: "ocg-import-primary-key".into(),
        ..crate::models::AppConfig::default()
    };
    NodeImportRecord {
        platform_links_authoritative: true,
        platform_accounts,
        platform_links,
        platform_catalogs: HashMap::new(),
        destination_controls: Vec::new(),
        accounts,
        account_order,
        config_json: serde_json::to_string(&config).unwrap(),
        sub_keys: Vec::new(),
        zen_free_enabled: false,
        zen_catalog: crate::kernel::zen::ZenFreeModelCatalog::default(),
        provider_contracts: crate::provider_contracts::PersistedContracts::default(),
        dynamic_providers: Vec::new(),
        custom_destinations: Vec::new(),
        custom_credential_destinations: HashMap::new(),
        identity_snapshot: None,
        draft_provider_ids: HashSet::new(),
        platform_observer_ciphers: HashMap::new(),
        platform_snapshots: HashMap::new(),
        platform_versions: HashMap::new(),
        cpa_base_url: None,
        cpa_management_key_cipher: None,
    }
}

pub(super) fn go_import_record(id: &str) -> AccountImportRecord {
    AccountImportRecord {
        account: account(id),
        custom_config: None,
        capabilities: Vec::new(),
        verification_status: ConnectionVerificationStatus::NotRequired,
        connection_verified_at: None,
        ollama_billing_tier: None,
    }
}

pub(super) const FIXTURE_ACCOUNT_PLAINTEXT: &str = "sk-fixture";

pub(super) fn test_host_cipher() -> Arc<dyn KeyCipher + Send + Sync> {
    Arc::new(StaticKeyCipher::new(TEST_HOST_SECRET))
}

pub(super) fn fixture_account_key_cipher() -> String {
    test_host_cipher()
        .encrypt(FIXTURE_ACCOUNT_PLAINTEXT)
        .expect("test host cipher should encrypt fixture account keys")
}

pub(super) fn open_with_host_cipher(dir: PathBuf) -> Result<Database> {
    Database::open_with_cipher(dir, test_host_cipher())
}

pub(super) fn assert_fixture_account_cipher(value: &str) {
    assert_eq!(
        test_host_cipher()
            .decrypt(value)
            .expect("fixture account cipher should decrypt with the test host"),
        FIXTURE_ACCOUNT_PLAINTEXT
    );
}

pub(super) fn temp_data_dir(label: &str) -> PathBuf {
    let mut dir = std::env::temp_dir();
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock should be after epoch")
        .as_nanos();
    dir.push(format!("ocg-db-test-{label}-{nanos}"));
    fs::create_dir_all(&dir).expect("test data dir should be created");
    dir
}

pub(super) fn create_v21_fixture(dir: &Path, include_reserved_account_conflict: bool) {
    let db = Database::open(dir.to_path_buf()).expect("fixture database should open");
    let mut rollback = account("rollback-account");
    rollback.key_cipher = fixture_account_key_cipher();
    db.create_account(&rollback)
        .expect("representative account should save");
    db.log_forward(&forward_log("rollback-account", "success", 4.25))
        .expect("representative forward log should save");
    if !include_reserved_account_conflict {
        db.conn
            .execute(
                "DELETE FROM credentials WHERE legacy_account_id = ?1",
                [ZEN_FREE_ACCOUNT_ID],
            )
            .expect("reserved v22 account should be removed from a normal v21 fixture");
    }
    drop(db);
    reverse_current_to_v34(dir);

    let conn = Connection::open(dir.join("data.sqlite")).expect("fixture db should reopen");
    conn.execute_batch(
        "PRAGMA foreign_keys=OFF;
             DROP TRIGGER IF EXISTS access_keys_protect_primary_delete;
             DROP TABLE IF EXISTS access_keys;
             CREATE TABLE IF NOT EXISTS sub_gateway_keys (
                id TEXT PRIMARY KEY,
                name TEXT NOT NULL,
                key TEXT NOT NULL,
                enabled INTEGER NOT NULL DEFAULT 1,
                deleted_at TEXT,
                created_at TEXT NOT NULL
             );
             CREATE UNIQUE INDEX IF NOT EXISTS idx_sub_gateway_keys_key
                ON sub_gateway_keys(key) WHERE deleted_at IS NULL AND key <> '';
             DROP INDEX IF EXISTS idx_forward_logs_route_account;
             DROP INDEX IF EXISTS idx_forward_logs_provider_offering;
             DROP INDEX IF EXISTS idx_account_model_capabilities_account;
             DROP TABLE IF EXISTS provider_usage_sync_state;
             DROP TABLE IF EXISTS provider_pricing_snapshots;
             DROP TABLE IF EXISTS credit_balances;
             DROP TABLE IF EXISTS quota_windows;
             DROP TABLE IF EXISTS account_custom_configs;
             DROP TABLE IF EXISTS account_model_capabilities;
             ALTER TABLE accounts DROP COLUMN verification_error;
             ALTER TABLE accounts DROP COLUMN connection_verified_at;
             ALTER TABLE accounts DROP COLUMN verification_status;
             ALTER TABLE forward_logs DROP COLUMN native_cost_currency;
             ALTER TABLE forward_logs DROP COLUMN native_cost_unit;
             ALTER TABLE forward_logs DROP COLUMN native_cost_value;
             ALTER TABLE forward_logs DROP COLUMN upstream_model;
             ALTER TABLE forward_logs DROP COLUMN resolved_alias;
             ALTER TABLE forward_logs DROP COLUMN requested_model;
             ALTER TABLE accounts DROP COLUMN free_alias_enabled;
             ALTER TABLE accounts DROP COLUMN quota_scope;
             ALTER TABLE accounts DROP COLUMN credential_kind;
             ALTER TABLE accounts DROP COLUMN offering_id;
             ALTER TABLE accounts DROP COLUMN provider_id;
             ALTER TABLE forward_logs DROP COLUMN effective_paid_cost_usd;
             ALTER TABLE forward_logs DROP COLUMN quota_debit;
             ALTER TABLE forward_logs DROP COLUMN raw_cost_usd;
             ALTER TABLE forward_logs DROP COLUMN credential_account_id;
             ALTER TABLE forward_logs DROP COLUMN offering_id;
             ALTER TABLE forward_logs DROP COLUMN provider_id;
             ALTER TABLE forward_logs DROP COLUMN route_account_id;
             DELETE FROM schema_version;
             INSERT INTO schema_version (version) VALUES (21);
             PRAGMA foreign_keys=ON;",
    )
    .expect("v21 fixture should be created");
    restore_usage_sync_account_columns(&conn);
}

pub(super) fn restore_usage_sync_account_columns(conn: &Connection) {
    for (column, definition) in [
        ("usage_sync_last_success_at", "TEXT"),
        ("usage_sync_last_attempt_at", "TEXT"),
        ("usage_sync_next_eligible_at", "TEXT"),
        ("usage_sync_failure_streak", "INTEGER NOT NULL DEFAULT 0"),
        ("usage_sync_last_expedited_at", "TEXT"),
    ] {
        if !table_has_column(conn, "accounts", column).unwrap() {
            conn.execute(
                &format!("ALTER TABLE accounts ADD COLUMN {column} {definition}"),
                [],
            )
            .unwrap();
        }
    }
}

pub(super) fn create_v20_fixture(dir: &Path, include_reserved_account_conflict: bool) {
    create_v21_fixture(dir, include_reserved_account_conflict);
    let conn = Connection::open(dir.join("data.sqlite")).expect("v21 fixture should reopen");
    for column in USAGE_SYNC_ACCOUNT_COLUMNS {
        if table_has_column(&conn, "accounts", column).unwrap() {
            conn.execute(&format!("ALTER TABLE accounts DROP COLUMN {column}"), [])
                .unwrap();
        }
    }
    conn.execute_batch(
        "DELETE FROM schema_version;
             INSERT INTO schema_version (version) VALUES (20);",
    )
    .expect("v20 fixture should be created");
}

pub(super) fn pre_v22_backup_paths(dir: &Path) -> Vec<PathBuf> {
    let mut paths = fs::read_dir(dir)
        .expect("fixture directory should be readable")
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path())
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| {
                    name.starts_with(PRE_V22_BACKUP_FILE_PREFIX) && name.ends_with(".bak")
                })
        })
        .collect::<Vec<_>>();
    paths.sort();
    paths
}

pub(super) fn backup_paths_with_prefix(dir: &Path, prefix: &str) -> Vec<PathBuf> {
    let mut paths = fs::read_dir(dir)
        .expect("fixture directory should be readable")
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path())
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with(prefix) && name.ends_with(".bak"))
        })
        .collect::<Vec<_>>();
    paths.sort();
    paths
}

pub(super) fn pre_v23_backup_paths(dir: &Path) -> Vec<PathBuf> {
    backup_paths_with_prefix(dir, PRE_V23_BACKUP_FILE_PREFIX)
}

pub(super) fn pre_v3_backup_paths(dir: &Path) -> Vec<PathBuf> {
    backup_paths_with_prefix(dir, PRE_V3_BACKUP_FILE_PREFIX)
}

pub(super) fn pre_v35_backup_paths(dir: &Path) -> Vec<PathBuf> {
    backup_paths_with_prefix(dir, PRE_V35_BACKUP_FILE_PREFIX)
}

pub(super) fn drop_unified_provider_tables(conn: &Connection) {
    conn.execute_batch(
        "DROP TABLE IF EXISTS provider_models;
         DROP TABLE IF EXISTS providers;
         DROP TABLE IF EXISTS provider_model_protocol_preferences_v42;",
    )
    .expect("pre-v42 fixtures must not carry unified provider tables");
}

pub(super) fn restore_v47_inert_columns(conn: &Connection) {
    account_store::materialize_legacy_accounts_for_rewind(conn).unwrap();
    conn.execute_batch(
        "ALTER TABLE accounts ADD COLUMN free_alias_enabled INTEGER NOT NULL DEFAULT 0;
         ALTER TABLE provider_contract_scopes ADD COLUMN chat_completions_enabled INTEGER NOT NULL DEFAULT 1;
         ALTER TABLE provider_contract_scopes ADD COLUMN responses_enabled INTEGER NOT NULL DEFAULT 1;
         ALTER TABLE provider_contract_scopes ADD COLUMN messages_enabled INTEGER NOT NULL DEFAULT 1;",
    )
    .expect("v47 inert columns should restore");
}

pub(super) fn rewind_current_to_v47(conn: &Connection) {
    restore_v47_inert_columns(conn);
    conn.execute_batch(
        "DELETE FROM schema_version;
         INSERT INTO schema_version (version) VALUES (47);",
    )
    .expect("schema should rewind to v47");
    assert_eq!(
        lifecycle::schema_version_on(conn).unwrap(),
        V47_SCHEMA_VERSION
    );
}

pub(super) fn reverse_current_to_v34(dir: &Path) {
    let path = dir.join("data.sqlite");
    let conn = Connection::open(&path).expect("migrated database should reopen for reverse");
    account_store::materialize_legacy_accounts_for_rewind(&conn).unwrap();
    drop_unified_provider_tables(&conn);
    restore_v47_inert_columns(&conn);
    conn.execute_batch(
        "
        PRAGMA foreign_keys=OFF;
        ALTER TABLE accounts ADD COLUMN offering_id TEXT NOT NULL DEFAULT 'go';
        UPDATE accounts SET offering_id = CASE provider_id
            WHEN 'opencode' THEN 'go'
            WHEN 'opencode-zen-free' THEN 'anonymous-free'
            WHEN 'command-code' THEN 'goat'
            WHEN 'minimax' THEN 'cn'
            WHEN 'kimi' THEN 'cn'
            WHEN 'custom' THEN 'api'
            WHEN 'cpa' THEN 'local'
            ELSE offering_id
        END;
        ALTER TABLE forward_logs ADD COLUMN offering_id TEXT;
        UPDATE forward_logs SET offering_id = CASE provider_id
            WHEN 'opencode' THEN 'go'
            WHEN 'opencode-zen-free' THEN 'anonymous-free'
            WHEN 'command-code' THEN 'goat'
            WHEN 'minimax' THEN 'cn'
            WHEN 'kimi' THEN 'cn'
            WHEN 'custom' THEN 'api'
            WHEN 'cpa' THEN 'local'
            ELSE offering_id
        END;
        DROP INDEX IF EXISTS idx_forward_logs_provider;
        CREATE INDEX IF NOT EXISTS idx_forward_logs_provider_offering
            ON forward_logs(provider_id, offering_id);
        CREATE TABLE provider_model_catalogs_v34 (
            provider_id TEXT NOT NULL,
            offering_id TEXT NOT NULL,
            models_json TEXT NOT NULL,
            refreshed_at TEXT,
            source_url TEXT NOT NULL,
            PRIMARY KEY (provider_id, offering_id)
        );
        INSERT INTO provider_model_catalogs_v34
            (provider_id, offering_id, models_json, refreshed_at, source_url)
        SELECT provider_id,
               CASE provider_id
                   WHEN 'opencode' THEN 'go'
                   WHEN 'opencode-zen-free' THEN 'anonymous-free'
                   WHEN 'command-code' THEN 'goat'
                   WHEN 'minimax' THEN 'cn'
                   WHEN 'kimi' THEN 'cn'
                   WHEN 'custom' THEN 'api'
                   WHEN 'cpa' THEN 'local'
                   ELSE 'unknown'
               END,
               models_json, refreshed_at, source_url
          FROM provider_model_catalogs;
        DROP TABLE provider_model_catalogs;
        ALTER TABLE provider_model_catalogs_v34 RENAME TO provider_model_catalogs;
        DROP INDEX IF EXISTS idx_provider_pricing_active;
        CREATE TABLE provider_pricing_snapshots_v34 (
            provider_id TEXT NOT NULL,
            offering_id TEXT NOT NULL,
            revision TEXT NOT NULL,
            activated_at TEXT NOT NULL,
            document_updated_at TEXT,
            source_url TEXT NOT NULL,
            content_hash TEXT NOT NULL,
            snapshot_json TEXT NOT NULL,
            PRIMARY KEY (provider_id, offering_id, revision)
        );
        INSERT INTO provider_pricing_snapshots_v34
            (provider_id, offering_id, revision, activated_at, document_updated_at,
             source_url, content_hash, snapshot_json)
        SELECT provider_id,
               CASE provider_id
                   WHEN 'opencode' THEN 'go'
                   WHEN 'opencode-zen-free' THEN 'anonymous-free'
                   WHEN 'command-code' THEN 'goat'
                   WHEN 'minimax' THEN 'cn'
                   WHEN 'kimi' THEN 'cn'
                   WHEN 'custom' THEN 'api'
                   WHEN 'cpa' THEN 'local'
                   ELSE 'unknown'
               END,
               revision, activated_at, document_updated_at, source_url, content_hash, snapshot_json
          FROM provider_pricing_snapshots;
        DROP TABLE provider_pricing_snapshots;
        ALTER TABLE provider_pricing_snapshots_v34 RENAME TO provider_pricing_snapshots;
        CREATE INDEX IF NOT EXISTS idx_provider_pricing_active
            ON provider_pricing_snapshots(provider_id, offering_id, activated_at DESC);
        DELETE FROM schema_version;
        INSERT INTO schema_version (version) VALUES (34);
        PRAGMA foreign_keys=ON;
        ",
    )
    .expect("schema should reverse to v34");
    assert_eq!(
        lifecycle::schema_version_on(&conn).unwrap(),
        V34_SCHEMA_VERSION
    );
}

pub(super) fn reverse_current_to_v26(dir: &Path) {
    reverse_current_to_v34(dir);
    let path = dir.join("data.sqlite");
    let conn = Connection::open(&path).expect("migrated database should reopen for reverse");
    let primary = conn
        .query_row(
            "SELECT key FROM access_keys WHERE is_primary = 1 LIMIT 1",
            [],
            |row| row.get::<_, String>(0),
        )
        .unwrap_or_default();
    if let Some(json) = conn
        .query_row(
            "SELECT value FROM settings WHERE key = 'config'",
            [],
            |row| row.get::<_, String>(0),
        )
        .optional()
        .unwrap()
    {
        let mut value: serde_json::Value =
            serde_json::from_str(&json).unwrap_or_else(|_| serde_json::json!({}));
        if let Some(object) = value.as_object_mut() {
            object.insert(
                "gateway_key".to_string(),
                serde_json::Value::String(primary.clone()),
            );
        }
        conn.execute(
            "INSERT INTO settings (key, value) VALUES ('config', ?1)
                 ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            [serde_json::to_string(&value).unwrap()],
        )
        .unwrap();
    } else if !primary.is_empty() {
        conn.execute(
            "INSERT INTO settings (key, value) VALUES ('config', ?1)",
            [serde_json::json!({ "gateway_key": primary }).to_string()],
        )
        .unwrap();
    }
    conn.execute_batch(
        "
            PRAGMA foreign_keys=OFF;
            CREATE TABLE IF NOT EXISTS sub_gateway_keys (
                id TEXT PRIMARY KEY,
                name TEXT NOT NULL,
                key TEXT NOT NULL,
                enabled INTEGER NOT NULL DEFAULT 1,
                deleted_at TEXT,
                created_at TEXT NOT NULL
            );
            CREATE UNIQUE INDEX IF NOT EXISTS idx_sub_gateway_keys_key
                ON sub_gateway_keys(key) WHERE deleted_at IS NULL AND key <> '';
            INSERT OR IGNORE INTO sub_gateway_keys (id, name, key, enabled, deleted_at, created_at)
                SELECT id, name, key, enabled, deleted_at, created_at
                FROM access_keys WHERE is_primary = 0;
            DROP TRIGGER IF EXISTS access_keys_protect_primary_delete;
            DROP TABLE IF EXISTS access_keys;
            ",
    )
    .expect("access_keys should reverse into sub_gateway_keys");
    restore_usage_sync_account_columns(&conn);
    if table_has_column(&conn, "accounts", "goat_model_access").unwrap() {
        conn.execute_batch("ALTER TABLE accounts DROP COLUMN goat_model_access;")
            .expect("v28 GOAT model access should reverse out of the v26 fixture");
    }
    conn.execute_batch(
        "DELETE FROM schema_version;
             INSERT INTO schema_version (version) VALUES (26);
             PRAGMA foreign_keys=ON;",
    )
    .expect("schema should reverse to v26");
    assert_eq!(
        lifecycle::schema_version_on(&conn).unwrap(),
        V26_SCHEMA_VERSION
    );
}

pub(super) fn account(id: &str) -> Account {
    Account {
        id: id.into(),
        provider_id: default_provider_id(),
        credential_kind: default_credential_kind(),
        quota_scope: default_quota_scope(),
        name: id.into(),
        username: None,
        password_cipher: None,
        key_cipher: "cipher".into(),
        enabled: true,
        account_type: AccountType::Key,
        setup_step: AccountSetupStep::Ready,
        referral_code: None,
        purchase_date: String::new(),
        expires_on: String::new(),
        cooldown_until: None,
        cooldown_generic_until: None,
        cooldown_5h_until: None,
        cooldown_week_until: None,
        cooldown_month_until: None,
        cooldown_free_until: None,
        last_error: None,
        auth_error: None,
        notes: None,
        created_at: Utc::now(),
        updated_at: Utc::now(),
    }
}

pub(super) fn persist_sanitation_account(
    db: &Database,
    plan: BuiltinProvider,
    id: &str,
    notes: &str,
) {
    let mut draft = account(id);
    draft.provider_id = plan.provider_id.to_string();
    draft.credential_kind = plan.credential_kind;
    draft.quota_scope = plan.quota_scope;
    draft.enabled = false;
    draft.notes = Some(notes.to_string());
    if plan_requires_custom_config(plan) {
        db.create_account_with_contract(
            &draft,
            Some(&AccountCustomConfigInput {
                endpoint_url: "https://api.example.com/v1/chat/completions".into(),
                upstream_protocol: UpstreamProtocolKind::ChatCompletions,
            }),
            &[AccountModelCapabilityInput {
                public_model: "org/model".into(),
                upstream_model: "org/model".into(),
                protocol: UpstreamProtocolKind::ChatCompletions,
                source: None,
            }],
        )
        .unwrap();
    } else {
        db.create_account_with_contract(&draft, None, &[]).unwrap();
    }
}

pub(super) fn leftover_enable(db: &Database, id: &str) {
    let changed = db
        .conn
        .execute(
            "UPDATE credentials SET enabled = 1 WHERE legacy_account_id = ?1",
            [id],
        )
        .unwrap();
    assert_eq!(changed, 1, "{id}");
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct SanitationSnapshot {
    enabled: bool,
    name: String,
    notes: Option<String>,
    updated_at: DateTime<Utc>,
    verification: ConnectionVerificationStatus,
    verification_error: Option<String>,
}

pub(super) fn sanitation_snapshot(db: &Database, id: &str) -> SanitationSnapshot {
    let account = db.get_account(id).unwrap().expect(id);
    let verification = db.account_verification_state(id).unwrap().expect(id);
    SanitationSnapshot {
        enabled: account.enabled,
        name: account.name,
        notes: account.notes,
        updated_at: account.updated_at,
        verification: verification.status,
        verification_error: verification.verification_error,
    }
}

pub(super) fn forward_log(account_id: &str, status: &str, cost: f64) -> ForwardLog {
    ForwardLog {
        id: 0,
        timestamp: Utc::now(),
        model: "test".into(),
        account_id: account_id.into(),
        account_name: account_id.into(),
        route_account_id: None,
        provider_id: None,
        credential_account_id: None,
        client_key_id: None,
        client_key_name: None,
        status: status.into(),
        http_status: Some(200),
        route: String::new(),
        prompt_tokens: 0,
        completion_tokens: 0,
        cached_tokens: 0,
        cache_creation_tokens: 0,
        cost: Some(cost),
        raw_cost_usd: None,
        quota_debit: None,
        effective_paid_cost_usd: None,
        pricing_revision_id: None,
        quota_multiplier: None,
        local_adjustment_multiplier: None,
        service_tier: None,
        cost_state: "legacy_estimate".into(),
        error_message: None,
        request_id: None,
        attempt: None,
        error_source: None,
        error_stage: None,
        duration_ms: None,
        diagnostic: None,
    }
}

#[test]
pub(super) fn managed_setup_requires_order_and_matching_verified_key() {
    let dir = temp_data_dir("managed-setup-state");
    let db = Database::open(dir.clone()).expect("db should open");
    let mut managed = account("managed");
    managed.account_type = AccountType::Managed;
    managed.setup_step = AccountSetupStep::GoogleAccount;
    managed.key_cipher.clear();
    managed.enabled = false;
    db.create_account(&managed).expect("draft should save");

    assert!(
        !db.advance_managed_setup(
            "managed",
            AccountSetupStep::OpencodeRegistration,
            AccountSetupStep::Payment,
        )
        .unwrap()
    );
    for (from, to) in [
        (
            AccountSetupStep::GoogleAccount,
            AccountSetupStep::OpencodeRegistration,
        ),
        (
            AccountSetupStep::OpencodeRegistration,
            AccountSetupStep::Payment,
        ),
    ] {
        assert!(db.advance_managed_setup("managed", from, to).unwrap());
    }
    db.conn
            .execute(
                "UPDATE credentials SET purchase_date = '2000-01-01', usage_month_window_cost_offset = 1 WHERE legacy_account_id = 'managed'",
                [],
            )
            .unwrap();
    assert!(
        db.advance_managed_setup(
            "managed",
            AccountSetupStep::Payment,
            AccountSetupStep::KeyVerification,
        )
        .unwrap()
    );
    let paid = db.get_account("managed").unwrap().unwrap();
    assert_eq!(paid.purchase_date, local_today());
    let month_offset: f64 = db
        .conn
        .query_row(
            "SELECT usage_month_window_cost_offset FROM credentials WHERE legacy_account_id = 'managed'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(month_offset, 0.0);
    assert!(
        db.save_managed_key_for_verification("managed", "candidate")
            .unwrap()
    );
    assert!(
        !db.complete_managed_setup_if_key_matches("managed", "stale")
            .unwrap()
    );
    assert!(
        db.complete_managed_setup_if_key_matches("managed", "candidate")
            .unwrap()
    );
    let ready = db.get_account("managed").unwrap().unwrap();
    assert_eq!(ready.setup_step, AccountSetupStep::Ready);
    assert!(ready.enabled);

    drop(db);
    fs::remove_dir_all(dir).expect("test data dir should be removed");
}

#[test]
pub(super) fn delete_account_removes_credential_grants() {
    use ocg_domain::credential::credential_id_for_legacy_account;

    let dir = temp_data_dir("delete-grants");
    let mut db = Database::open(dir.clone()).expect("db should open");
    db.create_account(&account("gone"))
        .expect("account should save");
    let credential_id = credential_id_for_legacy_account("gone").to_string();
    let before: i64 = db
        .conn
        .query_row(
            "SELECT COUNT(*) FROM credential_grants WHERE credential_id = ?1",
            [&credential_id],
            |row| row.get(0),
        )
        .unwrap();
    assert!(before > 0, "create should persist credential grants");
    db.delete_account("gone").expect("account should delete");
    let after: i64 = db
        .conn
        .query_row(
            "SELECT COUNT(*) FROM credential_grants WHERE credential_id = ?1",
            [&credential_id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(after, 0);
    drop(db);
    fs::remove_dir_all(dir).expect("test data dir should be removed");
}

#[test]
pub(super) fn managed_key_verification_transaction_rolls_back_after_candidate_write_failure() {
    let dir = temp_data_dir("managed-key-atomic-rollback");
    let db = Database::open(dir.clone()).expect("db should open");
    let mut managed = account("managed-atomic");
    managed.account_type = AccountType::Managed;
    managed.setup_step = AccountSetupStep::KeyVerification;
    managed.key_cipher = "original-cipher".into();
    managed.enabled = false;
    db.create_account(&managed).expect("draft should save");
    let cooldown = (Utc::now() + Duration::hours(2)).to_rfc3339();
    db.conn
        .execute(
            "UPDATE credentials
                 SET auth_error = 'original-auth', last_error = 'original-limit',
                     cooldown_until = ?2, cooldown_generic_until = ?2,
                     verification_error = 'original-verification'
                 WHERE legacy_account_id = ?1",
            params![managed.id, cooldown],
        )
        .expect("rollback sentinel state should save");
    let before = db.get_account(&managed.id).unwrap().unwrap();
    let before_verification = db.account_verification_state(&managed.id).unwrap().unwrap();

    // The first transaction update writes the candidate while leaving the
    // setup step unchanged. This trigger deterministically aborts the
    // following completion update, after that first intended write.
    db.conn
        .execute_batch(
            "CREATE TRIGGER fail_managed_verification_completion
                 BEFORE UPDATE ON credentials
                 WHEN OLD.legacy_account_id = 'managed-atomic'
                      AND OLD.setup_step = 'key_verification'
                      AND NEW.setup_step = 'ready'
                 BEGIN
                     SELECT RAISE(ABORT, 'injected managed verification completion failure');
                 END;",
        )
        .expect("fault trigger should install");

    let error = db
        .commit_managed_key_verification(
            &managed.id,
            &ManagedKeyVerificationCas::from_account(&before),
            "candidate-cipher",
            &ManagedKeyVerificationWrite::Verified {
                rate_limit: None,
                account_name: managed.name.clone(),
            },
        )
        .expect_err("second intended write should fail");
    assert!(
        error
            .to_string()
            .contains("injected managed verification completion failure"),
        "{error:#}"
    );

    let after = db.get_account(&managed.id).unwrap().unwrap();
    let after_verification = db.account_verification_state(&managed.id).unwrap().unwrap();
    assert_eq!(after.key_cipher, before.key_cipher);
    assert_eq!(after.enabled, before.enabled);
    assert_eq!(after.setup_step, before.setup_step);
    assert_eq!(after.auth_error, before.auth_error);
    assert_eq!(after.last_error, before.last_error);
    assert_eq!(after.cooldown_until, before.cooldown_until);
    assert_eq!(after.cooldown_generic_until, before.cooldown_generic_until);
    assert_eq!(after.updated_at, before.updated_at);
    assert_eq!(after_verification.status, before_verification.status);
    assert_eq!(
        after_verification.connection_verified_at,
        before_verification.connection_verified_at
    );
    assert_eq!(
        after_verification.verification_error,
        before_verification.verification_error
    );
    assert!(
        db.list_gateway_logs(10)
            .unwrap()
            .iter()
            .all(|log| !log.message.contains("managed-atomic")),
        "success log must roll back with the account writes"
    );

    drop(db);
    fs::remove_dir_all(dir).expect("test data dir should be removed");
}

#[test]
pub(super) fn managed_key_verification_transaction_rolls_back_if_gateway_audit_insert_aborts() {
    let dir = temp_data_dir("managed-key-audit-rollback");
    let db = Database::open(dir.clone()).expect("db should open");
    let mut managed = account("managed-audit-atomic");
    managed.account_type = AccountType::Managed;
    managed.setup_step = AccountSetupStep::KeyVerification;
    managed.key_cipher = "original-cipher".into();
    managed.enabled = false;
    db.create_account(&managed).expect("draft should save");
    let before = db.get_account(&managed.id).unwrap().unwrap();
    let before_verification = db.account_verification_state(&managed.id).unwrap().unwrap();

    db.conn
        .execute_batch(
            "CREATE TRIGGER fail_managed_verification_audit
                 BEFORE INSERT ON gateway_logs
                 BEGIN
                     SELECT RAISE(ABORT, 'injected managed verification audit failure');
                 END;",
        )
        .expect("fault trigger should install");

    let error = db
        .commit_managed_key_verification(
            &managed.id,
            &ManagedKeyVerificationCas::from_account(&before),
            "candidate-cipher",
            &ManagedKeyVerificationWrite::Verified {
                rate_limit: None,
                account_name: managed.name.clone(),
            },
        )
        .expect_err("gateway audit insert should fail");
    assert!(
        error
            .to_string()
            .contains("injected managed verification audit failure"),
        "{error:#}"
    );

    let after = db.get_account(&managed.id).unwrap().unwrap();
    let after_verification = db.account_verification_state(&managed.id).unwrap().unwrap();
    assert_eq!(after.key_cipher, before.key_cipher);
    assert_eq!(after.enabled, before.enabled);
    assert_eq!(after.setup_step, before.setup_step);
    assert_eq!(after.updated_at, before.updated_at);
    assert_eq!(after_verification.status, before_verification.status);
    assert_eq!(
        after_verification.connection_verified_at,
        before_verification.connection_verified_at
    );
    assert!(
        db.list_gateway_logs(10)
            .unwrap()
            .iter()
            .all(|log| !log.message.contains("managed-audit-atomic")),
        "aborted audit insert must not persist a success log"
    );

    drop(db);
    fs::remove_dir_all(dir).expect("test data dir should be removed");
}

pub(super) fn forward_log_at(
    account_id: &str,
    status: &str,
    cost: f64,
    timestamp: DateTime<Utc>,
) -> ForwardLog {
    let mut log = forward_log(account_id, status, cost);
    log.timestamp = timestamp;
    log
}

pub(super) fn finalize_success(
    db: &Database,
    account_id: &str,
    cost: f64,
    timestamp: DateTime<Utc>,
) {
    let id = db
        .log_forward(&forward_log_at(account_id, "streaming", 0.0, timestamp))
        .expect("log should insert");
    db.update_forward_log(
        id,
        "success",
        None,
        ForwardMetrics {
            cost,
            cost_state: "priced",
            ..ForwardMetrics::default()
        },
        None,
        None,
    )
    .expect("stream should finalize");
}

pub(super) fn assert_cost(actual: f64, expected: f64) {
    assert!(
        (actual - expected).abs() < 1e-9,
        "expected {expected}, got {actual}"
    );
}

pub(super) fn create_v6_database(
    dir: &std::path::Path,
    extra_cooldown_columns: &str,
    extra_indexes: &str,
) -> Connection {
    let conn = Connection::open(dir.join("data.sqlite")).expect("v6 db should open");
    conn.execute_batch(&format!(
        "CREATE TABLE schema_version (version INTEGER PRIMARY KEY);
             INSERT INTO schema_version (version) VALUES (6);
             CREATE TABLE accounts (
                 id TEXT PRIMARY KEY,
                 name TEXT NOT NULL,
                 key_cipher TEXT NOT NULL,
                 enabled INTEGER NOT NULL DEFAULT 1,
                 referral_code TEXT,
                 recharge_date TEXT,
                 created_at TEXT NOT NULL,
                 updated_at TEXT NOT NULL,
                 cooldown_until TEXT,
                 last_error TEXT,
                 username TEXT,
                 password_cipher TEXT,
                 usage_5h_baseline_percent REAL,
                 usage_5h_anchor_success_cost REAL,
                 usage_week_baseline_percent REAL,
                 usage_week_anchor_success_cost REAL,
                 usage_month_baseline_percent REAL,
                 usage_month_anchor_success_cost REAL,
                 sort_order INTEGER NOT NULL DEFAULT 0
                 {extra_cooldown_columns}
             );
             CREATE TABLE forward_logs (
                 id INTEGER PRIMARY KEY AUTOINCREMENT,
                 timestamp TEXT NOT NULL,
                 model TEXT NOT NULL,
                 account_id TEXT NOT NULL,
                 account_name TEXT NOT NULL,
                 status TEXT NOT NULL,
                 http_status INTEGER,
                 prompt_tokens INTEGER NOT NULL DEFAULT 0,
                 completion_tokens INTEGER NOT NULL DEFAULT 0,
                 cached_tokens INTEGER NOT NULL DEFAULT 0,
                 cost REAL NOT NULL DEFAULT 0,
                 error_message TEXT
             );
             {extra_indexes}"
    ))
    .expect("v6 schema should be created");
    conn
}

#[test]
pub(super) fn account_reads_fallback_after_v8_data_is_corrupted() {
    let dir = temp_data_dir("post-v8-purchase-date-corruption");
    let conn = create_v6_database(
        &dir,
        ", cooldown_generic_until TEXT, cooldown_5h_until TEXT, cooldown_week_until TEXT, cooldown_month_until TEXT",
        "",
    );
    conn.execute("INSERT INTO schema_version (version) VALUES (7)", [])
        .expect("v7 schema version should be recorded");
    drop(conn);

    let db = Database::open(dir.clone()).expect("database should open");
    let created_at = DateTime::parse_from_rfc3339("2026-01-02T01:30:00+02:00")
        .expect("fixed timestamp should parse")
        .with_timezone(&Utc);
    for id in ["null", "invalid"] {
        let mut legacy = account(id);
        legacy.purchase_date = "2025-12-31".to_string();
        legacy.created_at = created_at;
        legacy.updated_at = created_at;
        db.create_account(&legacy)
            .expect("account should be created before corruption");
    }
    // v12 重建 accounts 表后 recharge_date 是 NOT NULL（恢复 v1 原始约束），
    // 无法再被 UPDATE 成 NULL；只测试 invalid-text 这一支。
    db.conn
        .execute(
            "UPDATE credentials SET purchase_date = 'not-a-date' WHERE legacy_account_id = 'invalid'",
            [],
        )
        .expect("purchase date should be corrupted to invalid text");

    let accounts = db
        .list_accounts()
        .expect("one corrupt row must not break the account list");
    assert_eq!(accounts.len(), 3);
    // 仅 invalid 被破坏；null 仍持有原始 2025-12-31。
    let invalid_account = accounts
        .iter()
        .find(|a| a.id == "invalid")
        .expect("invalid account should be present");
    assert_eq!(
        invalid_account.purchase_date, "2026-01-01",
        "list_accounts should fall back to default date for corrupted rows"
    );
    let invalid = db
        .get_account("invalid")
        .expect("corrupt account query should work")
        .expect("corrupt account should exist");
    assert_eq!(invalid.purchase_date, "2026-01-01");
    assert_eq!(invalid.expires_on, "2026-02-01");
    let remains_invalid: bool = db
        .conn
        .query_row(
            "SELECT purchase_date = 'not-a-date' FROM credentials WHERE legacy_account_id = 'invalid'",
            [],
            |row| row.get(0),
        )
        .expect("raw purchase date should remain queryable");
    assert!(
        remains_invalid,
        "read fallback must not hide a migration rerun"
    );

    drop(db);
    fs::remove_dir_all(dir).expect("test data dir should be removed");
}

#[test]
pub(super) fn account_creation_defaults_dates_and_appends_to_saved_order() {
    let dir = temp_data_dir("create-order");
    let db = Database::open(dir.clone()).expect("db should open");
    let purchase_date_column: i64 = db
        .conn
        .query_row(
            "SELECT COUNT(*)
                 FROM pragma_table_info('credentials')
                 WHERE name = 'purchase_date'",
            [],
            |row| row.get(0),
        )
        .expect("fresh credential schema should expose purchase date");
    assert_eq!(purchase_date_column, 1);
    let mut first = account("first");
    first.created_at = Utc::now() + Duration::days(1);
    db.create_account(&first)
        .expect("first account should save");
    let mut second = account("second");
    second.created_at = Utc::now() - Duration::days(1);
    second.purchase_date = "2024-01-31".to_string();
    db.create_account(&second)
        .expect("second account should save");

    let accounts = db.list_accounts().expect("accounts should load");
    assert_eq!(
        accounts
            .iter()
            .map(|account| account.id.as_str())
            .collect::<Vec<_>>(),
        [ZEN_FREE_ACCOUNT_ID, "first", "second"]
    );
    assert_eq!(accounts[1].purchase_date, local_today());
    assert_eq!(
        accounts[1].expires_on,
        purchase_expires_on(&accounts[1].purchase_date)
            .expect("default date should have an expiry")
    );
    assert_eq!(accounts[2].expires_on, "2024-02-29");

    let mut invalid = account("invalid");
    invalid.purchase_date = "2026-2-03".to_string();
    assert!(db.create_account(&invalid).is_err());
    assert!(
        db.get_account("invalid")
            .expect("invalid account lookup should work")
            .is_none()
    );

    drop(db);
    fs::remove_dir_all(dir).expect("test data dir should be removed");
}

#[test]
pub(super) fn reorder_accounts_validates_atomically_and_persists_dense_order() {
    let dir = temp_data_dir("reorder");
    let db = Database::open(dir.clone()).expect("db should open");
    for id in ["a", "b", "c"] {
        db.create_account(&account(id))
            .expect("account should be created");
    }

    db.reorder_accounts(&[
        "c".into(),
        "a".into(),
        "b".into(),
        ZEN_FREE_ACCOUNT_ID.into(),
    ])
    .expect("valid reorder should save");
    assert_eq!(account_ids(&db), ["c", "a", "b", ZEN_FREE_ACCOUNT_ID]);

    let duplicate = db
        .reorder_accounts(&[
            "c".into(),
            "c".into(),
            "b".into(),
            ZEN_FREE_ACCOUNT_ID.into(),
        ])
        .expect_err("duplicates should fail");
    assert!(matches!(
        duplicate,
        ReorderAccountsError::DuplicateAccountId
    ));
    assert_eq!(account_ids(&db), ["c", "a", "b", ZEN_FREE_ACCOUNT_ID]);

    for stale in [
        vec!["c".into(), "a".into()],
        vec!["c".into(), "a".into(), "missing".into()],
        Vec::<String>::new(),
    ] {
        let error = db
            .reorder_accounts(&stale)
            .expect_err("stale account set should fail");
        assert!(matches!(error, ReorderAccountsError::AccountSetMismatch));
        assert_eq!(account_ids(&db), ["c", "a", "b", ZEN_FREE_ACCOUNT_ID]);
    }

    let sort_orders = db
        .conn
        .prepare("SELECT routing_rank FROM credentials ORDER BY routing_rank")
        .expect("sort query should prepare")
        .query_map([], |row| row.get::<_, i64>(0))
        .expect("sort query should run")
        .collect::<rusqlite::Result<Vec<_>>>()
        .expect("sort orders should load");
    assert_eq!(sort_orders, [0, 1, 2, 3]);
    drop(db);

    let reopened = Database::open(dir.clone()).expect("db should reopen");
    assert_eq!(account_ids(&reopened), ["c", "a", "b", ZEN_FREE_ACCOUNT_ID]);
    drop(reopened);

    let empty_dir = temp_data_dir("reorder-empty");
    let empty = Database::open(empty_dir.clone()).expect("empty db should open");
    empty
        .reorder_accounts(&[ZEN_FREE_ACCOUNT_ID.into()])
        .expect("the built-in Zen row is the complete empty-user order");
    drop(empty);

    fs::remove_dir_all(dir).expect("test data dir should be removed");
    fs::remove_dir_all(empty_dir).expect("empty test data dir should be removed");
}

#[test]
pub(super) fn reorder_accounts_rolls_back_when_an_update_fails_mid_transaction() {
    let dir = temp_data_dir("reorder-write-failure");
    let db = Database::open(dir.clone()).expect("db should open");
    for id in ["a", "b", "c"] {
        db.create_account(&account(id))
            .expect("account should be created");
    }
    db.conn
        .execute_batch(
            "CREATE TRIGGER reject_b_sort_update
                 BEFORE UPDATE OF routing_rank ON credentials
                 WHEN NEW.legacy_account_id = 'b'
                 BEGIN
                     SELECT RAISE(ABORT, 'forced reorder failure');
                 END;",
        )
        .expect("failure trigger should be installed");

    let error = db
        .reorder_accounts(&[
            "c".into(),
            "a".into(),
            "b".into(),
            ZEN_FREE_ACCOUNT_ID.into(),
        ])
        .expect_err("the trigger should interrupt the reorder");
    assert!(matches!(error, ReorderAccountsError::Database(_)));
    assert_eq!(account_ids(&db), [ZEN_FREE_ACCOUNT_ID, "a", "b", "c"]);

    drop(db);
    fs::remove_dir_all(dir).expect("test data dir should be removed");
}

pub(super) fn account_ids(db: &Database) -> Vec<String> {
    db.list_accounts()
        .expect("accounts should load")
        .into_iter()
        .map(|account| account.id)
        .collect()
}

#[test]
pub(super) fn replacing_key_clears_auth_error_but_other_updates_preserve_it() {
    let dir = temp_data_dir("auth-error-key-replacement");
    let db = Database::open(dir.clone()).expect("db should open");
    db.create_account(&account("auth-failed"))
        .expect("account should be created");
    let old_key_cipher = db
        .get_account("auth-failed")
        .expect("account should load")
        .expect("account should exist")
        .key_cipher;
    db.set_account_auth_error("auth-failed", Some("upstream auth error 401"))
        .expect("auth error should save");

    let rename = AccountUpdate {
        name: Some("renamed".into()),
        username: None,
        password: None,
        key: None,
        enabled: None,
        referral_code: None,
        purchase_date: None,
        notes: None,
    };
    db.update_account("auth-failed", &rename, None, None)
        .expect("non-key update should save");
    assert!(
        db.get_account("auth-failed")
            .expect("account should load")
            .expect("account should exist")
            .auth_error
            .is_some()
    );

    let no_fields = AccountUpdate {
        name: None,
        username: None,
        password: None,
        key: None,
        enabled: None,
        referral_code: None,
        purchase_date: None,
        notes: None,
    };
    db.update_account("auth-failed", &no_fields, Some("replacement-cipher"), None)
        .expect("key replacement should save");
    assert!(
        db.get_account("auth-failed")
            .expect("account should load")
            .expect("account should exist")
            .auth_error
            .is_none()
    );

    assert!(
        !db.set_account_auth_error_if_key_matches(
            "auth-failed",
            &old_key_cipher,
            Some("late old-key 401"),
        )
        .expect("stale auth response should be ignored")
    );
    assert!(
        db.get_account("auth-failed")
            .expect("account should load")
            .expect("account should exist")
            .auth_error
            .is_none(),
        "a delayed 401 from the old key must not break its replacement"
    );

    assert!(
        db.set_account_auth_error_if_key_matches(
            "auth-failed",
            "replacement-cipher",
            Some("new-key auth error"),
        )
        .expect("current-key auth response should save")
    );
    assert!(
        !db.set_account_auth_error_if_key_matches("auth-failed", &old_key_cipher, None)
            .expect("stale success response should be ignored")
    );
    assert_eq!(
        db.get_account("auth-failed")
            .expect("account should load")
            .expect("account should exist")
            .auth_error
            .as_deref(),
        Some("new-key auth error"),
        "a delayed success from the old key must not recover its replacement"
    );
    assert!(
        db.set_account_auth_error_if_key_matches("auth-failed", "replacement-cipher", None)
            .expect("current-key success should clear auth state")
    );

    let stale_cooldown = Utc::now() + Duration::days(3);
    assert!(
        !db.set_account_rate_limit_if_key_matches(
            "auth-failed",
            &old_key_cipher,
            stale_cooldown,
            "late old-key 429",
            None,
        )
        .expect("stale rate limit should be ignored")
    );
    let stored = db
        .get_account("auth-failed")
        .expect("account should load")
        .expect("account should exist");
    assert!(stored.cooldown_until.is_none());
    assert!(stored.last_error.is_none());

    drop(db);
    fs::remove_dir_all(dir).expect("test data dir should be removed");
}

pub(super) fn snapshot_limits() -> PricingLimits {
    PricingLimits {
        window_5h: 12.0,
        window_week: 30.0,
        window_month: 100.0,
    }
}

pub(super) fn usage_calibration(
    rolling_percent: f64,
    weekly_percent: f64,
    monthly_percent: f64,
    rolling_resets_in_minutes: i64,
    weekly_resets_in_minutes: i64,
) -> AccountUsageCalibrationSnapshot {
    AccountUsageCalibrationSnapshot {
        rolling_percent,
        weekly_percent,
        monthly_percent,
        rolling_resets_in_minutes,
        weekly_resets_in_minutes,
    }
}

pub(super) fn usage_offset_row(
    db: &Database,
    id: &str,
) -> (Option<String>, f64, Option<String>, f64, f64) {
    db.conn
        .query_row(
            "SELECT usage_5h_window_started_at, usage_5h_window_cost_offset,
                        usage_week_window_started_at, usage_week_window_cost_offset,
                        usage_month_window_cost_offset
                 FROM credentials WHERE legacy_account_id = ?1",
            [id],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                ))
            },
        )
        .expect("usage offset row should load")
}

pub(super) fn attributed_log(account_id: &str, key_id: Option<&str>, cost: f64) -> ForwardLog {
    let mut log = forward_log(account_id, "success", cost);
    log.client_key_id = key_id.map(str::to_string);
    log.client_key_name = key_id.map(|id| format!("Key-{id}"));
    log
}

pub(super) fn empty_forward_query<'a>() -> ForwardLogQueryOptions<'a> {
    ForwardLogQueryOptions {
        limit: 50,
        offset: 0,
        status: None,
        account_id: None,
        provider_id: None,
        route_account_id: None,
        credential_account_id: None,
        model: None,
        request_id: None,
        start_time: None,
        end_time: None,
        sort_by: None,
        sort_order: None,
        key_id: None,
    }
}

pub(super) fn insert_identity_log(
    db: &Database,
    log: ForwardLog,
    requested: Option<&str>,
    alias: Option<&str>,
    upstream: Option<&str>,
) -> i64 {
    let id = db.log_forward(&log).unwrap();
    db.set_forward_log_native_attribution(
        id,
        &ForwardLogNativeAttribution {
            requested_model: requested.map(str::to_string),
            resolved_alias: alias.map(str::to_string),
            upstream_model: upstream.map(str::to_string),
            native_cost_value: None,
            native_cost_unit: None,
            native_cost_currency: None,
        },
    )
    .unwrap();
    id
}

pub(super) fn clear_v23_identity(db: &Database, id: i64) {
    db.conn
        .execute(
            "UPDATE forward_logs
                 SET requested_model = NULL, resolved_alias = NULL, upstream_model = NULL
                 WHERE id = ?1",
            [id],
        )
        .unwrap();
}

#[test]
pub(super) fn fresh_go_accounts_project_live_provider_quota_windows() {
    let dir = temp_data_dir("fresh-provider-quota");
    let db = Database::open(dir.clone()).unwrap();
    db.create_account(&account("fresh-go")).unwrap();
    assert!(db.list_quota_windows("fresh-go").unwrap().is_empty());

    let limits = SEED_LIMITS;
    db.calibrate_account_usage(
        "fresh-go",
        UsageWindowKind::FiveHours,
        50.0,
        Some(180),
        limits.window_5h,
    )
    .unwrap();
    let windows = db
        .live_opencode_go_quota_windows("fresh-go", &limits)
        .unwrap();
    assert_eq!(windows.len(), 3);
    let rolling = windows
        .iter()
        .find(|window| window.window_kind == QUOTA_WINDOW_FIVE_HOURS)
        .unwrap();
    assert!((rolling.used - limits.window_5h * 0.5).abs() < 1e-9);
    assert_eq!(rolling.limit_value, Some(limits.window_5h));
    assert_eq!(rolling.source, "opencode-go-live");

    drop(db);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
pub(super) fn sub_gateway_key_crud_and_unique_index_backstop() {
    let dir = temp_data_dir("sub-keys-crud");
    let db = Database::open(dir.clone()).unwrap();
    let now = Utc::now();
    let key = SubGatewayKey {
        id: "sub-1".into(),
        name: "Laptop".into(),
        key: "ocg-laptop".into(),
        enabled: true,
        deleted_at: None,
        created_at: now,
    };
    db.insert_sub_gateway_key(&key).unwrap();
    assert_eq!(db.count_active_sub_gateway_keys().unwrap(), 1);
    let primary_value = db.primary_access_key_value().unwrap().unwrap();
    let collide_primary = SubGatewayKey {
        id: "sub-primary-collide".into(),
        name: "Collide".into(),
        key: primary_value,
        enabled: true,
        deleted_at: None,
        created_at: now,
    };
    assert!(db.insert_sub_gateway_key(&collide_primary).is_err());
    assert_eq!(
        db.list_active_sub_gateway_keys().unwrap(),
        vec![key.clone()]
    );
    assert_eq!(db.list_sub_gateway_keys().unwrap().len(), 1);

    // Duplicate active values are rejected by the partial unique index.
    let duplicate = SubGatewayKey {
        id: "sub-2".into(),
        name: "Twin".into(),
        key: "ocg-laptop".into(),
        enabled: true,
        deleted_at: None,
        created_at: now,
    };
    assert!(db.insert_sub_gateway_key(&duplicate).is_err());

    // Disabled keys keep their plaintext, so they still block duplicates.
    assert!(db.set_sub_gateway_key_enabled("sub-1", false).unwrap());
    assert!(db.insert_sub_gateway_key(&duplicate).is_err());
    assert!(db.sub_gateway_key_value_exists("ocg-laptop").unwrap());
    assert_eq!(
        db.active_sub_gateway_key_values().unwrap(),
        vec!["ocg-laptop".to_string()]
    );

    // Renaming and regenerating address only non-deleted rows.
    assert!(db.rename_sub_gateway_key("sub-1", "Deck").unwrap());
    assert!(
        db.update_sub_gateway_key_value("sub-1", "ocg-deck")
            .unwrap()
    );
    assert!(db.sub_gateway_key_value_exists("ocg-deck").unwrap());

    // Soft delete clears the plaintext; tombstones free the value and do
    // not count as active.
    assert!(db.soft_delete_sub_gateway_key("sub-1", now).unwrap());
    let tombstone = db.get_sub_gateway_key("sub-1").unwrap().unwrap();
    assert!(tombstone.deleted_at.is_some());
    assert!(tombstone.key.is_empty());
    assert!(!tombstone.enabled);
    assert_eq!(db.count_active_sub_gateway_keys().unwrap(), 0);
    assert!(!db.sub_gateway_key_value_exists("ocg-deck").unwrap());
    assert!(!db.rename_sub_gateway_key("sub-1", "Gone").unwrap());
    assert!(!db.set_sub_gateway_key_enabled("sub-1", true).unwrap());
    assert!(!db.soft_delete_sub_gateway_key("sub-1", now).unwrap());

    // The freed value is insertable again, and missing ids report false.
    let recycled = SubGatewayKey {
        id: "sub-3".into(),
        name: "Recycled".into(),
        key: "ocg-deck".into(),
        enabled: true,
        deleted_at: None,
        created_at: now,
    };
    db.insert_sub_gateway_key(&recycled).unwrap();
    assert!(!db.rename_sub_gateway_key("missing", "X").unwrap());
    assert!(db.get_sub_gateway_key("missing").unwrap().is_none());

    drop(db);
    fs::remove_dir_all(dir).unwrap();
}

pub(super) fn create_v22_fixture(dir: &Path) {
    let db = Database::open(dir.to_path_buf()).expect("fixture database should open");
    let mut v22_account = account("v22-account");
    v22_account.key_cipher = fixture_account_key_cipher();
    db.create_account(&v22_account)
        .expect("representative account should save");
    let mut goat = account("v22-goat");
    goat.key_cipher = fixture_account_key_cipher();
    goat.provider_id = COMMAND_CODE_PROVIDER_ID.to_string();
    goat.enabled = false;
    db.create_account(&goat)
        .expect("representative GOAT account should save");
    db.log_forward(&forward_log("v22-account", "success", 3.5))
        .expect("representative forward log should save");
    drop(db);
    reverse_current_to_v34(dir);

    let conn = Connection::open(dir.join("data.sqlite")).expect("v23 fixture should reopen");
    conn.execute_batch(
        "PRAGMA foreign_keys=OFF;
             DROP TRIGGER IF EXISTS access_keys_protect_primary_delete;
             DROP TABLE IF EXISTS access_keys;
             CREATE TABLE IF NOT EXISTS sub_gateway_keys (
                id TEXT PRIMARY KEY,
                name TEXT NOT NULL,
                key TEXT NOT NULL,
                enabled INTEGER NOT NULL DEFAULT 1,
                deleted_at TEXT,
                created_at TEXT NOT NULL
             );
             CREATE UNIQUE INDEX IF NOT EXISTS idx_sub_gateway_keys_key
                ON sub_gateway_keys(key) WHERE deleted_at IS NULL AND key <> '';
             DROP INDEX IF EXISTS idx_account_model_capabilities_account;
             DROP TABLE IF EXISTS account_custom_configs;
             DROP TABLE IF EXISTS account_model_capabilities;
             ALTER TABLE accounts DROP COLUMN verification_error;
             ALTER TABLE accounts DROP COLUMN connection_verified_at;
             ALTER TABLE accounts DROP COLUMN verification_status;
             ALTER TABLE forward_logs DROP COLUMN native_cost_currency;
             ALTER TABLE forward_logs DROP COLUMN native_cost_unit;
             ALTER TABLE forward_logs DROP COLUMN native_cost_value;
             ALTER TABLE forward_logs DROP COLUMN upstream_model;
             ALTER TABLE forward_logs DROP COLUMN resolved_alias;
             ALTER TABLE forward_logs DROP COLUMN requested_model;
             DELETE FROM schema_version;
             INSERT INTO schema_version (version) VALUES (22);
             UPDATE accounts SET enabled = 1 WHERE id = 'v22-goat';
             PRAGMA foreign_keys=ON;",
    )
    .expect("v22 fixture should be created");
    restore_usage_sync_account_columns(&conn);
}

pub(super) fn probe_observation(
    scope: ContractScope,
    model_id: &str,
    protocol: UpstreamProtocolKind,
    now: DateTime<Utc>,
) -> PersistedModelProtocol {
    PersistedModelProtocol {
        scope,
        model_id: model_id.into(),
        protocol,
        source: ContractEvidenceSource::ProbeConfirmed,
        verified_at: Some(now),
        observed_at: Some(now),
        last_probe_result: Some(ProbeResultKind::Success),
        last_probe_at: Some(now),
        last_probe_error: None,
    }
}

#[test]
pub(super) fn create_account_linked_to_platform_is_atomic_on_link_failure() {
    use crate::platform::{PlatformGroup, PlatformKind};

    let dir = temp_data_dir("atomic-create-link");
    let db = Database::open(dir.clone()).unwrap();
    db.create_platform_account(
        "parent-atomic",
        PlatformKind::NewApi,
        "Atomic Parent",
        "https://platform.example/v1",
        Some("mgmt-cipher"),
    )
    .unwrap();
    db.conn
        .execute_batch(
            "CREATE TRIGGER fail_platform_link
                 BEFORE UPDATE ON credentials
                 WHEN NEW.group_json IS NOT NULL AND OLD.group_json IS NULL
                 BEGIN
                     SELECT RAISE(ABORT, 'forced link failure');
                 END;",
        )
        .unwrap();

    let mut custom = account("linked-atomic");
    custom.provider_id = CUSTOM_PROVIDER_ID.to_string();
    custom.enabled = false;
    custom.credential_kind = CredentialKind::ApiKey;
    custom.quota_scope = QuotaScope::Key;
    custom.key_cipher = "linked-atomic-cipher".into();
    let error = db
        .create_account_with_contract_linked_to_platform(
            &custom,
            &AccountCustomConfigInput {
                endpoint_url: "https://platform.example/v1/chat/completions".into(),
                upstream_protocol: UpstreamProtocolKind::ChatCompletions,
            },
            &[AccountModelCapabilityInput {
                public_model: "org/model".into(),
                upstream_model: "org/model".into(),
                protocol: UpstreamProtocolKind::ChatCompletions,
                source: Some("discovery".into()),
            }],
            "parent-atomic",
            &PlatformGroup::default(),
        )
        .expect_err("forced link failure should abort the create");
    assert!(error.to_string().contains("forced link failure"), "{error}");
    assert!(db.get_account("linked-atomic").unwrap().is_none());
    assert!(
        db.list_platform_links()
            .unwrap()
            .into_iter()
            .all(|link| link.account_id != "linked-atomic")
    );

    db.conn
        .execute_batch("DROP TRIGGER fail_platform_link;")
        .unwrap();
    db.create_account_with_contract_linked_to_platform(
        &custom,
        &AccountCustomConfigInput {
            endpoint_url: "https://platform.example/v1/chat/completions".into(),
            upstream_protocol: UpstreamProtocolKind::ChatCompletions,
        },
        &[AccountModelCapabilityInput {
            public_model: "org/model".into(),
            upstream_model: "org/model".into(),
            protocol: UpstreamProtocolKind::ChatCompletions,
            source: Some("discovery".into()),
        }],
        "parent-atomic",
        &PlatformGroup::default(),
    )
    .unwrap();
    assert!(db.get_account("linked-atomic").unwrap().is_some());
    assert!(
        db.list_platform_links()
            .unwrap()
            .into_iter()
            .any(|link| link.account_id == "linked-atomic"
                && link.platform_account_id == "parent-atomic")
    );
    drop(db);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
pub(super) fn account_migration_batch_is_atomic_and_preserves_order() {
    let dir = temp_data_dir("account-migration-batch");
    let db = Database::open(dir.clone()).unwrap();
    let go = account("migration-go");
    let mut custom = account("migration-custom");
    custom.provider_id = CUSTOM_PROVIDER_ID.to_string();
    custom.credential_kind = CredentialKind::ApiKey;
    custom.quota_scope = QuotaScope::Key;
    custom.enabled = false;
    let records = vec![
        AccountImportRecord {
            account: go,
            custom_config: None,
            capabilities: Vec::new(),
            verification_status: ConnectionVerificationStatus::NotRequired,
            connection_verified_at: None,
            ollama_billing_tier: None,
        },
        AccountImportRecord {
            account: custom,
            custom_config: Some(AccountCustomConfigInput {
                endpoint_url: "https://api.example.com/v1/chat/completions".into(),
                upstream_protocol: UpstreamProtocolKind::ChatCompletions,
            }),
            capabilities: vec![AccountModelCapabilityInput {
                public_model: "org/model".into(),
                upstream_model: "org/model".into(),
                protocol: UpstreamProtocolKind::ChatCompletions,
                source: Some("import".into()),
            }],
            verification_status: ConnectionVerificationStatus::Pending,
            connection_verified_at: None,
            ollama_billing_tier: None,
        },
    ];
    db.conn
        .execute_batch(
            "CREATE TRIGGER fail_migration_custom_config
                 BEFORE INSERT ON destinations
                 WHEN NEW.legacy_kind = 'custom_account'
                 BEGIN
                     SELECT RAISE(ABORT, 'forced migration failure');
                 END;",
        )
        .unwrap();
    assert!(db.import_accounts_with_contracts(&records).is_err());
    assert!(db.get_account("migration-go").unwrap().is_none());
    assert!(db.get_account("migration-custom").unwrap().is_none());

    db.conn
        .execute_batch("DROP TRIGGER fail_migration_custom_config;")
        .unwrap();
    db.import_accounts_with_contracts(&records).unwrap();
    let imported = db
        .list_accounts()
        .unwrap()
        .into_iter()
        .filter(|account| account.id.starts_with("migration-"))
        .map(|account| account.id)
        .collect::<Vec<_>>();
    assert_eq!(imported, ["migration-go", "migration-custom"]);
    assert!(
        db.account_custom_config("migration-custom")
            .unwrap()
            .is_some()
    );
    let imported_satellites: i64 = db
        .conn
        .query_row(
            "SELECT COUNT(*) FROM credentials
             WHERE legacy_account_id IN ('migration-go', 'migration-custom')
               AND binding_id IS NOT NULL",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(imported_satellites, 2);
    let imported_identities: i64 = db
        .conn
        .query_row(
            "SELECT COUNT(*) FROM credentials
             WHERE legacy_account_id IN ('migration-go', 'migration-custom') AND identity_id IS NOT NULL",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(imported_identities, 2);

    drop(db);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
pub(super) fn custom_capability_protocol_must_equal_the_config_protocol() {
    let dir = temp_data_dir("custom-protocol-mismatch");
    let db = Database::open(dir.clone()).unwrap();
    let mut custom = account("custom-protocol");
    custom.provider_id = CUSTOM_PROVIDER_ID.to_string();
    custom.enabled = false;
    let mismatch = db.create_account_with_contract(
        &custom,
        Some(&AccountCustomConfigInput {
            endpoint_url: "https://api.example.com/v1/messages".into(),
            upstream_protocol: UpstreamProtocolKind::Messages,
        }),
        &[AccountModelCapabilityInput {
            public_model: "org/model".into(),
            upstream_model: "org/model".into(),
            protocol: UpstreamProtocolKind::ChatCompletions,
            source: None,
        }],
    );
    assert!(
        mismatch
            .unwrap_err()
            .to_string()
            .contains("must equal account custom_config.upstream_protocol")
    );
    assert!(db.get_account("custom-protocol").unwrap().is_none());

    db.create_account_with_contract(
        &custom,
        Some(&AccountCustomConfigInput {
            endpoint_url: "https://api.example.com/v1/messages".into(),
            upstream_protocol: UpstreamProtocolKind::Messages,
        }),
        &[AccountModelCapabilityInput {
            public_model: "org/model".into(),
            upstream_model: "org/model".into(),
            protocol: UpstreamProtocolKind::Messages,
            source: None,
        }],
    )
    .unwrap();
    let stored = db
        .list_account_model_capabilities("custom-protocol")
        .unwrap();
    assert_eq!(stored.len(), 1);
    assert_eq!(stored[0].protocol, UpstreamProtocolKind::Messages);

    let rejected = db.replace_account_model_capabilities(
        "custom-protocol",
        &[AccountModelCapabilityInput {
            public_model: "org/other".into(),
            upstream_model: "org/other".into(),
            protocol: UpstreamProtocolKind::Responses,
            source: None,
        }],
    );
    assert!(
        rejected
            .unwrap_err()
            .to_string()
            .contains("must equal account custom_config.upstream_protocol")
    );
    let kept = db
        .list_account_model_capabilities("custom-protocol")
        .unwrap();
    assert_eq!(kept.len(), 1);
    assert!(kept.iter().all(|row| row.public_model == "org/model"));

    drop(db);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
pub(super) fn custom_capabilities_allow_shared_upstream_but_reject_duplicate_public_names() {
    let dir = temp_data_dir("custom-model-mapping-uniqueness");
    let db = Database::open(dir.clone()).unwrap();
    let mut custom = account("custom-mapping");
    custom.provider_id = CUSTOM_PROVIDER_ID.to_string();
    custom.enabled = false;
    db.create_account_with_contract(
        &custom,
        Some(&AccountCustomConfigInput {
            endpoint_url: "https://api.example.com/v1/chat/completions".into(),
            upstream_protocol: UpstreamProtocolKind::ChatCompletions,
        }),
        &[
            AccountModelCapabilityInput {
                public_model: "public-one".into(),
                upstream_model: "shared-upstream:0731".into(),
                protocol: UpstreamProtocolKind::ChatCompletions,
                source: None,
            },
            AccountModelCapabilityInput {
                public_model: "public-two".into(),
                upstream_model: "shared-upstream:0731".into(),
                protocol: UpstreamProtocolKind::ChatCompletions,
                source: None,
            },
        ],
    )
    .unwrap();
    let saved = db
        .list_account_model_capabilities("custom-mapping")
        .unwrap();
    assert_eq!(saved.len(), 2);
    assert!(
        saved
            .iter()
            .all(|row| row.upstream_model == "shared-upstream:0731")
    );

    let duplicate = db.replace_account_model_capabilities(
        "custom-mapping",
        &[
            AccountModelCapabilityInput {
                public_model: "Public-One".into(),
                upstream_model: "upstream-a".into(),
                protocol: UpstreamProtocolKind::ChatCompletions,
                source: None,
            },
            AccountModelCapabilityInput {
                public_model: "public-one".into(),
                upstream_model: "upstream-b".into(),
                protocol: UpstreamProtocolKind::ChatCompletions,
                source: None,
            },
        ],
    );
    assert!(
        duplicate
            .unwrap_err()
            .to_string()
            .contains("duplicate model capability")
    );
    assert_eq!(
        db.list_account_model_capabilities("custom-mapping")
            .unwrap(),
        saved
    );

    drop(db);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
pub(super) fn custom_mutations_repend_but_keep_verified_accounts_enabled() {
    let dir = temp_data_dir("custom-lifecycle-stale");
    let db = Database::open(dir.clone()).unwrap();
    let mut custom = account("custom-stale");
    custom.provider_id = CUSTOM_PROVIDER_ID.to_string();
    custom.enabled = false;
    db.create_account_with_contract(
        &custom,
        Some(&AccountCustomConfigInput {
            endpoint_url: "https://api.example.com/v1/chat/completions".into(),
            upstream_protocol: UpstreamProtocolKind::ChatCompletions,
        }),
        &[AccountModelCapabilityInput {
            public_model: "org/model".into(),
            upstream_model: "org/model".into(),
            protocol: UpstreamProtocolKind::ChatCompletions,
            source: None,
        }],
    )
    .unwrap();
    db.set_account_verification(
        "custom-stale",
        ConnectionVerificationStatus::Verified,
        Some(Utc::now()),
        Some("previous"),
    )
    .unwrap();
    db.conn
        .execute(
            "UPDATE credentials SET enabled = 1 WHERE legacy_account_id = 'custom-stale'",
            [],
        )
        .unwrap();

    db.upsert_account_custom_config(
        "custom-stale",
        &AccountCustomConfigInput {
            endpoint_url: "https://api.example.net/v2/chat/completions".into(),
            upstream_protocol: UpstreamProtocolKind::ChatCompletions,
        },
    )
    .unwrap();
    let after_url = db.get_account("custom-stale").unwrap().unwrap();
    let after_url_state = db
        .account_verification_state("custom-stale")
        .unwrap()
        .unwrap();
    assert!(after_url.enabled);
    assert_eq!(
        after_url_state.status,
        ConnectionVerificationStatus::Pending
    );
    assert!(after_url_state.connection_verified_at.is_none());
    assert!(after_url_state.verification_error.is_none());

    db.set_account_verification(
        "custom-stale",
        ConnectionVerificationStatus::Verified,
        Some(Utc::now()),
        None,
    )
    .unwrap();
    db.conn
        .execute(
            "UPDATE credentials SET enabled = 1 WHERE legacy_account_id = 'custom-stale'",
            [],
        )
        .unwrap();
    db.replace_account_model_capabilities(
        "custom-stale",
        &[AccountModelCapabilityInput {
            public_model: "org/other".into(),
            upstream_model: "org/other".into(),
            protocol: UpstreamProtocolKind::ChatCompletions,
            source: None,
        }],
    )
    .unwrap();
    let after_caps = db.get_account("custom-stale").unwrap().unwrap();
    let after_caps_state = db
        .account_verification_state("custom-stale")
        .unwrap()
        .unwrap();
    assert!(after_caps.enabled);
    assert_eq!(
        after_caps_state.status,
        ConnectionVerificationStatus::Pending
    );
    assert!(after_caps_state.connection_verified_at.is_none());

    db.set_account_verification(
        "custom-stale",
        ConnectionVerificationStatus::Verified,
        Some(Utc::now()),
        Some("stale"),
    )
    .unwrap();
    db.conn
        .execute(
            "UPDATE credentials SET enabled = 1 WHERE legacy_account_id = 'custom-stale'",
            [],
        )
        .unwrap();
    db.update_account(
        "custom-stale",
        &AccountUpdate {
            key: Some("rotated".into()),
            enabled: Some(true),
            ..AccountUpdate::default()
        },
        Some("new-cipher"),
        None,
    )
    .unwrap();
    let after_key = db.get_account("custom-stale").unwrap().unwrap();
    let after_key_state = db
        .account_verification_state("custom-stale")
        .unwrap()
        .unwrap();
    assert!(after_key.enabled);
    assert_eq!(
        after_key_state.status,
        ConnectionVerificationStatus::Pending
    );
    assert!(after_key_state.connection_verified_at.is_none());
    assert!(after_key_state.verification_error.is_none());
    let caps_after_key = db.list_account_model_capabilities("custom-stale").unwrap();
    assert_eq!(caps_after_key.len(), 1);
    assert_eq!(caps_after_key[0].public_model, "org/other");

    drop(db);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
pub(super) fn custom_verification_cas_rejects_stale_key_config_caps_and_delete() {
    let dir = temp_data_dir("custom-verify-cas");
    let mut db = Database::open(dir.clone()).unwrap();
    let mut custom = account("custom-cas");
    custom.provider_id = CUSTOM_PROVIDER_ID.to_string();
    custom.enabled = false;
    custom.key_cipher = "cipher-a".into();
    db.create_account_with_contract(
        &custom,
        Some(&AccountCustomConfigInput {
            endpoint_url: "https://api.example.com/v1/chat/completions".into(),
            upstream_protocol: UpstreamProtocolKind::ChatCompletions,
        }),
        &[AccountModelCapabilityInput {
            public_model: "one".into(),
            upstream_model: "one".into(),
            protocol: UpstreamProtocolKind::ChatCompletions,
            source: None,
        }],
    )
    .unwrap();

    let contract = db
        .capture_custom_verification_contract("custom-cas")
        .unwrap()
        .unwrap();
    assert_eq!(contract.key_cipher, "cipher-a");
    assert_eq!(contract.capabilities[0].0, "one");

    db.update_account(
        "custom-cas",
        &AccountUpdate {
            key: Some("rotated".into()),
            ..AccountUpdate::default()
        },
        Some("cipher-b"),
        None,
    )
    .unwrap();
    assert!(
        !db.commit_custom_verification_if_contract_matches(
            &contract,
            ConnectionVerificationStatus::Verified,
            Some(Utc::now()),
            None,
        )
        .unwrap()
    );
    assert_eq!(
        db.account_verification_state("custom-cas")
            .unwrap()
            .unwrap()
            .status,
        ConnectionVerificationStatus::Pending
    );

    let after_key = db
        .capture_custom_verification_contract("custom-cas")
        .unwrap()
        .unwrap();
    db.upsert_account_custom_config(
        "custom-cas",
        &AccountCustomConfigInput {
            endpoint_url: "https://api.example.net/v2/chat/completions".into(),
            upstream_protocol: UpstreamProtocolKind::ChatCompletions,
        },
    )
    .unwrap();
    assert!(
        !db.commit_custom_verification_if_contract_matches(
            &after_key,
            ConnectionVerificationStatus::Verified,
            Some(Utc::now()),
            None,
        )
        .unwrap()
    );

    let after_config = db
        .capture_custom_verification_contract("custom-cas")
        .unwrap()
        .unwrap();
    db.replace_account_model_capabilities(
        "custom-cas",
        &[AccountModelCapabilityInput {
            public_model: "two".into(),
            upstream_model: "two".into(),
            protocol: UpstreamProtocolKind::ChatCompletions,
            source: None,
        }],
    )
    .unwrap();
    assert!(
        !db.commit_custom_verification_if_contract_matches(
            &after_config,
            ConnectionVerificationStatus::Verified,
            Some(Utc::now()),
            None,
        )
        .unwrap()
    );

    let matching = db
        .capture_custom_verification_contract("custom-cas")
        .unwrap()
        .unwrap();
    assert!(
        db.commit_custom_verification_if_contract_matches(
            &matching,
            ConnectionVerificationStatus::Verified,
            Some(Utc::now()),
            None,
        )
        .unwrap()
    );
    assert_eq!(
        db.account_verification_state("custom-cas")
            .unwrap()
            .unwrap()
            .status,
        ConnectionVerificationStatus::Verified
    );
    assert!(
        !db.commit_custom_verification_if_contract_matches(
            &matching,
            ConnectionVerificationStatus::Failed,
            None,
            Some("stale"),
        )
        .unwrap()
    );
    assert_eq!(
        db.account_verification_state("custom-cas")
            .unwrap()
            .unwrap()
            .status,
        ConnectionVerificationStatus::Verified
    );

    let mut leftover = account("custom-delete");
    leftover.provider_id = CUSTOM_PROVIDER_ID.to_string();
    leftover.enabled = false;
    db.create_account_with_contract(
        &leftover,
        Some(&AccountCustomConfigInput {
            endpoint_url: "https://api.example.com/v1/chat/completions".into(),
            upstream_protocol: UpstreamProtocolKind::ChatCompletions,
        }),
        &[AccountModelCapabilityInput {
            public_model: "one".into(),
            upstream_model: "one".into(),
            protocol: UpstreamProtocolKind::ChatCompletions,
            source: None,
        }],
    )
    .unwrap();
    let deleted_contract = db
        .capture_custom_verification_contract("custom-delete")
        .unwrap()
        .unwrap();
    db.delete_account("custom-delete").unwrap();
    assert!(
        !db.commit_custom_verification_if_contract_matches(
            &deleted_contract,
            ConnectionVerificationStatus::Verified,
            Some(Utc::now()),
            None,
        )
        .unwrap()
    );

    drop(db);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
pub(super) fn reopen_repairs_legacy_goat_verification_without_changing_other_accounts() {
    let dir = temp_data_dir("unroutable-sanitation");
    let db = Database::open(dir.clone()).unwrap();

    let mut go = account("go-keep");
    go.notes = Some("go-notes".into());
    db.create_account(&go).unwrap();
    leftover_enable(&db, "go-keep");

    let mut unknown = account("unknown-keep");
    unknown.notes = Some("unknown-notes".into());
    db.create_account(&unknown).unwrap();
    leftover_enable(&db, "unknown-keep");
    db.conn
        .execute(
            "UPDATE credentials
                 SET provider_id = 'unknown-provider'
                 WHERE legacy_account_id = 'unknown-keep'",
            [],
        )
        .unwrap();

    persist_sanitation_account(
        &db,
        builtin_provider(COMMAND_CODE_PROVIDER_ID).unwrap(),
        "goat-pending",
        "goat-pending-notes",
    );
    leftover_enable(&db, "goat-pending");

    persist_sanitation_account(
        &db,
        builtin_provider(COMMAND_CODE_PROVIDER_ID).unwrap(),
        "goat-verified",
        "goat-verified-notes",
    );
    db.conn
        .execute(
            "UPDATE credentials
                 SET enabled = 1, verification_status = 'verified', verification_error = NULL
                 WHERE legacy_account_id = 'goat-verified'",
            [],
        )
        .unwrap();

    persist_sanitation_account(
        &db,
        builtin_provider(COMMAND_CODE_PROVIDER_ID).unwrap(),
        "goat-failed",
        "goat-failed-notes",
    );
    db.conn
        .execute(
            "UPDATE credentials
                 SET enabled = 1, verification_status = 'failed', verification_error = 'boom'
                 WHERE legacy_account_id = 'goat-failed'",
            [],
        )
        .unwrap();

    persist_sanitation_account(
        &db,
        builtin_provider(CUSTOM_PROVIDER_ID).unwrap(),
        "draft-api",
        "draft-api-notes",
    );
    leftover_enable(&db, "draft-api");

    // An enabled Ollama Cloud row is now legitimate (routable offering),
    // so open must leave it untouched.
    persist_sanitation_account(
        &db,
        builtin_provider(OLLAMA_PROVIDER_ID).unwrap(),
        "ollama-leftover",
        "ollama-leftover-notes",
    );
    leftover_enable(&db, "ollama-leftover");

    let zen_before = sanitation_snapshot(&db, ZEN_FREE_ACCOUNT_ID);
    let go_before = sanitation_snapshot(&db, "go-keep");
    let unknown_before = sanitation_snapshot(&db, "unknown-keep");
    let goat_pending_before = sanitation_snapshot(&db, "goat-pending");
    let goat_verified_before = sanitation_snapshot(&db, "goat-verified");
    let goat_failed_before = sanitation_snapshot(&db, "goat-failed");
    let custom_before = sanitation_snapshot(&db, "draft-api");
    let ollama_before = sanitation_snapshot(&db, "ollama-leftover");
    assert!(go_before.enabled);
    assert!(custom_before.enabled);
    assert!(ollama_before.enabled);
    assert!(unknown_before.enabled);
    assert!(goat_pending_before.enabled);
    assert!(goat_verified_before.enabled);
    assert!(goat_failed_before.enabled);
    assert_eq!(
        goat_pending_before.verification,
        ConnectionVerificationStatus::NotRequired
    );
    assert_eq!(
        goat_verified_before.verification,
        ConnectionVerificationStatus::Verified
    );
    assert_eq!(
        goat_failed_before.verification,
        ConnectionVerificationStatus::Failed
    );

    drop(db);
    let db = Database::open(dir.clone()).unwrap();

    let zen_after = sanitation_snapshot(&db, ZEN_FREE_ACCOUNT_ID);
    let go_after = sanitation_snapshot(&db, "go-keep");
    let unknown_after = sanitation_snapshot(&db, "unknown-keep");
    assert_eq!(zen_after, zen_before);
    assert_eq!(go_after, go_before);
    assert_eq!(unknown_after, unknown_before);

    let goat_pending_after = sanitation_snapshot(&db, "goat-pending");
    assert_eq!(goat_pending_after, goat_pending_before);

    let goat_verified_after = sanitation_snapshot(&db, "goat-verified");
    assert_eq!(goat_verified_after.name, goat_verified_before.name);
    assert_eq!(goat_verified_after.notes, goat_verified_before.notes);
    assert_eq!(
        goat_verified_after.updated_at,
        goat_verified_before.updated_at
    );
    assert!(goat_verified_after.enabled);
    assert_eq!(
        goat_verified_after.verification,
        ConnectionVerificationStatus::NotRequired
    );

    let goat_failed_after = sanitation_snapshot(&db, "goat-failed");
    assert_eq!(goat_failed_after.name, goat_failed_before.name);
    assert_eq!(goat_failed_after.notes, goat_failed_before.notes);
    assert_eq!(goat_failed_after.updated_at, goat_failed_before.updated_at);
    assert!(goat_failed_after.enabled);
    assert_eq!(
        goat_failed_after.verification,
        ConnectionVerificationStatus::NotRequired
    );
    assert!(goat_failed_after.verification_error.is_none());

    let custom_after = sanitation_snapshot(&db, "draft-api");
    assert_eq!(custom_after, custom_before);

    let ollama_after = sanitation_snapshot(&db, "ollama-leftover");
    assert_eq!(ollama_after, ollama_before);

    let first_pass: Vec<_> = [
        ZEN_FREE_ACCOUNT_ID,
        "go-keep",
        "unknown-keep",
        "goat-pending",
        "goat-verified",
        "goat-failed",
        "draft-api",
        "ollama-leftover",
    ]
    .into_iter()
    .map(|id| (id.to_string(), sanitation_snapshot(&db, id)))
    .collect();

    drop(db);
    let db = Database::open(dir.clone()).unwrap();
    for (id, expected) in &first_pass {
        assert_eq!(
            sanitation_snapshot(&db, id),
            *expected,
            "second open must be idempotent for {id}"
        );
    }

    drop(db);
    fs::remove_dir_all(dir).unwrap();
}

pub(super) struct V27HookGuard;
impl Drop for V27HookGuard {
    fn drop(&mut self) {
        v27_test_hooks::reset();
    }
}

pub(super) fn arm_v27_fault(point: V27MigrationFault) -> V27HookGuard {
    v27_test_hooks::reset();
    v27_test_hooks::set_fault(Some(point));
    V27HookGuard
}

pub(super) fn populate_v26_source(dir: &Path) -> (String, String) {
    let db = Database::open(dir.to_path_buf()).expect("fixture database should open");
    let mut v26_account = account("v26-account");
    v26_account.key_cipher = fixture_account_key_cipher();
    db.create_account(&v26_account)
        .expect("representative account should save");
    let now = Utc::now();
    db.insert_sub_gateway_key(&SubGatewayKey {
        id: "sub-v26".into(),
        name: "Laptop".into(),
        key: "ocg-v26-laptop".into(),
        enabled: true,
        deleted_at: None,
        created_at: now,
    })
    .expect("sub key should save");
    let config = serde_json::json!({
        "gateway_port": 9042,
        "gateway_key": "ocg-v26-primary",
        "upstream_base_url": "https://opencode.ai/zen/go" });
    db.set_config(&config.to_string())
        .expect("v26 config should persist");
    drop(db);
    reverse_current_to_v26(dir);
    ("ocg-v26-primary".into(), "ocg-v26-laptop".into())
}

pub(super) fn effective_from_db(db: &Database) -> crate::provider_contracts::EffectiveContractSet {
    crate::provider_contracts::build_effective_contracts(
        &db.zen_free_model_catalog().unwrap().unwrap_or_default(),
        &[],
        db.load_persisted_contracts().unwrap(),
    )
}

pub(super) fn v35_column_names(conn: &Connection, table: &str) -> Vec<String> {
    let mut stmt = conn
        .prepare(&format!("PRAGMA table_info({table})"))
        .unwrap();
    stmt.query_map([], |row| row.get::<_, String>(1))
        .unwrap()
        .map(|column| column.unwrap())
        .collect()
}

pub(super) fn v35_index_sql(conn: &Connection, name: &str) -> Option<String> {
    conn.query_row(
        "SELECT sql FROM sqlite_master WHERE type = 'index' AND name = ?1",
        [name],
        |row| row.get::<_, Option<String>>(0),
    )
    .optional()
    .unwrap()
    .flatten()
}

pub(super) fn onboarding_runtime(
    provider_id: &str,
    name: &str,
) -> crate::dynamic::DynamicProviderRuntime {
    let now = Utc::now();
    crate::dynamic::DynamicProviderRuntime {
        preset_id: None,
        id: provider_id.to_string(),
        name: name.into(),
        endpoint_url: "http://127.0.0.1:9".into(),
        upstream_protocol: crate::provider::UpstreamProtocolKind::ChatCompletions,
        auth_kind: ocg_domain::dynamic::DynamicAuthKind::Bearer,
        mappings: vec![ocg_domain::dynamic::DynamicModelMapping {
            public_model: "lab-opus".into(),
            upstream_model: "vendor/opus".into(),
            upstream_override: None,
        }],
        created_at: now,
        updated_at: now,
        origin: ocg_domain::provider::ProviderOrigin::Custom,
        offering: "api".to_string(),
    }
}

pub(super) fn onboarding_operation(
    operation_id: &str,
    digest: &str,
    result_json: &str,
) -> NewDashboardOperation {
    NewDashboardOperation {
        operation_id: operation_id.to_string(),
        kind: "onboarding_commit".into(),
        payload_digest: digest.to_string(),
        result_json: result_json.to_string(),
    }
}

#[test]
pub(super) fn commit_transaction_fault_after_provider_insert_leaves_no_partial_rows() {
    let dir = temp_data_dir("onboard-provider-fault");
    let db = open_with_host_cipher(dir.clone()).unwrap();
    let provider_id = uuid::Uuid::new_v4().to_string();
    let runtime = onboarding_runtime(&provider_id, "FaultyOnboard");
    let mut first = account("onboard-fault");
    first.provider_id = provider_id.clone();
    first.key_cipher = fixture_account_key_cipher();
    let operation = onboarding_operation(
        &uuid::Uuid::new_v4().to_string(),
        "digest-not-a-secret",
        r#"{"connectionId":"c","credentialId":"a","targetIds":[]}"#,
    );
    crate::db::dynamic_provider_fault::install("after_provider_insert");
    let error = db
        .commit_onboarding_new(&runtime, Some(&first), false, &operation)
        .unwrap_err();
    crate::db::dynamic_provider_fault::clear();
    assert!(
        error
            .to_string()
            .contains("injected dynamic provider fault")
    );
    assert!(db.get_dynamic_provider(&provider_id).unwrap().is_none());
    assert_eq!(db.count_accounts_for_provider(&provider_id).unwrap(), 0);
    assert!(
        db.find_dashboard_operation(&operation.operation_id)
            .unwrap()
            .is_none()
    );
    drop(db);
    fs::remove_dir_all(dir).unwrap();
}
