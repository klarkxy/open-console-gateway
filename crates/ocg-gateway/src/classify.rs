//! Pure attempt-adjacent provider/transport error classification policy.
//!
//! [`ProviderErrorClass`] is data only: the host forwarder still owns logging,
//! CAS, cooldown writes, usage-sync scheduling, and wire envelopes. This table
//! exists so `forwarder` does not grow more `provider_id` policy branches.
//!
//! Status and exact structured-error decoding stay side-effect free.
//! The Core failure module turns these dialects into facts and applies one
//! restriction policy; unknown 429s never imply a fixed account cooldown.
//!
//! HTTP classification takes `free_channel: bool` rather than a host channel
//! enum. Window parsing, cooldown durations, and 429 body text stay in the host.
//!
//! Items are rust-public only as the cross-crate bridge; the host crate's
//! `gateway::classify` compatibility facade keeps them crate-private.

use crate::attempt::TransportFailureKind;
use ocg_domain::ids::{OPENCODE_PROVIDER_ID, OPENCODE_ZEN_FREE_PROVIDER_ID};
use ocg_domain::provider::ProviderAdapterKind;
use serde_json::Value;

/// Semantic class of one attempt failure. Side effects stay in the forwarder.
///
/// Public only as the cross-crate bridge; the host crate's `gateway::classify`
/// facade keeps this type crate-private.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[doc(hidden)]
pub enum ProviderErrorClass {
    RouteUnavailable,
    DecryptFailed,
    Connect,
    OutcomeUnknown,
    RateLimited {
        profile: ErrorProfile,
    },
    UnauthorizedPassthrough,
    UnauthorizedRotate,
    ForbiddenStop,
    ForbiddenRotate,
    HttpRequestTimeout,
    ClientError,
    /// Explicit account credit rejection with no advertised recovery time.
    InsufficientCredits,
    ServerError,
    StreamRetryEligible,
    StreamNoReplay,
}

/// Static error dialect. This carries no duration, retry, or state-write policy.
///
/// Public only as the cross-crate bridge; the host crate's `gateway::classify`
/// facade keeps this type crate-private.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[doc(hidden)]
pub enum ErrorProfile {
    OpenCodeGo,
    ZenFree,
    CommandCodeGoat,
    GenericHttp,
}

/// Public only as the cross-crate bridge; the host crate's `gateway::classify`
/// facade keeps this type crate-private.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[doc(hidden)]
pub enum RateLimitFallback {
    ExhaustFreeChannel,
    TryNextAccount,
}

/// Public only as the cross-crate bridge; the host crate's `gateway::classify`
/// facade keeps this type crate-private.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[doc(hidden)]
pub enum Auth401Policy {
    Passthrough,
    RotatePersistAuthError,
}

/// Static authentication behavior and error dialect. Free-channel overlay is applied by
/// [`classify_http`] from the `free_channel` flag, not from adapter identity.
///
/// Public only as the cross-crate bridge; the host crate's `gateway::classify`
/// facade keeps this type crate-private.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[doc(hidden)]
pub struct ProviderErrorPolicy {
    #[doc(hidden)]
    pub inference_401: Auth401Policy,
    #[doc(hidden)]
    pub error_profile: ErrorProfile,
}

/// Public only as the cross-crate bridge; the host crate's `gateway::classify`
/// facade keeps this type crate-private.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[doc(hidden)]
pub enum PreflightKind {
    Route,
    Decrypt,
}

/// Public only as the cross-crate bridge; the host crate's `gateway::classify`
/// facade keeps this type crate-private.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[doc(hidden)]
pub enum TransportClassifyInput {
    Connect,
    SendTimeout,
    HeaderTimeout,
    BodyTimeout,
    OtherSendFailure,
}

/// Public only as the cross-crate bridge; the host crate's `gateway::classify`
/// facade keeps this type crate-private.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[doc(hidden)]
pub enum StreamClassifyInput {
    InterruptedBeforeOutput,
    EndedIncompleteBeforeOutput,
    ConversionFailedBeforeOutput,
    IdleTimeoutBeforeOutput,
    AfterDownstreamBytes,
}

impl From<TransportFailureKind> for TransportClassifyInput {
    fn from(kind: TransportFailureKind) -> Self {
        match kind {
            TransportFailureKind::Connect => Self::Connect,
            TransportFailureKind::Timeout => Self::SendTimeout,
            TransportFailureKind::Other => Self::OtherSendFailure,
        }
    }
}

/// Public only as the cross-crate bridge; the host crate's `gateway::classify`
/// facade keeps this function crate-private.
#[doc(hidden)]
pub fn provider_error_policy(provider_id: &str) -> ProviderErrorPolicy {
    match ProviderAdapterKind::from_provider_id(provider_id) {
        Some(kind) => policy_for_kind(kind),
        None => ProviderErrorPolicy {
            // Stage 0 matched OpenCode/Zen 401 on provider_id only.
            inference_401: if is_opencode_family_provider(provider_id) {
                Auth401Policy::Passthrough
            } else {
                Auth401Policy::RotatePersistAuthError
            },
            error_profile: ErrorProfile::GenericHttp,
        },
    }
}

fn policy_for_kind(kind: ProviderAdapterKind) -> ProviderErrorPolicy {
    match kind {
        ProviderAdapterKind::OpenCodeGo => ProviderErrorPolicy {
            // Status-only default: Go uses 401 for ModelError as well as
            // account failures. classify_http_response refines only the exact
            // structured CreditsError case.
            inference_401: Auth401Policy::Passthrough,
            error_profile: ErrorProfile::OpenCodeGo,
        },
        ProviderAdapterKind::ZenFree => ProviderErrorPolicy {
            inference_401: Auth401Policy::Passthrough,
            error_profile: ErrorProfile::OpenCodeGo,
        },
        ProviderAdapterKind::CommandCodeGoat => ProviderErrorPolicy {
            inference_401: Auth401Policy::RotatePersistAuthError,
            error_profile: ErrorProfile::CommandCodeGoat,
        },
        ProviderAdapterKind::MiniMaxCn
        | ProviderAdapterKind::KimiCn
        | ProviderAdapterKind::OllamaCloud
        | ProviderAdapterKind::ConfigurableHttp
        | ProviderAdapterKind::Cpa => ProviderErrorPolicy {
            inference_401: Auth401Policy::RotatePersistAuthError,
            error_profile: ErrorProfile::GenericHttp,
        },
    }
}

fn is_opencode_family_provider(provider_id: &str) -> bool {
    matches!(
        provider_id,
        OPENCODE_PROVIDER_ID | OPENCODE_ZEN_FREE_PROVIDER_ID
    )
}

/// Public only as the cross-crate bridge; the host crate's `gateway::classify`
/// facade keeps this function crate-private.
#[doc(hidden)]
pub fn classify_preflight(kind: PreflightKind) -> ProviderErrorClass {
    match kind {
        PreflightKind::Route => ProviderErrorClass::RouteUnavailable,
        PreflightKind::Decrypt => ProviderErrorClass::DecryptFailed,
    }
}

/// Public only as the cross-crate bridge; the host crate's `gateway::classify`
/// facade keeps this function crate-private.
#[doc(hidden)]
pub fn classify_transport(input: TransportClassifyInput) -> ProviderErrorClass {
    match input {
        TransportClassifyInput::Connect => ProviderErrorClass::Connect,
        TransportClassifyInput::SendTimeout
        | TransportClassifyInput::HeaderTimeout
        | TransportClassifyInput::BodyTimeout
        | TransportClassifyInput::OtherSendFailure => ProviderErrorClass::OutcomeUnknown,
    }
}

/// Public only as the cross-crate bridge; the host crate's `gateway::classify`
/// facade keeps this function crate-private.
#[doc(hidden)]
pub fn classify_stream(input: StreamClassifyInput) -> ProviderErrorClass {
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

/// Public only as the cross-crate bridge; the host crate's `gateway::classify`
/// facade keeps this function crate-private.
#[doc(hidden)]
pub fn classify_http(
    status: u16,
    provider_id: &str,
    free_channel: bool,
    anonymous: bool,
) -> ProviderErrorClass {
    if (500..600).contains(&status) {
        return ProviderErrorClass::ServerError;
    }
    if status == 429 {
        let profile = match provider_error_policy(provider_id).error_profile {
            ErrorProfile::OpenCodeGo if free_channel => ErrorProfile::ZenFree,
            profile => profile,
        };
        return ProviderErrorClass::RateLimited { profile };
    }

    if status == 408 {
        return ProviderErrorClass::HttpRequestTimeout;
    }
    if status == 401 {
        return match provider_error_policy(provider_id).inference_401 {
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

/// Refines an HTTP classification with a bounded upstream response body.
///
/// OpenCode Go uses 401 for both unsupported models and account-level credit
/// failures. Only its structured `CreditsError` is strong enough evidence to
/// skip and break the current account; malformed, unknown, and `ModelError`
/// responses retain the conservative 401 passthrough behavior.
#[doc(hidden)]
pub fn classify_http_response(
    status: u16,
    provider_id: &str,
    free_channel: bool,
    anonymous: bool,
    response_body: &str,
) -> ProviderErrorClass {
    let base = classify_http(status, provider_id, free_channel, anonymous);
    if status == 401
        && ProviderAdapterKind::from_provider_id(provider_id)
            == Some(ProviderAdapterKind::OpenCodeGo)
        && response_has_error_type(response_body, "CreditsError")
    {
        ProviderErrorClass::UnauthorizedRotate
    } else if status == 400
        && !anonymous
        && ProviderAdapterKind::from_provider_id(provider_id)
            == Some(ProviderAdapterKind::CommandCodeGoat)
        && response_has_insufficient_credits(response_body)
    {
        ProviderErrorClass::InsufficientCredits
    } else {
        base
    }
}

// Only the observed GOAT account-level envelope is evidence. A context-length,
// model, or reasoning validation error must never trigger another billable send.
fn response_has_insufficient_credits(body: &str) -> bool {
    let Ok(value) = serde_json::from_str::<Value>(body) else {
        return false;
    };
    value.pointer("/error/code").and_then(Value::as_str) == Some("BAD_REQUEST")
        && value.pointer("/error/type").and_then(Value::as_str) == Some("invalid_request_error")
        && value.pointer("/error/message").and_then(Value::as_str).is_some_and(|message| {
            message == "You have insufficient credits to make this request."
                || message == "You have insufficient credits to make this request. Please purchase more credits to continue using the service."
        })
}

fn response_has_error_type(response_body: &str, expected: &str) -> bool {
    let Ok(value) = serde_json::from_str::<Value>(response_body) else {
        return false;
    };
    value
        .pointer("/error/type")
        .and_then(Value::as_str)
        .is_some_and(|kind| kind == expected)
}

/// Public only as the cross-crate bridge; the host crate's `gateway::classify`
/// facade keeps this function crate-private.
#[doc(hidden)]
pub fn schedule_go_usage_sync(class: ProviderErrorClass) -> bool {
    matches!(
        class,
        ProviderErrorClass::RateLimited {
            profile: ErrorProfile::OpenCodeGo
        }
    )
}

impl ProviderErrorClass {
    pub fn same_account_retry_eligible(self) -> bool {
        matches!(self, Self::Connect | Self::StreamRetryEligible)
    }
}

#[cfg(test)]
mod tests;
