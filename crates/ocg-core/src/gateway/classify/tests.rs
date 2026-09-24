use super::*;
#[test]
fn r02_401_invalidates_only_the_failing_credential_and_may_try_b() {
    use crate::gateway::forwarder::{ForwardAction, forward_action_for_class};
    use crate::provider::CUSTOM_PROVIDER_ID;

    let rotate_a = classify_http_response(
        401,
        CUSTOM_PROVIDER_ID,
        UpstreamChannel::Go,
        false,
        "invalid key",
    );
    assert_eq!(rotate_a, ProviderErrorClass::UnauthorizedRotate);
    assert_eq!(
        forward_action_for_class(rotate_a, false, None),
        ForwardAction::TryNextAccount,
        "account B may still be tried after A's rotatable 401"
    );

    let go_model_error = classify_http_response(
        401,
        crate::provider::OPENCODE_PROVIDER_ID,
        UpstreamChannel::Go,
        false,
        r#"{"error":{"type":"ModelError","message":"not supported"}}"#,
    );
    assert_eq!(go_model_error, ProviderErrorClass::UnauthorizedPassthrough);
    assert_eq!(
        forward_action_for_class(go_model_error, false, None),
        ForwardAction::Return,
        "safe-error 401 must not fan out to B"
    );
}

#[test]
fn r04_header_timeout_after_send_started_does_not_replay() {
    use crate::gateway::forwarder::{ForwardAction, forward_action_for_class};

    for input in [
        TransportClassifyInput::HeaderTimeout,
        TransportClassifyInput::SendTimeout,
        TransportClassifyInput::BodyTimeout,
    ] {
        let class = classify_transport(input);
        assert_eq!(class, ProviderErrorClass::OutcomeUnknown);
        assert_eq!(
            forward_action_for_class(class, true, None),
            ForwardAction::Return,
            "{input:?} must not auto-replay after send may have started"
        );
    }
}

#[test]
fn r05_sse_bytes_started_does_not_splice_another_account() {
    use crate::gateway::forwarder::{ForwardAction, forward_action_for_class};

    let class = classify_stream(StreamClassifyInput::AfterDownstreamBytes);
    assert_eq!(class, ProviderErrorClass::StreamNoReplay);
    assert_eq!(
        forward_action_for_class(class, true, None),
        ForwardAction::Return
    );
}

#[test]
fn r08_cpa_errors_do_not_add_unbounded_retry() {
    use crate::gateway::forwarder::{ForwardAction, forward_action_for_class};
    use crate::provider::CPA_PROVIDER_ID;

    assert_eq!(
        forward_action_for_class(ProviderErrorClass::ServerError, true, None),
        ForwardAction::Return
    );
    assert_eq!(
        forward_action_for_class(ProviderErrorClass::OutcomeUnknown, true, None),
        ForwardAction::Return
    );
    let cpa_401 = classify_http(401, CPA_PROVIDER_ID, UpstreamChannel::Go, false);
    assert_eq!(cpa_401, ProviderErrorClass::UnauthorizedRotate);
    assert_eq!(
        forward_action_for_class(cpa_401, false, None),
        ForwardAction::TryNextAccount
    );
    assert!(!cpa_401.same_account_retry_eligible());
}
