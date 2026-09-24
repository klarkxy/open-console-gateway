use super::*;
use ocg_domain::ids::{
    COMMAND_CODE_PROVIDER_ID, CUSTOM_PROVIDER_ID, OPENCODE_PROVIDER_ID,
    OPENCODE_ZEN_FREE_PROVIDER_ID,
};

fn classify(
    status: u16,
    provider_id: &str,
    free_channel: bool,
    anonymous: bool,
) -> ProviderErrorClass {
    classify_http(status, provider_id, free_channel, anonymous)
}

#[test]
fn provider_error_policy_covers_every_adapter_kind() {
    for kind in ProviderAdapterKind::ALL {
        let policy = policy_for_kind(kind);
        match kind {
            ProviderAdapterKind::OpenCodeGo => {
                assert_eq!(policy.inference_401, Auth401Policy::Passthrough);
                assert_eq!(policy.error_profile, ErrorProfile::OpenCodeGo);
            }
            ProviderAdapterKind::ZenFree => {
                assert_eq!(policy.inference_401, Auth401Policy::Passthrough);
                assert_eq!(policy.error_profile, ErrorProfile::OpenCodeGo);
            }
            ProviderAdapterKind::CommandCodeGoat => {
                assert_eq!(policy.inference_401, Auth401Policy::RotatePersistAuthError);
                assert_eq!(policy.error_profile, ErrorProfile::CommandCodeGoat);
            }
            ProviderAdapterKind::MiniMaxCn
            | ProviderAdapterKind::KimiCn
            | ProviderAdapterKind::OllamaCloud
            | ProviderAdapterKind::ConfigurableHttp
            | ProviderAdapterKind::Cpa => {
                assert_eq!(policy.inference_401, Auth401Policy::RotatePersistAuthError);
                assert_eq!(policy.error_profile, ErrorProfile::GenericHttp);
            }
        }
    }
}

#[test]
fn opencode_401_passthrough_but_zen_free_rejection_rotates() {
    assert_eq!(
        classify(401, OPENCODE_PROVIDER_ID, false, false),
        ProviderErrorClass::UnauthorizedPassthrough
    );
    assert_eq!(
        classify(401, OPENCODE_ZEN_FREE_PROVIDER_ID, true, true),
        ProviderErrorClass::FreeRejected
    );
}

#[test]
fn opencode_go_credits_401_rotates_but_model_and_unknown_401_passthrough() {
    let classify_body =
        |body| classify_http_response(401, OPENCODE_PROVIDER_ID, false, false, body);
    assert_eq!(
        classify_body(
            r#"{"type":"error","error":{"type":"CreditsError","message":"No active subscription"}}"#
        ),
        ProviderErrorClass::UnauthorizedRotate
    );
    assert_eq!(
        classify_body(
            r#"{"type":"error","error":{"type":"ModelError","message":"not supported"}}"#
        ),
        ProviderErrorClass::UnauthorizedPassthrough
    );
    assert_eq!(
        classify_body(r#"{"error":{"message":"expired key"}}"#),
        ProviderErrorClass::UnauthorizedPassthrough
    );
    assert_eq!(
        classify_body(r#"{"error":{"type":"OtherError"}}"#),
        ProviderErrorClass::UnauthorizedPassthrough
    );
    assert_eq!(
        classify_body(r#"{"error":{"type":"creditserror"}}"#),
        ProviderErrorClass::UnauthorizedPassthrough
    );
    assert_eq!(
        classify_body("not json"),
        ProviderErrorClass::UnauthorizedPassthrough
    );
}

#[test]
fn credits_error_refinement_is_go_only() {
    let body = r#"{"error":{"type":"CreditsError"}}"#;
    assert_eq!(
        classify_http_response(401, OPENCODE_ZEN_FREE_PROVIDER_ID, true, true, body),
        ProviderErrorClass::FreeRejected
    );
    assert_eq!(
        classify_http_response(401, CUSTOM_PROVIDER_ID, false, false, body),
        ProviderErrorClass::UnauthorizedRotate
    );
}

#[test]
fn ordinary_401_rotates_and_persists_auth_error() {
    for provider_id in [
        CUSTOM_PROVIDER_ID,
        COMMAND_CODE_PROVIDER_ID,
        "unknown-provider",
    ] {
        assert_eq!(
            classify(401, provider_id, false, false),
            ProviderErrorClass::UnauthorizedRotate,
            "{provider_id}"
        );
    }
}

#[test]
fn go_zen_free_and_generic_429_policies() {
    assert_eq!(
        classify(429, OPENCODE_PROVIDER_ID, false, false),
        ProviderErrorClass::RateLimited {
            profile: ErrorProfile::OpenCodeGo
        }
    );
    assert_eq!(
        classify(429, OPENCODE_ZEN_FREE_PROVIDER_ID, true, true),
        ProviderErrorClass::RateLimited {
            profile: ErrorProfile::ZenFree
        }
    );
    assert_eq!(
        classify(429, CUSTOM_PROVIDER_ID, false, false),
        ProviderErrorClass::RateLimited {
            profile: ErrorProfile::GenericHttp
        }
    );
    assert_eq!(
        classify(429, COMMAND_CODE_PROVIDER_ID, false, false),
        ProviderErrorClass::RateLimited {
            profile: ErrorProfile::CommandCodeGoat
        }
    );
    assert!(schedule_go_usage_sync(ProviderErrorClass::RateLimited {
        profile: ErrorProfile::OpenCodeGo
    }));
    assert!(!schedule_go_usage_sync(ProviderErrorClass::RateLimited {
        profile: ErrorProfile::ZenFree
    }));
    assert!(!schedule_go_usage_sync(ProviderErrorClass::RateLimited {
        profile: ErrorProfile::GenericHttp
    }));
}

#[test]
fn unknown_and_dynamic_shaped_429_use_generic_http_dialect() {
    for provider_id in [
        "unknown-provider",
        "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa",
        "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb",
    ] {
        assert_eq!(
            classify(429, provider_id, false, false),
            ProviderErrorClass::RateLimited {
                profile: ErrorProfile::GenericHttp
            },
            "{provider_id}"
        );
        assert_eq!(
            classify(429, provider_id, true, false),
            ProviderErrorClass::RateLimited {
                profile: ErrorProfile::GenericHttp
            },
            "{provider_id}"
        );
        assert!(!schedule_go_usage_sync(classify(
            429,
            provider_id,
            false,
            false
        )));
    }
}

#[test]
fn generic_429_wins_over_free_channel_and_zen_go_channel_parses_windows() {
    assert_eq!(
        classify(429, CUSTOM_PROVIDER_ID, true, false),
        ProviderErrorClass::RateLimited {
            profile: ErrorProfile::GenericHttp
        }
    );
    assert_eq!(
        classify(429, OPENCODE_ZEN_FREE_PROVIDER_ID, false, true),
        ProviderErrorClass::RateLimited {
            profile: ErrorProfile::OpenCodeGo
        }
    );
}

#[test]
fn credentialed_403_rotates_and_zen_free_403_rejects_channel() {
    assert_eq!(
        classify(403, OPENCODE_PROVIDER_ID, false, false),
        ProviderErrorClass::ForbiddenRotate
    );
    assert_eq!(
        classify(403, OPENCODE_ZEN_FREE_PROVIDER_ID, true, true),
        ProviderErrorClass::FreeRejected
    );
    assert_eq!(
        classify(403, CUSTOM_PROVIDER_ID, false, false),
        ProviderErrorClass::ForbiddenRotate
    );
}

#[test]
fn zen_free_http_errors_reject_the_channel_without_changing_other_routes() {
    for status in [400, 401, 403, 408, 500, 502, 503] {
        assert_eq!(
            classify(status, OPENCODE_ZEN_FREE_PROVIDER_ID, true, true),
            ProviderErrorClass::FreeRejected,
            "{status}"
        );
    }
    assert_eq!(
        classify(403, OPENCODE_ZEN_FREE_PROVIDER_ID, false, true),
        ProviderErrorClass::ForbiddenStop
    );
    assert_eq!(
        classify(500, OPENCODE_PROVIDER_ID, false, false),
        ProviderErrorClass::ServerError
    );
}

#[test]
fn http_408_is_outcome_unknown_and_5xx_passthrough() {
    assert_eq!(
        classify(408, OPENCODE_PROVIDER_ID, false, false),
        ProviderErrorClass::HttpRequestTimeout
    );
    for status in [500, 502, 503, 599] {
        assert_eq!(
            classify(status, OPENCODE_PROVIDER_ID, false, false),
            ProviderErrorClass::ServerError,
            "{status}"
        );
    }
    for status in [400, 404, 413] {
        assert_eq!(
            classify(status, CUSTOM_PROVIDER_ID, false, false),
            ProviderErrorClass::ClientError,
            "{status}"
        );
    }
}

#[test]
fn connect_is_retry_eligible_non_connect_transport_is_outcome_unknown() {
    assert_eq!(
        classify_transport(TransportClassifyInput::Connect),
        ProviderErrorClass::Connect
    );
    assert!(ProviderErrorClass::Connect.same_account_retry_eligible());
    for input in [
        TransportClassifyInput::SendTimeout,
        TransportClassifyInput::HeaderTimeout,
        TransportClassifyInput::BodyTimeout,
        TransportClassifyInput::OtherSendFailure,
    ] {
        let class = classify_transport(input);
        assert_eq!(class, ProviderErrorClass::OutcomeUnknown, "{input:?}");
        assert!(!class.same_account_retry_eligible());
    }
}

#[test]
fn transport_failure_kind_from_impl_matches_classify_input() {
    assert_eq!(
        TransportClassifyInput::from(TransportFailureKind::Connect),
        TransportClassifyInput::Connect
    );
    assert_eq!(
        TransportClassifyInput::from(TransportFailureKind::Timeout),
        TransportClassifyInput::SendTimeout
    );
    assert_eq!(
        TransportClassifyInput::from(TransportFailureKind::Other),
        TransportClassifyInput::OtherSendFailure
    );
}

#[test]
fn stream_retry_only_before_downstream_bytes_for_interrupt_or_incomplete() {
    assert_eq!(
        classify_stream(StreamClassifyInput::InterruptedBeforeOutput),
        ProviderErrorClass::StreamRetryEligible
    );
    assert_eq!(
        classify_stream(StreamClassifyInput::EndedIncompleteBeforeOutput),
        ProviderErrorClass::StreamRetryEligible
    );
    assert!(ProviderErrorClass::StreamRetryEligible.same_account_retry_eligible());
    for input in [
        StreamClassifyInput::ConversionFailedBeforeOutput,
        StreamClassifyInput::IdleTimeoutBeforeOutput,
        StreamClassifyInput::AfterDownstreamBytes,
    ] {
        let class = classify_stream(input);
        assert_eq!(class, ProviderErrorClass::StreamNoReplay, "{input:?}");
        assert!(!class.same_account_retry_eligible());
    }
}

#[test]
fn route_and_decrypt_preflight_are_explicit_classes() {
    assert_eq!(
        classify_preflight(PreflightKind::Route),
        ProviderErrorClass::RouteUnavailable
    );
    assert_eq!(
        classify_preflight(PreflightKind::Decrypt),
        ProviderErrorClass::DecryptFailed
    );
    assert!(!ProviderErrorClass::RouteUnavailable.same_account_retry_eligible());
    assert!(!ProviderErrorClass::DecryptFailed.same_account_retry_eligible());
}

#[test]
fn goat_insufficient_credits_is_not_a_generic_client_error() {
    let body = r#"{"error":{"code":"BAD_REQUEST","type":"invalid_request_error","message":"You have insufficient credits to make this request. Please purchase more credits to continue using the service."}}"#;
    assert_eq!(
        classify_http_response(400, COMMAND_CODE_PROVIDER_ID, false, false, body),
        ProviderErrorClass::InsufficientCredits
    );
    for provider in [OPENCODE_PROVIDER_ID, CUSTOM_PROVIDER_ID, "unknown-provider"] {
        assert_eq!(
            classify_http_response(400, provider, false, false, body),
            ProviderErrorClass::ClientError
        );
    }
    for invalid in [
        "not json",
        "{}",
        r#"{"error":{"message":"insufficient credits"}}"#,
        r#"{"error":{"code":"BAD_REQUEST","type":"invalid_request_error","message":"maximum context length exceeded"}}"#,
        r#"{"error":{"code":"BAD_REQUEST","type":"invalid_request_error","message":"The reasoning_content must be passed back"}}"#,
        r#"{"error":{"code":"BAD_REQUEST","type":"invalid_request_error","message":"unknown model"}}"#,
    ] {
        assert_eq!(
            classify_http_response(400, COMMAND_CODE_PROVIDER_ID, false, false, invalid),
            ProviderErrorClass::ClientError,
            "{invalid}"
        );
    }
    assert_eq!(
        classify_http_response(413, COMMAND_CODE_PROVIDER_ID, false, false, body),
        ProviderErrorClass::ClientError
    );
    assert!(!ProviderErrorClass::InsufficientCredits.same_account_retry_eligible());
    assert!(!schedule_go_usage_sync(
        ProviderErrorClass::InsufficientCredits
    ));
}
