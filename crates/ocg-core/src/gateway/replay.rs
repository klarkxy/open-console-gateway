//! Observed-route domain for native opaque history.
//!
//! The digest is a public, non-secret identity. It never includes a Key,
//! ciphertext, or plaintext credential. [`ocg_gateway::protocol::ReplayDomain`]
//! only checks that this digest matches the marker already stored by the client.

use ocg_domain::destination::AdapterKind;
use ocg_gateway::attempt::UpstreamAuth;
use ocg_gateway::protocol::ReplayDomain;
use sha2::{Digest, Sha256};

use super::protocol::{ApiFormat, ProtocolError};

/// Facts that define one observed send route. All of them are stable across
/// process restarts. Rotation is represented by `credential_version`.
pub(crate) struct ReplayRouteIdentity<'a> {
    pub adapter: AdapterKind,
    pub upstream: ApiFormat,
    pub upstream_model: &'a str,
    pub request_url: &'a str,
    pub credential_id: &'a str,
    pub credential_version: u64,
    pub destination_id: &'a str,
    pub authorization_connection_id: &'a str,
    pub binding_id: &'a str,
    pub auth: UpstreamAuth,
}

pub(crate) fn replay_domain_for(
    identity: &ReplayRouteIdentity<'_>,
) -> Result<ReplayDomain, ProtocolError> {
    let digest = Sha256::digest(canonical_identity(identity).as_bytes());
    let token = hex_encode(&digest);
    ReplayDomain::parse(&token).map_err(|error| ProtocolError::new(error.message))
}

fn canonical_identity(identity: &ReplayRouteIdentity<'_>) -> String {
    format!(
        "ocg-replay-domain-v1\nadapter={adapter}\nupstream={upstream}\nmodel={model}\nurl={url}\ncredential_id={credential_id}\ncredential_version={credential_version}\ndestination_id={destination_id}\nauthorization_connection_id={authorization_connection_id}\nbinding_id={binding_id}\nauth_scheme={auth}\n",
        adapter = identity.adapter.as_str(),
        upstream = upstream_name(identity.upstream),
        model = identity.upstream_model,
        url = identity.request_url,
        credential_id = identity.credential_id,
        credential_version = identity.credential_version,
        destination_id = identity.destination_id,
        authorization_connection_id = identity.authorization_connection_id,
        binding_id = identity.binding_id,
        auth = auth_name(identity.auth),
    )
}

fn upstream_name(upstream: ApiFormat) -> &'static str {
    match upstream {
        ApiFormat::ChatCompletions => "chat_completions",
        ApiFormat::Responses => "responses",
        ApiFormat::Messages => "messages",
        ApiFormat::Gemini => "gemini",
    }
}

fn auth_name(auth: UpstreamAuth) -> &'static str {
    match auth {
        UpstreamAuth::OpenCodeProtocolDefault => "opencode_protocol_default",
        UpstreamAuth::Bearer => "bearer",
        UpstreamAuth::XApiKey => "x_api_key",
        UpstreamAuth::ApiKey => "api_key",
        UpstreamAuth::None => "none",
    }
}

fn hex_encode(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(HEX[(byte >> 4) as usize] as char);
        out.push(HEX[(byte & 0x0f) as usize] as char);
    }
    out
}

#[cfg(test)]
mod tests;
