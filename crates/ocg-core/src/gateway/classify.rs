//! Explicit Gateway provider/transport error classification.
//!
//! Pure data: the forwarder still owns logging, CAS, cooldown writes, usage-sync
//! scheduling, and wire envelopes. This table exists so `forwarder` does not grow
//! more `provider_id` policy branches.
//!
//! Runtime 401/429 rules here freeze Stage 0 `forwarder` behavior, including
//! cases that differ from unused [`crate::provider::ErrorCooldownDescriptor`]
//! flags (`inference_401_passthrough` is false for OpenCode Go; SCNet 429 still
//! parses Go windows because it is not in the generic GOAT/Custom set).

use crate::gateway::limit::{parse_free_reset_or_default, parse_reset, parse_usage_limit_window};
use crate::models::{UpstreamChannel, UsageWindowKind};
use crate::provider::{OPENCODE_PROVIDER_ID, OPENCODE_ZEN_FREE_PROVIDER_ID, ProviderAdapterKind};
use chrono::Duration;

/// Semantic class of one attempt failure. Side effects stay in the forwarder.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ProviderErrorClass {
    RouteUnavailable,
    DecryptFailed,
    Connect,
    OutcomeUnknown,
    RateLimited { policy: RateLimitPolicy },
    UnauthorizedPassthrough,
    UnauthorizedRotate,
    ForbiddenStop,
    ForbiddenRotate,
    HttpRequestTimeout,
    ClientError,
    ServerError,
    StreamRetryEligible,
    StreamNoReplay,
}

/// How a classified 429 cools down and whether it may fall through.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RateLimitPolicy {
    /// Parse OpenCode Go window text, rotate, key-match CAS, deferred usage sync.
    GoWindow,
    /// Shared egress-IP Free channel; exhaust it, no key rotate, no usage sync.
    ZenFreeShared,
    /// Custom/GOAT: five-minute generic cooldown, no Go window parse, no usage sync.
    GenericFiveMinute,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RateLimitFallback {
    ExhaustFreeChannel,
    TryNextAccount,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Auth401Policy {
    Passthrough,
    RotatePersistAuthError,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RateLimit429Policy {
    GoWindow,
    GenericFiveMinute,
}

/// Static per-adapter 401/429 policy. Free-channel 429 overlay is applied by
/// [`classify_http`] from the request channel, not from adapter identity.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ProviderErrorPolicy {
    pub inference_401: Auth401Policy,
    pub rate_limit_429: RateLimit429Policy,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PreflightKind {
    Route,
    Decrypt,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TransportClassifyInput {
    Connect,
    SendTimeout,
    HeaderTimeout,
    BodyTimeout,
    OtherSendFailure,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum StreamClassifyInput {
    InterruptedBeforeOutput,
    EndedIncompleteBeforeOutput,
    ConversionFailedBeforeOutput,
    IdleTimeoutBeforeOutput,
    AfterDownstreamBytes,
}

pub(crate) fn provider_error_policy(provider_id: &str, offering_id: &str) -> ProviderErrorPolicy {
    match ProviderAdapterKind::from_offering(provider_id, offering_id) {
        Some(kind) => policy_for_kind(kind),
        None => ProviderErrorPolicy {
            // Stage 0 matched OpenCode/Zen 401 on provider_id only.
            inference_401: if is_opencode_family_provider(provider_id) {
                Auth401Policy::Passthrough
            } else {
                Auth401Policy::RotatePersistAuthError
            },
            rate_limit_429: RateLimit429Policy::GoWindow,
        },
    }
}

fn policy_for_kind(kind: ProviderAdapterKind) -> ProviderErrorPolicy {
    match kind {
        ProviderAdapterKind::OpenCodeGo => ProviderErrorPolicy {
            // Stage 0 passthrough: Go uses 401 for ModelError as well as bad keys.
            inference_401: Auth401Policy::Passthrough,
            rate_limit_429: RateLimit429Policy::GoWindow,
        },
        ProviderAdapterKind::ZenFree => ProviderErrorPolicy {
            inference_401: Auth401Policy::Passthrough,
            rate_limit_429: RateLimit429Policy::GoWindow,
        },
        ProviderAdapterKind::CommandCodeGoat | ProviderAdapterKind::ConfigurableHttp => {
            ProviderErrorPolicy {
                inference_401: Auth401Policy::RotatePersistAuthError,
                rate_limit_429: RateLimit429Policy::GenericFiveMinute,
            }
        }
        ProviderAdapterKind::Scnet => ProviderErrorPolicy {
            inference_401: Auth401Policy::RotatePersistAuthError,
            rate_limit_429: RateLimit429Policy::GoWindow,
        },
    }
}

fn is_opencode_family_provider(provider_id: &str) -> bool {
    matches!(
        provider_id,
        OPENCODE_PROVIDER_ID | OPENCODE_ZEN_FREE_PROVIDER_ID
    )
}

pub(crate) fn classify_preflight(kind: PreflightKind) -> ProviderErrorClass {
    match kind {
        PreflightKind::Route => ProviderErrorClass::RouteUnavailable,
        PreflightKind::Decrypt => ProviderErrorClass::DecryptFailed,
    }
}

pub(crate) fn classify_transport(input: TransportClassifyInput) -> ProviderErrorClass {
    match input {
        TransportClassifyInput::Connect => ProviderErrorClass::Connect,
        TransportClassifyInput::SendTimeout
        | TransportClassifyInput::HeaderTimeout
        | TransportClassifyInput::BodyTimeout
        | TransportClassifyInput::OtherSendFailure => ProviderErrorClass::OutcomeUnknown,
    }
}

pub(crate) fn classify_stream(input: StreamClassifyInput) -> ProviderErrorClass {
    match input {
        StreamClassifyInput::InterruptedBeforeOutput
        | StreamClassifyInput::EndedIncompleteBeforeOutput => {
            ProviderErrorClass::StreamRetryEligible
        }
        StreamClassifyInput::ConversionFailedBeforeOutput
        | StreamClassifyInput::IdleTimeoutBeforeOutput
        | StreamClassifyInput::AfterDownstreamBytes => ProviderErrorClass::StreamNoReplay,
    }
}

pub(crate) fn classify_http(
    status: u16,
    provider_id: &str,
    offering_id: &str,
    channel: UpstreamChannel,
    anonymous: bool,
) -> ProviderErrorClass {
    if (500..600).contains(&status) {
        return ProviderErrorClass::ServerError;
    }
    if status == 429 {
        let policy = provider_error_policy(provider_id, offering_id);
        let rate = match policy.rate_limit_429 {
            RateLimit429Policy::GenericFiveMinute => RateLimitPolicy::GenericFiveMinute,
            RateLimit429Policy::GoWindow if channel == UpstreamChannel::Free => {
                RateLimitPolicy::ZenFreeShared
            }
            RateLimit429Policy::GoWindow => RateLimitPolicy::GoWindow,
        };
        return ProviderErrorClass::RateLimited { policy: rate };
    }
    if status == 408 {
        return ProviderErrorClass::HttpRequestTimeout;
    }
    if status == 401 {
        return match provider_error_policy(provider_id, offering_id).inference_401 {
            Auth401Policy::Passthrough => ProviderErrorClass::UnauthorizedPassthrough,
            Auth401Policy::RotatePersistAuthError => ProviderErrorClass::UnauthorizedRotate,
        };
    }
    if status == 403 {
        return if anonymous {
            ProviderErrorClass::ForbiddenStop
        } else {
            ProviderErrorClass::ForbiddenRotate
        };
    }
    if (400..500).contains(&status) {
        return ProviderErrorClass::ClientError;
    }
    ProviderErrorClass::ClientError
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

pub(crate) fn schedule_go_usage_sync(class: ProviderErrorClass) -> bool {
    matches!(
        class,
        ProviderErrorClass::RateLimited {
            policy: RateLimitPolicy::GoWindow
        }
    )
}

impl ProviderErrorClass {
    pub(crate) fn same_account_retry_eligible(self) -> bool {
        matches!(self, Self::Connect | Self::StreamRetryEligible)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::{
        ANONYMOUS_FREE_OFFERING_ID, COMMAND_CODE_PROVIDER_ID, CUSTOM_API_OFFERING_ID,
        CUSTOM_PROVIDER_ID, GO_OFFERING_ID, GOAT_OFFERING_ID, SCNET_PROVIDER_ID,
        SCNET_TOKEN_PLAN_BASIC_OFFERING_ID,
    };

    fn classify(
        status: u16,
        provider_id: &str,
        offering_id: &str,
        channel: UpstreamChannel,
        anonymous: bool,
    ) -> ProviderErrorClass {
        classify_http(status, provider_id, offering_id, channel, anonymous)
    }

    #[test]
    fn provider_error_policy_covers_every_adapter_kind() {
        for kind in ProviderAdapterKind::ALL {
            let policy = policy_for_kind(kind);
            match kind {
                ProviderAdapterKind::OpenCodeGo => {
                    assert_eq!(policy.inference_401, Auth401Policy::Passthrough);
                    assert_eq!(policy.rate_limit_429, RateLimit429Policy::GoWindow);
                }
                ProviderAdapterKind::ZenFree => {
                    assert_eq!(policy.inference_401, Auth401Policy::Passthrough);
                    assert_eq!(policy.rate_limit_429, RateLimit429Policy::GoWindow);
                }
                ProviderAdapterKind::CommandCodeGoat | ProviderAdapterKind::ConfigurableHttp => {
                    assert_eq!(policy.inference_401, Auth401Policy::RotatePersistAuthError);
                    assert_eq!(policy.rate_limit_429, RateLimit429Policy::GenericFiveMinute);
                }
                ProviderAdapterKind::Scnet => {
                    assert_eq!(policy.inference_401, Auth401Policy::RotatePersistAuthError);
                    assert_eq!(policy.rate_limit_429, RateLimit429Policy::GoWindow);
                }
            }
        }
    }

    #[test]
    fn opencode_and_zen_401_passthrough_without_rotation() {
        assert_eq!(
            classify(
                401,
                OPENCODE_PROVIDER_ID,
                GO_OFFERING_ID,
                UpstreamChannel::Go,
                false
            ),
            ProviderErrorClass::UnauthorizedPassthrough
        );
        assert_eq!(
            classify(
                401,
                OPENCODE_ZEN_FREE_PROVIDER_ID,
                ANONYMOUS_FREE_OFFERING_ID,
                UpstreamChannel::Free,
                true
            ),
            ProviderErrorClass::UnauthorizedPassthrough
        );
        assert_eq!(
            classify(
                401,
                OPENCODE_PROVIDER_ID,
                "not-a-catalog-offering",
                UpstreamChannel::Go,
                false
            ),
            ProviderErrorClass::UnauthorizedPassthrough
        );
    }

    #[test]
    fn ordinary_401_rotates_and_persists_auth_error() {
        for (provider_id, offering_id) in [
            (CUSTOM_PROVIDER_ID, CUSTOM_API_OFFERING_ID),
            (COMMAND_CODE_PROVIDER_ID, GOAT_OFFERING_ID),
            (SCNET_PROVIDER_ID, SCNET_TOKEN_PLAN_BASIC_OFFERING_ID),
            ("unknown-provider", "unknown-offering"),
        ] {
            assert_eq!(
                classify(401, provider_id, offering_id, UpstreamChannel::Go, false),
                ProviderErrorClass::UnauthorizedRotate,
                "{provider_id}/{offering_id}"
            );
        }
    }

    #[test]
    fn go_zen_free_and_generic_429_policies() {
        assert_eq!(
            classify(
                429,
                OPENCODE_PROVIDER_ID,
                GO_OFFERING_ID,
                UpstreamChannel::Go,
                false
            ),
            ProviderErrorClass::RateLimited {
                policy: RateLimitPolicy::GoWindow
            }
        );
        assert_eq!(
            classify(
                429,
                OPENCODE_ZEN_FREE_PROVIDER_ID,
                ANONYMOUS_FREE_OFFERING_ID,
                UpstreamChannel::Free,
                true
            ),
            ProviderErrorClass::RateLimited {
                policy: RateLimitPolicy::ZenFreeShared
            }
        );
        assert_eq!(
            classify(
                429,
                CUSTOM_PROVIDER_ID,
                CUSTOM_API_OFFERING_ID,
                UpstreamChannel::Go,
                false
            ),
            ProviderErrorClass::RateLimited {
                policy: RateLimitPolicy::GenericFiveMinute
            }
        );
        assert_eq!(
            classify(
                429,
                COMMAND_CODE_PROVIDER_ID,
                GOAT_OFFERING_ID,
                UpstreamChannel::Go,
                false
            ),
            ProviderErrorClass::RateLimited {
                policy: RateLimitPolicy::GenericFiveMinute
            }
        );
        assert_eq!(
            classify(
                429,
                SCNET_PROVIDER_ID,
                SCNET_TOKEN_PLAN_BASIC_OFFERING_ID,
                UpstreamChannel::Go,
                false
            ),
            ProviderErrorClass::RateLimited {
                policy: RateLimitPolicy::GoWindow
            }
        );
        assert!(schedule_go_usage_sync(ProviderErrorClass::RateLimited {
            policy: RateLimitPolicy::GoWindow
        }));
        assert!(!schedule_go_usage_sync(ProviderErrorClass::RateLimited {
            policy: RateLimitPolicy::ZenFreeShared
        }));
        assert!(!schedule_go_usage_sync(ProviderErrorClass::RateLimited {
            policy: RateLimitPolicy::GenericFiveMinute
        }));
    }

    #[test]
    fn generic_429_wins_over_free_channel_and_zen_go_channel_parses_windows() {
        assert_eq!(
            classify(
                429,
                CUSTOM_PROVIDER_ID,
                CUSTOM_API_OFFERING_ID,
                UpstreamChannel::Free,
                false
            ),
            ProviderErrorClass::RateLimited {
                policy: RateLimitPolicy::GenericFiveMinute
            }
        );
        assert_eq!(
            classify(
                429,
                OPENCODE_ZEN_FREE_PROVIDER_ID,
                ANONYMOUS_FREE_OFFERING_ID,
                UpstreamChannel::Go,
                true
            ),
            ProviderErrorClass::RateLimited {
                policy: RateLimitPolicy::GoWindow
            }
        );
    }

    #[test]
    fn free_429_does_not_rotate_keys() {
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

    #[test]
    fn credentialed_403_rotates_anonymous_403_stops() {
        assert_eq!(
            classify(
                403,
                OPENCODE_PROVIDER_ID,
                GO_OFFERING_ID,
                UpstreamChannel::Go,
                false
            ),
            ProviderErrorClass::ForbiddenRotate
        );
        assert_eq!(
            classify(
                403,
                OPENCODE_ZEN_FREE_PROVIDER_ID,
                ANONYMOUS_FREE_OFFERING_ID,
                UpstreamChannel::Free,
                true
            ),
            ProviderErrorClass::ForbiddenStop
        );
        assert_eq!(
            classify(
                403,
                CUSTOM_PROVIDER_ID,
                CUSTOM_API_OFFERING_ID,
                UpstreamChannel::Go,
                false
            ),
            ProviderErrorClass::ForbiddenRotate
        );
    }

    #[test]
    fn http_408_is_outcome_unknown_and_5xx_passthrough() {
        assert_eq!(
            classify(
                408,
                OPENCODE_PROVIDER_ID,
                GO_OFFERING_ID,
                UpstreamChannel::Go,
                false
            ),
            ProviderErrorClass::HttpRequestTimeout
        );
        for status in [500, 502, 503, 599] {
            assert_eq!(
                classify(
                    status,
                    OPENCODE_PROVIDER_ID,
                    GO_OFFERING_ID,
                    UpstreamChannel::Go,
                    false
                ),
                ProviderErrorClass::ServerError,
                "{status}"
            );
        }
        for status in [400, 404, 413] {
            assert_eq!(
                classify(
                    status,
                    CUSTOM_PROVIDER_ID,
                    CUSTOM_API_OFFERING_ID,
                    UpstreamChannel::Go,
                    false
                ),
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
}
