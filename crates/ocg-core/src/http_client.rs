use crate::models::{AppConfig, ProxyMode};
use std::time::Duration;

/// Applies the process-wide outbound proxy policy while leaving callers free to
/// choose their own redirect, total-timeout, and response-size policies.
///
/// Custom API traffic must compose on this builder via [`crate::custom_http`]
/// rather than [`build`]. `build()` is the shared Go/Zen client and must not
/// grow Custom DNS pinning, `redirect::Policy::none()`, or preflight. Custom
/// still inherits Auto/Manual/Direct from here so the global proxy stays
/// authoritative.
pub(crate) fn configured_builder(config: &AppConfig) -> crate::Result<reqwest::ClientBuilder> {
    let builder = match config.proxy_mode {
        ProxyMode::Auto => reqwest::Client::builder(),
        ProxyMode::Manual => {
            reqwest::Client::builder().proxy(reqwest::Proxy::all(config.proxy_url.as_str())?)
        }
        ProxyMode::Direct => reqwest::Client::builder().no_proxy(),
    };
    Ok(builder
        // Drop idle pooled connections earlier than the default so a stale connection
        // closed by the upstream/CDN isn't reused. Keep-alive probes further reduce
        // silent drops for long-lived gateways.
        .pool_idle_timeout(Duration::from_secs(30))
        .tcp_keepalive(Duration::from_secs(30)))
}

pub(crate) fn build(config: &AppConfig) -> crate::Result<reqwest::Client> {
    Ok(configured_builder(config)?
        .connect_timeout(Duration::from_secs(config.connect_timeout_secs))
        .build()?)
}
