use super::*;
use crate::crypto::{KeyCipher, StaticKeyCipher};
use crate::db::Database;
use crate::dynamic::DynamicProviderRuntime;
use crate::models::{Account, AccountSetupStep, AccountType};
use crate::provider::{CredentialKind, ProviderOrigin, QuotaScope, UpstreamProtocolKind};
use crate::state::CoreStateInner;
use chrono::Utc;
use ocg_domain::destination::{CatalogModel, Protocol, destination_id_for_dynamic};
use ocg_domain::dynamic::{DynamicAuthKind, DynamicModelMapping};
use std::path::PathBuf;
use std::sync::Arc;

struct Fixture {
    state: Option<CoreState>,
    cipher: Arc<StaticKeyCipher>,
    dir: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let dir = std::env::temp_dir().join(format!(
            "ocg-model-metadata-list-{}",
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let db = Database::open(dir.clone()).unwrap();
        let cipher = Arc::new(StaticKeyCipher::new("model-metadata-list"));
        let state = Arc::new(CoreStateInner::new(db, dir.clone(), cipher.clone()).unwrap());
        Self {
            state: Some(state),
            cipher,
            dir,
        }
    }

    fn state(&self) -> CoreState {
        self.state.as_ref().unwrap().clone()
    }

    /** One dynamic provider with an account and a single catalog model. */
    fn add_provider(&self, label: &str, public_model: &str) -> String {
        let state = self.state();
        let db = state.db.lock();
        let now = Utc::now();
        let provider_id = format!("prov-{label}");
        db.create_dynamic_provider_definition(&DynamicProviderRuntime {
            preset_id: None,
            id: provider_id.clone(),
            name: label.into(),
            endpoint_url: "https://upstream.example/v1".into(),
            upstream_protocol: UpstreamProtocolKind::ChatCompletions,
            auth_kind: DynamicAuthKind::Bearer,
            mappings: vec![DynamicModelMapping {
                public_model: public_model.into(),
                upstream_model: format!("vendor/{public_model}"),
                upstream_override: None,
            }],
            created_at: now,
            updated_at: now,
            origin: ProviderOrigin::Custom,
            offering: "api".into(),
        })
        .unwrap();
        db.create_account(&Account {
            id: format!("{label}-account"),
            provider_id: provider_id.clone(),
            credential_kind: CredentialKind::ApiKey,
            quota_scope: QuotaScope::Key,
            name: format!("{label}-account"),
            username: None,
            password_cipher: None,
            key_cipher: self.cipher.encrypt("list-test-secret").unwrap(),
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
            created_at: now,
            updated_at: now,
        })
        .unwrap();
        let destination_id = destination_id_for_dynamic(&provider_id);
        crate::db::destination_store::replace_destination_catalog(
            &db.conn,
            &destination_id,
            &[CatalogModel {
                public_model: public_model.into(),
                upstream_model: format!("vendor/{public_model}"),
                protocols: vec![Protocol::ChatCompletions],
                preferred: Some(Protocol::ChatCompletions),
                enabled: true,
                upstream_override: None,
            }],
        )
        .unwrap();
        destination_id
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.state.take();
        std::fs::remove_dir_all(&self.dir).ok();
    }
}

#[tokio::test]
async fn list_reports_every_destination_with_effective_sources_in_one_payload() {
    let fixture = Fixture::new();
    let state = fixture.state();
    let first = fixture.add_provider("alpha", "opus");
    let second = fixture.add_provider("beta", "haiku");

    // One operator declaration on the first destination's model.
    {
        let db = state.db.lock();
        let snapshot = crate::routing_snapshot::RoutingSnapshot::load(&db).unwrap();
        let destination = snapshot
            .projection
            .destinations
            .iter()
            .find(|d| d.id == first)
            .unwrap();
        let model = destination
            .catalog
            .iter()
            .find(|m| m.public_model == "opus")
            .unwrap();
        crate::model_metadata::declare(
            &db,
            destination,
            model,
            Some(ModelMetadata {
                input_modalities: Some(vec!["text".into(), "image".into()]),
                ..ModelMetadata::default()
            }),
        )
        .unwrap();
    }

    let Json(catalog) = list(State(state)).await.unwrap();
    let by_destination: std::collections::BTreeMap<_, _> = catalog
        .destinations
        .iter()
        .map(|d| (d.destination_id.as_str(), d))
        .collect();
    let declared = by_destination
        .get(first.as_str())
        .expect("first destination is in the catalog");
    let entry = declared
        .models
        .iter()
        .find(|m| m.public_model == "opus")
        .unwrap();
    assert_eq!(entry.source, "operator");
    assert_eq!(
        entry.metadata.input_modalities.as_deref(),
        Some(["text".to_string(), "image".to_string()].as_slice())
    );
    let undeclared = by_destination
        .get(second.as_str())
        .expect("second destination is in the catalog");
    assert_eq!(undeclared.models.len(), 1);
    assert_eq!(undeclared.models[0].source, "unknown");
}
