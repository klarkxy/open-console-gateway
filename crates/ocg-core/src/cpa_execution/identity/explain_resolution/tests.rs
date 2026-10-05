use super::{HitClass, ResolutionClass, classify_hit, resolve_current_catalog};
use crate::db::Database;
use crate::db::destination_store::{insert_destination_row, replace_destination_catalog};
use crate::db::native_binding::{self, NativeModelInsert, OWNED_NATIVE_LEGACY_ID};
use ocg_domain::catalog::UpstreamProtocolKind;
use ocg_domain::destination::{
    AdapterKind, AuthScheme, CatalogModel, Destination, LegacyDestinationRef, ModelResolution,
    sealed_capabilities,
};
use ocg_domain::ids::{CPA_PROVIDER_ID, MINIMAX_PROVIDER_ID};
use rusqlite::params;
use std::path::PathBuf;
use uuid::Uuid;

fn model(public_model: &str, upstream: &str, enabled: bool) -> CatalogModel {
    CatalogModel {
        public_model: public_model.into(),
        upstream_model: upstream.into(),
        protocols: vec![UpstreamProtocolKind::ChatCompletions],
        preferred: Some(UpstreamProtocolKind::ChatCompletions),
        enabled,
        upstream_override: None,
    }
}

fn curated(spelling: &str) -> HitClass {
    HitClass::Alias {
        spelling: spelling.into(),
        curated: true,
    }
}

fn public_alias(spelling: &str) -> HitClass {
    HitClass::Alias {
        spelling: spelling.into(),
        curated: false,
    }
}

#[test]
fn minimax_alias_and_exact_upstream_stay_distinct() {
    let row = model("MiniMax-M3", "MiniMax-M3", true);
    assert_eq!(
        classify_hit(MINIMAX_PROVIDER_ID, &row, "minimax-m3"),
        Some(curated("minimax-m3"))
    );
    assert_eq!(
        classify_hit(MINIMAX_PROVIDER_ID, &row, "MINIMAX-M3"),
        Some(curated("minimax-m3"))
    );
    assert_eq!(
        classify_hit(MINIMAX_PROVIDER_ID, &row, "MiniMax-M3"),
        Some(HitClass::PinnedRaw)
    );
}

#[test]
fn raw_shaped_case_variant_is_not_a_pin() {
    let raw = model("vendor/Model", "vendor/Model", true);
    assert_eq!(
        classify_hit("custom-http", &raw, "vendor/Model"),
        Some(HitClass::PinnedRaw)
    );
    assert!(classify_hit("custom-http", &raw, "vendor/model").is_none());
    assert!(classify_hit("custom-http", &raw, "Vendor/Model").is_none());
    let plain = model("codex-test", "codex-test", true);
    assert_eq!(
        classify_hit("cpa", &plain, "codex-test"),
        Some(HitClass::PinnedRaw)
    );
    assert!(classify_hit("cpa", &plain, "Codex-Test").is_none());
    assert!(classify_hit("cpa", &plain, "missing-model").is_none());
}

#[test]
fn distinct_public_name_keeps_its_stored_spelling() {
    let row = model("my-local", "vendor/raw", true);
    assert_eq!(
        classify_hit("custom-http", &row, "My-Local"),
        Some(public_alias("my-local"))
    );
}

struct TempDb {
    dir: PathBuf,
    db: Database,
}

impl Drop for TempDb {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn open_db() -> TempDb {
    let dir = std::env::temp_dir().join(format!("ocg-explain-resolution-{}", Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let db = Database::open(dir.clone()).unwrap();
    TempDb { dir, db }
}

fn insert_destination(
    db: &Database,
    id: &str,
    adapter: AdapterKind,
    provider_id: &str,
    draft: bool,
) {
    insert_destination_row(
        &db.conn,
        &Destination {
            id: id.into(),
            legacy: LegacyDestinationRef::CustomAccount(id.into()),
            adapter,
            name: id.into(),
            brand_family: None,
            base_url: Some("https://catalog.example".into()),
            protocols: vec![UpstreamProtocolKind::ChatCompletions],
            protocol_routes: Vec::new(),
            auth_scheme: AuthScheme::ApiKey,
            model_resolution: ModelResolution::PublicAndUpstream,
            catalog: Vec::new(),
            capabilities: sealed_capabilities(adapter),
            plan: None,
            max_credentials: None,
            observer_credential_id: None,
            enabled: true,
        },
    )
    .unwrap();
    db.conn
        .execute(
            "UPDATE destinations SET onboarding_draft = ?2 WHERE id = ?1",
            params![id, i64::from(draft)],
        )
        .unwrap();
    db.conn
        .execute(
            "INSERT INTO credentials (
                id, legacy_account_id, destination_id, name, has_secret, enabled, routing_rank,
                scope_json, auth_state, key_cipher, provider_id, credential_kind, quota_scope,
                account_type, setup_step, verification_status, credential_version,
                authorization_connection_id, created_at, updated_at
             ) VALUES (
                ?1, ?2, ?3, ?1, 0, 1, 1, '{\"kind\":\"all\"}', 'unknown', '', ?4,
                'key', 'key', 'key', 'ready', 'verified', 1, '',
                '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z'
             )",
            params![id, format!("acct-{id}"), id, provider_id],
        )
        .unwrap();
}

fn delete_models(db: &Database, names: &[&str]) {
    for name in names {
        db.conn
            .execute(
                "DELETE FROM destination_models
                 WHERE upstream_model = ?1 OR public_model = ?1",
                [name],
            )
            .unwrap();
    }
}

fn store_model(db: &Database, id: &str, public_model: &str, upstream: &str, enabled: bool) {
    replace_destination_catalog(&db.conn, id, &[model(public_model, upstream, enabled)]).unwrap();
}

#[test]
fn current_catalog_keeps_disabled_draft_and_ignores_retired_global_rows() {
    let temp = open_db();
    insert_destination(
        &temp.db,
        "dest-disabled",
        AdapterKind::Http,
        "custom-http",
        false,
    );
    insert_destination(
        &temp.db,
        "dest-draft",
        AdapterKind::Http,
        "custom-http",
        true,
    );
    insert_destination(
        &temp.db,
        "dest-known",
        AdapterKind::Http,
        "custom-http",
        false,
    );
    store_model(
        &temp.db,
        "dest-disabled",
        "ocg-resolution-disabled",
        "ocg-resolution-disabled",
        false,
    );
    store_model(
        &temp.db,
        "dest-draft",
        "ocg-resolution-draft",
        "ocg-resolution-draft",
        true,
    );
    store_model(
        &temp.db,
        "dest-known",
        "ocg-resolution-known",
        "ocg-resolution-known",
        true,
    );
    temp.db
        .conn
        .execute(
            "INSERT INTO provider_model_catalogs (
                provider_id, models_json, refreshed_at, source_url
             ) VALUES ('cpa', ?1, '2026-01-01T00:00:00Z', 'memory')
             ON CONFLICT(provider_id) DO UPDATE SET models_json = excluded.models_json",
            params![r#"["retired-global-cpa-only"]"#],
        )
        .unwrap();
    let tx = temp.db.conn.unchecked_transaction().unwrap();
    let disabled = resolve_current_catalog(&tx, "ocg-resolution-disabled").unwrap();
    assert!(disabled.known);
    assert_eq!(disabled.kind, Some(ResolutionClass::PinnedRaw));
    assert!(disabled.alias.is_none());
    assert!(!disabled.ambiguous);
    let disabled_row = disabled
        .mappings
        .iter()
        .find(|item| item.destination_id == "dest-disabled")
        .expect("disabled catalog row");
    assert_eq!(disabled_row.provider_id, "custom-http");
    assert_eq!(disabled_row.upstream_model, "ocg-resolution-disabled");
    assert_eq!(disabled_row.adapter_kind, AdapterKind::Http.as_str());

    let draft = resolve_current_catalog(&tx, "ocg-resolution-draft").unwrap();
    assert!(draft.known);
    assert!(
        draft
            .mappings
            .iter()
            .any(|item| item.destination_id == "dest-draft")
    );

    let known = resolve_current_catalog(&tx, "ocg-resolution-known").unwrap();
    assert!(known.mappings.iter().any(|item| {
        item.destination_id == "dest-known" && item.upstream_model == "ocg-resolution-known"
    }));

    let retired = resolve_current_catalog(&tx, "retired-global-cpa-only").unwrap();
    assert!(!retired.known);
    assert!(retired.kind.is_none());
    assert!(retired.mappings.is_empty());
    assert!(!retired.ambiguous);
    drop(tx);
}

#[test]
fn owned_native_storage_is_one_canonical_provider() {
    let temp = open_db();
    let destination_id = native_binding::ensure_owned_destination(&temp.db.conn).unwrap();
    native_binding::bind_native_account(&temp.db.conn, "codex", "owned-a.json").unwrap();
    let inserted = native_binding::insert_native_models_if_new(
        &temp.db.conn,
        &destination_id,
        &[NativeModelInsert {
            public_model: "ocg-owned-native-pin".into(),
            upstream_model: "ocg-owned-native-pin".into(),
            protocols: vec!["chat_completions".into()],
        }],
    )
    .unwrap();
    assert_eq!(inserted, 1);
    let tx = temp.db.conn.unchecked_transaction().unwrap();
    let resolved = resolve_current_catalog(&tx, "ocg-owned-native-pin").unwrap();
    assert!(resolved.known);
    assert!(!resolved.ambiguous);
    assert_eq!(resolved.kind, Some(ResolutionClass::PinnedRaw));
    assert!(resolved.alias.is_none());
    assert_eq!(resolved.mappings.len(), 1);
    assert_eq!(resolved.mappings[0].destination_id, destination_id);
    assert_eq!(resolved.mappings[0].provider_id, CPA_PROVIDER_ID);
    assert_ne!(resolved.mappings[0].provider_id, OWNED_NATIVE_LEGACY_ID);
    assert_eq!(resolved.mappings[0].upstream_model, "ocg-owned-native-pin");
    drop(tx);
}

#[test]
fn cross_provider_native_and_http_stay_ambiguous() {
    let temp = open_db();
    let destination_id = native_binding::ensure_owned_destination(&temp.db.conn).unwrap();
    native_binding::bind_native_account(&temp.db.conn, "codex", "owned-b.json").unwrap();
    native_binding::insert_native_models_if_new(
        &temp.db.conn,
        &destination_id,
        &[NativeModelInsert {
            public_model: "ocg-shared-raw".into(),
            upstream_model: "ocg-shared-raw".into(),
            protocols: vec!["chat_completions".into()],
        }],
    )
    .unwrap();
    insert_destination(
        &temp.db,
        "dest-other-provider",
        AdapterKind::Http,
        "custom-http",
        false,
    );
    store_model(
        &temp.db,
        "dest-other-provider",
        "ocg-shared-raw",
        "ocg-shared-raw",
        true,
    );
    let tx = temp.db.conn.unchecked_transaction().unwrap();
    let resolved = resolve_current_catalog(&tx, "ocg-shared-raw").unwrap();
    assert!(resolved.known);
    assert!(resolved.ambiguous);
    assert!(resolved.kind.is_none());
    assert!(resolved.alias.is_none());
    assert!(resolved.mappings.iter().any(|item| {
        item.destination_id == destination_id && item.provider_id == CPA_PROVIDER_ID
    }));
    assert!(resolved.mappings.iter().any(|item| {
        item.destination_id == "dest-other-provider" && item.provider_id == "custom-http"
    }));
    assert!(
        resolved
            .mappings
            .iter()
            .all(|item| item.provider_id != OWNED_NATIVE_LEGACY_ID)
    );
    drop(tx);
}

#[test]
fn exact_raw_pin_precedes_a_different_alias() {
    let temp = open_db();
    delete_models(&temp.db, &["MiniMax-M3", "minimax-m3"]);
    insert_destination(
        &temp.db,
        "dest-minimax-raw",
        AdapterKind::Minimax,
        MINIMAX_PROVIDER_ID,
        false,
    );
    store_model(
        &temp.db,
        "dest-minimax-raw",
        "MiniMax-M3",
        "MiniMax-M3",
        true,
    );
    insert_destination(
        &temp.db,
        "dest-public-alias",
        AdapterKind::Http,
        "custom-http",
        false,
    );
    store_model(
        &temp.db,
        "dest-public-alias",
        "MiniMax-M3",
        "ocg-other-upstream",
        true,
    );
    let tx = temp.db.conn.unchecked_transaction().unwrap();
    let pinned = resolve_current_catalog(&tx, "MiniMax-M3").unwrap();
    assert!(pinned.known);
    assert!(!pinned.ambiguous);
    assert_eq!(pinned.kind, Some(ResolutionClass::PinnedRaw));
    assert!(pinned.alias.is_none());
    assert_eq!(pinned.mappings.len(), 1);
    assert_eq!(pinned.mappings[0].destination_id, "dest-minimax-raw");
    assert_eq!(pinned.mappings[0].provider_id, MINIMAX_PROVIDER_ID);
    assert_eq!(pinned.mappings[0].upstream_model, "MiniMax-M3");

    let alias = resolve_current_catalog(&tx, "minimax-m3").unwrap();
    assert!(alias.known);
    assert!(!alias.ambiguous);
    assert_eq!(alias.kind, Some(ResolutionClass::Alias));
    assert_eq!(alias.alias.as_deref(), Some("minimax-m3"));
    assert!(
        alias
            .mappings
            .iter()
            .all(|item| item.destination_id == "dest-minimax-raw")
    );
    assert!(
        alias
            .mappings
            .iter()
            .all(|item| item.upstream_model == "MiniMax-M3")
    );
    drop(tx);
}

#[test]
fn two_exact_raw_providers_stay_ambiguous() {
    let temp = open_db();
    delete_models(&temp.db, &["MiniMax-M3", "minimax-m3"]);
    insert_destination(
        &temp.db,
        "dest-minimax-raw",
        AdapterKind::Minimax,
        MINIMAX_PROVIDER_ID,
        false,
    );
    store_model(
        &temp.db,
        "dest-minimax-raw",
        "MiniMax-M3",
        "MiniMax-M3",
        true,
    );
    insert_destination(
        &temp.db,
        "dest-other-raw",
        AdapterKind::Http,
        "custom-http",
        false,
    );
    store_model(&temp.db, "dest-other-raw", "MiniMax-M3", "MiniMax-M3", true);
    insert_destination(
        &temp.db,
        "dest-public-alias",
        AdapterKind::Http,
        "custom-http",
        false,
    );
    store_model(
        &temp.db,
        "dest-public-alias",
        "MiniMax-M3",
        "ocg-other-upstream",
        true,
    );
    let tx = temp.db.conn.unchecked_transaction().unwrap();
    let resolved = resolve_current_catalog(&tx, "MiniMax-M3").unwrap();
    assert!(resolved.known);
    assert!(resolved.ambiguous);
    assert!(resolved.kind.is_none());
    assert!(resolved.alias.is_none());
    assert_eq!(resolved.mappings.len(), 2);
    assert!(
        resolved
            .mappings
            .iter()
            .all(|item| item.upstream_model == "MiniMax-M3")
    );
    assert!(
        resolved
            .mappings
            .iter()
            .any(|item| item.provider_id == MINIMAX_PROVIDER_ID)
    );
    assert!(
        resolved
            .mappings
            .iter()
            .any(|item| item.provider_id == "custom-http")
    );
    drop(tx);
}

#[test]
fn raw_case_variants_do_not_collapse() {
    let temp = open_db();
    delete_models(&temp.db, &["vendor/Model", "vendor/model", "Vendor/Model"]);
    insert_destination(
        &temp.db,
        "dest-upper",
        AdapterKind::Http,
        "custom-http",
        false,
    );
    store_model(&temp.db, "dest-upper", "vendor/Model", "vendor/Model", true);
    insert_destination(
        &temp.db,
        "dest-lower",
        AdapterKind::Http,
        "custom-http",
        false,
    );
    store_model(&temp.db, "dest-lower", "vendor/model", "vendor/model", true);
    let tx = temp.db.conn.unchecked_transaction().unwrap();
    let lower = resolve_current_catalog(&tx, "vendor/model").unwrap();
    assert!(lower.known);
    assert!(!lower.ambiguous);
    assert_eq!(lower.kind, Some(ResolutionClass::PinnedRaw));
    assert_eq!(lower.mappings.len(), 1);
    assert_eq!(lower.mappings[0].destination_id, "dest-lower");
    assert_eq!(lower.mappings[0].upstream_model, "vendor/model");

    let upper = resolve_current_catalog(&tx, "vendor/Model").unwrap();
    assert!(upper.known);
    assert!(!upper.ambiguous);
    assert_eq!(upper.mappings.len(), 1);
    assert_eq!(upper.mappings[0].destination_id, "dest-upper");
    assert_eq!(upper.mappings[0].upstream_model, "vendor/Model");

    let folded = resolve_current_catalog(&tx, "Vendor/Model").unwrap();
    assert!(!folded.known);
    assert!(folded.mappings.is_empty());
    assert!(!folded.ambiguous);
    drop(tx);
}
