//! Transport-boundary description of one upstream inference attempt.
//!
//! [`AttemptSpec`] is data only: endpoint/base URL, path, upstream protocol,
//! auth scheme, redirect policy, an opaque credential handle, and the
//! proxy-routing model. Provider adapters produce this value. They never
//! receive Host state, a database handle, or an HTTP client, and they never
//! see plaintext credentials.
//!
//! [`CredentialResolver`] is the Host-side seam. The single-attempt executor
//! resolves the handle, constructs the authorization header, and selects the
//! outbound client from [`ProxyRoutingModel`]. This slice does not rewrite
//! the outer fallback loop.
//!
//! [`AttemptTimeouts`] and [`AttemptTransportError`] describe the single POST
//! boundary. `forward_once` in `forwarder` performs exactly one `.send()` and
//! owns only transport selection and those timeouts.
//!
//! The temporary process-host resolver and `DbAttemptSink` live in `forwarder`
//! because `state.rs` / `db.rs` are outside this lease. A later host slice
//! should move the concrete resolver next to `KeyHost`.
//!
//! Items are rust-public only as the cross-crate bridge; the host crate's
//! `gateway::attempt` compatibility facade keeps them crate-private.

use crate::wire::WireNormalization;
use ocg_domain::protocol::ApiFormat;
use std::time::Duration;

/// Authentication belongs to the provider adapter, not to the wire
/// protocol. In particular, a Messages endpoint does not imply `x-api-key`
/// for every future provider.
///
/// Public only as the cross-crate bridge; the host crate's `gateway::attempt`
/// facade keeps this type crate-private.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[doc(hidden)]
pub enum UpstreamAuth {
    OpenCodeProtocolDefault,
    Bearer,
    XApiKey,
    None,
}

/// How the single-attempt executor selects an outbound client and URL/header
/// policy. This replaces `provider_id` / `custom_route` branches in the
/// forwarder send path.
///
/// Public only as the cross-crate bridge; the host crate's `gateway::attempt`
/// facade keeps this type crate-private.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[doc(hidden)]
pub enum ProxyRoutingModel {
    /// Frozen request-entry `ForwardRouteSet` snapshot (OpenCode Go / Zen Free).
    /// Follows redirects. Restricted URL (https or loopback http). Forwards
    /// harmless client headers.
    RequestEntrySnapshot,
    /// Process-wide default-leg client with redirects disabled (GOAT loopback).
    /// Restricted URL. Forwards harmless client headers.
    ProcessWideNoRedirect,
    /// User-operated local external integration. Direct connection only,
    /// redirects disabled, and client headers isolated.
    LocalExternalIntegration,
    /// Custom trusted-admin isolated client: process-wide proxy, no redirects,
    /// no client-header forwarding, administrator-trusted URL.
    IsolatedTrustedAdmin,
}

/// Opaque credential identity. Adapters store only an account id (or none);
/// plaintext is resolved by [`CredentialResolver`].
///
/// Public only as the cross-crate bridge; the host crate's `gateway::attempt`
/// facade keeps this type crate-private.
#[derive(Debug, Clone, PartialEq, Eq)]
#[doc(hidden)]
pub enum CredentialHandle {
    /// Anonymous / keyless route (Zen Free). Host must not decrypt.
    None,
    /// Host decrypts this account's stored key. Never contains plaintext.
    Account { id: String },
}

impl CredentialHandle {
    pub fn account_id(&self) -> Option<&str> {
        match self {
            Self::None => None,
            Self::Account { id } => Some(id.as_str()),
        }
    }
}

/// Data-only description of one upstream inference attempt.
///
/// Public only as the cross-crate bridge; the host crate's `gateway::attempt`
/// facade keeps this type and its fields crate-private.
#[derive(Debug, Clone, PartialEq, Eq)]
#[doc(hidden)]
pub struct AttemptSpec {
    #[doc(hidden)]
    pub base_url: String,
    #[doc(hidden)]
    pub path: String,
    #[doc(hidden)]
    pub upstream: ApiFormat,
    #[doc(hidden)]
    pub auth: UpstreamAuth,
    /// Redirect policy: OpenCode Go / Zen follow redirects; GOAT and Custom
    /// trusted-admin do not. The selected routing model must enforce the same
    /// policy; this field keeps that transport contract explicit and testable.
    #[doc(hidden)]
    pub follow_redirects: bool,
    #[doc(hidden)]
    pub credential: CredentialHandle,
    #[doc(hidden)]
    pub proxy_routing: ProxyRoutingModel,
    /// Fixed provider wire normalization applied per attempt. Data-only
    /// marker: every adapter except Ollama Cloud leaves it `None` and the
    /// forwarder must send the shared request-plan bytes untouched, keeping
    /// mixed candidate chains byte-isolated per family.
    #[doc(hidden)]
    pub wire_normalization: WireNormalization,
}

impl AttemptSpec {
    pub fn credential_account_id(&self) -> Option<&str> {
        self.credential.account_id()
    }

    /// Restricted OpenCode/GOAT URL check: https or loopback http. Custom
    /// trusted-admin destinations skip this.
    pub fn restricted_upstream_url(&self) -> bool {
        !matches!(self.proxy_routing, ProxyRoutingModel::IsolatedTrustedAdmin)
    }

    pub fn isolates_client_headers(&self) -> bool {
        matches!(
            self.proxy_routing,
            ProxyRoutingModel::IsolatedTrustedAdmin | ProxyRoutingModel::LocalExternalIntegration
        )
    }

    pub fn is_local_external_integration(&self) -> bool {
        matches!(
            self.proxy_routing,
            ProxyRoutingModel::LocalExternalIntegration
        )
    }

    /// Wire auth after OpenCode protocol-default mapping. Messages uses
    /// `x-api-key`; other OpenCode defaults use Bearer.
    pub fn wire_auth(&self) -> UpstreamAuth {
        match self.auth {
            UpstreamAuth::OpenCodeProtocolDefault if self.upstream == ApiFormat::Messages => {
                UpstreamAuth::XApiKey
            }
            UpstreamAuth::OpenCodeProtocolDefault => UpstreamAuth::Bearer,
            auth => auth,
        }
    }

    pub fn request_url(&self) -> Result<String, String> {
        let path = if self.path.is_empty() {
            self.upstream
                .upstream_path()
                .ok_or_else(|| "Gemini is a client-only protocol".to_string())?
                .to_string()
        } else {
            self.path.clone()
        };
        Ok(format!("{}{}", self.base_url.trim_end_matches('/'), path))
    }
}

/// Timeouts applied at the single-POST boundary. Non-stream uses the HTTP
/// client's per-request timeout; stream wraps `.send()` with a header-wait
/// timeout. Body idle timeouts, SSE conversion, and retry stay outside
/// `forward_once`.
///
/// Public only as the cross-crate bridge; the host crate's `gateway::attempt`
/// facade keeps this type and its fields crate-private.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[doc(hidden)]
pub struct AttemptTimeouts {
    #[doc(hidden)]
    pub non_stream: Duration,
    #[doc(hidden)]
    pub stream_header: Duration,
}

impl AttemptTimeouts {
    pub fn from_secs(non_stream: u64, stream_header: u64) -> Self {
        Self {
            non_stream: Duration::from_secs(non_stream),
            stream_header: Duration::from_secs(stream_header),
        }
    }
}

/// Classification kind of one host send failure. Connect is assigned first so a
/// connect timeout stays retry-eligible; [`TransportSendFailure::timed_out`] is
/// the independent timeout bit used for Stage 0 user/log text and HTTP status.
///
/// Public only as the cross-crate bridge; the host crate's `gateway::attempt`
/// facade keeps this type crate-private.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[doc(hidden)]
pub enum TransportFailureKind {
    Timeout,
    Connect,
    Other,
}

/// Neutral owned send failure. Captured at the host `.send()` site; does not
/// retain a concrete HTTP-client error type. `message` is the Stage 0 user/log
/// text (`upstream request timed out|failed: …`) copied from the host error
/// Display. Caller-side sanitizers still run on this string.
///
/// Public only as the cross-crate bridge; the host crate's `gateway::attempt`
/// facade keeps this type and its fields crate-private.
#[derive(Debug, Clone, PartialEq, Eq)]
#[doc(hidden)]
pub struct TransportSendFailure {
    #[doc(hidden)]
    pub kind: TransportFailureKind,
    #[doc(hidden)]
    pub timed_out: bool,
    #[doc(hidden)]
    pub message: String,
}

impl TransportSendFailure {
    /// Convert host send flags at the single `.send()` boundary.
    /// `is_connect` wins for [`Self::kind`]; `is_timeout` independently selects
    /// the Stage 0 timeout prefix and [`Self::timed_out`].
    pub fn from_send_error(is_connect: bool, is_timeout: bool, cause: impl Into<String>) -> Self {
        let cause = cause.into();
        let kind = if is_connect {
            TransportFailureKind::Connect
        } else if is_timeout {
            TransportFailureKind::Timeout
        } else {
            TransportFailureKind::Other
        };
        let message = if is_timeout {
            format!("upstream request timed out: {cause}")
        } else {
            format!("upstream request failed: {cause}")
        };
        Self {
            kind,
            timed_out: is_timeout,
            message,
        }
    }
}

impl std::fmt::Display for TransportSendFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

/// Errors from the single upstream POST. Classification, logging, cooldown,
/// CAS, usage scheduling, and retry stay in the caller.
///
/// Public only as the cross-crate bridge; the host crate's `gateway::attempt`
/// facade keeps this type crate-private.
#[derive(Debug)]
#[doc(hidden)]
pub enum AttemptTransportError {
    HeaderTimeout { timeout: Duration },
    Send(TransportSendFailure),
}

impl std::fmt::Display for AttemptTransportError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::HeaderTimeout { timeout } => write!(
                f,
                "upstream did not return response headers within {}s",
                timeout.as_secs()
            ),
            Self::Send(failure) => write!(f, "{failure}"),
        }
    }
}

impl std::error::Error for AttemptTransportError {}

/// Host-side seam: decrypts an opaque [`CredentialHandle`]. Provider adapters
/// never receive this trait or the resulting plaintext. The single-attempt
/// executor constructs the authorization header from the resolved secret and
/// [`AttemptSpec::wire_auth`].
///
/// Public only as the cross-crate bridge; the host crate's `gateway::attempt`
/// facade keeps this trait crate-private.
#[doc(hidden)]
pub trait CredentialResolver {
    fn resolve_credential(
        &self,
        handle: &CredentialHandle,
    ) -> Result<Option<String>, CredentialResolveError>;
}

/// Public only as the cross-crate bridge; the host crate's `gateway::attempt`
/// facade keeps this type crate-private.
#[derive(Debug)]
#[doc(hidden)]
pub enum CredentialResolveError {
    Decrypt(anyhow::Error),
    HandleMismatch { expected: String, actual: String },
}

impl std::fmt::Display for CredentialResolveError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Decrypt(error) => write!(f, "{error}"),
            Self::HandleMismatch { expected, actual } => write!(
                f,
                "credential handle `{actual}` does not match selected account `{expected}`"
            ),
        }
    }
}

impl std::error::Error for CredentialResolveError {}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(
        auth: UpstreamAuth,
        upstream: ApiFormat,
        follow_redirects: bool,
        credential: CredentialHandle,
        proxy_routing: ProxyRoutingModel,
    ) -> AttemptSpec {
        AttemptSpec {
            base_url: "https://opencode.ai/zen/go".into(),
            path: "/v1/chat/completions".into(),
            upstream,
            auth,
            follow_redirects,
            credential,
            proxy_routing,
            wire_normalization: WireNormalization::None,
        }
    }

    #[test]
    fn attempt_spec_is_data_only_and_describes_the_transport_boundary() {
        let spec = spec(
            UpstreamAuth::OpenCodeProtocolDefault,
            ApiFormat::ChatCompletions,
            true,
            CredentialHandle::Account { id: "go-1".into() },
            ProxyRoutingModel::RequestEntrySnapshot,
        );
        assert_eq!(spec.base_url, "https://opencode.ai/zen/go");
        assert_eq!(spec.path, "/v1/chat/completions");
        assert_eq!(spec.upstream, ApiFormat::ChatCompletions);
        assert_eq!(spec.auth, UpstreamAuth::OpenCodeProtocolDefault);
        assert!(spec.follow_redirects);
        assert_eq!(spec.credential_account_id(), Some("go-1"));
        assert_eq!(spec.proxy_routing, ProxyRoutingModel::RequestEntrySnapshot);
        assert!(spec.restricted_upstream_url());
        assert!(!spec.isolates_client_headers());
        assert_eq!(
            spec.request_url().unwrap(),
            "https://opencode.ai/zen/go/v1/chat/completions"
        );
        let debug = format!("{spec:?}");
        assert!(debug.contains("go-1"));
    }

    #[test]
    fn wire_auth_maps_opencode_protocol_default_without_provider_id() {
        let chat = spec(
            UpstreamAuth::OpenCodeProtocolDefault,
            ApiFormat::ChatCompletions,
            true,
            CredentialHandle::Account { id: "go-1".into() },
            ProxyRoutingModel::RequestEntrySnapshot,
        );
        assert_eq!(chat.wire_auth(), UpstreamAuth::Bearer);
        let messages = AttemptSpec {
            path: "/v1/messages".into(),
            upstream: ApiFormat::Messages,
            ..chat.clone()
        };
        assert_eq!(messages.wire_auth(), UpstreamAuth::XApiKey);
        let custom = spec(
            UpstreamAuth::XApiKey,
            ApiFormat::ChatCompletions,
            false,
            CredentialHandle::Account {
                id: "custom-1".into(),
            },
            ProxyRoutingModel::IsolatedTrustedAdmin,
        );
        assert_eq!(custom.wire_auth(), UpstreamAuth::XApiKey);
        assert!(!custom.restricted_upstream_url());
        assert!(custom.isolates_client_headers());
        assert!(!custom.follow_redirects);
        let goat = spec(
            UpstreamAuth::Bearer,
            ApiFormat::ChatCompletions,
            false,
            CredentialHandle::Account {
                id: "goat-1".into(),
            },
            ProxyRoutingModel::ProcessWideNoRedirect,
        );
        assert!(goat.restricted_upstream_url());
        assert!(!goat.isolates_client_headers());
        let zen = spec(
            UpstreamAuth::None,
            ApiFormat::ChatCompletions,
            true,
            CredentialHandle::None,
            ProxyRoutingModel::RequestEntrySnapshot,
        );
        assert_eq!(zen.wire_auth(), UpstreamAuth::None);
        assert!(zen.credential_account_id().is_none());
    }

    #[test]
    fn empty_path_uses_upstream_protocol_path() {
        let spec = AttemptSpec {
            path: String::new(),
            ..spec(
                UpstreamAuth::Bearer,
                ApiFormat::Responses,
                true,
                CredentialHandle::None,
                ProxyRoutingModel::RequestEntrySnapshot,
            )
        };
        assert_eq!(
            spec.request_url().unwrap(),
            "https://opencode.ai/zen/go/v1/responses"
        );
        let gemini = AttemptSpec {
            path: String::new(),
            upstream: ApiFormat::Gemini,
            ..spec
        };
        assert!(
            gemini
                .request_url()
                .unwrap_err()
                .contains("Gemini is a client-only protocol")
        );
    }

    #[test]
    fn attempt_timeouts_are_transport_durations_only() {
        let timeouts = AttemptTimeouts::from_secs(900, 300);
        assert_eq!(timeouts.non_stream, Duration::from_secs(900));
        assert_eq!(timeouts.stream_header, Duration::from_secs(300));
        assert_ne!(timeouts.non_stream, timeouts.stream_header);
    }

    #[test]
    fn attempt_transport_error_messages_match_stage0_send_text() {
        let header = AttemptTransportError::HeaderTimeout {
            timeout: Duration::from_secs(300),
        };
        assert_eq!(
            header.to_string(),
            "upstream did not return response headers within 300s"
        );
        assert!(matches!(
            header,
            AttemptTransportError::HeaderTimeout { .. }
        ));
    }

    #[test]
    fn send_failure_kinds_keep_stage0_display_and_connect_first_classification() {
        let timeout = TransportSendFailure::from_send_error(false, true, "operation timed out");
        assert_eq!(timeout.kind, TransportFailureKind::Timeout);
        assert!(timeout.timed_out);
        assert_eq!(
            timeout.message,
            "upstream request timed out: operation timed out"
        );
        let timeout_err = AttemptTransportError::Send(timeout.clone());
        assert_eq!(timeout_err.to_string(), timeout.message);
        assert!(!matches!(
            timeout_err,
            AttemptTransportError::HeaderTimeout { .. }
        ));

        let connect = TransportSendFailure::from_send_error(true, false, "connection refused");
        assert_eq!(connect.kind, TransportFailureKind::Connect);
        assert!(!connect.timed_out);
        assert_eq!(
            connect.message,
            "upstream request failed: connection refused"
        );
        assert_eq!(
            AttemptTransportError::Send(connect.clone()).to_string(),
            connect.message
        );

        let connect_timeout =
            TransportSendFailure::from_send_error(true, true, "error trying to connect");
        assert_eq!(connect_timeout.kind, TransportFailureKind::Connect);
        assert!(connect_timeout.timed_out);
        assert_eq!(
            connect_timeout.message,
            "upstream request timed out: error trying to connect"
        );

        let other = TransportSendFailure::from_send_error(false, false, "connection reset");
        assert_eq!(other.kind, TransportFailureKind::Other);
        assert!(!other.timed_out);
        assert_eq!(other.message, "upstream request failed: connection reset");
        assert_eq!(
            AttemptTransportError::Send(other.clone()).to_string(),
            other.message
        );
    }
}
