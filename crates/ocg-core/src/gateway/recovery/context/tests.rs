use super::destination_identity;
use ocg_domain::catalog::UpstreamProtocolKind;
use ocg_domain::destination::{
    AdapterKind, AuthScheme, CatalogModel, LegacyDestinationFacts, destination_from_legacy,
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
