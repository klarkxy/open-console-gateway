use super::*;
use ocg_domain::destination::CatalogModel;

fn data_dir(label: &str) -> PathBuf {
    std::env::temp_dir().join(format!("ocg-v67-{label}-{}", uuid::Uuid::new_v4()))
}

fn model(
    public: &str,
    upstream: &str,
    protocols: &[UpstreamProtocolKind],
    enabled: bool,
) -> CatalogModel {
    CatalogModel {
        public_model: public.into(),
        upstream_model: upstream.into(),
        protocols: protocols.to_vec(),
        preferred: protocols.first().copied(),
        enabled,
        upstream_override: None,
    }
}

fn save_catalog(db: &Database, provider: &str, models: &[CatalogModel]) {
    let now = Utc::now();
    db.set_contract_catalog(
        &ContractScope::provider(provider),
        &models
            .iter()
            .map(|m| m.upstream_model.clone())
            .collect::<Vec<_>>(),
        Some(now),
        "fixture",
        "https://example.test/models",
        now,
    )
    .unwrap();
    let destination = destination_store::ensure_builtin_destination(&db.conn, provider).unwrap();
    destination_store::replace_destination_catalog(&db.conn, &destination, models).unwrap();
}

fn rewind(db: &Database) {
    db.conn
        .execute_batch("DELETE FROM schema_version; INSERT INTO schema_version VALUES (66);")
        .unwrap();
}

fn names(db: &Database, provider: &str) -> Vec<(String, String)> {
    destination_store::load_destination_catalog(&db.conn, &destination_id_for_builtin(provider))
        .unwrap()
        .into_iter()
        .map(|m| (m.public_model, m.upstream_model))
        .collect()
}

fn backup_count(dir: &Path) -> usize {
    std::fs::read_dir(dir)
        .unwrap()
        .flatten()
        .filter(|entry| {
            let name = entry.file_name().to_string_lossy().into_owned();
            name.starts_with(PRE_V67_BACKUP_FILE_PREFIX) && name.ends_with(".bak")
        })
        .count()
}

#[test]
fn legacy_defaults_are_preserved_once_with_verified_backup_and_refresh() {
    let dir = data_dir("names");
    let db = Database::open(dir.clone()).unwrap();
    save_catalog(
        &db,
        KIMI_PROVIDER_ID,
        &[
            model("k3", "k3", &[UpstreamProtocolKind::ChatCompletions], true),
            model(
                "k3-256k",
                "k3-256k",
                &[UpstreamProtocolKind::Messages],
                true,
            ),
            model("operator-name", "renamed", &[], false),
        ],
    );
    save_catalog(
        &db,
        COMMAND_CODE_PROVIDER_ID,
        &[
            model(
                "nvidia/nemotron-3-ultra-550b-a55b",
                "nvidia/nemotron-3-ultra-550b-a55b",
                &[UpstreamProtocolKind::ChatCompletions],
                true,
            ),
            model("unknown", "fixture/unknown", &[], false),
        ],
    );
    rewind(&db);
    drop(db);
    let db = Database::open(dir.clone()).unwrap();
    assert_eq!(schema_version_on(&db.conn).unwrap(), 67);
    assert_eq!(
        names(&db, KIMI_PROVIDER_ID),
        vec![
            ("kimi-k3".into(), "k3".into()),
            ("kimi-k3-256k".into(), "k3-256k".into()),
            ("operator-name".into(), "renamed".into()),
        ]
    );
    assert_eq!(
        names(&db, COMMAND_CODE_PROVIDER_ID)[0].0,
        "nemotron-3-ultra"
    );
    let contracts = crate::provider_contracts::build_effective_contracts(
        &Default::default(),
        &[],
        db.load_persisted_contracts().unwrap(),
    );
    let goat = &contracts.providers[COMMAND_CODE_PROVIDER_ID];
    assert!(
        goat.model("nvidia/nemotron-3-ultra-550b-a55b")
            .unwrap()
            .has_enabled_protocol()
    );
    assert!(
        !goat
            .model("fixture/unknown")
            .unwrap()
            .has_enabled_protocol()
    );
    let before = names(&db, KIMI_PROVIDER_ID);
    db.refresh_contract_catalog_preserving_settings(
        &ContractScope::provider(KIMI_PROVIDER_ID),
        &["k3".into(), "k3-256k".into(), "renamed".into()],
        Utc::now(),
        "fixture",
        "https://example.test/models",
    )
    .unwrap();
    assert_eq!(names(&db, KIMI_PROVIDER_ID), before);
    assert_eq!(backup_count(&dir), 1);
    let backup = std::fs::read_dir(&dir)
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .find(|p| {
            p.file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with(PRE_V67_BACKUP_FILE_PREFIX)
                && p.extension().is_some_and(|e| e == "bak")
        })
        .unwrap();
    let bytes = std::fs::read(&backup).unwrap();
    let digest = format!("{:x}", Sha256::digest(bytes));
    let sidecar = std::fs::read_to_string(format!("{}.sha256", backup.display())).unwrap();
    assert_eq!(sidecar.split_whitespace().next().unwrap(), digest);
    let snapshot = Connection::open(&backup).unwrap();
    assert_eq!(schema_version_on(&snapshot).unwrap(), 66);
    drop(snapshot);
    drop(db);
    let db = Database::open(dir.clone()).unwrap();
    assert_eq!(names(&db, KIMI_PROVIDER_ID), before);
    assert_eq!(backup_count(&dir), 1);
    // Deleting and reintroducing a historical ID never replays compatibility.
    db.remove_contract_catalog_models(
        &ContractScope::provider(KIMI_PROVIDER_ID),
        &["k3".into()],
        Utc::now(),
    )
    .unwrap();
    db.refresh_contract_catalog_preserving_settings(
        &ContractScope::provider(KIMI_PROVIDER_ID),
        &["k3".into(), "k3-256k".into(), "renamed".into()],
        Utc::now(),
        "fixture",
        "https://example.test/models",
    )
    .unwrap();
    assert_eq!(
        names(&db, KIMI_PROVIDER_ID)
            .iter()
            .find(|(_, raw)| raw == "k3")
            .unwrap()
            .0,
        "k3"
    );
    drop(db);
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn migration_preserves_explicit_names_same_provider_collisions_and_disabled_protocol_facts() {
    let dir = data_dir("collisions");
    let db = Database::open(dir.clone()).unwrap();
    save_catalog(
        &db,
        KIMI_PROVIDER_ID,
        &[
            model(
                "operator-k3",
                "k3",
                &[UpstreamProtocolKind::ChatCompletions],
                true,
            ),
            model(
                "k3-256k",
                "k3-256k",
                &[UpstreamProtocolKind::ChatCompletions],
                true,
            ),
            model("kimi-k3-256k", "fixture/other", &[], false),
        ],
    );
    save_catalog(
        &db,
        COMMAND_CODE_PROVIDER_ID,
        &[
            model(
                "nvidia/nemotron-3-ultra-550b-a55b",
                "nvidia/nemotron-3-ultra-550b-a55b",
                &[UpstreamProtocolKind::ChatCompletions],
                false,
            ),
            model("nemotron-3-ultra", "fixture/conflict", &[], false),
            model(
                "responses",
                "fixture/saved-responses",
                &[UpstreamProtocolKind::Responses],
                true,
            ),
            model("unsupported", "fixture/unsupported", &[], false),
        ],
    );
    rewind(&db);
    drop(db);
    let db = Database::open(dir.clone()).unwrap();
    assert_eq!(names(&db, KIMI_PROVIDER_ID)[0].0, "operator-k3");
    assert_eq!(names(&db, KIMI_PROVIDER_ID)[1].0, "k3-256k");
    assert_eq!(
        names(&db, COMMAND_CODE_PROVIDER_ID)[0].0,
        "nvidia/nemotron-3-ultra-550b-a55b"
    );
    let persisted = db.load_persisted_contracts().unwrap();
    let scope = ContractScope::provider(COMMAND_CODE_PROVIDER_ID);
    assert!(
        persisted.evidence[&scope]
            .iter()
            .all(|row| row.model_id != "fixture/unsupported")
    );
    let contracts =
        crate::provider_contracts::build_effective_contracts(&Default::default(), &[], persisted);
    let goat = &contracts.providers[COMMAND_CODE_PROVIDER_ID];
    assert!(
        !goat
            .model("nvidia/nemotron-3-ultra-550b-a55b")
            .unwrap()
            .has_enabled_protocol()
    );
    assert!(goat.model("fixture/saved-responses").unwrap().protocols["responses"].enabled);
    drop(db);
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn failed_migration_rolls_back_protocol_evidence_names_and_version() {
    let dir = data_dir("rollback");
    let db = Database::open(dir.clone()).unwrap();
    save_catalog(&db, KIMI_PROVIDER_ID, &[model("k3", "k3", &[], false)]);
    save_catalog(
        &db,
        COMMAND_CODE_PROVIDER_ID,
        &[model(
            "fixture/saved",
            "fixture/saved",
            &[UpstreamProtocolKind::Responses],
            true,
        )],
    );
    rewind(&db);
    db.conn
        .execute_batch(
            "CREATE TRIGGER reject_v67_name BEFORE UPDATE OF public_model ON destination_models
        BEGIN SELECT RAISE(ABORT, 'fixture rejects rewrite'); END;",
        )
        .unwrap();
    assert!(migrate_to_v67(&db.conn, &dir.join("data.sqlite"), false).is_err());
    assert_eq!(schema_version_on(&db.conn).unwrap(), 66);
    assert_eq!(names(&db, KIMI_PROVIDER_ID)[0].0, "k3");
    assert!(db.load_persisted_contracts().unwrap().evidence.is_empty());
    assert_eq!(backup_count(&dir), 1);
    db.conn
        .execute_batch("DROP TRIGGER reject_v67_name;")
        .unwrap();
    migrate_to_v67(&db.conn, &dir.join("data.sqlite"), false).unwrap();
    assert_eq!(names(&db, KIMI_PROVIDER_ID)[0].0, "kimi-k3");
    drop(db);
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn fresh_catalogs_do_not_receive_legacy_names_or_backups() {
    let dir = data_dir("fresh");
    let db = Database::open(dir.clone()).unwrap();
    assert_eq!(backup_count(&dir), 0);
    save_catalog(&db, KIMI_PROVIDER_ID, &[model("k3", "k3", &[], false)]);
    drop(db);
    let db = Database::open(dir.clone()).unwrap();
    assert_eq!(names(&db, KIMI_PROVIDER_ID)[0].0, "k3");
    assert_eq!(backup_count(&dir), 0);
    drop(db);
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn shared_sealed_canonical_names_survive_but_unknown_cross_provider_raw_names_do_not_shadow() {
    let dir = data_dir("shared");
    let db = Database::open(dir.clone()).unwrap();
    save_catalog(
        &db,
        KIMI_PROVIDER_ID,
        &[
            model("k3", "k3", &[], false),
            model("k3-256k", "k3-256k", &[], false),
        ],
    );
    save_catalog(
        &db,
        OPENCODE_PROVIDER_ID,
        &[model("kimi-k3", "kimi-k3", &[], false)],
    );
    save_catalog(
        &db,
        OLLAMA_PROVIDER_ID,
        &[model("fixture-other", "kimi-k3-256k", &[], false)],
    );
    rewind(&db);
    drop(db);
    let db = Database::open(dir.clone()).unwrap();
    assert_eq!(
        names(&db, KIMI_PROVIDER_ID),
        vec![
            ("kimi-k3".into(), "k3".into()),
            ("k3-256k".into(), "k3-256k".into()),
        ]
    );
    assert_eq!(names(&db, OLLAMA_PROVIDER_ID)[0].0, "fixture-other");
    drop(db);
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn migration_preserves_case_only_explicit_names_and_unmatched_raw_spellings() {
    let dir = data_dir("case-only-names");
    let db = Database::open(dir.clone()).unwrap();
    save_catalog(
        &db,
        KIMI_PROVIDER_ID,
        &[
            model("K3", "k3", &[UpstreamProtocolKind::ChatCompletions], true),
            model(
                "K3-256K",
                "K3-256K",
                &[UpstreamProtocolKind::Messages],
                true,
            ),
        ],
    );
    let before = names(&db, KIMI_PROVIDER_ID);
    rewind(&db);
    drop(db);
    let db = Database::open(dir.clone()).unwrap();
    assert_eq!(names(&db, KIMI_PROVIDER_ID), before);
    drop(db);
    std::fs::remove_dir_all(dir).unwrap();
}
