use super::*;
use crate::catalog::CredentialKind;

fn fact(enabled: bool, has_auth_error: bool, verified: bool, cooling: bool) -> CredentialFacts {
    CredentialFacts {
        enabled,
        has_auth_error,
        verified,
        cooling,
    }
}

#[test]
fn connection_ids_are_deterministic_and_kind_scoped() {
    let first = connection_id_for_legacy(LegacyConnectionKind::BuiltinProvider, "opencode");
    let second = connection_id_for_legacy(LegacyConnectionKind::BuiltinProvider, "opencode");
    assert_eq!(first, second);
    assert_eq!(first.as_str(), second.as_str());
    assert!(Uuid::parse_str(first.as_str()).is_ok());

    let dynamic = connection_id_for_legacy(LegacyConnectionKind::DynamicProvider, "opencode");
    let custom = connection_id_for_legacy(LegacyConnectionKind::CustomAccount, "opencode");
    assert_ne!(first, dynamic);
    assert_ne!(first, custom);
    assert_ne!(dynamic, custom);
}

#[test]
fn endpoint_and_target_ids_are_deterministic_and_distinct() {
    let connection = connection_id_for_legacy(LegacyConnectionKind::DynamicProvider, "lab");
    let chat = endpoint_id_for(&connection, EndpointOperation::ChatCreate);
    let chat_again = endpoint_id_for(&connection, EndpointOperation::ChatCreate);
    let messages = endpoint_id_for(&connection, EndpointOperation::MessageCreate);
    assert_eq!(chat, chat_again);
    assert_ne!(chat, messages);

    let override_a = endpoint_id_for_route(
        &connection,
        EndpointOperation::ChatCreate,
        "https://a.example/v1/chat/completions",
    );
    let override_b = endpoint_id_for_route(
        &connection,
        EndpointOperation::ChatCreate,
        "https://b.example/v1/chat/completions",
    );
    assert_ne!(chat, override_a);
    assert_ne!(override_a, override_b);

    let target = target_id_for(&connection, "lab-opus");
    let target_again = target_id_for(&connection, "lab-opus");
    let other = target_id_for(&connection, "lab-sonnet");
    assert_eq!(target, target_again);
    assert_ne!(target, other);
}

#[test]
fn endpoint_operation_maps_one_to_one_with_upstream_protocol() {
    assert_eq!(
        EndpointOperation::from(UpstreamProtocolKind::ChatCompletions),
        EndpointOperation::ChatCreate
    );
    assert_eq!(
        EndpointOperation::from(UpstreamProtocolKind::Responses),
        EndpointOperation::ResponseCreate
    );
    assert_eq!(
        EndpointOperation::from(UpstreamProtocolKind::Messages),
        EndpointOperation::MessageCreate
    );
    assert_eq!(
        UpstreamProtocolKind::from(EndpointOperation::ChatCreate),
        UpstreamProtocolKind::ChatCompletions
    );
    assert_eq!(
        serde_json::to_value(LegacyConnectionKind::BuiltinProvider).unwrap(),
        serde_json::json!("builtin_provider")
    );
    assert_eq!(
        serde_json::to_value(EndpointOperation::ResponseCreate).unwrap(),
        serde_json::json!("response_create")
    );
}

#[test]
fn authorization_state_table() {
    assert_eq!(
        derive_authorization(CredentialKind::None, &[]),
        AuthorizationState::NotRequired,
        "none-empty"
    );
    assert_eq!(
        derive_authorization(CredentialKind::None, &[fact(true, true, false, false)]),
        AuthorizationState::NotRequired,
        "none-with-facts"
    );
    assert_eq!(
        derive_authorization(CredentialKind::ApiKey, &[]),
        AuthorizationState::Missing,
        "zero-credentials"
    );
    assert_eq!(
        derive_authorization(
            CredentialKind::ApiKey,
            &[
                fact(false, false, true, false),
                fact(true, false, true, false)
            ]
        ),
        AuthorizationState::Valid,
        "verified-enabled"
    );
    assert_eq!(
        derive_authorization(CredentialKind::ApiKey, &[fact(true, false, false, false)]),
        AuthorizationState::Unknown,
        "enabled-unverified"
    );
    assert_eq!(
        derive_authorization(
            CredentialKind::ApiKey,
            &[
                fact(true, true, false, false),
                fact(false, true, false, false)
            ]
        ),
        AuthorizationState::Invalid,
        "all-auth-errors"
    );
    assert_eq!(
        derive_authorization(
            CredentialKind::ApiKey,
            &[
                fact(false, false, false, false),
                fact(false, true, false, false)
            ]
        ),
        AuthorizationState::Unknown,
        "all-disabled-not-universal-error"
    );
}

#[test]
fn eligibility_state_table() {
    assert_eq!(
        derive_eligibility(
            ConnectionLifecycle::Disabled,
            AuthorizationState::Valid,
            2,
            false,
            1
        ),
        (
            EligibilityState::Ineligible,
            EligibilityReason::ConnectionDisabled
        ),
        "disabled-lifecycle"
    );
    assert_eq!(
        derive_eligibility(
            ConnectionLifecycle::Configured,
            AuthorizationState::Missing,
            1,
            false,
            0
        ),
        (
            EligibilityState::Ineligible,
            EligibilityReason::MissingCredential
        ),
        "missing-credential"
    );
    assert_eq!(
        derive_eligibility(
            ConnectionLifecycle::Configured,
            AuthorizationState::Unknown,
            1,
            false,
            0
        ),
        (
            EligibilityState::Ineligible,
            EligibilityReason::AllCredentialsDisabled
        ),
        "all-credentials-disabled"
    );
    assert_eq!(
        derive_eligibility(
            ConnectionLifecycle::Configured,
            AuthorizationState::Invalid,
            1,
            false,
            1
        ),
        (
            EligibilityState::Ineligible,
            EligibilityReason::AllCredentialsInvalid
        ),
        "all-credentials-invalid"
    );
    assert_eq!(
        derive_eligibility(
            ConnectionLifecycle::Configured,
            AuthorizationState::Unknown,
            0,
            false,
            1
        ),
        (
            EligibilityState::Ineligible,
            EligibilityReason::NoEnabledTarget
        ),
        "no-enabled-target"
    );
    assert_eq!(
        derive_eligibility(
            ConnectionLifecycle::Configured,
            AuthorizationState::Unknown,
            1,
            true,
            1
        ),
        (EligibilityState::Cooling, EligibilityReason::Cooling),
        "cooling-usable"
    );
    assert_eq!(
        derive_eligibility(
            ConnectionLifecycle::Configured,
            AuthorizationState::Unknown,
            1,
            false,
            1
        ),
        (EligibilityState::Eligible, EligibilityReason::None),
        "unknown-still-eligible"
    );
    assert_eq!(
        derive_eligibility(
            ConnectionLifecycle::Configured,
            AuthorizationState::NotRequired,
            1,
            false,
            0
        ),
        (EligibilityState::Eligible, EligibilityReason::None),
        "not-required-zero-credentials"
    );
}

#[test]
fn cooling_all_usable_requires_every_enabled_non_error_credential() {
    assert!(!cooling_all_usable(&[]));
    assert!(!cooling_all_usable(&[fact(false, false, false, true)]));
    assert!(cooling_all_usable(&[fact(true, false, false, true)]));
    assert!(!cooling_all_usable(&[
        fact(true, false, false, true),
        fact(true, false, false, false)
    ]));
    assert!(cooling_all_usable(&[
        fact(true, false, false, true),
        fact(true, true, false, false)
    ]));
}
