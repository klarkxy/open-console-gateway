//! Host facade for pure status/auth/transport decoding.
//! Limit facts and their unified policy live in `failure`; recovery scheduling
//! lives in `recovery`, separate from durable account quota windows.
use crate::models::{UpstreamChannel, UsageWindowKind};
pub(crate) use ocg_gateway::classify::{
    PreflightKind, ProviderErrorClass, RateLimitFallback, StreamClassifyInput,
    TransportClassifyInput, classify_preflight, classify_stream, classify_transport,
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

pub(crate) fn rate_limit_fallback(window: Option<UsageWindowKind>) -> RateLimitFallback {
    if window == Some(UsageWindowKind::Free) {
        RateLimitFallback::ExhaustFreeChannel
    } else {
        RateLimitFallback::TryNextAccount
    }
}

#[cfg(test)]
mod tests;
