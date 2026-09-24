use super::*;

#[test]
fn openrouter_free_preset_cannot_refresh_the_mixed_paid_catalog() {
    let temp_root = std::env::temp_dir().canonicalize().unwrap();
    let dir = temp_root.join(format!("ocg-openrouter-free-{}", uuid::Uuid::new_v4()));
    let db = super::super::Database::open(dir.clone()).unwrap();
    let now = chrono::Utc::now();
    for (preset_id, discoverable) in [("openrouter-free", false), ("openrouter", true)] {
        let id = uuid::Uuid::new_v4().to_string();
        let runtime = DynamicProviderRuntime {
            preset_id: Some(preset_id.into()),
            id: id.clone(),
            name: preset_id.into(),
            endpoint_url: "https://openrouter.ai/api/v1/chat/completions".into(),
            upstream_protocol: ocg_domain::catalog::UpstreamProtocolKind::ChatCompletions,
            auth_kind: ocg_domain::dynamic::DynamicAuthKind::Bearer,
            mappings: vec![ocg_domain::dynamic::DynamicModelMapping {
                public_model: format!("{preset_id}/free"),
                upstream_model: "openrouter/free".into(),
                upstream_override: None,
            }],
            created_at: now,
            updated_at: now,
            origin: ocg_domain::provider::ProviderOrigin::Preset,
            offering: "api".into(),
        };
        upsert_dynamic_destination_on(&db.conn, &runtime, Some(false)).unwrap();
        let destination_id = ocg_domain::destination::destination_id_for_dynamic(&id);
        let raw: String = db
            .conn
            .query_row(
                "SELECT capabilities_json FROM destinations WHERE id = ?1",
                [&destination_id],
                |row| row.get(0),
            )
            .unwrap();
        let capabilities: ocg_domain::destination::Capabilities =
            serde_json::from_str(&raw).unwrap();
        assert_eq!(
            capabilities.discoverable_models, discoverable,
            "{preset_id}"
        );
    }
    drop(db);
    assert!(dir.canonicalize().unwrap().starts_with(&temp_root));
    std::fs::remove_dir_all(dir).unwrap();
}
