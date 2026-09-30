//! Behavioral tests split from the root db test module.

use super::tests::*;
use super::*;
use ocg_domain::dynamic::DynamicAuthKind;
use std::collections::HashSet;
use std::fs;

#[test]
pub(super) fn import_node_state_moves_linked_models_onto_parent_and_keeps_scopes() {
    use crate::platform::{PortablePlatformAccount, PortablePlatformLink};
    use ocg_domain::credential::ModelScope;

    let source_dir = temp_data_dir("import-models-source");
    let source = Database::open(source_dir.clone()).unwrap();
    seed_linked_platform_keys(
        &source,
        "parent-import",
        &[("key-a", "model-a", "up-a"), ("key-b", "model-b", "up-b")],
    );
    assert_eq!(
        stored_model_scope(&source, "key-a"),
        ModelScope::Only {
            models: vec!["model-a".into()]
        }
    );
    let parents = source
        .list_platform_accounts()
        .unwrap()
        .into_iter()
        .map(|parent| PortablePlatformAccount {
            id: parent.id,
            kind: parent.kind,
            name: parent.name,
            base_url: parent.base_url,
        })
        .collect::<Vec<_>>();
    let links = source
        .list_platform_links()
        .unwrap()
        .into_iter()
        .map(|link| PortablePlatformLink {
            account_id: link.account_id,
            platform_account_id: link.platform_account_id,
            group: {
                let mut group = link.group;
                group.verified = false;
                group.subscription_type = None;
                group
            },
        })
        .collect::<Vec<_>>();
    let mut record = node_import_record(
        &source,
        vec![
            custom_platform_import_record("key-a", "model-a", "up-a"),
            custom_platform_import_record("key-b", "model-b", "up-b"),
        ],
        parents,
        links,
    );
    record.identity_snapshot = Some(identity_snapshot_forcing_all(&source, &["key-a", "key-b"]));
    drop(source);
    fs::remove_dir_all(source_dir).unwrap();

    let dest_dir = temp_data_dir("import-models-dest");
    let dest = Database::open(dest_dir.clone()).unwrap();
    dest.import_node_state(&record, |_| -> Result<()> { Ok(()) })
        .unwrap();
    drop(dest);

    let dest = Database::open(dest_dir.clone()).unwrap();
    assert_restored_platform_catalog_and_scopes(&dest, "parent-import");
    dest.import_node_state(&record, |_| -> Result<()> { Ok(()) })
        .unwrap();
    assert_restored_platform_catalog_and_scopes(&dest, "parent-import");
    drop(dest);

    let dest = Database::open(dest_dir.clone()).unwrap();
    assert_restored_platform_catalog_and_scopes(&dest, "parent-import");
    drop(dest);
    fs::remove_dir_all(dest_dir).unwrap();
}

#[test]
pub(super) fn import_v7_platform_catalog_merges_target_models_and_preserves_exact_scopes() {
    use crate::platform::{PortablePlatformAccount, PortablePlatformLink};
    use ocg_domain::credential::ModelScope;
    use ocg_domain::destination::CatalogModel;

    let source_dir = temp_data_dir("import-v7-platform-source");
    let source = Database::open(source_dir.clone()).unwrap();
    seed_linked_platform_keys(
        &source,
        "parent-import",
        &[("key-a", "model-a", "up-a"), ("key-b", "model-b", "up-b")],
    );
    let parents = source
        .list_platform_accounts()
        .unwrap()
        .into_iter()
        .map(|parent| PortablePlatformAccount {
            id: parent.id,
            kind: parent.kind,
            name: parent.name,
            base_url: parent.base_url,
        })
        .collect::<Vec<_>>();
    let links = source
        .list_platform_links()
        .unwrap()
        .into_iter()
        .map(|link| PortablePlatformLink {
            account_id: link.account_id,
            platform_account_id: link.platform_account_id,
            group: link.group,
        })
        .collect::<Vec<_>>();
    let mut accounts = vec![
        custom_platform_import_record("key-a", "model-a", "up-a"),
        custom_platform_import_record("key-b", "model-b", "up-b"),
    ];
    for account in &mut accounts {
        account.custom_config = None;
        account.capabilities.clear();
    }
    let mut record = node_import_record(&source, accounts, parents, links);
    let mut identity = identity_snapshot_forcing_all(&source, &["key-a", "key-b"]);
    for row in &mut identity.accounts {
        row.binding_model_scope = if row.account_id == "key-a" {
            ModelScope::Only {
                models: vec!["model-a".into()],
            }
        } else {
            ModelScope::Only { models: Vec::new() }
        };
    }
    record.identity_snapshot = Some(identity);
    record.platform_catalogs.insert(
        "parent-import".into(),
        [("model-a", "up-a"), ("model-b", "up-b")]
            .into_iter()
            .map(|(public_model, upstream_model)| CatalogModel {
                public_model: public_model.into(),
                upstream_model: upstream_model.into(),
                protocols: vec![UpstreamProtocolKind::ChatCompletions],
                preferred: Some(UpstreamProtocolKind::ChatCompletions),
                enabled: true,
                upstream_override: None,
            })
            .collect(),
    );
    drop(source);
    fs::remove_dir_all(source_dir).unwrap();

    let dest_dir = temp_data_dir("import-v7-platform-dest");
    let dest = Database::open(dest_dir.clone()).unwrap();
    seed_linked_platform_keys(
        &dest,
        "parent-import",
        &[("key-target", "model-target", "up-target")],
    );
    dest.import_node_state(&record, |_| -> Result<()> { Ok(()) })
        .unwrap();

    let mut catalog = parent_catalog_pairs(&dest, "parent-import");
    catalog.sort();
    assert_eq!(
        catalog,
        vec![
            ("model-a".into(), "up-a".into()),
            ("model-b".into(), "up-b".into()),
            ("model-target".into(), "up-target".into()),
        ]
    );
    assert_eq!(
        stored_model_scope(&dest, "key-a"),
        ModelScope::Only {
            models: vec!["model-a".into()]
        }
    );
    assert_eq!(
        stored_model_scope(&dest, "key-b"),
        ModelScope::Only { models: Vec::new() }
    );
    assert_eq!(
        stored_model_scope(&dest, "key-target"),
        ModelScope::Only {
            models: vec!["model-target".into()]
        }
    );
    assert_eq!(
        capability_pairs(&dest, "key-a"),
        vec![("model-a".into(), "up-a".into())]
    );
    assert!(capability_pairs(&dest, "key-b").is_empty());
    assert_eq!(
        capability_pairs(&dest, "key-target"),
        vec![("model-target".into(), "up-target".into())]
    );

    let mut conflicting = record.clone();
    conflicting
        .platform_catalogs
        .get_mut("parent-import")
        .unwrap()[0]
        .upstream_model = "different-upstream".into();
    let error = dest
        .import_node_state(&conflicting, |_| -> Result<()> { Ok(()) })
        .expect_err("a conflicting public-to-upstream map must roll back");
    assert!(error.to_string().contains("conflicting upstream mappings"));
    let mut after_conflict = parent_catalog_pairs(&dest, "parent-import");
    after_conflict.sort();
    assert_eq!(after_conflict, catalog);

    drop(dest);
    fs::remove_dir_all(dest_dir).unwrap();
}

#[test]
pub(super) fn import_v8_custom_stable_id_collision_rolls_back_whole_node() {
    let dir = temp_data_dir("import-custom-id-collision");
    let db = open_with_host_cipher(dir.clone()).unwrap();
    let now = Utc::now();
    db.create_dynamic_provider_definition(&DynamicProviderRuntime {
        preset_id: None,
        id: "collision-dynamic".into(),
        name: "Collision".into(),
        endpoint_url: "https://dynamic.example/v1".into(),
        upstream_protocol: UpstreamProtocolKind::ChatCompletions,
        auth_kind: DynamicAuthKind::Bearer,
        mappings: Vec::new(),
        created_at: now,
        updated_at: now,
        origin: ProviderOrigin::Custom,
        offering: "api".into(),
    })
    .unwrap();
    let custom_legacy_id = "00000000-0000-4000-8000-00000000c011";
    let custom_id = ocg_domain::destination::destination_id_for_custom_account(custom_legacy_id);
    let dynamic_id = ocg_domain::destination::destination_id_for_dynamic("collision-dynamic");
    db.conn
        .execute(
            "UPDATE destinations SET id = ?2 WHERE id = ?1",
            params![dynamic_id, custom_id],
        )
        .unwrap();
    let before_primary = db.primary_access_key_value().unwrap();
    let mut record = node_import_record(&db, Vec::new(), Vec::new(), Vec::new());
    record.custom_destinations.push(ImportedCustomDestination {
        id: custom_id.clone(),
        legacy_id: custom_legacy_id.into(),
        name: "Imported Custom".into(),
        endpoint_url: "https://custom.example/v1/chat/completions".into(),
        protocol: UpstreamProtocolKind::ChatCompletions,
        auth_scheme: AuthScheme::Bearer,
        models: vec![ocg_domain::dynamic::DynamicModelMapping {
            public_model: "custom-model".into(),
            upstream_model: "vendor/custom-model".into(),
            upstream_override: None,
        }],
        enabled: true,
    });
    let error = db.import_node_state(&record, |_| Ok(())).unwrap_err();
    assert!(error.to_string().contains("collides"), "{error:#}");
    assert_eq!(db.primary_access_key_value().unwrap(), before_primary);
    let row: (String, String) = db
        .conn
        .query_row(
            "SELECT legacy_kind, legacy_id FROM destinations WHERE id = ?1",
            [&custom_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(row, ("dynamic".into(), "collision-dynamic".into()));

    drop(db);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
pub(super) fn import_same_platform_id_different_site_writes_nothing() {
    use crate::platform::{PlatformKind, PortablePlatformAccount};
    let dir = temp_data_dir("import-platform-site-conflict");
    let db = Database::open(dir.clone()).unwrap();
    db.create_platform_account(
        "00000000-0000-4000-8000-0000000000aa",
        PlatformKind::NewApi,
        "Destination",
        "https://dest.example",
        None,
    )
    .unwrap();
    let before_accounts = db
        .list_accounts()
        .unwrap()
        .into_iter()
        .map(|account| account.id)
        .collect::<Vec<_>>();
    let before_primary = db.primary_access_key_value().unwrap();
    let record = node_import_record(
        &db,
        vec![go_import_record("imported-go")],
        vec![PortablePlatformAccount {
            id: "00000000-0000-4000-8000-0000000000aa".into(),
            kind: PlatformKind::NewApi,
            name: "Source".into(),
            base_url: "https://other.example".into(),
        }],
        Vec::new(),
    );
    let error = db
        .import_node_state(&record, |_| -> Result<()> {
            Err(anyhow::anyhow!("should not build a snapshot"))
        })
        .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("imported platform identity conflicts with immutable origin"),
        "{error}"
    );
    assert_eq!(
        db.list_accounts()
            .unwrap()
            .into_iter()
            .map(|account| account.id)
            .collect::<Vec<_>>(),
        before_accounts
    );
    assert!(db.get_account("imported-go").unwrap().is_none());
    assert_eq!(
        db.platform_account("00000000-0000-4000-8000-0000000000aa")
            .unwrap()
            .unwrap()
            .base_url,
        "https://dest.example"
    );
    assert_eq!(db.primary_access_key_value().unwrap(), before_primary);
    drop(db);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
pub(super) fn import_node_state_does_not_commit_when_runtime_snapshot_fails() {
    let dir = temp_data_dir("import-snapshot-fail");
    let db = Database::open(dir.clone()).unwrap();
    let before_accounts = db
        .list_accounts()
        .unwrap()
        .into_iter()
        .map(|account| account.id)
        .collect::<Vec<_>>();
    let before_primary = db.primary_access_key_value().unwrap();
    let record = node_import_record(
        &db,
        vec![go_import_record("snapshot-go")],
        Vec::new(),
        Vec::new(),
    );
    let error = db
        .import_node_state(&record, |_| -> Result<()> {
            Err(anyhow::anyhow!("forced snapshot failure"))
        })
        .unwrap_err();
    assert!(
        error.to_string().contains("forced snapshot failure"),
        "{error}"
    );
    assert_eq!(
        db.list_accounts()
            .unwrap()
            .into_iter()
            .map(|account| account.id)
            .collect::<Vec<_>>(),
        before_accounts
    );
    assert!(db.get_account("snapshot-go").unwrap().is_none());
    assert_eq!(db.primary_access_key_value().unwrap(), before_primary);
    let satellites: i64 = db
        .conn
        .query_row(
            "SELECT COUNT(*) FROM credentials WHERE legacy_account_id = 'snapshot-go'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(satellites, 0);
    drop(db);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
pub(super) fn import_v6_identity_conflict_writes_nothing() {
    use crate::db::identity::{
        IdentityImportSnapshot, ImportedAccountIdentity, ImportedIdentity, ImportedQuotaPool,
    };
    use ocg_domain::credential::{
        ModelScope, credential_id_for_legacy_account, identity_id_for_legacy_account,
        quota_pool_id_for_identity,
    };

    let dir = temp_data_dir("import-v6-identity-conflict");
    let db = Database::open(dir.clone()).unwrap();
    db.import_accounts_with_contracts(&[go_import_record("dest-go")])
        .unwrap();
    let dest_identity = db
        .list_identity_model()
        .unwrap()
        .accounts
        .into_iter()
        .find(|row| row.account.id == "dest-go")
        .unwrap()
        .identity_id;
    let before_accounts = db
        .list_accounts()
        .unwrap()
        .into_iter()
        .map(|account| account.id)
        .collect::<Vec<_>>();
    let imported_id = "00000000-0000-4000-8000-0000000000b1";
    let credential_id = credential_id_for_legacy_account(imported_id).to_string();
    let binding_id = "00000000-0000-4000-8000-0000000000b2".to_string();
    let mut record = node_import_record(
        &db,
        vec![go_import_record(imported_id)],
        Vec::new(),
        Vec::new(),
    );
    record.identity_snapshot = Some(IdentityImportSnapshot {
        identities: vec![ImportedIdentity {
            id: dest_identity.clone(),
            label: "Shared".into(),
            identity_confidence: "opaque".into(),
            authority_site: None,
            authority_subject: None,
            enabled: true,
            notes: None,
        }],
        accounts: vec![ImportedAccountIdentity {
            account_id: imported_id.into(),
            identity_id: dest_identity.clone(),
            credential_id: credential_id.clone(),
            credential_version: 1,
            auth_state_version: 1,
            binding_id: binding_id.clone(),
            binding_enabled: true,
            binding_model_scope: ModelScope::All,
            allowed_endpoint_ids: Vec::new(),
            allowed_origins: Vec::new(),
        }],
        quota_pools: vec![ImportedQuotaPool {
            id: quota_pool_id_for_identity(&dest_identity).to_string(),
            subject_kind: "credential".into(),
            subject_ref: dest_identity.clone(),
            relation_confidence: "unknown".into(),
            policy_mode: "authoritative_limit".into(),
            member_account_ids: vec![imported_id.into()],
        }],
    });
    let error = db
        .import_node_state(&record, |_| -> Result<()> { Ok(()) })
        .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("already attached to a destination-only account"),
        "{error}"
    );
    assert_eq!(
        db.list_accounts()
            .unwrap()
            .into_iter()
            .map(|account| account.id)
            .collect::<Vec<_>>(),
        before_accounts
    );
    assert!(db.get_account(imported_id).unwrap().is_none());
    assert_eq!(
        identity_id_for_legacy_account("dest-go").to_string(),
        dest_identity
    );
    drop(db);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
pub(super) fn imported_dynamic_auth_change_rejects_destination_only_accounts() {
    let dir = temp_data_dir("dyn-import-auth-conflict");
    let db = open_with_host_cipher(dir.clone()).unwrap();
    let now = Utc::now();
    let provider_id = uuid::Uuid::new_v4().to_string();
    let mut runtime = crate::dynamic::DynamicProviderRuntime {
        preset_id: None,
        id: provider_id.clone(),
        name: "Auth conflict".into(),
        endpoint_url: "http://127.0.0.1:9".into(),
        upstream_protocol: crate::provider::UpstreamProtocolKind::ChatCompletions,
        auth_kind: ocg_domain::dynamic::DynamicAuthKind::None,
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
    let mut destination_only = account("dyn-destination-only");
    destination_only.provider_id = provider_id.clone();
    destination_only.credential_kind = CredentialKind::None;
    destination_only.key_cipher.clear();
    db.create_dynamic_provider(&runtime, &destination_only)
        .unwrap();

    runtime.auth_kind = ocg_domain::dynamic::DynamicAuthKind::Bearer;
    let error = upsert_imported_dynamic_provider_on(&db.conn, &runtime, &HashSet::new(), false)
        .expect_err("destination-only account must block an auth-boundary change");
    assert!(
        error.to_string().contains("destination-only accounts"),
        "{error}"
    );
    assert_eq!(
        db.get_dynamic_provider(&provider_id)
            .unwrap()
            .unwrap()
            .auth_kind,
        ocg_domain::dynamic::DynamicAuthKind::None
    );

    drop(db);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
pub(super) fn imported_http_controls_survive_fresh_merge_and_failed_preflight() {
    use ocg_domain::destination::{CatalogModel, HttpProtocolRoute, ModelResolution, Protocol};
    let dir = temp_data_dir("http-control-import");
    let db = open_with_host_cipher(dir.clone()).unwrap();
    let legacy_id = "00000000-0000-4000-8000-00000000c099";
    let id = ocg_domain::destination::destination_id_for_custom_account(legacy_id);
    let model = CatalogModel {
        public_model: "public-model".into(),
        upstream_model: "upstream-model".into(),
        protocols: vec![Protocol::Messages],
        preferred: Some(Protocol::Messages),
        enabled: false,
        upstream_override: None,
    };
    let mut record = node_import_record(&db, Vec::new(), Vec::new(), Vec::new());
    record.custom_destinations.push(ImportedCustomDestination {
        id: id.clone(),
        legacy_id: legacy_id.into(),
        name: "No Key".into(),
        endpoint_url: "https://custom.example/v1/chat/completions".into(),
        protocol: UpstreamProtocolKind::ChatCompletions,
        auth_scheme: AuthScheme::None,
        models: vec![ocg_domain::dynamic::DynamicModelMapping {
            public_model: model.public_model.clone(),
            upstream_model: model.upstream_model.clone(),
            upstream_override: None,
        }],
        enabled: false,
    });
    db.import_node_state(&record, |_| Ok(())).unwrap();
    let mut destination = crate::destination_projection::load_persisted(&db)
        .unwrap()
        .destinations
        .into_iter()
        .find(|d| d.id == id)
        .unwrap();
    destination.enabled = false;
    destination.protocols = vec![Protocol::ChatCompletions, Protocol::Messages];
    destination.protocol_routes = vec![
        HttpProtocolRoute {
            protocol: Protocol::ChatCompletions,
            endpoint_url: "https://custom.example/v1/chat/completions".into(),
            auth_scheme: AuthScheme::None,
        },
        HttpProtocolRoute {
            protocol: Protocol::Messages,
            endpoint_url: "https://custom.example/v1/messages".into(),
            auth_scheme: AuthScheme::None,
        },
    ];
    destination.catalog = vec![model.clone()];
    destination.model_resolution = ModelResolution::PublicOnly;
    let expected_routes = destination.protocol_routes.clone();
    record.destination_controls = vec![destination];
    db.conn
        .execute(
            "DELETE FROM destination_models WHERE destination_id = ?1",
            [&id],
        )
        .unwrap();
    db.conn
        .execute("DELETE FROM destinations WHERE id = ?1", [&id])
        .unwrap();
    db.import_node_state(&record, |db| {
        let d = crate::destination_projection::load_persisted(db)?
            .destinations
            .into_iter()
            .find(|d| d.id == id)
            .unwrap();
        assert!(!d.enabled);
        assert_eq!(d.auth_scheme, AuthScheme::None);
        assert_eq!(
            d.protocols,
            vec![Protocol::ChatCompletions, Protocol::Messages]
        );
        assert_eq!(
            d.protocol_routes,
            vec![
                HttpProtocolRoute {
                    protocol: Protocol::ChatCompletions,
                    endpoint_url: "https://custom.example/v1/chat/completions".into(),
                    auth_scheme: AuthScheme::None,
                },
                HttpProtocolRoute {
                    protocol: Protocol::Messages,
                    endpoint_url: "https://custom.example/v1/messages".into(),
                    auth_scheme: AuthScheme::None,
                },
            ]
        );
        assert_eq!(d.catalog, vec![model.clone()]);
        Ok(())
    })
    .unwrap();
    let mut target_only = model.clone();
    target_only.public_model = "target-only".into();
    target_only.upstream_model = "target-upstream".into();
    target_only.enabled = true;
    target_only.protocols = vec![UpstreamProtocolKind::ChatCompletions];
    let mut target_model = model.clone();
    target_model.enabled = true;
    target_model.protocols = vec![UpstreamProtocolKind::ChatCompletions];
    target_model.preferred = Some(UpstreamProtocolKind::ChatCompletions);
    destination_store::replace_destination_catalog(
        &db.conn,
        &id,
        &[target_model, target_only.clone()],
    )
    .unwrap();
    db.conn
        .execute("UPDATE destinations SET enabled = 1 WHERE id = ?1", [&id])
        .unwrap();
    let before = destination_store::load_destination_catalog(&db.conn, &id).unwrap();
    let before_destination = crate::destination_projection::load_persisted(&db)
        .unwrap()
        .destinations
        .into_iter()
        .find(|d| d.id == id)
        .unwrap();
    assert!(
        db.import_node_state(&record, |_| -> Result<()> {
            anyhow::bail!("preflight refused")
        })
        .is_err()
    );
    assert_eq!(
        destination_store::load_destination_catalog(&db.conn, &id).unwrap(),
        before
    );
    assert_eq!(
        crate::destination_projection::load_persisted(&db)
            .unwrap()
            .destinations
            .into_iter()
            .find(|d| d.id == id)
            .unwrap(),
        before_destination
    );
    db.import_node_state(&record, |_| Ok(())).unwrap();
    assert_eq!(
        destination_store::load_destination_catalog(&db.conn, &id).unwrap(),
        vec![model.clone(), target_only.clone()]
    );
    let restored = crate::destination_projection::load_persisted(&db)
        .unwrap()
        .destinations
        .into_iter()
        .find(|d| d.id == id)
        .unwrap();
    assert_eq!(
        restored.protocols,
        vec![Protocol::ChatCompletions, Protocol::Messages]
    );
    assert_eq!(restored.protocol_routes, expected_routes);
    assert_eq!(restored.catalog, vec![model, target_only]);
    assert_eq!(
        db.conn
            .query_row(
                "SELECT enabled FROM destinations WHERE id = ?1",
                [&id],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
        0
    );
    drop(db);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
pub(super) fn imported_http_routes_remap_old_grants_by_operation_and_url_without_new_routes() {
    use ocg_domain::connection::{
        EndpointOperation, LegacyConnectionKind, connection_id_for_legacy,
    };
    use ocg_domain::credential::{RouteSpec, remap_route_grant_ids};
    use ocg_domain::destination::{HttpProtocolRoute, Protocol};

    let dir = temp_data_dir("import-http-grant-remap");
    let db = open_with_host_cipher(dir.clone()).unwrap();
    let provider_id = uuid::Uuid::new_v4().to_string();
    let target_chat = "https://target.example/v1/chat/completions";
    let target_messages = "https://target.example/v1/messages";
    let target_responses = "https://target-other.example/v1/responses";
    let source_responses = "https://source.example/v1/responses";
    let mut runtime = onboarding_runtime(&provider_id, "Import Grant Remap");
    runtime.endpoint_url = target_chat.into();
    let target_routes = vec![
        HttpProtocolRoute {
            protocol: Protocol::ChatCompletions,
            endpoint_url: target_chat.into(),
            auth_scheme: AuthScheme::Bearer,
        },
        HttpProtocolRoute {
            protocol: Protocol::Messages,
            endpoint_url: target_messages.into(),
            auth_scheme: AuthScheme::Bearer,
        },
        HttpProtocolRoute {
            protocol: Protocol::Responses,
            endpoint_url: target_responses.into(),
            auth_scheme: AuthScheme::Bearer,
        },
    ];
    let mut key = account("import-grant-key");
    key.provider_id = provider_id.clone();
    key.key_cipher = fixture_account_key_cipher();
    db.commit_onboarding_new_with_routes(
        &runtime,
        Some(&key),
        false,
        &onboarding_operation(
            &uuid::Uuid::new_v4().to_string(),
            "import-grant-target",
            "{}",
        ),
        Some(&target_routes),
    )
    .unwrap();
    let mut source_key = account("import-source-key");
    source_key.provider_id = provider_id.clone();
    source_key.key_cipher = fixture_account_key_cipher();
    db.create_account(&source_key).unwrap();
    let destination_id = ocg_domain::destination::destination_id_for_dynamic(&provider_id);
    let before = crate::destination_projection::load_persisted(&db).unwrap();
    let before_destination = before
        .destinations
        .iter()
        .find(|destination| destination.id == destination_id)
        .unwrap()
        .clone();
    let before_key = before
        .credentials
        .iter()
        .find(|credential| credential.legacy_account_id == key.id)
        .unwrap()
        .clone();
    assert_eq!(before_key.grants.allowed_endpoint_ids.len(), 2);

    let source_routes = vec![
        HttpProtocolRoute {
            protocol: Protocol::Responses,
            endpoint_url: source_responses.into(),
            auth_scheme: AuthScheme::Bearer,
        },
        HttpProtocolRoute {
            protocol: Protocol::Messages,
            endpoint_url: target_messages.into(),
            auth_scheme: AuthScheme::Bearer,
        },
        HttpProtocolRoute {
            protocol: Protocol::ChatCompletions,
            endpoint_url: target_chat.into(),
            auth_scheme: AuthScheme::Bearer,
        },
    ];
    let mut source_destination = before_destination.clone();
    source_destination.protocol_routes = source_routes.clone();
    source_destination.protocols = source_routes.iter().map(|route| route.protocol).collect();
    source_destination.base_url = Some(source_responses.into());
    source_destination.catalog[0].protocols = vec![Protocol::Messages];
    source_destination.catalog[0].preferred = Some(Protocol::Messages);
    source_destination.catalog[0].enabled = false;
    let mut source_import = source_key.clone();
    source_import.key_cipher = fixture_account_key_cipher();
    let mut record = node_import_record(
        &db,
        vec![AccountImportRecord {
            account: source_import,
            custom_config: None,
            capabilities: Vec::new(),
            verification_status: ConnectionVerificationStatus::NotRequired,
            connection_verified_at: None,
            ollama_billing_tier: None,
        }],
        Vec::new(),
        Vec::new(),
    );
    record.destination_controls = vec![source_destination];
    let mut source_snapshot = identity_snapshot_forcing_all(&db, &[source_key.id.as_str()]);
    source_snapshot.accounts[0].allowed_endpoint_ids.clear();
    source_snapshot.accounts[0].allowed_origins.clear();
    record.identity_snapshot = Some(source_snapshot);

    let connection = connection_id_for_legacy(LegacyConnectionKind::DynamicProvider, &provider_id);
    let route_specs = |routes: &[HttpProtocolRoute]| {
        routes
            .iter()
            .map(|route| RouteSpec {
                operation: EndpointOperation::from(route.protocol),
                url: Some(route.endpoint_url.clone()),
            })
            .collect::<Vec<_>>()
    };
    let expected_target = remap_route_grant_ids(
        &connection,
        &route_specs(&target_routes),
        &route_specs(&source_routes),
        &before_key.grants.allowed_endpoint_ids,
    );
    db.import_node_state(&record, |_| Ok(())).unwrap();
    let after = crate::destination_projection::load_persisted(&db).unwrap();
    let after_destination = after
        .destinations
        .iter()
        .find(|destination| destination.id == destination_id)
        .unwrap();
    assert_eq!(after_destination.protocol_routes, source_routes);
    let after_key = after
        .credentials
        .iter()
        .find(|credential| credential.legacy_account_id == key.id)
        .unwrap();
    assert_eq!(after_key.grants.allowed_endpoint_ids, expected_target);
    assert_eq!(
        after_key.grants.allowed_origins,
        before_key.grants.allowed_origins
    );
    assert!(!after_key.grants.allowed_endpoint_ids.iter().any(|id| {
        id == &ocg_domain::connection::endpoint_id_for(
            &connection,
            EndpointOperation::ResponseCreate,
        )
        .to_string()
    }));
    assert!(
        !after_key
            .grants
            .allowed_origins
            .contains(&"https://source.example".into())
    );
    let source_after = after
        .credentials
        .iter()
        .find(|credential| credential.legacy_account_id == source_key.id)
        .unwrap();
    assert!(source_after.grants.allowed_endpoint_ids.is_empty());
    assert!(source_after.grants.allowed_origins.is_empty());
    drop(db);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
pub(super) fn imported_builtin_controls_replace_target_choices_for_old_and_new_payloads() {
    let dir = temp_data_dir("builtin-control-import");
    let db = open_with_host_cipher(dir.clone()).unwrap();
    let scope = ContractScope::provider(OPENCODE_PROVIDER_ID);
    let now = Utc::now();
    db.set_contract_catalog(
        &scope,
        &["gpt-5.6-luna".into()],
        Some(now),
        "test",
        "https://example.test/models",
        now,
    )
    .unwrap();
    let destination_id = ocg_domain::destination::destination_id_for_builtin(OPENCODE_PROVIDER_ID);
    let changes = [
        UpstreamProtocolKind::ChatCompletions,
        UpstreamProtocolKind::Responses,
        UpstreamProtocolKind::Messages,
    ]
    .map(|p| ("gpt-5.6-luna".into(), p, ProtocolOverrideState::ForceOff));
    db.set_model_protocol_overrides(&scope, &changes, now)
        .unwrap();
    let source = crate::destination_projection::load_persisted(&db)
        .unwrap()
        .destinations
        .into_iter()
        .find(|d| d.id == destination_id)
        .unwrap();
    assert!(!source.catalog[0].enabled);
    let mut record = node_import_record(&db, Vec::new(), Vec::new(), Vec::new());
    record.provider_contracts = db.load_persisted_contracts().unwrap();
    for canonical in [false, true] {
        db.set_model_protocol_overrides(
            &scope,
            &[(
                "gpt-5.6-luna".into(),
                UpstreamProtocolKind::ChatCompletions,
                ProtocolOverrideState::ForceOn,
            )],
            now,
        )
        .unwrap();
        assert!(
            destination_store::load_destination_catalog(&db.conn, &destination_id).unwrap()[0]
                .enabled
        );
        record.destination_controls = if canonical {
            vec![source.clone()]
        } else {
            vec![]
        };
        db.import_node_state(&record, |db| {
            let catalog = destination_store::load_destination_catalog(&db.conn, &destination_id)?;
            assert!(!catalog[0].enabled);
            assert!(catalog[0].protocols.is_empty());
            Ok(())
        })
        .unwrap();
    }
    let fresh_dir = temp_data_dir("builtin-control-import-fresh");
    let fresh = open_with_host_cipher(fresh_dir.clone()).unwrap();
    fresh.import_node_state(&record, |_| Ok(())).unwrap();
    assert_eq!(
        destination_store::load_destination_catalog(&fresh.conn, &destination_id).unwrap(),
        source.catalog
    );
    let empty_dir = temp_data_dir("builtin-control-import-empty");
    let empty = open_with_host_cipher(empty_dir.clone()).unwrap();
    record.provider_contracts = PersistedContracts::default();
    record.destination_controls[0].catalog.clear();
    empty.import_node_state(&record, |_| Ok(())).unwrap();
    assert!(destination_store::destination_exists(&empty.conn, &destination_id).unwrap());
    assert!(
        destination_store::load_destination_catalog(&empty.conn, &destination_id)
            .unwrap()
            .is_empty()
    );
    drop(fresh);
    drop(empty);
    fs::remove_dir_all(fresh_dir).unwrap();
    fs::remove_dir_all(empty_dir).unwrap();
    drop(db);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
pub(super) fn imported_builtin_alias_replaces_same_upstream_and_remains_editable() {
    let dir = temp_data_dir("builtin-alias-import");
    let db = open_with_host_cipher(dir.clone()).unwrap();
    let scope = ContractScope::provider("minimax");
    let now = Utc::now();
    db.set_contract_catalog(&scope, &["MiniMax-M2".into()], Some(now), "test", "", now)
        .unwrap();
    let id = ocg_domain::destination::destination_id_for_builtin("minimax");
    let mut source = crate::destination_projection::load_persisted(&db)
        .unwrap()
        .destinations
        .into_iter()
        .find(|d| d.id == id)
        .unwrap();
    source.catalog[0].public_model = "imported-minimax".into();
    source.catalog[0].protocols = vec![ocg_domain::destination::Protocol::Messages];
    source.catalog[0].preferred = Some(ocg_domain::destination::Protocol::Messages);
    source.catalog[0].enabled = true;
    let mut record = node_import_record(&db, Vec::new(), Vec::new(), Vec::new());
    record.provider_contracts = db.load_persisted_contracts().unwrap();
    record.destination_controls = vec![source.clone()];
    for _ in 0..2 {
        db.import_node_state(&record, |_| Ok(())).unwrap();
        assert_eq!(
            destination_store::load_destination_catalog(&db.conn, &id).unwrap(),
            source.catalog
        );
    }
    let mut edited = source.catalog[0].clone();
    edited.public_model = "edited-minimax".into();
    db.edit_contract_catalog_model(&scope, Some("MiniMax-M2"), edited.clone(), now)
        .unwrap();
    assert_eq!(
        destination_store::load_destination_catalog(&db.conn, &id).unwrap(),
        vec![edited]
    );
    drop(db);
    fs::remove_dir_all(dir).unwrap();
}
