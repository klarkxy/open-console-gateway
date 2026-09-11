//! Compatibility facade for [`ocg_gateway::classify`].
//!
//! Crate-private items match the historical `ocg_core::gateway::classify`
//! surface. The public module path is unchanged; item visibility is not
//! widened. Do not glob-reexport or reexport the module itself.
//!
//! Pure classification policy lives in `ocg-gateway`. This module keeps the
//! host `classify_http` signature, 429 window/cooldown parsing, and fallback
//! derived from [`UsageWindowKind`].

use crate::gateway::limit::{parse_free_reset_or_default, parse_reset, parse_usage_limit_window};
use crate::models::{UpstreamChannel, UsageWindowKind};
use chrono::Duration;

pub(crate) use ocg_gateway::classify::{
    PreflightKind, ProviderErrorClass, RateLimitFallback, RateLimitPolicy, StreamClassifyInput,
    TransportClassifyInput, classify_preflight, classify_stream, classify_transport,
    schedule_go_usage_sync,
};

/// Host compatibility wrapper: converts [`UpstreamChannel::Free`] to the
/// gateway classifier's `free_channel` flag.
pub(crate) fn classify_http(
    status: u16,
    provider_id: &str,
    channel: UpstreamChannel,
    anonymous: bool,
) -> ProviderErrorClass {
    ocg_gateway::classify::classify_http(
        status,
        provider_id,
        channel == UpstreamChannel::Free,
        anonymous,
    )
}

/// Host compatibility wrapper for body-aware inference error classification.
pub(crate) fn classify_http_response(
    status: u16,
    provider_id: &str,
    channel: UpstreamChannel,
    anonymous: bool,
    response_body: &str,
) -> ProviderErrorClass {
    ocg_gateway::classify::classify_http_response(
        status,
        provider_id,
        channel == UpstreamChannel::Free,
        anonymous,
        response_body,
    )
}

pub(crate) fn rate_limit_window_and_cooldown(
    policy: RateLimitPolicy,
    text: &str,
) -> (Option<UsageWindowKind>, Duration) {
    match policy {
        RateLimitPolicy::GenericFiveMinute => (None, Duration::minutes(5)),
        RateLimitPolicy::ZenFreeShared => (
            Some(UsageWindowKind::Free),
            parse_free_reset_or_default(text),
        ),
        RateLimitPolicy::GoWindow => {
            let window = parse_usage_limit_window(text);
            let cooldown = if window == Some(UsageWindowKind::Free) {
                parse_free_reset_or_default(text)
            } else {
                parse_reset(text).unwrap_or_else(|| Duration::minutes(5))
            };
            (window, cooldown)
        }
    }
}

pub(crate) fn rate_limit_fallback(window: Option<UsageWindowKind>) -> RateLimitFallback {
    if window == Some(UsageWindowKind::Free) {
        RateLimitFallback::ExhaustFreeChannel
    } else {
        RateLimitFallback::TryNextAccount
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn r03_free_429_does_not_rotate_keys() {
        for misleading_body in [
            "5-hour usage limit reached. Resets in 13min.",
            "Weekly usage limit reached. Resets in 4 days.",
            "Monthly usage limit reached. Resets in 13 days.",
        ] {
            let (window, _) =
                rate_limit_window_and_cooldown(RateLimitPolicy::ZenFreeShared, misleading_body);
            assert_eq!(window, Some(UsageWindowKind::Free), "{misleading_body}");
            assert_eq!(
                rate_limit_fallback(window),
                RateLimitFallback::ExhaustFreeChannel
            );
        }
        assert_eq!(
            rate_limit_fallback(Some(UsageWindowKind::FiveHours)),
            RateLimitFallback::TryNextAccount
        );
        assert_eq!(rate_limit_fallback(None), RateLimitFallback::TryNextAccount);
    }

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

    #[test]
    fn goat_429_is_generic_and_ignores_go_limit_windows() {
        for misleading_body in [
            "5-hour usage limit reached. Resets in 13min.",
            "Weekly usage limit reached. Resets in 4 days.",
            "Monthly usage limit reached. Resets in 13 days.",
            r#"{"type":"GoUsageLimitError","message":"Weekly usage limit reached. Resets in 3 days."}"#,
        ] {
            let (window, cooldown) =
                rate_limit_window_and_cooldown(RateLimitPolicy::GenericFiveMinute, misleading_body);
            assert_eq!(window, None, "{misleading_body}");
            assert_eq!(cooldown, Duration::minutes(5), "{misleading_body}");
        }
        let (go_window, go_cooldown) = rate_limit_window_and_cooldown(
            RateLimitPolicy::GoWindow,
            "Weekly usage limit reached. Resets in 4 days.",
        );
        assert_eq!(go_window, Some(UsageWindowKind::Week));
        assert_eq!(go_cooldown, Duration::days(4));
    }

    #[test]
    fn go_429_free_wording_still_exhausts_the_free_window() {
        let (window, _) = rate_limit_window_and_cooldown(
            RateLimitPolicy::GoWindow,
            "Free usage limit reached. Resets in 13min.",
        );
        assert_eq!(window, Some(UsageWindowKind::Free));
        assert_eq!(
            rate_limit_fallback(window),
            RateLimitFallback::ExhaustFreeChannel
        );
    }
}
