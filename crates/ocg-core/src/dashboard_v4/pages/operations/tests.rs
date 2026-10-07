use super::super::{accounts, snapshot};
use super::*;
use crate::crypto::{KeyCipher, StaticKeyCipher};
use crate::db::Database;
use crate::dynamic::DynamicProviderRuntime;
use crate::models::{Account, AccountSetupStep, AccountType, ProxyMode, RoutingMode};
use crate::provider::{CredentialKind, ProviderOrigin, QuotaScope, UpstreamProtocolKind};
use crate::state::CoreStateInner;
use ocg_domain::dynamic::{DynamicAuthKind, DynamicModelMapping};
use std::path::PathBuf;

struct Fixture {
    state: Option<CoreState>,
    dir: PathBuf,
}

impl Fixture {
    fn state(&self) -> CoreState {
        self.state.as_ref().unwrap().clone()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.state.take();
        std::fs::remove_dir_all(&self.dir).ok();
    }
}

fn fixture() -> Fixture {
    fixture_at(
        "https://a.example.test/v1",
        "https://b.example.test/v1",
        None,
    )
}
fn fixture_at(a_url: &str, b_url: &str, preset: Option<&str>) -> Fixture {
    let dir = std::env::temp_dir().join(format!("ocg-management-pages-{}", uuid::Uuid::new_v4()));
    let db = Database::open(dir.clone()).unwrap();
    let cipher: Arc<dyn KeyCipher + Send + Sync> = Arc::new(StaticKeyCipher::new("card-test"));
    let now = Utc::now();
    for (id, url) in [("supplier-a", a_url), ("supplier-b", b_url)] {
        db.create_dynamic_provider_definition(&DynamicProviderRuntime {
            preset_id: preset.map(str::to_string),
            id: id.into(),
            name: id.into(),
            endpoint_url: url.into(),
            upstream_protocol: UpstreamProtocolKind::ChatCompletions,
            auth_kind: DynamicAuthKind::Bearer,
            mappings: vec![DynamicModelMapping {
                public_model: "card-test".into(),
                upstream_model: format!("{id}-upstream"),
                upstream_override: None,
            }],
            created_at: now,
            updated_at: now,
            origin: ProviderOrigin::Custom,
            offering: "api".into(),
        })
        .unwrap();
    }
    for (id, provider) in [
        ("a1", "supplier-a"),
        ("a2", "supplier-a"),
        ("b1", "supplier-b"),
    ] {
        db.create_account(&Account {
            id: id.into(),
            provider_id: provider.into(),
            credential_kind: CredentialKind::ApiKey,
            quota_scope: QuotaScope::Key,
            name: id.into(),
            username: None,
            password_cipher: None,
            key_cipher: cipher.encrypt(&format!("dummy-{id}")).unwrap(),
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
            notes: Some(format!("preserve-{id}")),
            created_at: now,
            updated_at: now,
        })
        .unwrap();
    }
    let state = Arc::new(CoreStateInner::new(db, dir.clone(), cipher).unwrap());
    let mut config = state.config();
    config.gateway_key = "dummy-gateway-key".into();
    config.proxy_mode = ProxyMode::Direct;
    config.routing_mode = RoutingMode::StrictPriority;
    config.conversation_sticky = false;
    state.set_config(config).unwrap();
    Fixture {
        state: Some(state),
        dir,
    }
}

#[test]
fn selected_operation_uses_saved_identity_and_creation_capability() {
    let f = fixture();
    let state = f.state();
    let s = snapshot(&state).unwrap();
    let selected = accounts::detail(&state, &s, "a1").unwrap();
    let op = &selected.operations;
    assert!(op.rotate && op.binding && op.create);
    assert_eq!(
        op.credential_id.as_deref(),
        selected
            .identity
            .as_ref()
            .unwrap()
            .credentials
            .first()
            .map(|c| c.credential.id.as_str())
    );
    assert!(
        op.allowed_connections
            .iter()
            .all(|c| c.credential_create.allowed)
    );
    assert_eq!(op.share_targets.len(), 1);
    assert_eq!(op.share_targets[0].label, "a1");
    assert_eq!(op.test_models.len(), 1);
    assert_eq!(op.test_models[0].model_id, "card-test");
    assert_eq!(
        op.test_models[0].protocol,
        dashboard_v3::AccountUpstreamProtocol::ChatCompletions
    );
}

#[test]
fn saved_operations_hide_observers_no_auth_setup_and_external_controls() {
    let f = fixture();
    let state = f.state();
    let s = snapshot(&state).unwrap();
    let selected = accounts::detail(&state, &s, "a1").unwrap();
    let mut account = selected.account.clone();
    let mut destination = selected.destination.clone().unwrap();
    let mut identity = selected.identity.clone().unwrap();
    let check = |account: &dashboard_v3::Account,
                 destination: &DestinationDto,
                 identity: &IdentitySummary,
                 reason: &str| {
        let op = detail(&s, account, Some(destination), Some(identity));
        assert!(!op.rotate && !op.binding && !op.create);
        assert_eq!(op.unsupported_reason.as_deref(), Some(reason));
        assert!(op.allowed_connections.is_empty());
    };
    identity.credentials[0].credential.purpose = CredentialPurpose::PlatformObserver;
    check(&account, &destination, &identity, "credential_missing");
    assert!(
        detail(&s, &account, Some(&destination), Some(&identity))
            .share_targets
            .is_empty()
    );
    identity = selected.identity.clone().unwrap();
    account.credential_kind = dashboard_v3::AccountCredentialKind::None;
    check(&account, &destination, &identity, "no_authentication");
    account = selected.account.clone();
    account.setup_step = dashboard_v3::AccountSetupStep::OpencodeRegistration;
    check(&account, &destination, &identity, "setup_required");
    account = selected.account.clone();
    destination.capabilities.external_integration = true;
    check(&account, &destination, &identity, "external_integration");
    assert!(
        detail(&s, &account, Some(&destination), Some(&identity))
            .test_models
            .is_empty()
    );
    destination.capabilities.external_integration = false;
    destination.account_controls.toggle_write = AccountToggleWriteDto::ProviderSettings;
    check(&account, &destination, &identity, "provider_settings");
}

#[test]
fn share_choices_are_same_identity_inference_keys_and_selected_binding_is_exact() {
    let f = fixture();
    let state = f.state();
    let s = snapshot(&state).unwrap();
    let a = accounts::detail(&state, &s, "a1").unwrap();
    let b = accounts::detail(&state, &s, "a2").unwrap();
    let mut identity = a.identity.clone().unwrap();
    identity
        .credentials
        .insert(0, b.identity.unwrap().credentials.remove(0));
    let mut observer = identity.credentials[0].clone();
    observer.credential.id = "observer".into();
    observer.credential.purpose = CredentialPurpose::PlatformObserver;
    identity.credentials.push(observer);
    let mut anonymous = identity.credentials[0].clone();
    anonymous.credential.id = "anonymous".into();
    anonymous.subject = RuntimeSubjectKind::Anonymous;
    identity.credentials.push(anonymous);
    let op = detail(&s, &a.account, a.destination.as_ref(), Some(&identity));
    assert_eq!(op.credential_id, a.operations.credential_id);
    assert_eq!(op.binding_id, a.operations.binding_id);
    assert_eq!(
        op.share_targets
            .iter()
            .map(|c| c.label.as_str())
            .collect::<Vec<_>>(),
        vec!["a2", "a1"]
    );
}

#[test]
fn saved_grants_require_id_and_current_origin_and_keep_stale_values_visible() {
    let f = fixture();
    let state = f.state();
    let s = snapshot(&state).unwrap();
    let a = accounts::detail(&state, &s, "a1").unwrap();
    let mut connection = a.connection.unwrap();
    let mut binding = a.identity.unwrap().credentials.remove(0).bindings.remove(0);
    let id = connection.endpoints[0].id.clone();
    binding.allowed_endpoint_ids = vec![id.clone(), "removed".into()];
    binding.allowed_origins = vec![
        "https://a.example.test:443".into(),
        "https://stale.example.test".into(),
    ];
    let (granted, stale_ids, stale_origins) = saved_grants(Some(&binding), Some(&connection));
    assert_eq!(granted, vec![id.clone()]);
    assert_eq!(stale_ids, vec!["removed"]);
    assert_eq!(stale_origins, vec!["https://stale.example.test"]);
    connection.endpoints[0].url = Some("https://moved.example.test/v1".into());
    assert!(saved_grants(Some(&binding), Some(&connection)).0.is_empty());
    connection.endpoints[0].url = None;
    assert_eq!(saved_grants(Some(&binding), Some(&connection)).0, vec![id]);
    assert!(saved_grants(Some(&binding), None).0.is_empty());
}

#[test]
fn model_choices_use_http_route_fallback_and_dedupe_only_routable_models() {
    let f = fixture();
    let state = f.state();
    let s = snapshot(&state).unwrap();
    let a = accounts::detail(&state, &s, "a1").unwrap();
    let mut destination = a.destination.unwrap();
    let mut duplicate = destination.catalog[0].clone();
    duplicate.public_model = " CARD-TEST ".into();
    let mut off = duplicate.clone();
    off.public_model = "disabled".into();
    off.enabled = false;
    destination.catalog.extend([duplicate, off]);
    destination.catalog[0].preferred = Some(ProtocolDto::Messages);
    let choices = test_models(&s, Some(&destination));
    assert_eq!(choices.len(), 1);
    assert_eq!(choices[0].model_id, "card-test");
    assert_eq!(
        choices[0].protocol,
        dashboard_v3::AccountUpstreamProtocol::ChatCompletions
    );
    destination.protocols.clear();
    destination.protocol_routes.clear();
    assert!(test_models(&s, Some(&destination)).is_empty());
}

#[test]
fn builtin_and_custom_operations_use_current_destination_and_contract_scope() {
    let f = fixture();
    let state = f.state();
    let mut account = state.db.lock().get_account("a1").unwrap().unwrap();
    account.id = "builtin".into();
    account.provider_id = "opencode".into();
    state.db.lock().create_account(&account).unwrap();
    account.id = "custom".into();
    account.provider_id = "custom".into();
    account.account_type = AccountType::Key;
    state
        .db
        .lock()
        .create_account_with_contract(
            &account,
            Some(&crate::models::AccountCustomConfigInput {
                endpoint_url: "https://custom.example.test/v1".into(),
                upstream_protocol: UpstreamProtocolKind::ChatCompletions,
            }),
            &[crate::models::AccountModelCapabilityInput {
                public_model: "public-choice".into(),
                upstream_model: "vendor/choice".into(),
                protocol: UpstreamProtocolKind::ChatCompletions,
                source: None,
            }],
        )
        .unwrap();
    let s = snapshot(&state).unwrap();
    let builtin = accounts::detail(&state, &s, "builtin").unwrap();
    assert!(builtin.operations.rotate && builtin.operations.binding && builtin.operations.create);
    let exact = providers::scope(&s, builtin.destination.as_ref().unwrap())
        .unwrap()
        .1;
    assert_eq!(
        builtin.operations.test_models.len(),
        exact.iter().filter(|m| m.routable).count()
    );
    let custom = accounts::detail(&state, &s, "custom").unwrap();
    assert!(custom.operations.rotate && custom.operations.binding);
    assert!(!custom.operations.create);
    assert_eq!(
        custom.operations.unsupported_reason.as_deref(),
        Some("dedicated_account_flow")
    );
    assert!(custom.operations.allowed_connections.is_empty());
    assert_eq!(custom.operations.test_models.len(), 1);
    assert_eq!(custom.operations.test_models[0].model_id, "public-choice");
}
