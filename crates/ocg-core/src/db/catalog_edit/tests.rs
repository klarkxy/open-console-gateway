use super::*;

#[test]
fn editing_checks_complete_catalog_names_after_removing_the_old_row() {
    let dir = std::env::temp_dir().join(format!("ocg-catalog-edit-{}", uuid::Uuid::new_v4()));
    let db = Database::open(dir.clone()).unwrap();
    let scope = ContractScope::provider(MINIMAX_PROVIDER_ID);
    let now = Utc::now();
    db.set_contract_catalog(
        &scope,
        &["vendor/shared-leaf".into(), "shared_leaf".into()],
        Some(now),
        "test",
        "",
        now,
    )
    .unwrap();
    let edited = ocg_domain::destination::CatalogModel {
        public_model: "shared-leaf".into(),
        upstream_model: "different-target".into(),
        protocols: vec![],
        preferred: None,
        enabled: false,
        upstream_override: None,
    };
    assert!(
        db.edit_contract_catalog_model(&scope, Some("shared_leaf"), edited, now)
            .is_err()
    );
    let id = ocg_domain::destination::destination_id_for_builtin(MINIMAX_PROVIDER_ID);
    let rows = destination_store::load_destination_catalog(&db.conn, &id).unwrap();
    assert!(rows.iter().any(|row| row.upstream_model == "shared_leaf"));
    assert!(
        !rows
            .iter()
            .any(|row| row.upstream_model == "different-target")
    );
    drop(db);
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn renaming_an_ollama_tag_cannot_steal_another_tags_generated_name() {
    let dir = std::env::temp_dir().join(format!("ocg-catalog-tags-{}", uuid::Uuid::new_v4()));
    let db = Database::open(dir.clone()).unwrap();
    let scope = ContractScope::provider(OLLAMA_PROVIDER_ID);
    let now = Utc::now();
    db.set_contract_catalog(
        &scope,
        &["sample:a".into(), "sample:b".into()],
        Some(now),
        "test",
        "",
        now,
    )
    .unwrap();
    let id = ocg_domain::destination::destination_id_for_builtin(OLLAMA_PROVIDER_ID);
    let before = destination_store::load_destination_catalog(&db.conn, &id).unwrap();
    let mut edited = before
        .iter()
        .find(|row| row.upstream_model == "sample:a")
        .unwrap()
        .clone();
    edited.public_model = "sample-b".into();
    assert!(
        db.edit_contract_catalog_model(&scope, Some("sample:a"), edited, now)
            .is_err()
    );
    assert_eq!(
        destination_store::load_destination_catalog(&db.conn, &id).unwrap(),
        before
    );
    drop(db);
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn editing_ollama_enablement_checks_the_proposed_pin_selection() {
    let dir = std::env::temp_dir().join(format!("ocg-catalog-pin-edit-{}", uuid::Uuid::new_v4()));
    let db = Database::open(dir.clone()).unwrap();
    let scope = ContractScope::provider(OLLAMA_PROVIDER_ID);
    let now = Utc::now();
    db.set_contract_catalog(
        &scope,
        &["sample:a".into(), "sample:b".into()],
        Some(now),
        "test",
        "",
        now,
    )
    .unwrap();
    for id in ["sample:a", "sample:b"] {
        db.set_model_protocol_overrides(
            &scope,
            &[(
                id.to_string(),
                UpstreamProtocolKind::ChatCompletions,
                ProtocolOverrideState::ForceOn,
            )],
            now,
        )
        .unwrap();
    }
    let destination = ocg_domain::destination::destination_id_for_builtin(OLLAMA_PROVIDER_ID);
    let before = destination_store::load_destination_catalog(&db.conn, &destination).unwrap();
    let mut edited = before
        .iter()
        .find(|row| row.upstream_model == "sample:a")
        .unwrap()
        .clone();
    edited.public_model = "sample".into();
    edited.enabled = false;
    assert!(
        db.edit_contract_catalog_model(&scope, Some("sample:a"), edited, now)
            .is_err()
    );
    assert_eq!(
        destination_store::load_destination_catalog(&db.conn, &destination).unwrap(),
        before
    );
    drop(db);
    std::fs::remove_dir_all(dir).unwrap();
}
