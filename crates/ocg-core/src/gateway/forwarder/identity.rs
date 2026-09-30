//! Provider identity headers applied before the outbound send.

use super::*;

#[cfg(test)]
mod tests;

const OPENCODE_ZEN_FREE_CLIENT: &str = "cli";
const OPENCODE_ZEN_FREE_USER_AGENT: &str = "opencode";

pub(super) fn identity_headers_enabled(adapter: ProviderAdapterKind) -> bool {
    sealed_capabilities(AdapterKind::from(adapter)).identity_headers
}

/// Shared by inference and operational probes so a working account is not
/// rejected merely because its test omitted provider-required identity.
/// Session / anonymous headers follow destination `identity_headers` and
/// adapter kind, not a reserved provider UUID.
pub fn apply_provider_identity_headers(
    upstream: &mut reqwest::header::HeaderMap,
    client_headers: &HeaderMap,
    adapter: ProviderAdapterKind,
    client: ApiFormat,
    model: &str,
    body: &[u8],
    request_id: &str,
) {
    if !identity_headers_enabled(adapter) {
        return;
    }
    let session = resolve_opencode_session_header(client_headers, client, model, body, request_id);
    upstream.insert("x-opencode-session", session.clone());
    copy_explicit_opencode_identity_headers(upstream, client_headers);
    if adapter == ProviderAdapterKind::ZenFree {
        apply_zen_free_identity_headers(upstream, &session, request_id);
    }
}

pub(super) fn resolve_opencode_session_header(
    headers: &HeaderMap,
    client: ApiFormat,
    model: &str,
    client_body: &[u8],
    request_id: &str,
) -> reqwest::header::HeaderValue {
    headers
        .get("x-opencode-session")
        .or_else(|| headers.get("x-session-id"))
        .or_else(|| headers.get("x-session-affinity"))
        .cloned()
        .or_else(|| {
            resolve_conversation_key(client, model, headers, client_body)
                .and_then(|key| format!("ocg-{key}").parse().ok())
        })
        .unwrap_or_else(|| {
            request_id
                .parse()
                .expect("generated request id must be a valid header value")
        })
}

pub(crate) fn copy_explicit_opencode_identity_headers(
    upstream: &mut reqwest::header::HeaderMap,
    client: &HeaderMap,
) {
    for name in [
        "x-opencode-client",
        "x-opencode-request",
        "x-opencode-project",
    ] {
        if let Some(value) = client.get(name) {
            upstream.insert(name, value.clone());
        }
    }
}

pub(super) fn apply_zen_free_identity_headers(
    headers: &mut reqwest::header::HeaderMap,
    session: &reqwest::header::HeaderValue,
    request_id: &str,
) {
    if headers.get("x-opencode-client").is_none() {
        headers.insert(
            "x-opencode-client",
            reqwest::header::HeaderValue::from_static(OPENCODE_ZEN_FREE_CLIENT),
        );
    }
    if headers.get("x-opencode-request").is_none()
        && let Ok(value) = request_id.parse()
    {
        headers.insert("x-opencode-request", value);
    }
    if headers.get("x-opencode-project").is_none() {
        headers.insert("x-opencode-project", session.clone());
    }
    let ua_is_opencode = headers
        .get(reqwest::header::USER_AGENT)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.to_ascii_lowercase().contains("opencode"));
    if !ua_is_opencode {
        headers.insert(
            reqwest::header::USER_AGENT,
            reqwest::header::HeaderValue::from_static(OPENCODE_ZEN_FREE_USER_AGENT),
        );
    }
}
