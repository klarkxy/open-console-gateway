//! Behavioral tests split from the root db test module.

use super::tests::*;
use super::*;
use std::fs;

#[test]
pub(super) fn o02_rotate_invalidates_custom_probe_evidence_and_keeps_builtin_catalog() {
    let dir = temp_data_dir("o02-rotate-probe");
    let db = open_with_host_cipher(dir.clone()).unwrap();
    let mut custom = account("o02-custom");
    custom.provider_id = CUSTOM_PROVIDER_ID.into();
    custom.key_cipher = fixture_account_key_cipher();
    db.create_account_with_contract(
        &custom,
        Some(&AccountCustomConfigInput {
            endpoint_url: "https://o02.example/v1/chat/completions".into(),
            upstream_protocol: UpstreamProtocolKind::ChatCompletions,
        }),
        &[AccountModelCapabilityInput {
            public_model: "lab-model".into(),
            upstream_model: "lab-model".into(),
            protocol: UpstreamProtocolKind::ChatCompletions,
            source: None,
        }],
    )
    .unwrap();
    let mut go = account("o02-go");
    go.key_cipher = fixture_account_key_cipher();
    db.create_account(&go).unwrap();

    let now = Utc::now();
    let custom_scope = ContractScope::custom_endpoint("o02-custom");
    let go_scope = ContractScope::provider(OPENCODE_PROVIDER_ID);
    db.upsert_model_protocol(&probe_row(custom_scope.clone(), "lab-model", now))
        .unwrap();
    db.upsert_model_protocol(&probe_row(go_scope.clone(), "glm-5.2", now))
        .unwrap();

    db.rotate_account_credential("o02-custom", "replacement-custom")
        .unwrap();
    db.rotate_account_credential("o02-go", "replacement-go")
        .unwrap();

    assert!(
        db.load_model_protocol(
            &custom_scope,
            "lab-model",
            UpstreamProtocolKind::ChatCompletions
        )
        .unwrap()
        .is_none(),
        "rotated Custom Key must drop probe evidence"
    );
    assert!(
        db.load_model_protocol(&go_scope, "glm-5.2", UpstreamProtocolKind::ChatCompletions)
            .unwrap()
            .is_some(),
        "builtin catalog probe rows must survive a Go Key rotate"
    );
    drop(db);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
pub(super) fn o02_endpoint_change_invalidates_custom_and_dynamic_probe_evidence() {
    let dir = temp_data_dir("o02-endpoint-probe");
    let db = open_with_host_cipher(dir.clone()).unwrap();
    let mut custom = account("o02-endpoint-custom");
    custom.provider_id = CUSTOM_PROVIDER_ID.into();
    custom.key_cipher = fixture_account_key_cipher();
    db.create_account_with_contract(
        &custom,
        Some(&AccountCustomConfigInput {
            endpoint_url: "https://old-o02.example/v1/chat/completions".into(),
            upstream_protocol: UpstreamProtocolKind::ChatCompletions,
        }),
        &[AccountModelCapabilityInput {
            public_model: "lab-model".into(),
            upstream_model: "lab-model".into(),
            protocol: UpstreamProtocolKind::ChatCompletions,
            source: None,
        }],
    )
    .unwrap();
    let now = Utc::now();
    let provider_id = "o02-dyn";
    let runtime = crate::dynamic::DynamicProviderRuntime {
        preset_id: None,
        id: provider_id.into(),
        name: "O02 Dyn".into(),
        endpoint_url: "https://dyn-old.example/v1/chat/completions".into(),
        upstream_protocol: UpstreamProtocolKind::ChatCompletions,
        auth_kind: ocg_domain::dynamic::DynamicAuthKind::Bearer,
        mappings: vec![ocg_domain::dynamic::DynamicModelMapping {
            public_model: "lab".into(),
            upstream_model: "vendor/lab".into(),
            upstream_override: None,
        }],
        created_at: now,
        updated_at: now,
        origin: ocg_domain::provider::ProviderOrigin::Custom,
        offering: "api".into(),
    };
    let mut dynamic = account("o02-dyn-key");
    dynamic.provider_id = provider_id.into();
    dynamic.key_cipher = fixture_account_key_cipher();
    db.create_dynamic_provider(&runtime, &dynamic).unwrap();

    let custom_scope = ContractScope::custom_endpoint("o02-endpoint-custom");
    let dyn_scope = ContractScope::custom_endpoint("o02-dyn-key");
    db.upsert_model_protocol(&probe_row(custom_scope.clone(), "lab-model", now))
        .unwrap();
    db.upsert_model_protocol(&probe_row(dyn_scope.clone(), "lab", now))
        .unwrap();

    db.upsert_account_custom_config(
        "o02-endpoint-custom",
        &AccountCustomConfigInput {
            endpoint_url: "https://new-o02.example/v1/chat/completions".into(),
            upstream_protocol: UpstreamProtocolKind::ChatCompletions,
        },
    )
    .unwrap();
    let mut moved = runtime.clone();
    moved.endpoint_url = "https://dyn-new.example/v1/chat/completions".into();
    moved.updated_at = Utc::now();
    db.replace_dynamic_provider(&moved, true, false, None)
        .unwrap();

    assert!(
        db.load_model_protocol(
            &custom_scope,
            "lab-model",
            UpstreamProtocolKind::ChatCompletions
        )
        .unwrap()
        .is_none()
    );
    assert!(
        db.load_model_protocol(&dyn_scope, "lab", UpstreamProtocolKind::ChatCompletions)
            .unwrap()
            .is_none()
    );
    drop(db);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
pub(super) fn zen_enabled_has_a_dedicated_writer_and_generic_update_is_rejected() {
    let dir = temp_data_dir("zen-enabled-writer");
    let db = Database::open(dir.clone()).expect("db should open");
    db.set_setting("config", r#"{"marker":"before"}"#)
        .expect("initial config should save");
    let zen_before = db
        .get_account(ZEN_FREE_ACCOUNT_ID)
        .expect("Zen lookup should work")
        .expect("Zen singleton should exist");

    let generic = AccountUpdate {
        name: None,
        username: None,
        password: None,
        key: None,
        enabled: Some(!zen_before.enabled),
        referral_code: None,
        purchase_date: None,
        notes: None,
    };
    assert!(
        db.update_account(ZEN_FREE_ACCOUNT_ID, &generic, None, None)
            .is_err(),
        "generic account writers must not bypass the Zen facade"
    );

    db.conn
        .execute_batch(&format!(
            "CREATE TRIGGER reject_zen_provider_settings
                 BEFORE UPDATE OF enabled ON credentials
                 WHEN OLD.legacy_account_id = '{ZEN_FREE_ACCOUNT_ID}'
                 BEGIN
                     SELECT RAISE(ABORT, 'forced Zen settings failure');
                 END;"
        ))
        .expect("failure trigger should install");
    db.set_config(r#"{"marker":"after"}"#)
        .expect("ordinary config should save independently");
    let error = db
        .set_zen_free_enabled(!zen_before.enabled)
        .expect_err("Zen row failure must abort the config write");
    assert!(error.to_string().contains("forced Zen settings failure"));
    assert_eq!(
        db.get_setting("config").unwrap().as_deref(),
        Some(r#"{"marker":"after"}"#)
    );
    let zen_after_failure = db.get_account(ZEN_FREE_ACCOUNT_ID).unwrap().unwrap();
    assert_eq!(zen_after_failure.enabled, zen_before.enabled);

    db.conn
        .execute("DROP TRIGGER reject_zen_provider_settings", [])
        .expect("failure trigger should drop");
    db.set_zen_free_enabled(true)
        .expect("Zen enabled setting should save");
    let zen_after = db.get_account(ZEN_FREE_ACCOUNT_ID).unwrap().unwrap();
    assert!(zen_after.enabled);

    drop(db);
    fs::remove_dir_all(dir).expect("test data dir should be removed");
}

#[test]
pub(super) fn zen_free_model_catalog_survives_reopen() {
    let dir = temp_data_dir("zen-free-model-catalog");
    let refreshed_at = Utc::now();
    {
        let db = Database::open(dir.clone()).unwrap();
        db.set_zen_free_model_catalog(&crate::kernel::zen::ZenFreeModelCatalog {
            models: vec!["persisted-coder-free".into()],
            refreshed_at: Some(refreshed_at),
            source_url: crate::kernel::zen::ZEN_MODELS_SOURCE_URL.into(),
        })
        .unwrap();
    }
    {
        let db = Database::open(dir.clone()).unwrap();
        let catalog = db.zen_free_model_catalog().unwrap().unwrap();
        assert_eq!(catalog.models, ["persisted-coder-free"]);
        assert_eq!(catalog.refreshed_at, Some(refreshed_at));
    }
    fs::remove_dir_all(dir).unwrap();
}

#[test]
pub(super) fn provider_and_custom_contract_scopes_are_isolated() {
    let dir = temp_data_dir("v26-scope-isolation");
    let db = Database::open(dir.clone()).unwrap();
    let now = Utc::now();
    let go = ContractScope::provider(OPENCODE_PROVIDER_ID);
    let custom = ContractScope::custom_endpoint("custom-a");
    db.upsert_model_protocol(&PersistedModelProtocol {
        scope: go.clone(),
        model_id: "glm-5.2".into(),
        protocol: UpstreamProtocolKind::ChatCompletions,
        source: ContractEvidenceSource::ProbeConfirmed,
        verified_at: Some(now),
        observed_at: Some(now),
        last_probe_result: Some(ProbeResultKind::Success),
        last_probe_at: Some(now),
        last_probe_error: None,
    })
    .unwrap();
    db.upsert_model_protocol(&PersistedModelProtocol {
        scope: custom.clone(),
        model_id: "local-model".into(),
        protocol: UpstreamProtocolKind::ChatCompletions,
        source: ContractEvidenceSource::Preset,
        verified_at: Some(now),
        observed_at: Some(now),
        last_probe_result: None,
        last_probe_at: None,
        last_probe_error: None,
    })
    .unwrap();
    db.set_model_protocol_overrides(
        &go,
        &[(
            "glm-5.2".into(),
            UpstreamProtocolKind::Messages,
            ProtocolOverrideState::ForceOff,
        )],
        now,
    )
    .unwrap();
    let persisted = db.load_persisted_contracts().unwrap();
    assert!(
        persisted
            .evidence
            .get(&go)
            .unwrap()
            .iter()
            .any(|row| row.model_id == "glm-5.2")
    );
    assert!(
        persisted
            .evidence
            .get(&custom)
            .unwrap()
            .iter()
            .all(|row| row.model_id != "glm-5.2")
    );
    assert!(
        persisted
            .overrides
            .get(&go)
            .unwrap()
            .iter()
            .any(|row| row.model_id == "glm-5.2" && row.state == ProtocolOverrideState::ForceOff)
    );
    assert!(
        persisted
            .overrides
            .get(&custom)
            .map(|rows| rows.iter().all(|row| row.model_id != "glm-5.2"))
            .unwrap_or(true)
    );
    drop(db);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
pub(super) fn probe_evidence_and_catalog_mutations_advance_scope_revision_atomically() {
    let dir = temp_data_dir("v26-revision-bump");
    let db = Database::open(dir.clone()).unwrap();
    let now = Utc::now();
    let scope = ContractScope::provider(OPENCODE_PROVIDER_ID);
    assert!(db.load_persisted_scope(&scope).unwrap().is_none());

    let success = db
        .upsert_model_protocol(&PersistedModelProtocol {
            scope: scope.clone(),
            model_id: "grok-4.5".into(),
            protocol: UpstreamProtocolKind::ChatCompletions,
            source: ContractEvidenceSource::ProbeConfirmed,
            verified_at: Some(now),
            observed_at: Some(now),
            last_probe_result: Some(ProbeResultKind::Success),
            last_probe_at: Some(now),
            last_probe_error: None,
        })
        .unwrap();
    assert_eq!(success.revision, 2);
    let after_success = db.load_persisted_scope(&scope).unwrap().unwrap();
    assert_eq!(after_success.revision, 2);

    let failure = db
        .upsert_model_protocol(&PersistedModelProtocol {
            scope: scope.clone(),
            model_id: "grok-4.5".into(),
            protocol: UpstreamProtocolKind::Messages,
            source: ContractEvidenceSource::ProbeObserved,
            verified_at: None,
            observed_at: Some(now),
            last_probe_result: Some(ProbeResultKind::Failure),
            last_probe_at: Some(now),
            last_probe_error: Some("upstream 500".into()),
        })
        .unwrap();
    assert_eq!(failure.revision, 3);

    let catalog = db
        .set_contract_catalog(
            &scope,
            &["grok-4.5".into()],
            Some(now),
            crate::provider_contracts::CATALOG_SOURCE_STATIC,
            "",
            now,
        )
        .unwrap();
    assert_eq!(catalog.revision, 4);

    db.conn
        .execute_batch(
            "CREATE TRIGGER fail_probe_write BEFORE INSERT ON provider_contract_model_protocols
                 BEGIN SELECT RAISE(ABORT, 'injected write failure'); END;",
        )
        .unwrap();
    let before_failed = db.load_persisted_scope(&scope).unwrap().unwrap().revision;
    let failed = db.upsert_model_protocol(&PersistedModelProtocol {
        scope: scope.clone(),
        model_id: "glm-5.3".into(),
        protocol: UpstreamProtocolKind::Responses,
        source: ContractEvidenceSource::ProbeObserved,
        verified_at: None,
        observed_at: Some(now),
        last_probe_result: Some(ProbeResultKind::Failure),
        last_probe_at: Some(now),
        last_probe_error: Some("should roll back".into()),
    });
    assert!(failed.is_err());
    let after_failed = db.load_persisted_scope(&scope).unwrap().unwrap();
    assert_eq!(after_failed.revision, before_failed);
    assert!(
        db.load_model_protocol(&scope, "glm-5.3", UpstreamProtocolKind::Responses)
            .unwrap()
            .is_none()
    );

    drop(db);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
pub(super) fn probe_observation_batch_upserts_atomically_and_bumps_scope_once() {
    let dir = temp_data_dir("v26-probe-batch");
    let db = Database::open(dir.clone()).unwrap();
    let now = Utc::now();
    let go = ContractScope::provider(OPENCODE_PROVIDER_ID);
    let custom = ContractScope::custom_endpoint("custom-a");

    let empty = db.upsert_model_protocols(&[]);
    assert!(empty.is_err(), "{empty:?}");
    assert!(db.load_persisted_scope(&go).unwrap().is_none());

    let mixed = db.upsert_model_protocols(&[
        probe_observation(
            go.clone(),
            "grok-4.5",
            UpstreamProtocolKind::ChatCompletions,
            now,
        ),
        probe_observation(
            custom.clone(),
            "local-model",
            UpstreamProtocolKind::ChatCompletions,
            now,
        ),
    ]);
    assert!(mixed.is_err(), "{mixed:?}");
    assert!(db.load_persisted_scope(&go).unwrap().is_none());
    assert!(db.load_persisted_scope(&custom).unwrap().is_none());
    assert!(
        db.load_model_protocol(&go, "grok-4.5", UpstreamProtocolKind::ChatCompletions)
            .unwrap()
            .is_none()
    );

    let persisted = db
        .upsert_model_protocols(&[
            probe_observation(
                go.clone(),
                "grok-4.5",
                UpstreamProtocolKind::ChatCompletions,
                now,
            ),
            probe_observation(go.clone(), "grok-4.5", UpstreamProtocolKind::Responses, now),
        ])
        .unwrap();
    assert_eq!(persisted.revision, 2);
    let after = db.load_persisted_scope(&go).unwrap().unwrap();
    assert_eq!(after.revision, 2);
    assert!(
        db.load_model_protocol(&go, "grok-4.5", UpstreamProtocolKind::ChatCompletions)
            .unwrap()
            .is_some()
    );
    assert!(
        db.load_model_protocol(&go, "grok-4.5", UpstreamProtocolKind::Responses)
            .unwrap()
            .is_some()
    );

    db.conn
        .execute_batch(
            "CREATE TRIGGER fail_second_probe_observation_write
                 BEFORE INSERT ON provider_contract_model_protocols
                 WHEN NEW.protocol = 'messages'
                 BEGIN SELECT RAISE(ABORT, 'injected second observation write failure'); END;",
        )
        .unwrap();
    let before_failed = db.load_persisted_scope(&go).unwrap().unwrap().revision;
    let failed = db.upsert_model_protocols(&[
        probe_observation(
            go.clone(),
            "glm-5.3",
            UpstreamProtocolKind::ChatCompletions,
            now,
        ),
        probe_observation(go.clone(), "glm-5.3", UpstreamProtocolKind::Messages, now),
    ]);
    assert!(failed.is_err(), "{failed:?}");
    assert_eq!(
        db.load_persisted_scope(&go).unwrap().unwrap().revision,
        before_failed
    );
    assert!(
        db.load_model_protocol(&go, "glm-5.3", UpstreamProtocolKind::ChatCompletions)
            .unwrap()
            .is_none()
    );
    assert!(
        db.load_model_protocol(&go, "glm-5.3", UpstreamProtocolKind::Messages)
            .unwrap()
            .is_none()
    );

    drop(db);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
pub(super) fn create_account_with_contract_is_atomic_on_custom_config_failure() {
    let dir = temp_data_dir("v23-atomic-create");
    let db = Database::open(dir.clone()).unwrap();
    let mut custom = account("custom-atomic");
    custom.provider_id = CUSTOM_PROVIDER_ID.to_string();
    custom.enabled = false;
    db.conn
        .execute_batch(
            "CREATE TRIGGER fail_custom_config
                 BEFORE INSERT ON destinations
                 WHEN NEW.legacy_kind = 'custom_account'
                 BEGIN
                     SELECT RAISE(ABORT, 'forced custom config failure');
                 END;",
        )
        .unwrap();

    let error = db
        .create_account_with_contract(
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
        .expect_err("forced custom config failure should abort the create");
    assert!(
        error.to_string().contains("forced custom config failure"),
        "{error}"
    );
    assert!(db.get_account("custom-atomic").unwrap().is_none());
    assert!(db.account_custom_config("custom-atomic").unwrap().is_none());
    assert!(
        db.list_account_model_capabilities("custom-atomic")
            .unwrap()
            .is_empty()
    );

    db.conn
        .execute_batch("DROP TRIGGER fail_custom_config;")
        .unwrap();
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
    assert!(db.get_account("custom-atomic").unwrap().is_some());
    assert!(db.account_custom_config("custom-atomic").unwrap().is_some());

    let mut go = account("go-rejects-custom");
    let rejected = db.create_account_with_contract(
        &go,
        Some(&AccountCustomConfigInput {
            endpoint_url: "https://api.example.com/v1/chat/completions".into(),
            upstream_protocol: UpstreamProtocolKind::ChatCompletions,
        }),
        &[],
    );
    assert!(rejected.is_err(), "non-Custom accounts must reject config");
    assert!(db.get_account("go-rejects-custom").unwrap().is_none());

    go.id = "go-rejects-caps".into();
    go.name = "go-rejects-caps".into();
    let rejected_caps = db.create_account_with_contract(
        &go,
        None,
        &[AccountModelCapabilityInput {
            public_model: "org/model".into(),
            upstream_model: "org/model".into(),
            protocol: UpstreamProtocolKind::ChatCompletions,
            source: None,
        }],
    );
    assert!(
        rejected_caps.is_err(),
        "non-Custom accounts must reject capabilities"
    );
    assert!(db.get_account("go-rejects-caps").unwrap().is_none());

    let mut custom_empty = account("custom-empty-caps");
    custom_empty.provider_id = CUSTOM_PROVIDER_ID.to_string();
    custom_empty.enabled = false;
    let empty_caps = db.create_account_with_contract(
        &custom_empty,
        Some(&AccountCustomConfigInput {
            endpoint_url: "https://api.example.com/v1/chat/completions".into(),
            upstream_protocol: UpstreamProtocolKind::ChatCompletions,
        }),
        &[],
    );
    assert!(
        empty_caps.is_err(),
        "Custom create must require at least one model capability"
    );
    assert!(db.get_account("custom-empty-caps").unwrap().is_none());

    drop(db);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
pub(super) fn model_protocol_override_upsert_and_auto_delete_round_trip() {
    let dir = temp_data_dir("override-roundtrip");
    let db = Database::open(dir.clone()).unwrap();
    let now = Utc::now();
    let scope = ContractScope::provider(OPENCODE_PROVIDER_ID);

    db.set_model_protocol_overrides(
        &scope,
        &[
            (
                "glm-5.2".into(),
                UpstreamProtocolKind::ChatCompletions,
                ProtocolOverrideState::ForceOn,
            ),
            (
                "glm-5.2".into(),
                UpstreamProtocolKind::Messages,
                ProtocolOverrideState::ForceOff,
            ),
        ],
        now,
    )
    .unwrap();

    let persisted = db.load_persisted_contracts().unwrap();
    let overrides = persisted.overrides.get(&scope).unwrap();
    assert_eq!(overrides.len(), 2);
    assert!(overrides.iter().any(|row| row.model_id == "glm-5.2"
        && row.protocol == UpstreamProtocolKind::ChatCompletions
        && row.state == ProtocolOverrideState::ForceOn));
    assert!(overrides.iter().any(|row| row.model_id == "glm-5.2"
        && row.protocol == UpstreamProtocolKind::Messages
        && row.state == ProtocolOverrideState::ForceOff));

    db.set_model_protocol_overrides(
        &scope,
        &[(
            "glm-5.2".into(),
            UpstreamProtocolKind::ChatCompletions,
            ProtocolOverrideState::Auto,
        )],
        now,
    )
    .unwrap();

    let persisted = db.load_persisted_contracts().unwrap();
    let overrides = persisted.overrides.get(&scope).unwrap();
    assert_eq!(overrides.len(), 1);
    assert_eq!(overrides[0].protocol, UpstreamProtocolKind::Messages);
    assert_eq!(overrides[0].state, ProtocolOverrideState::ForceOff);

    drop(db);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
pub(super) fn catalog_refresh_preserves_settings_and_does_not_force_off_new_models() {
    let dir = temp_data_dir("catalog-refresh-preserving-settings");
    let db = Database::open(dir.clone()).unwrap();
    let now = Utc::now();
    let scope = ContractScope::provider(OPENCODE_PROVIDER_ID);

    db.set_contract_catalog(
        &scope,
        &["grok-4.5".into()],
        None,
        crate::provider_contracts::CATALOG_SOURCE_STATIC,
        "",
        now,
    )
    .unwrap();
    db.set_model_protocol_overrides(
        &scope,
        &[
            (
                "grok-4.5".into(),
                UpstreamProtocolKind::Responses,
                ProtocolOverrideState::ForceOn,
            ),
            (
                "glm-5.2".into(),
                UpstreamProtocolKind::ChatCompletions,
                ProtocolOverrideState::ForceOff,
            ),
            (
                "future-go-model".into(),
                UpstreamProtocolKind::Messages,
                ProtocolOverrideState::ForceOn,
            ),
        ],
        now,
    )
    .unwrap();
    let revision_before = db.load_persisted_scope(&scope).unwrap().unwrap().revision;

    let refreshed = db
        .refresh_contract_catalog_preserving_settings(
            &scope,
            &[
                "grok-4.5".into(),
                "glm-5.2".into(),
                "future-go-model".into(),
                "omen-alpha".into(),
            ],
            now,
            crate::provider_contracts::CATALOG_SOURCE_OPENCODE_MODELS,
            "https://opencode.ai/zen/go/v1/models",
        )
        .unwrap();

    assert_eq!(refreshed.revision, revision_before + 1);
    assert_eq!(
        refreshed.catalog_models,
        vec!["grok-4.5", "glm-5.2", "future-go-model", "omen-alpha"]
    );
    let persisted = db.load_persisted_contracts().unwrap();
    let overrides = persisted.overrides.get(&scope).unwrap();
    assert!(overrides.iter().any(|row| row.model_id == "grok-4.5"
        && row.protocol == UpstreamProtocolKind::Responses
        && row.state == ProtocolOverrideState::ForceOn));
    assert!(overrides.iter().any(|row| row.model_id == "glm-5.2"
        && row.protocol == UpstreamProtocolKind::ChatCompletions
        && row.state == ProtocolOverrideState::ForceOff));
    assert!(overrides.iter().any(|row| row.model_id == "future-go-model"
        && row.protocol == UpstreamProtocolKind::Messages
        && row.state == ProtocolOverrideState::ForceOn));
    assert_eq!(overrides.len(), 3, "refresh must not invent force_off rows");
    assert!(
        overrides.iter().all(|row| row.model_id != "omen-alpha"),
        "unknown new models must not receive guessed overrides: {overrides:?}"
    );

    let go = effective_from_db(&db)
        .providers
        .remove(OPENCODE_PROVIDER_ID)
        .unwrap();
    assert!(
        go.model("glm-5.2")
            .is_some_and(|model| !model.protocols["chat_completions"].enabled)
    );
    assert!(
        go.model("omen-alpha")
            .is_some_and(|model| model.enabled_protocols().is_empty() && !model.routable)
    );
    drop(db);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
pub(super) fn catalog_refresh_enables_new_models_with_official_or_known_baseline() {
    let dir = temp_data_dir("catalog-refresh-official-on");
    let db = Database::open(dir.clone()).unwrap();
    let now = Utc::now();
    let go_scope = ContractScope::provider(OPENCODE_PROVIDER_ID);
    let goat_scope = ContractScope::provider(COMMAND_CODE_PROVIDER_ID);
    let extra = "vendor/future-command-model";

    db.refresh_contract_catalog_preserving_settings(
        &go_scope,
        &["glm-5.2".into(), "future-go-model".into()],
        now,
        crate::provider_contracts::CATALOG_SOURCE_OPENCODE_MODELS,
        "https://opencode.ai/zen/go/v1/models",
    )
    .unwrap();
    db.apply_official_protocol_baseline(
        &go_scope,
        &["glm-5.2".into(), "future-go-model".into()],
        &crate::official_protocols::OfficialProtocolBaseline::mapped([(
            "future-go-model",
            UpstreamProtocolKind::Responses,
        )]),
        now,
    )
    .unwrap();

    db.refresh_contract_catalog_preserving_settings(
        &goat_scope,
        &["gpt-6-luna".into()],
        now,
        CATALOG_SOURCE_COMMAND_CODE_MODELS,
        COMMAND_CODE_GOAT_BASE_URL,
    )
    .unwrap();
    db.refresh_contract_catalog_preserving_settings(
        &goat_scope,
        &["gpt-6-luna".into(), extra.into()],
        now,
        CATALOG_SOURCE_COMMAND_CODE_MODELS,
        COMMAND_CODE_GOAT_BASE_URL,
    )
    .unwrap();
    db.apply_official_protocol_baseline(
        &goat_scope,
        &[extra.into()],
        &crate::official_protocols::OfficialProtocolBaseline::mapped([(
            extra,
            UpstreamProtocolKind::ChatCompletions,
        )]),
        now,
    )
    .unwrap();

    let set = effective_from_db(&db);
    let go = set.providers.get(OPENCODE_PROVIDER_ID).unwrap();
    let glm = go.model("glm-5.2").unwrap();
    assert!(glm.protocols["chat_completions"].enabled);
    assert_eq!(
        glm.protocols["chat_completions"].r#override,
        ProtocolOverrideState::Auto
    );
    let future = go.model("future-go-model").unwrap();
    assert!(future.protocols["responses"].enabled);
    assert_eq!(
        future.protocols["responses"].r#override,
        ProtocolOverrideState::Auto
    );
    assert!(
        future
            .protocols
            .get("chat_completions")
            .is_none_or(|row| !row.enabled && !row.available)
    );
    let goat = set.providers.get(COMMAND_CODE_PROVIDER_ID).unwrap();
    let extra_model = goat.model(extra).unwrap();
    assert!(extra_model.protocols["chat_completions"].enabled);
    assert_eq!(
        extra_model.protocols["chat_completions"].r#override,
        ProtocolOverrideState::Auto
    );

    drop(db);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
pub(super) fn unavailable_official_baseline_does_not_drop_catalog_or_overrides() {
    let dir = temp_data_dir("catalog-refresh-unavailable-baseline");
    let db = Database::open(dir.clone()).unwrap();
    let now = Utc::now();
    let scope = ContractScope::provider(OPENCODE_PROVIDER_ID);
    db.set_contract_catalog(
        &scope,
        &["grok-4.5".into()],
        Some(now),
        crate::provider_contracts::CATALOG_SOURCE_OPENCODE_MODELS,
        "https://opencode.ai/zen/go/v1/models",
        now,
    )
    .unwrap();
    db.set_model_protocol_overrides(
        &scope,
        &[(
            "grok-4.5".into(),
            UpstreamProtocolKind::Responses,
            ProtocolOverrideState::ForceOff,
        )],
        now,
    )
    .unwrap();
    let before = db.load_persisted_contracts().unwrap();

    db.apply_official_protocol_baseline(
        &scope,
        &["grok-4.5".into()],
        &crate::official_protocols::OfficialProtocolBaseline::Unavailable,
        now,
    )
    .unwrap();

    let after = db.load_persisted_contracts().unwrap();
    assert_eq!(
        after.scopes.get(&scope).unwrap().catalog_models,
        before.scopes.get(&scope).unwrap().catalog_models
    );
    assert_eq!(after.overrides.get(&scope), before.overrides.get(&scope));
    assert_eq!(after.evidence.get(&scope), before.evidence.get(&scope));
    let grok = effective_from_db(&db)
        .providers
        .remove(OPENCODE_PROVIDER_ID)
        .unwrap()
        .model("grok-4.5")
        .unwrap()
        .clone();
    assert!(!grok.protocols["responses"].enabled);
    assert_eq!(
        grok.protocols["responses"].r#override,
        ProtocolOverrideState::ForceOff
    );

    drop(db);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
pub(super) fn catalog_remove_drops_models_and_satellite_rows_without_rewriting_source() {
    let dir = temp_data_dir("catalog-remove-models");
    let db = Database::open(dir.clone()).unwrap();
    let now = Utc::now();
    let scope = ContractScope::provider(OPENCODE_PROVIDER_ID);
    let refreshed_at = now;

    db.set_contract_catalog(
        &scope,
        &["keep-me".into(), "drop-me".into()],
        Some(refreshed_at),
        crate::provider_contracts::CATALOG_SOURCE_OPENCODE_MODELS,
        "https://opencode.ai/zen/go/v1/models",
        now,
    )
    .unwrap();
    db.set_model_protocol_overrides(
        &scope,
        &[
            (
                "keep-me".into(),
                UpstreamProtocolKind::ChatCompletions,
                ProtocolOverrideState::ForceOn,
            ),
            (
                "drop-me".into(),
                UpstreamProtocolKind::ChatCompletions,
                ProtocolOverrideState::ForceOn,
            ),
        ],
        now,
    )
    .unwrap();
    let revision_before = db.load_persisted_scope(&scope).unwrap().unwrap().revision;

    let removed = db
        .remove_contract_catalog_models(&scope, &["drop-me".into()], now)
        .unwrap();

    assert_eq!(removed.revision, revision_before + 1);
    assert_eq!(removed.catalog_models, vec!["keep-me"]);
    assert_eq!(removed.catalog_refreshed_at, Some(refreshed_at));
    assert_eq!(
        removed.catalog_source,
        crate::provider_contracts::CATALOG_SOURCE_OPENCODE_MODELS
    );
    assert_eq!(
        removed.catalog_source_url,
        "https://opencode.ai/zen/go/v1/models"
    );
    let persisted = db.load_persisted_contracts().unwrap();
    let overrides = persisted.overrides.get(&scope).unwrap();
    assert!(overrides.iter().all(|row| row.model_id != "drop-me"));
    assert!(overrides.iter().any(|row| row.model_id == "keep-me"
        && row.protocol == UpstreamProtocolKind::ChatCompletions
        && row.state == ProtocolOverrideState::ForceOn));

    drop(db);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
pub(super) fn zen_catalog_remove_last_and_all_stay_empty_after_reopen() {
    let dir = temp_data_dir("zen-catalog-remove-empty");
    let now = Utc::now();
    let scope = ContractScope::provider(OPENCODE_ZEN_FREE_PROVIDER_ID);
    let snapshot = crate::kernel::zen::ZenFreeModelCatalog {
        models: vec!["review-model-free".into(), "second-free".into()],
        refreshed_at: Some(now),
        source_url: crate::kernel::zen::ZEN_MODELS_SOURCE_URL.into(),
    };

    {
        let db = Database::open(dir.clone()).unwrap();
        db.set_zen_free_model_catalog_preserving_settings(&snapshot)
            .unwrap();
        db.remove_contract_catalog_models(&scope, &["second-free".into()], now)
            .unwrap();
        let after_one = db.load_persisted_scope(&scope).unwrap().unwrap();
        assert_eq!(after_one.catalog_models, vec!["review-model-free"]);
        db.remove_contract_catalog_models(&scope, &["review-model-free".into()], now)
            .unwrap();
        let after_last = db.load_persisted_scope(&scope).unwrap().unwrap();
        assert!(after_last.catalog_models.is_empty());
        let live = crate::provider_contracts::build_effective_contracts(
            &snapshot,
            &[],
            db.load_persisted_contracts().unwrap(),
        );
        let zen = live.scope(&scope).unwrap();
        assert!(zen.catalog.models.is_empty());
        assert!(zen.model("review-model-free").is_none());
        assert!(zen.model("second-free").is_none());
    }

    let reopened = Database::open(dir.clone()).unwrap();
    let stored_snapshot = reopened.zen_free_model_catalog().unwrap().unwrap();
    assert_eq!(
        stored_snapshot.models,
        vec!["review-model-free", "second-free"]
    );
    let persisted = reopened.load_persisted_contracts().unwrap();
    assert!(
        persisted
            .scopes
            .get(&scope)
            .is_some_and(|row| row.catalog_models.is_empty())
    );
    let restored =
        crate::provider_contracts::build_effective_contracts(&stored_snapshot, &[], persisted);
    let zen = restored.scope(&scope).unwrap();
    assert!(zen.catalog.models.is_empty());
    assert!(zen.model("review-model-free").is_none());
    assert!(!zen.model_has_enabled_protocol("review-model-free"));
    drop(reopened);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
pub(super) fn zen_catalog_remove_all_at_once_stays_empty() {
    let dir = temp_data_dir("zen-catalog-remove-all");
    let now = Utc::now();
    let scope = ContractScope::provider(OPENCODE_ZEN_FREE_PROVIDER_ID);
    let snapshot = crate::kernel::zen::ZenFreeModelCatalog {
        models: vec!["review-model-free".into(), "second-free".into()],
        refreshed_at: Some(now),
        source_url: crate::kernel::zen::ZEN_MODELS_SOURCE_URL.into(),
    };
    let db = Database::open(dir.clone()).unwrap();
    db.set_zen_free_model_catalog_preserving_settings(&snapshot)
        .unwrap();
    db.remove_contract_catalog_models(
        &scope,
        &["review-model-free".into(), "second-free".into()],
        now,
    )
    .unwrap();
    let stored = db.load_persisted_scope(&scope).unwrap().unwrap();
    assert!(stored.catalog_models.is_empty());
    let live = crate::provider_contracts::build_effective_contracts(
        &snapshot,
        &[],
        db.load_persisted_contracts().unwrap(),
    );
    assert!(live.scope(&scope).unwrap().catalog.models.is_empty());
    drop(db);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
pub(super) fn zen_official_static_preference_saves_responses_and_messages_not_probe_rows() {
    let dir = temp_data_dir("zen-official-protocol-preference");
    let now = Utc::now();
    let scope = ContractScope::provider(OPENCODE_ZEN_FREE_PROVIDER_ID);
    let db = Database::open(dir.clone()).unwrap();
    db.set_zen_free_model_catalog_preserving_settings(&crate::kernel::zen::ZenFreeModelCatalog {
        models: vec!["review-model-free".into(), "messages-model-free".into()],
        refreshed_at: Some(now),
        source_url: crate::kernel::zen::ZEN_MODELS_SOURCE_URL.into(),
    })
    .unwrap();
    db.apply_official_protocol_baseline(
        &scope,
        &["review-model-free".into(), "messages-model-free".into()],
        &crate::official_protocols::OfficialProtocolBaseline::mapped([
            ("review-model", UpstreamProtocolKind::Responses),
            ("messages-model", UpstreamProtocolKind::Messages),
        ]),
        now,
    )
    .unwrap();
    let saved = db.load_persisted_contracts().unwrap();
    let preferences = saved.preferences.get(&scope).cloned().unwrap_or_default();
    assert!(preferences.iter().any(|(model, protocol)| {
        model == "review-model-free" && *protocol == UpstreamProtocolKind::Responses
    }));
    assert!(preferences.iter().any(|(model, protocol)| {
        model == "messages-model-free" && *protocol == UpstreamProtocolKind::Messages
    }));
    db.set_model_protocol_settings(
        &scope,
        &[(
            "review-model-free".into(),
            UpstreamProtocolKind::Responses,
            ProtocolOverrideState::ForceOn,
        )],
        &[("review-model-free".into(), UpstreamProtocolKind::Responses)],
        now,
    )
    .unwrap();
    db.set_model_protocol_settings(
        &scope,
        &[(
            "messages-model-free".into(),
            UpstreamProtocolKind::Messages,
            ProtocolOverrideState::ForceOn,
        )],
        &[("messages-model-free".into(), UpstreamProtocolKind::Messages)],
        now,
    )
    .unwrap();

    db.upsert_model_protocol(&PersistedModelProtocol {
        scope: scope.clone(),
        model_id: "review-model-free".into(),
        protocol: UpstreamProtocolKind::Messages,
        source: ContractEvidenceSource::ProbeObserved,
        verified_at: Some(now),
        observed_at: Some(now),
        last_probe_result: Some(ProbeResultKind::Success),
        last_probe_at: Some(now),
        last_probe_error: None,
    })
    .unwrap();
    let rejected = db.set_model_protocol_settings(
        &scope,
        &[(
            "review-model-free".into(),
            UpstreamProtocolKind::Messages,
            ProtocolOverrideState::ForceOn,
        )],
        &[("review-model-free".into(), UpstreamProtocolKind::Messages)],
        now,
    );
    assert!(
        rejected.is_err(),
        "probe-manufactured evidence must not expand Zen preference admission: {rejected:?}"
    );
    drop(db);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
pub(super) fn command_first_refresh_enables_goat_cohort_then_new_discoveries() {
    let dir = temp_data_dir("command-first-refresh-goat-cohort");
    let db = Database::open(dir.clone()).unwrap();
    let now = Utc::now();
    let scope = ContractScope::provider(COMMAND_CODE_PROVIDER_ID);
    let included = "gpt-6-luna".to_string();
    let premium = "vendor/premium-model".to_string();
    let preenabled = "vendor/saved-on".to_string();
    let later = "vendor/new-model".to_string();
    let baseline = crate::official_protocols::OfficialProtocolBaseline::mapped([
        (included.as_str(), UpstreamProtocolKind::ChatCompletions),
        (premium.as_str(), UpstreamProtocolKind::ChatCompletions),
        (preenabled.as_str(), UpstreamProtocolKind::ChatCompletions),
        (later.as_str(), UpstreamProtocolKind::ChatCompletions),
    ]);
    set_model_protocol_override_on(
        &db.conn,
        &scope,
        &preenabled,
        UpstreamProtocolKind::ChatCompletions,
        ProtocolOverrideState::ForceOn,
        now,
    )
    .unwrap();

    db.refresh_contract_catalog_preserving_settings(
        &scope,
        &[included.clone(), premium.clone(), preenabled.clone()],
        now,
        CATALOG_SOURCE_COMMAND_CODE_MODELS,
        COMMAND_CODE_GOAT_BASE_URL,
    )
    .unwrap();
    db.apply_official_protocol_baseline(
        &scope,
        &[included.clone(), premium.clone(), preenabled.clone()],
        &baseline,
        now,
    )
    .unwrap();
    let initial = effective_from_db(&db)
        .providers
        .remove(COMMAND_CODE_PROVIDER_ID)
        .unwrap();
    assert!(initial.model(&included).unwrap().has_enabled_protocol());
    assert!(!initial.model(&premium).unwrap().has_enabled_protocol());
    assert!(initial.model(&preenabled).unwrap().has_enabled_protocol());

    db.refresh_contract_catalog_preserving_settings(
        &scope,
        &[
            included.clone(),
            premium.clone(),
            preenabled.clone(),
            later.clone(),
        ],
        now,
        CATALOG_SOURCE_COMMAND_CODE_MODELS,
        COMMAND_CODE_GOAT_BASE_URL,
    )
    .unwrap();
    db.apply_official_protocol_baseline(
        &scope,
        &[included, premium.clone(), preenabled.clone(), later.clone()],
        &baseline,
        now,
    )
    .unwrap();
    let refreshed = effective_from_db(&db)
        .providers
        .remove(COMMAND_CODE_PROVIDER_ID)
        .unwrap();
    assert!(!refreshed.model(&premium).unwrap().has_enabled_protocol());
    assert!(refreshed.model(&preenabled).unwrap().has_enabled_protocol());
    assert!(refreshed.model(&later).unwrap().has_enabled_protocol());

    drop(db);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
pub(super) fn command_catalog_reappearing_preset_returns_to_auto_enabled() {
    let dir = temp_data_dir("command-catalog-reappearing-preset");
    let db = Database::open(dir.clone()).unwrap();
    let now = Utc::now();
    let scope = ContractScope::provider(COMMAND_CODE_PROVIDER_ID);
    let preset = COMMAND_CODE_GOAT_INCLUDED_MODEL_IDS[0].to_string();
    let extra = "vendor/future-command-model".to_string();

    db.set_contract_catalog(
        &scope,
        std::slice::from_ref(&preset),
        Some(now),
        CATALOG_SOURCE_COMMAND_CODE_MODELS,
        COMMAND_CODE_GOAT_BASE_URL,
        now,
    )
    .unwrap();
    db.refresh_contract_catalog_preserving_settings(
        &scope,
        std::slice::from_ref(&extra),
        now,
        CATALOG_SOURCE_COMMAND_CODE_MODELS,
        COMMAND_CODE_GOAT_BASE_URL,
    )
    .unwrap();
    db.refresh_contract_catalog_preserving_settings(
        &scope,
        &[extra.clone(), preset.clone()],
        now,
        CATALOG_SOURCE_COMMAND_CODE_MODELS,
        COMMAND_CODE_GOAT_BASE_URL,
    )
    .unwrap();

    let persisted = db.load_persisted_contracts().unwrap();
    let overrides = persisted.overrides.get(&scope);
    assert!(
        overrides.is_none_or(|rows| rows.is_empty()),
        "catalog refresh must not invent GOAT overrides: {overrides:?}"
    );
    let goat = effective_from_db(&db)
        .providers
        .remove(COMMAND_CODE_PROVIDER_ID)
        .unwrap();
    assert!(
        goat.model(&preset)
            .is_some_and(crate::provider_contracts::EffectiveModelContract::has_enabled_protocol)
    );
    assert!(
        goat.model(&extra)
            .is_some_and(|model| !model.has_enabled_protocol()),
        "GOAT extras stay off until official-docs Static evidence exists"
    );

    drop(db);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
pub(super) fn catalog_refresh_preserves_disabled_models_when_baseline_adds_responses() {
    let dir = temp_data_dir("catalog-refresh-disabled-baseline");
    let db = open_with_host_cipher(dir.clone()).unwrap();
    let now = Utc::now();
    for (provider_id, old_model, new_model, source, source_url) in [
        (
            OPENCODE_PROVIDER_ID,
            "grok-4.5",
            "new-go-model",
            crate::provider_contracts::CATALOG_SOURCE_OPENCODE_MODELS,
            "https://opencode.ai/zen/go/v1/models",
        ),
        (
            COMMAND_CODE_PROVIDER_ID,
            COMMAND_CODE_GOAT_DEEPSEEK_V4_FLASH_UPSTREAM,
            "vendor/new-command-model",
            CATALOG_SOURCE_COMMAND_CODE_MODELS,
            COMMAND_CODE_GOAT_BASE_URL,
        ),
    ] {
        let scope = ContractScope::provider(provider_id);
        db.set_contract_catalog(
            &scope,
            &[old_model.into()],
            Some(now),
            source,
            source_url,
            now,
        )
        .unwrap();
        db.upsert_model_protocol(&PersistedModelProtocol {
            scope: scope.clone(),
            model_id: old_model.into(),
            protocol: UpstreamProtocolKind::ChatCompletions,
            source: ContractEvidenceSource::Static,
            verified_at: Some(now),
            observed_at: Some(now),
            last_probe_result: Some(ProbeResultKind::Success),
            last_probe_at: Some(now),
            last_probe_error: None,
        })
        .unwrap();
        db.set_model_protocol_settings(
            &scope,
            &[(
                old_model.into(),
                UpstreamProtocolKind::ChatCompletions,
                ProtocolOverrideState::ForceOff,
            )],
            &[(old_model.into(), UpstreamProtocolKind::ChatCompletions)],
            now,
        )
        .unwrap();
        let before_destination = crate::destination_projection::load_persisted(&db)
            .unwrap()
            .destinations
            .into_iter()
            .find(|destination| {
                destination.id == ocg_domain::destination::destination_id_for_builtin(provider_id)
            })
            .unwrap();
        assert!(
            !before_destination
                .catalog
                .iter()
                .find(|model| model.public_model.eq_ignore_ascii_case(old_model))
                .unwrap()
                .enabled
        );
        assert!(
            !db.load_persisted_contracts().unwrap().overrides[&scope]
                .iter()
                .any(|row| row.protocol == UpstreamProtocolKind::Responses),
            "new Responses protocol must start Auto so this test detects accidental reopening"
        );
        let before_evidence = db
            .load_persisted_contracts()
            .unwrap()
            .evidence
            .get(&scope)
            .cloned()
            .unwrap();
        db.refresh_contract_catalog_preserving_settings(
            &scope,
            &[old_model.into(), new_model.into()],
            now,
            source,
            source_url,
        )
        .unwrap();
        db.apply_official_protocol_baseline(
            &scope,
            &[old_model.into(), new_model.into()],
            &crate::official_protocols::OfficialProtocolBaseline::mapped_protocols([
                (
                    old_model,
                    vec![
                        UpstreamProtocolKind::ChatCompletions,
                        UpstreamProtocolKind::Responses,
                    ],
                ),
                (new_model, vec![UpstreamProtocolKind::Responses]),
            ]),
            now,
        )
        .unwrap();
        let persisted = db.load_persisted_contracts().unwrap();
        assert!(
            persisted.preferences[&scope]
                .iter()
                .any(|(model, protocol)| {
                    model == old_model && *protocol == UpstreamProtocolKind::ChatCompletions
                })
        );
        assert!(before_evidence.iter().all(|before| {
            persisted.evidence[&scope]
                .iter()
                .any(|after| after == before)
        }));
        let effective = effective_from_db(&db);
        let contract = effective.providers.get(provider_id).unwrap();
        let old = contract.model(old_model).unwrap();
        let new = contract.model(new_model).unwrap();
        assert!(
            !old.has_enabled_protocol(),
            "old disabled model was resurrected"
        );
        assert!(new.protocols["responses"].enabled);
        let destination_id = ocg_domain::destination::destination_id_for_builtin(provider_id);
        let destination = crate::destination_projection::load_persisted(&db)
            .unwrap()
            .destinations
            .into_iter()
            .find(|destination| destination.id == destination_id)
            .unwrap();
        assert!(
            !destination
                .catalog
                .iter()
                .find(|model| model.public_model.eq_ignore_ascii_case(old_model))
                .unwrap()
                .enabled
        );
        assert!(
            destination
                .catalog
                .iter()
                .find(|model| model.public_model.eq_ignore_ascii_case(new_model))
                .unwrap()
                .enabled
        );
    }
    drop(db);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
pub(super) fn raw_disabled_unknown_model_stays_off_when_first_official_protocol_arrives() {
    let dir = temp_data_dir("raw-disabled-unknown-model");
    let db = open_with_host_cipher(dir.clone()).unwrap();
    let scope = ContractScope::provider(COMMAND_CODE_PROVIDER_ID);
    let model = "vendor/awaiting-official-evidence";
    let now = Utc::now();
    db.set_contract_catalog(
        &scope,
        &[model.into()],
        Some(now),
        CATALOG_SOURCE_COMMAND_CODE_MODELS,
        COMMAND_CODE_GOAT_BASE_URL,
        now,
    )
    .unwrap();
    db.set_model_protocol_overrides(
        &scope,
        &[(
            model.into(),
            UpstreamProtocolKind::ChatCompletions,
            ProtocolOverrideState::ForceOff,
        )],
        now,
    )
    .unwrap();
    assert!(
        !effective_from_db(&db)
            .scope(&scope)
            .unwrap()
            .model(model)
            .unwrap()
            .has_enabled_protocol()
    );
    db.refresh_contract_catalog_preserving_settings(
        &scope,
        &[model.into()],
        now,
        CATALOG_SOURCE_COMMAND_CODE_MODELS,
        COMMAND_CODE_GOAT_BASE_URL,
    )
    .unwrap();
    db.apply_official_protocol_baseline(
        &scope,
        &[model.into()],
        &crate::official_protocols::OfficialProtocolBaseline::mapped([(
            model,
            UpstreamProtocolKind::Responses,
        )]),
        now,
    )
    .unwrap();
    assert!(
        !effective_from_db(&db)
            .scope(&scope)
            .unwrap()
            .model(model)
            .unwrap()
            .has_enabled_protocol()
    );
    let id = ocg_domain::destination::destination_id_for_builtin(COMMAND_CODE_PROVIDER_ID);
    assert!(!destination_store::load_destination_catalog(&db.conn, &id).unwrap()[0].enabled);
    drop(db);
    fs::remove_dir_all(dir).unwrap();
}
