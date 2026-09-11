use super::*;
use crate::connection::{LegacyConnectionKind, connection_id_for_legacy};
use chrono::TimeZone;

fn sample_facts() -> LegacyAccountFacts {
    LegacyAccountFacts {
        account_id: "acct-1".into(),
        name: "Primary".into(),
        notes: Some("note".into()),
        enabled: true,
        sort_order: 2,
        has_auth_error: false,
        verified: false,
        anonymous: false,
        declared_relation: None,
    }
}

fn sample_endpoints() -> Vec<AssignedEndpoint> {
    vec![
        AssignedEndpoint {
            id: "ep-chat".into(),
            url: Some("https://lab.example/v1/chat/completions".into()),
        },
        AssignedEndpoint {
            id: "ep-sealed".into(),
            url: None,
        },
    ]
}

#[test]
fn identity_and_credential_ids_are_deterministic_and_distinct() {
    let first = identity_id_for_legacy_account("acct-1");
    let again = identity_id_for_legacy_account("acct-1");
    assert_eq!(first, again);
    assert!(Uuid::parse_str(first.as_str()).is_ok());

    let platform = identity_id_for_platform_account("acct-1");
    let credential = credential_id_for_legacy_account("acct-1");
    let observer = observer_credential_id_for_platform_account("acct-1");
    assert_ne!(first.as_str(), platform.as_str());
    assert_ne!(first.as_str(), credential.as_str());
    assert_ne!(credential.as_str(), observer.as_str());
    assert_ne!(
        identity_id_for_legacy_account("a").as_str(),
        identity_id_for_legacy_account("b").as_str()
    );
}

#[test]
fn binding_ids_are_deterministic_and_scoped() {
    let connection = connection_id_for_legacy(LegacyConnectionKind::BuiltinProvider, "opencode");
    let credential = credential_id_for_legacy_account("acct-1");
    let first = binding_id_for(&credential, &connection);
    let again = binding_id_for(&credential, &connection);
    assert_eq!(first, again);
    let other = binding_id_for(&credential_id_for_legacy_account("acct-2"), &connection);
    assert_ne!(first.as_str(), other.as_str());
    let anonymous = anonymous_binding_id_for(&connection);
    let anonymous_again = anonymous_binding_id_for(&connection);
    assert_eq!(anonymous, anonymous_again);
    assert_ne!(first.as_str(), anonymous.as_str());
}

#[test]
fn onboarding_task_ids_are_deterministic() {
    let first = onboarding_task_id_for_legacy_account("acct-1");
    assert_eq!(first, onboarding_task_id_for_legacy_account("acct-1"));
    assert_ne!(
        first.as_str(),
        onboarding_task_id_for_legacy_account("acct-2").as_str()
    );
}

#[test]
fn derive_auth_state_prefers_auth_error_then_verified() {
    assert_eq!(derive_auth_state(true, true), AuthState::Invalid);
    assert_eq!(derive_auth_state(true, false), AuthState::Invalid);
    assert_eq!(derive_auth_state(false, true), AuthState::Valid);
    assert_eq!(derive_auth_state(false, false), AuthState::Unknown);
}

#[test]
fn cooldown_windows_omit_past_and_preserve_exact_future_instants() {
    let now = Utc.with_ymd_and_hms(2026, 9, 11, 3, 0, 0).unwrap();
    let future = Utc.with_ymd_and_hms(2026, 9, 11, 8, 0, 0).unwrap();
    let past = Utc.with_ymd_and_hms(2026, 9, 11, 1, 0, 0).unwrap();
    let facts = CooldownFacts {
        account_id: "acct-1".into(),
        generic: Some(past),
        five_hours: Some(future),
        week: Some(future),
        month: None,
        free: Some(future),
    };
    let windows = cooldown_windows(&facts, now);
    assert_eq!(windows.len(), 3);
    assert!(windows.iter().all(|window| window.metric.is_none()));
    let five = windows
        .iter()
        .find(|window| window.period == QuotaPeriod::FiveHours)
        .unwrap();
    assert_eq!(five.subject, QuotaSubject::Credential);
    assert_eq!(
        five.subject_ref,
        credential_id_for_legacy_account("acct-1").as_str()
    );
    assert_eq!(five.blocked_until, Some(future));
    assert_eq!(five.relation_confidence, RelationConfidence::Declared);
    assert_eq!(five.policy_mode, QuotaPolicyMode::AuthoritativeLimit);

    let free = windows
        .iter()
        .find(|window| window.period == QuotaPeriod::Free)
        .unwrap();
    assert_eq!(free.subject, QuotaSubject::Egress);
    assert_eq!(free.subject_ref, "free_channel");
    assert_eq!(free.blocked_until, Some(future));
}

#[test]
fn mapper_uses_account_name_opaque_confidence_and_sort_order() {
    let connection = connection_id_for_legacy(LegacyConnectionKind::DynamicProvider, "lab");
    let (identity, credential, binding) =
        legacy_account_objects(sample_facts(), &connection, &sample_endpoints());
    assert_eq!(identity.id, identity_id_for_legacy_account("acct-1"));
    assert_eq!(identity.label, "Primary");
    assert_eq!(identity.identity_confidence, IdentityConfidence::Opaque);
    assert_eq!(identity.authority_ref, None);
    assert!(identity.enabled);
    assert_eq!(identity.notes.as_deref(), Some("note"));
    assert_eq!(credential.id, credential_id_for_legacy_account("acct-1"));
    assert_eq!(credential.identity_id, identity.id);
    assert_eq!(credential.purpose, CredentialPurpose::Inference);
    assert_eq!(credential.material_kind, MaterialKind::ApiKey);
    assert_eq!(credential.secret_ref, "account:acct-1");
    assert_eq!(credential.auth_state, AuthState::Unknown);
    assert!(credential.enabled);
    assert_eq!(binding.routing_rank, 2);
    assert_eq!(binding.model_scope, ModelScope::All);
    assert_eq!(
        binding.allowed_endpoint_ids,
        vec!["ep-chat".to_string(), "ep-sealed".to_string()]
    );
    assert_eq!(
        binding.allowed_origins,
        vec!["https://lab.example".to_string()]
    );
    assert_eq!(binding.id, binding_id_for(&credential.id, &connection));
}

#[test]
fn mapper_marks_declared_identity_when_platform_relation_is_supplied() {
    let mut facts = sample_facts();
    facts.declared_relation = Some(DeclaredPlatformRelation {
        platform_account_id: "parent".into(),
        group: "default".into(),
        parent_base_url: "https://new.example".into(),
    });
    facts.has_auth_error = true;
    facts.enabled = false;
    let connection = connection_id_for_legacy(LegacyConnectionKind::CustomAccount, "acct-1");
    let (identity, credential, binding) =
        legacy_account_objects(facts, &connection, &sample_endpoints());
    assert_eq!(identity.identity_confidence, IdentityConfidence::Declared);
    assert_eq!(
        identity
            .authority_ref
            .as_ref()
            .map(|r| r.issuer_or_site.as_str()),
        Some("https://new.example")
    );
    assert_eq!(credential.auth_state, AuthState::Invalid);
    assert!(!credential.enabled);
    assert!(!binding.enabled);
}

#[test]
fn sealed_endpoints_contribute_no_origins() {
    let connection = connection_id_for_legacy(LegacyConnectionKind::BuiltinProvider, "opencode");
    let (_, _, binding) = legacy_account_objects(
        sample_facts(),
        &connection,
        &[AssignedEndpoint {
            id: "sealed".into(),
            url: None,
        }],
    );
    assert!(binding.allowed_origins.is_empty());
    assert_eq!(binding.allowed_endpoint_ids, vec!["sealed".to_string()]);
}

#[test]
fn anonymous_binding_uses_anonymous_id_and_rank() {
    let connection =
        connection_id_for_legacy(LegacyConnectionKind::BuiltinProvider, "opencode-zen-free");
    let binding = anonymous_binding_for(&connection, &["ep-1".into()], 4);
    assert_eq!(binding.id, anonymous_binding_id_for(&connection));
    assert_eq!(binding.allowed_endpoint_ids, vec!["ep-1".to_string()]);
    assert_eq!(binding.routing_rank, 4);
    assert!(binding.enabled);

    let mut facts = sample_facts();
    facts.anonymous = true;
    let (_, _, mapped) = legacy_account_objects(facts, &connection, &[]);
    assert_eq!(mapped.id, anonymous_binding_id_for(&connection));
}

#[test]
fn model_scope_and_enums_use_producible_wire_values() {
    assert_eq!(
        serde_json::to_value(ModelScope::All).unwrap(),
        serde_json::json!({"kind":"all"})
    );
    assert_eq!(
        serde_json::to_value(ModelScope::Only {
            models: vec!["a".into()]
        })
        .unwrap(),
        serde_json::json!({"kind":"only","models":["a"]})
    );
    assert_eq!(
        serde_json::to_value(CredentialPurpose::PlatformObserver).unwrap(),
        serde_json::json!("platform_observer")
    );
    assert_eq!(
        serde_json::to_value(MaterialKind::ApiKey).unwrap(),
        serde_json::json!("api_key")
    );
    assert_eq!(
        serde_json::to_value(AuthState::Invalid).unwrap(),
        serde_json::json!("invalid")
    );
    assert_eq!(
        serde_json::to_value(IdentityConfidence::Declared).unwrap(),
        serde_json::json!("declared")
    );
    assert_eq!(
        serde_json::to_value(RuntimeSubjectKind::Anonymous).unwrap(),
        serde_json::json!("anonymous")
    );
    assert_eq!(
        serde_json::to_value(QuotaSubject::Egress).unwrap(),
        serde_json::json!("egress")
    );
    assert_eq!(
        serde_json::to_value(QuotaPolicyMode::AuthoritativeLimit).unwrap(),
        serde_json::json!("authoritative_limit")
    );
    assert_eq!(
        serde_json::to_value(QuotaPeriod::FiveHours).unwrap(),
        serde_json::json!("five_hours")
    );
    assert_eq!(
        serde_json::to_value(OnboardingTaskKind::ManagedRegistration).unwrap(),
        serde_json::json!("managed_registration")
    );
    assert_eq!(
        serde_json::to_value(OnboardingTaskState::InProgress).unwrap(),
        serde_json::json!("in_progress")
    );
    assert_eq!(
        serde_json::to_value(SubscriptionSource::LegacyManual).unwrap(),
        serde_json::json!("legacy_manual")
    );
}

#[test]
fn origin_from_endpoint_url_strips_path() {
    assert_eq!(
        origin_from_endpoint_url("https://lab.example:8443/v1/chat/completions"),
        Some("https://lab.example:8443".into())
    );
    assert_eq!(origin_from_endpoint_url("not-a-url"), None);
}
