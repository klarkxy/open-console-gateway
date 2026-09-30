//! Behavioral tests split from the root db test module.

use super::tests::*;
use super::*;
use ocg_domain::dynamic::DynamicAuthKind;
use std::fs;

#[test]
pub(super) fn dynamic_provider_crud_and_onboarding_survive_reopen_without_leftover_tables() {
    let dir = temp_data_dir("v56-crud-reopen");
    let db = open_with_host_cipher(dir.clone()).unwrap();
    let provider_id = uuid::Uuid::new_v4().to_string();
    let draft_id = uuid::Uuid::new_v4().to_string();
    let runtime = onboarding_runtime(&provider_id, "Persist Lab");
    let mut first = account("persist-lab");
    first.provider_id = provider_id.clone();
    first.key_cipher = fixture_account_key_cipher();
    db.create_dynamic_provider(&runtime, &first).unwrap();
    let mut changed = runtime.clone();
    changed.name = "Persist Lab Updated".into();
    changed.endpoint_url = "https://persist.example/v1".into();
    db.replace_dynamic_provider(&changed, false, false, None)
        .unwrap();
    db.commit_onboarding_new(
        &onboarding_runtime(&draft_id, "Draft Lab"),
        None,
        true,
        &onboarding_operation(
            &uuid::Uuid::new_v4().to_string(),
            "draft-reopen",
            r#"{"connectionId":"c","credentialId":null,"targetIds":[]}"#,
        ),
    )
    .unwrap();
    db.commit_onboarding_resume(
        &onboarding_runtime(&draft_id, "Draft Lab Done"),
        false,
        None,
        None,
        None,
        None,
        None,
        &onboarding_operation(
            &uuid::Uuid::new_v4().to_string(),
            "draft-complete",
            r#"{"connectionId":"c","credentialId":null,"targetIds":[]}"#,
        ),
    )
    .unwrap();
    drop(db);

    let db = open_with_host_cipher(dir.clone()).unwrap();
    assert_leftover_dynamic_provider_storage_absent(&db.conn);
    let loaded = db
        .get_dynamic_provider(&provider_id)
        .unwrap()
        .expect("reopened");
    assert_eq!(loaded.name, "Persist Lab Updated");
    assert_eq!(loaded.endpoint_url, "https://persist.example/v1");
    assert_eq!(db.count_accounts_for_provider(&provider_id).unwrap(), 1);
    let completed = db.get_dynamic_provider(&draft_id).unwrap().expect("draft");
    assert_eq!(completed.name, "Draft Lab Done");
    assert_eq!(
        db.provider_is_onboarding_draft(&draft_id).unwrap(),
        Some(false)
    );
    db.delete_dynamic_provider(&draft_id).unwrap();
    drop(db);

    let db = open_with_host_cipher(dir.clone()).unwrap();
    assert!(db.get_dynamic_provider(&draft_id).unwrap().is_none());
    assert!(db.get_dynamic_provider(&provider_id).unwrap().is_some());
    drop(db);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
pub(super) fn list_dynamic_providers_excludes_drafts_and_control_plane_includes_them() {
    let dir = temp_data_dir("draft-list-boundary");
    let db = open_with_host_cipher(dir.clone()).unwrap();
    let provider_id = uuid::Uuid::new_v4().to_string();
    let runtime = onboarding_runtime(&provider_id, "DraftBoundary");
    db.commit_onboarding_new(
        &runtime,
        None,
        true,
        &onboarding_operation(
            &uuid::Uuid::new_v4().to_string(),
            "draft-digest",
            r#"{"connectionId":"c","credentialId":null,"targetIds":[]}"#,
        ),
    )
    .unwrap();
    assert!(
        db.list_dynamic_providers()
            .unwrap()
            .iter()
            .all(|row| row.id != provider_id)
    );
    assert!(
        db.list_control_plane_dynamic_providers()
            .unwrap()
            .iter()
            .any(|row| row.id == provider_id)
    );
    assert_eq!(
        db.provider_is_onboarding_draft(&provider_id).unwrap(),
        Some(true)
    );
    assert!(
        db.onboarding_draft_provider_ids()
            .unwrap()
            .contains(&provider_id)
    );
    let loaded = db.get_dynamic_provider(&provider_id).unwrap().unwrap();
    assert_eq!(loaded.id, provider_id);
    db.replace_dynamic_provider(&runtime, false, false, None)
        .unwrap();
    assert_eq!(
        db.provider_is_onboarding_draft(&provider_id).unwrap(),
        Some(true),
        "ordinary replace must preserve the draft flag"
    );
    drop(db);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
pub(super) fn dynamic_provider_round_trip_and_duplicate_public_model_rejection() {
    let dir = temp_data_dir("v35-dynamic-providers");
    let db = open_with_host_cipher(dir.clone()).unwrap();
    assert_leftover_dynamic_provider_storage_absent(&db.conn);
    let dest_columns = v35_column_names(&db.conn, "destinations");
    for required in [
        "id",
        "name",
        "base_url",
        "legacy_kind",
        "legacy_id",
        "origin",
    ] {
        assert!(
            dest_columns.iter().any(|name| name == required),
            "{required}"
        );
    }
    let model_columns = v35_column_names(&db.conn, "destination_models");
    assert!(model_columns.iter().any(|name| name == "public_model_key"));
    assert!(model_columns.iter().any(|name| name == "upstream_override"));

    let now = Utc::now();
    let provider_id = uuid::Uuid::new_v4().to_string();
    let runtime = crate::dynamic::DynamicProviderRuntime {
        preset_id: None,
        id: provider_id.clone(),
        name: "Lab".into(),
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
    };
    let mut first = account("dyn-acct");
    first.provider_id = provider_id.clone();
    first.key_cipher = fixture_account_key_cipher();
    db.create_dynamic_provider(&runtime, &first).unwrap();
    let loaded = db.get_dynamic_provider(&provider_id).unwrap().unwrap();
    assert_eq!(loaded.name, "Lab");
    assert_eq!(loaded.mappings[0].public_model, "lab-opus");
    assert_eq!(db.count_accounts_for_provider(&provider_id).unwrap(), 1);

    let mut changed = loaded.clone();
    changed.mappings[0].upstream_override =
        Some(ocg_domain::dynamic::DynamicModelUpstreamOverride {
            protocol: crate::provider::UpstreamProtocolKind::Messages,
            endpoint_url: "https://example.test/anthropic/v1/messages".into(),
        });
    db.replace_dynamic_provider(&changed, true, false, None)
        .unwrap();
    let saved = db.get_dynamic_provider(&provider_id).unwrap().unwrap();
    assert_eq!(saved.mappings, changed.mappings);
    assert_eq!(saved.upstream_protocol, loaded.upstream_protocol);
    assert_eq!(
        db.get_account(&first.id).unwrap().unwrap().key_cipher,
        first.key_cipher
    );
    db.replace_dynamic_provider(&loaded, true, false, None)
        .unwrap();
    assert!(
        db.get_dynamic_provider(&provider_id)
            .unwrap()
            .unwrap()
            .mappings[0]
            .upstream_override
            .is_none()
    );

    let dest_id = ocg_domain::destination::destination_id_for_dynamic(&provider_id);
    let duplicate = db.conn.execute(
        "INSERT INTO destination_models
         (destination_id, public_model, public_model_key, upstream_model,
          protocols_json, preferred, enabled)
         VALUES (?1, 'LAB-OPUS', 'lab-opus', 'other', '[]', NULL, 1)",
        [&dest_id],
    );
    assert!(duplicate.is_err());
    drop(db);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
pub(super) fn replace_dynamic_provider_keeps_persisted_credential_projection_in_sync() {
    let dir = temp_data_dir("dynamic-provider-projection-sync");
    let db = open_with_host_cipher(dir.clone()).unwrap();
    let now = Utc::now();
    let provider_id = uuid::Uuid::new_v4().to_string();
    let runtime = crate::dynamic::DynamicProviderRuntime {
        preset_id: None,
        id: provider_id.clone(),
        name: "Projection sync".into(),
        endpoint_url: "http://127.0.0.1:9".into(),
        upstream_protocol: crate::provider::UpstreamProtocolKind::ChatCompletions,
        auth_kind: ocg_domain::dynamic::DynamicAuthKind::Bearer,
        mappings: vec![ocg_domain::dynamic::DynamicModelMapping {
            public_model: "lab-model".into(),
            upstream_model: "vendor/model".into(),
            upstream_override: None,
        }],
        created_at: now,
        updated_at: now,
        origin: ocg_domain::provider::ProviderOrigin::Custom,
        offering: "api".to_string(),
    };
    let mut account = account("dynamic-projection-account");
    account.provider_id = provider_id.clone();
    account.key_cipher = fixture_account_key_cipher();
    db.create_dynamic_provider(&runtime, &account).unwrap();

    db.conn
        .execute(
            "UPDATE credentials
             SET auth_error = 'stale auth failure', auth_state = 'invalid'
             WHERE legacy_account_id = ?1",
            [&account.id],
        )
        .unwrap();
    let mut edited = runtime.clone();
    edited.endpoint_url = "http://127.0.0.1:10".into();
    db.replace_dynamic_provider(&edited, true, false, None)
        .unwrap();
    let auth_state: String = db
        .conn
        .query_row(
            "SELECT auth_state FROM credentials WHERE legacy_account_id = ?1",
            [&account.id],
            |row| row.get(0),
        )
        .unwrap();
    assert_ne!(
        auth_state, "invalid",
        "cleared auth_error must update the projection"
    );

    let mut none = edited.clone();
    none.auth_kind = ocg_domain::dynamic::DynamicAuthKind::None;
    db.replace_dynamic_provider(&none, true, true, None)
        .unwrap();
    let has_secret: i64 = db
        .conn
        .query_row(
            "SELECT has_secret FROM credentials WHERE legacy_account_id = ?1",
            [&account.id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        has_secret, 0,
        "bearer to none must clear projected secret state"
    );

    let mut bearer = none;
    bearer.auth_kind = ocg_domain::dynamic::DynamicAuthKind::Bearer;
    let replacement = test_host_cipher().encrypt("sk-replacement").unwrap();
    db.replace_dynamic_provider(&bearer, true, false, Some(&replacement))
        .unwrap();
    let has_secret: i64 = db
        .conn
        .query_row(
            "SELECT has_secret FROM credentials WHERE legacy_account_id = ?1",
            [&account.id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        has_secret, 1,
        "none to bearer must set projected secret state"
    );

    drop(db);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
pub(super) fn create_dynamic_provider_definition_persists_without_an_account() {
    let dir = temp_data_dir("dyn-definition-only");
    let db = open_with_host_cipher(dir.clone()).unwrap();
    let now = Utc::now();
    let provider_id = uuid::Uuid::new_v4().to_string();
    let runtime = crate::dynamic::DynamicProviderRuntime {
        preset_id: Some("tencent-token-global".into()),
        id: provider_id.clone(),
        name: "Tencent Token Plan".into(),
        endpoint_url: "http://127.0.0.1:9".into(),
        upstream_protocol: crate::provider::UpstreamProtocolKind::ChatCompletions,
        auth_kind: ocg_domain::dynamic::DynamicAuthKind::Bearer,
        mappings: vec![ocg_domain::dynamic::DynamicModelMapping {
            public_model: "tencent-model".into(),
            upstream_model: "tencent/upstream".into(),
            upstream_override: None,
        }],
        created_at: now,
        updated_at: now,
        origin: ocg_domain::provider::ProviderOrigin::Preset,
        offering: "plan".to_string(),
    };
    db.create_dynamic_provider_definition(&runtime).unwrap();
    let loaded = db.get_dynamic_provider(&provider_id).unwrap().unwrap();
    assert_eq!(loaded.name, "Tencent Token Plan");
    assert_eq!(loaded.preset_id.as_deref(), Some("tencent-token-global"));
    assert_eq!(db.count_accounts_for_provider(&provider_id).unwrap(), 0);
    drop(db);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
pub(super) fn dynamic_provider_create_fault_rolls_back_provider_and_account() {
    let dir = temp_data_dir("dyn-create-fault");
    let db = open_with_host_cipher(dir.clone()).unwrap();
    let now = Utc::now();
    let provider_id = uuid::Uuid::new_v4().to_string();
    let runtime = crate::dynamic::DynamicProviderRuntime {
        preset_id: None,
        id: provider_id.clone(),
        name: "Faulty".into(),
        endpoint_url: "http://127.0.0.1:9".into(),
        upstream_protocol: crate::provider::UpstreamProtocolKind::Responses,
        auth_kind: ocg_domain::dynamic::DynamicAuthKind::None,
        mappings: vec![ocg_domain::dynamic::DynamicModelMapping {
            public_model: "free-model".into(),
            upstream_model: "free-model".into(),
            upstream_override: None,
        }],
        created_at: now,
        updated_at: now,
        origin: ocg_domain::provider::ProviderOrigin::Custom,
        offering: "api".to_string(),
    };
    let mut first = account("dyn-none");
    first.provider_id = provider_id.clone();
    first.credential_kind = crate::provider::CredentialKind::None;
    first.key_cipher = String::new();
    crate::db::dynamic_provider_fault::install("after_account_insert");
    let error = db.create_dynamic_provider(&runtime, &first).unwrap_err();
    crate::db::dynamic_provider_fault::clear();
    assert!(
        error
            .to_string()
            .contains("injected dynamic provider fault")
    );
    assert!(db.get_dynamic_provider(&provider_id).unwrap().is_none());
    assert_eq!(db.count_accounts_for_provider(&provider_id).unwrap(), 0);
    drop(db);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
pub(super) fn onboarding_resume_route_changes_remap_existing_key_grants_without_authorizing_new_routes()
 {
    use ocg_domain::connection::{
        EndpointOperation, LegacyConnectionKind, connection_id_for_legacy,
    };
    use ocg_domain::credential::{RouteSpec, assigned_endpoints_for_routes, remap_route_grant_ids};
    use ocg_domain::destination::{HttpProtocolRoute, Protocol};

    let dir = temp_data_dir("onboard-resume-route-remap");
    let db = open_with_host_cipher(dir.clone()).unwrap();
    let provider_id = uuid::Uuid::new_v4().to_string();
    let old_chat = "https://resume-old.example/v1/chat/completions";
    let old_messages = "https://resume-old.example/v1/messages";
    let old_responses = "https://resume-other.example/v1/responses";
    let new_responses = "https://resume-new.example/v1/responses";
    let mut initial = onboarding_runtime(&provider_id, "Resume Routes");
    initial.endpoint_url = old_chat.into();
    initial.upstream_protocol = UpstreamProtocolKind::ChatCompletions;
    initial.auth_kind = DynamicAuthKind::Bearer;
    let old_routes = vec![
        HttpProtocolRoute {
            protocol: Protocol::ChatCompletions,
            endpoint_url: old_chat.into(),
            auth_scheme: AuthScheme::Bearer,
        },
        HttpProtocolRoute {
            protocol: Protocol::Messages,
            endpoint_url: old_messages.into(),
            auth_scheme: AuthScheme::Bearer,
        },
        HttpProtocolRoute {
            protocol: Protocol::Responses,
            endpoint_url: old_responses.into(),
            auth_scheme: AuthScheme::Bearer,
        },
    ];
    let mut first = account("resume-route-key");
    first.provider_id = provider_id.clone();
    first.key_cipher = fixture_account_key_cipher();
    db.commit_onboarding_new_with_routes(
        &initial,
        Some(&first),
        true,
        &onboarding_operation(
            &uuid::Uuid::new_v4().to_string(),
            "resume-route-draft",
            "{}",
        ),
        Some(&old_routes),
    )
    .unwrap();
    let destination_id = ocg_domain::destination::destination_id_for_dynamic(&provider_id);
    let mut draft_catalog =
        destination_store::load_destination_catalog(&db.conn, &destination_id).unwrap();
    draft_catalog[0].protocols = vec![Protocol::Messages];
    draft_catalog[0].preferred = Some(Protocol::Messages);
    draft_catalog[0].enabled = false;
    destination_store::replace_destination_catalog(&db.conn, &destination_id, &draft_catalog)
        .unwrap();
    let connection = connection_id_for_legacy(LegacyConnectionKind::DynamicProvider, &provider_id);
    let to_spec = |route: &HttpProtocolRoute| RouteSpec {
        operation: EndpointOperation::from(route.protocol),
        url: Some(route.endpoint_url.clone()),
    };
    let old_specs = old_routes.iter().map(to_spec).collect::<Vec<_>>();
    let before = db
        .list_identity_model()
        .unwrap()
        .accounts
        .into_iter()
        .find(|row| row.account.id == first.id)
        .unwrap();
    let expected_old_grants = before.allowed_endpoint_ids.clone();
    let mut updated = initial.clone();
    updated.endpoint_url = new_responses.into();
    updated.upstream_protocol = UpstreamProtocolKind::Responses;
    updated.auth_kind = DynamicAuthKind::ApiKey;
    let new_routes = vec![
        HttpProtocolRoute {
            protocol: Protocol::Responses,
            endpoint_url: new_responses.into(),
            auth_scheme: AuthScheme::ApiKey,
        },
        HttpProtocolRoute {
            protocol: Protocol::Messages,
            endpoint_url: old_messages.into(),
            auth_scheme: AuthScheme::XApiKey,
        },
        HttpProtocolRoute {
            protocol: Protocol::ChatCompletions,
            endpoint_url: old_chat.into(),
            auth_scheme: AuthScheme::XApiKey,
        },
    ];
    let new_specs = new_routes.iter().map(to_spec).collect::<Vec<_>>();
    let expected = remap_route_grant_ids(&connection, &old_specs, &new_specs, &expected_old_grants);
    db.commit_onboarding_resume_with_routes(
        &updated,
        false,
        None,
        None,
        None,
        None,
        None,
        &onboarding_operation(
            &uuid::Uuid::new_v4().to_string(),
            "resume-route-complete",
            "{}",
        ),
        Some(&new_routes),
    )
    .unwrap();
    let persisted = crate::destination_projection::load_persisted(&db).unwrap();
    let destination = persisted
        .destinations
        .iter()
        .find(|destination| destination.id == destination_id)
        .unwrap();
    assert_eq!(destination.protocol_routes, new_routes);
    assert_eq!(destination.catalog[0].protocols, vec![Protocol::Messages]);
    assert_eq!(destination.catalog[0].preferred, Some(Protocol::Messages));
    assert!(!destination.catalog[0].enabled);
    let after = persisted
        .credentials
        .iter()
        .find(|credential| credential.legacy_account_id == first.id)
        .unwrap();
    assert_eq!(after.grants.allowed_endpoint_ids, expected);
    assert!(
        !after
            .grants
            .allowed_endpoint_ids
            .iter()
            .any(|id| id == &assigned_endpoints_for_routes(&connection, &new_specs)[0].id),
        "the new first Responses route requires explicit authorization"
    );
    assert_eq!(
        after.grants.allowed_origins,
        vec!["https://resume-old.example"]
    );
    drop(db);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
pub(super) fn dashboard_operations_prune_rows_older_than_30_days_on_insert() {
    let dir = temp_data_dir("onboard-prune");
    let db = open_with_host_cipher(dir.clone()).unwrap();
    let old_id = uuid::Uuid::new_v4().to_string();
    let old_time = (Utc::now() - Duration::days(31)).to_rfc3339();
    db.conn
        .execute(
            "INSERT INTO dashboard_operations
             (operation_id, kind, payload_digest, result_json, created_at)
             VALUES (?1, 'onboarding_commit', 'old-digest', '{}', ?2)",
            rusqlite::params![old_id, old_time],
        )
        .unwrap();
    let provider_id = uuid::Uuid::new_v4().to_string();
    let runtime = onboarding_runtime(&provider_id, "PruneOnboard");
    let mut first = account("onboard-prune");
    first.provider_id = provider_id.clone();
    first.key_cipher = fixture_account_key_cipher();
    let new_id = uuid::Uuid::new_v4().to_string();
    db.commit_onboarding_new(
        &runtime,
        Some(&first),
        false,
        &onboarding_operation(&new_id, "new-digest", "{}"),
    )
    .unwrap();
    assert!(db.find_dashboard_operation(&old_id).unwrap().is_none());
    assert!(db.find_dashboard_operation(&new_id).unwrap().is_some());
    drop(db);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
pub(super) fn onboarding_operation_ledger_preserves_result_without_account_cipher() {
    let dir = temp_data_dir("onboard-secret-free");
    let db = open_with_host_cipher(dir.clone()).unwrap();
    let provider_id = uuid::Uuid::new_v4().to_string();
    let runtime = onboarding_runtime(&provider_id, "SecretOnboard");
    let mut first = account("onboard-secret");
    first.provider_id = provider_id.clone();
    first.key_cipher = fixture_account_key_cipher();
    let operation_id = uuid::Uuid::new_v4().to_string();
    let result_json = r#"{"connectionId":"conn-1","credentialId":"acct-1","targetIds":["t1"]}"#;
    db.commit_onboarding_new(
        &runtime,
        Some(&first),
        false,
        &onboarding_operation(&operation_id, "hmac-digest-without-secret", result_json),
    )
    .unwrap();
    let row = db
        .find_dashboard_operation(&operation_id)
        .unwrap()
        .expect("operation row");
    assert_eq!(row.result_json, result_json);
    assert_eq!(row.payload_digest, "hmac-digest-without-secret");
    for haystack in [&row.result_json, &row.payload_digest] {
        assert!(
            !haystack.contains(&first.key_cipher),
            "key cipher leaked in {haystack}"
        );
    }
    drop(db);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
pub(super) fn dynamic_provider_patch_fault_rolls_back_mappings_and_runtime_state() {
    let dir = temp_data_dir("dyn-patch-fault");
    let db = open_with_host_cipher(dir.clone()).unwrap();
    let now = Utc::now();
    let provider_id = uuid::Uuid::new_v4().to_string();
    let runtime = crate::dynamic::DynamicProviderRuntime {
        preset_id: None,
        id: provider_id.clone(),
        name: "PatchFault".into(),
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
    };
    let mut first = account("dyn-patch");
    first.provider_id = provider_id.clone();
    first.key_cipher = fixture_account_key_cipher();
    db.create_dynamic_provider(&runtime, &first).unwrap();
    db.conn
        .execute(
            "UPDATE credentials SET auth_error = 'stale' WHERE legacy_account_id = ?1",
            [&first.id],
        )
        .unwrap();

    let mut updated = runtime.clone();
    updated.endpoint_url = "http://127.0.0.1:10".into();
    updated.mappings = vec![ocg_domain::dynamic::DynamicModelMapping {
        public_model: "lab-opus".into(),
        upstream_model: "vendor/opus-2".into(),
        upstream_override: None,
    }];
    crate::db::dynamic_provider_fault::install("after_mapping_replace");
    let error = db
        .replace_dynamic_provider(&updated, true, false, None)
        .unwrap_err();
    crate::db::dynamic_provider_fault::clear();
    assert!(
        error
            .to_string()
            .contains("injected dynamic provider fault")
    );
    let loaded = db.get_dynamic_provider(&provider_id).unwrap().unwrap();
    assert_eq!(loaded.endpoint_url, "http://127.0.0.1:9");
    assert_eq!(loaded.mappings[0].upstream_model, "vendor/opus");
    let account = db.get_account(&first.id).unwrap().unwrap();
    assert_eq!(account.auth_error.as_deref(), Some("stale"));
    drop(db);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
pub(super) fn replace_dynamic_provider_refuses_to_fan_out_a_replacement_key() {
    let dir = temp_data_dir("dyn-no-fanout");
    let db = open_with_host_cipher(dir.clone()).unwrap();
    let now = Utc::now();
    let provider_id = uuid::Uuid::new_v4().to_string();
    let runtime = crate::dynamic::DynamicProviderRuntime {
        preset_id: None,
        id: provider_id.clone(),
        name: "Fanout".into(),
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
    };
    let mut first = account("dyn-fanout-1");
    first.provider_id = provider_id.clone();
    first.key_cipher = fixture_account_key_cipher();
    db.create_dynamic_provider(&runtime, &first).unwrap();
    let mut second = account("dyn-fanout-2");
    second.provider_id = provider_id.clone();
    second.key_cipher = test_host_cipher().encrypt("sk-second").unwrap();
    db.create_account(&second).unwrap();

    let error = db
        .replace_dynamic_provider(&runtime, false, false, Some("cipher-new"))
        .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("replacement Key requires exactly one credential"),
        "{error}"
    );
    let first_loaded = db.get_account(&first.id).unwrap().unwrap();
    let second_loaded = db.get_account(&second.id).unwrap().unwrap();
    assert_eq!(first_loaded.key_cipher, first.key_cipher);
    assert_eq!(second_loaded.key_cipher, second.key_cipher);
    drop(db);
    fs::remove_dir_all(dir).unwrap();
}
