use super::destination_identity;
use crate::destination_projection::DestinationProjection;
use crate::goat_plan_cooldowns::GoatPlanCooldowns;
use crate::routing_snapshot::{ExecutionCredential, RoutingSnapshot};
use ocg_domain::catalog::UpstreamProtocolKind;
use ocg_domain::credential::{AuthState, ModelScope};
use ocg_domain::destination::{
    AdapterKind, AuthScheme, CatalogModel, Cooldowns, Credential, Destination, Grants,
    HttpProtocolRoute, LegacyDestinationFacts, destination_from_legacy,
};

#[test]
fn only_anonymous_sealed_free_recovery_ignores_catalog_replacement() {
    let original = destination_from_legacy(&LegacyDestinationFacts::Builtin {
        provider_id: ocg_domain::ids::OPENCODE_ZEN_FREE_PROVIDER_ID.into(),
    })
    .unwrap();
    let mut refreshed = original.clone();
    refreshed.catalog.push(CatalogModel {
        public_model: "replacement-free".into(),
        upstream_model: "replacement-free".into(),
        protocols: vec![UpstreamProtocolKind::ChatCompletions],
        preferred: Some(UpstreamProtocolKind::ChatCompletions),
        enabled: true,
        upstream_override: None,
    });
    assert_eq!(
        destination_identity(&original, true),
        destination_identity(&refreshed, true)
    );
    assert_ne!(
        destination_identity(&original, false),
        destination_identity(&refreshed, false)
    );
    for (adapter, auth) in [
        (AdapterKind::Http, AuthScheme::None),
        (AdapterKind::Zen, AuthScheme::Bearer),
    ] {
        let mut before = original.clone();
        before.adapter = adapter;
        before.auth_scheme = auth;
        let mut after = before.clone();
        after.catalog = refreshed.catalog.clone();
        assert_ne!(
            destination_identity(&before, true),
            destination_identity(&after, true)
        );
    }
    let mut moved = refreshed.clone();
    moved.base_url = Some("https://changed.example/v1".into());
    assert_ne!(
        destination_identity(&original, true),
        destination_identity(&moved, true)
    );
    refreshed.enabled = !original.enabled;
    assert_ne!(
        destination_identity(&original, true),
        destination_identity(&refreshed, true)
    );
}

#[test]
fn empty_endpoint_skips_model_policy_identity() {
    let missing = super::ResourceSet::fixture(1, 1, 1, &["a"], false).without_model();
    assert!(
        missing
            .policy_key(crate::gateway::policy::RestrictionScope::CredentialModel)
            .is_none()
    );
    assert!(
        missing
            .policy_key(crate::gateway::policy::RestrictionScope::Credential)
            .is_some()
    );
    let labeled = super::restriction_endpoint_identity(
        "http://127.0.0.1/v1/chat/completions",
        "Direct",
        "ChatCompletions",
        None,
    );
    assert!(labeled.contains("127.0.0.1"));
    assert!(!labeled.is_empty());
}

fn empty_cooldowns() -> Cooldowns {
    Cooldowns {
        generic_until: None,
        five_hour_until: None,
        week_until: None,
        month_until: None,
        free_until: None,
    }
}

fn http_destination() -> Destination {
    destination_from_legacy(&LegacyDestinationFacts::CustomAccount {
        account_id: "acct-retry".into(),
        name: "Lab".into(),
        endpoint_url: "https://lab.example/v1".into(),
        protocol: UpstreamProtocolKind::ChatCompletions,
        model_capabilities: vec![("public-model".into(), "upstream-model".into())],
    })
    .unwrap()
}

fn execution_row(destination: &Destination, version: u64, cipher: &str) -> ExecutionCredential {
    ExecutionCredential {
        id: "exec-retry".into(),
        credential_id: "cred-retry".into(),
        destination_id: destination.id.clone(),
        provider_id: "custom".into(),
        name: "Lab".into(),
        key_cipher: cipher.into(),
        enabled: true,
        ready: true,
        auth_error: None,
        cooldowns: empty_cooldowns(),
        binding_id: "binding-retry".into(),
        binding_enabled: true,
        credential_version: version,
        authorization_connection_id: "conn-retry".into(),
        scope: ModelScope::All,
        grants: Grants {
            allowed_endpoint_ids: Vec::new(),
            allowed_origins: Vec::new(),
        },
        quota_recovery: None,
        quota_probe: false,
        goat_plan: GoatPlanCooldowns::default(),
    }
}

fn projected_credential(execution: &ExecutionCredential) -> Credential {
    Credential {
        id: execution.credential_id.clone(),
        legacy_account_id: execution.id.clone(),
        destination_id: execution.destination_id.clone(),
        name: execution.name.clone(),
        notes: None,
        has_secret: true,
        enabled: execution.enabled,
        routing_rank: 0,
        scope: execution.scope.clone(),
        grants: execution.grants.clone(),
        auth_state: AuthState::Valid,
        last_error: None,
        cooldowns: execution.cooldowns.clone(),
        quota_pool_id: None,
        onboarding_task: None,
        purchase_date: None,
    }
}

/// Production derivation. `with_catalog_generation` only paints digest bytes.
fn derive(destination: Destination, version: u64, cipher: &str) -> super::ResourceSet {
    let execution = execution_row(&destination, version, cipher);
    let snapshot = RoutingSnapshot {
        projection: DestinationProjection {
            destinations: vec![destination],
            credentials: vec![projected_credential(&execution)],
        },
        credentials: vec![execution.clone()],
        ollama_pinned: Vec::new(),
    };
    super::ResourceSet::from_snapshot(
        &snapshot,
        &execution,
        "https://send.example/v1/chat/completions",
        "model-a",
        false,
    )
    .unwrap()
}

fn retry_parts(set: &super::ResourceSet) -> (crate::gateway::recovery::ResourceKey, [u8; 32]) {
    let key = set.key(crate::gateway::recovery::ResourceKind::CredentialRetry);
    let owner = set.owner_generation(&key);
    (key, owner)
}

#[test]
fn from_snapshot_catalog_addition_keeps_retry_and_moves_full_and_policy_identity() {
    let original = derive(http_destination(), 3, "cipher-a");
    let mut refreshed_destination = http_destination();
    refreshed_destination.catalog.push(CatalogModel {
        public_model: "unrelated-added".into(),
        upstream_model: "unrelated-added".into(),
        protocols: vec![UpstreamProtocolKind::ChatCompletions],
        preferred: Some(UpstreamProtocolKind::ChatCompletions),
        enabled: true,
        upstream_override: None,
    });
    let refreshed = derive(refreshed_destination, 3, "cipher-a");
    let (original_key, original_owner) = retry_parts(&original);
    let (refreshed_key, refreshed_owner) = retry_parts(&refreshed);
    assert_eq!(original_key, refreshed_key);
    assert_eq!(original_owner, refreshed_owner);
    assert!(!original.same_generation(&refreshed));
    assert!(!original.same_policy_identity(&refreshed));
    assert_ne!(
        original.key(crate::gateway::recovery::ResourceKind::PolicyCredential),
        refreshed.key(crate::gateway::recovery::ResourceKind::PolicyCredential)
    );
    assert_ne!(
        original.key(crate::gateway::recovery::ResourceKind::PolicyCredentialModel),
        refreshed.key(crate::gateway::recovery::ResourceKind::PolicyCredentialModel)
    );
}

#[test]
fn from_snapshot_endpoint_auth_and_protocol_routes_move_retry_identity() {
    let original = derive(http_destination(), 3, "cipher-a");
    let (original_key, original_owner) = retry_parts(&original);

    let mut moved_url = http_destination();
    moved_url.base_url = Some("https://moved.example/v1".into());
    let url_changed = derive(moved_url, 3, "cipher-a");
    let (url_key, url_owner) = retry_parts(&url_changed);
    assert_ne!(original_key, url_key);
    assert_ne!(original_owner, url_owner);
    assert!(!original.same_generation(&url_changed));
    assert!(!original.same_policy_identity(&url_changed));

    let mut moved_auth = http_destination();
    moved_auth.auth_scheme = AuthScheme::ApiKey;
    let auth_changed = derive(moved_auth, 3, "cipher-a");
    let (auth_key, auth_owner) = retry_parts(&auth_changed);
    assert_ne!(original_key, auth_key);
    assert_ne!(original_owner, auth_owner);
    assert!(!original.same_generation(&auth_changed));
    assert!(!original.same_policy_identity(&auth_changed));

    let mut moved_routes = http_destination();
    moved_routes.protocol_routes.push(HttpProtocolRoute {
        protocol: UpstreamProtocolKind::Messages,
        endpoint_url: "https://lab.example/v1/messages".into(),
        auth_scheme: AuthScheme::XApiKey,
    });
    let routes_changed = derive(moved_routes, 3, "cipher-a");
    let (routes_key, routes_owner) = retry_parts(&routes_changed);
    assert_ne!(original_key, routes_key);
    assert_ne!(original_owner, routes_owner);
    assert!(!original.same_generation(&routes_changed));
    assert!(!original.same_policy_identity(&routes_changed));
}

#[test]
fn from_snapshot_credential_version_and_cipher_move_retry_identity() {
    let original = derive(http_destination(), 3, "cipher-a");
    let (original_key, original_owner) = retry_parts(&original);

    let version_changed = derive(http_destination(), 4, "cipher-a");
    let (version_key, version_owner) = retry_parts(&version_changed);
    assert_ne!(original_key, version_key);
    assert_ne!(original_owner, version_owner);

    let cipher_changed = derive(http_destination(), 3, "cipher-b");
    let (cipher_key, cipher_owner) = retry_parts(&cipher_changed);
    assert_ne!(original_key, cipher_key);
    assert_ne!(original_owner, cipher_owner);
}
