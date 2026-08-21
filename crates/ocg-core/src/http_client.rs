use crate::models::{AppConfig, ProxyMode};
use reqwest::redirect::Policy;
use std::time::Duration;

/// Applies the process-wide outbound proxy policy while leaving callers free to
/// choose their own redirect, total-timeout, and response-size policies.
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

pub(crate) fn no_redirect_policy() -> Policy {
    Policy::none()
}

/// Same global proxy policy as [`build`], with redirects disabled. Command Code
/// GOAT inference uses this seam; it must not follow Location hop-off.
pub(crate) fn build_no_redirect(config: &AppConfig) -> crate::Result<reqwest::Client> {
    Ok(configured_builder(config)?
        .redirect(no_redirect_policy())
        .connect_timeout(Duration::from_secs(config.connect_timeout_secs))
        .build()?)
}

pub(crate) fn build(config: &AppConfig) -> crate::Result<reqwest::Client> {
    Ok(configured_builder(config)?
        .connect_timeout(Duration::from_secs(config.connect_timeout_secs))
        .build()?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::AppConfig;
    use axum::Router;
    use axum::http::StatusCode;
    use axum::response::Redirect;
    use axum::routing::get;

    #[test]
    fn no_redirect_builder_keeps_global_proxy_and_disables_follow() {
        let mut config = AppConfig::default();
        config.proxy_mode = ProxyMode::Direct;
        let client = build_no_redirect(&config).expect("no-redirect client");
        let _ = client;
        let auto = AppConfig::default();
        assert!(matches!(auto.proxy_mode, ProxyMode::Auto));
        let _ = configured_builder(&auto)
            .expect("proxy builder")
            .redirect(no_redirect_policy());
    }

    #[tokio::test]
    async fn no_redirect_client_does_not_follow_location() {
        let app = Router::new()
            .route("/from", get(|| async { Redirect::temporary("/to") }))
            .route("/to", get(|| async { "followed" }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let mut config = AppConfig::default();
        config.proxy_mode = ProxyMode::Direct;
        let client = build_no_redirect(&config).unwrap();
        let response = client
            .get(format!("http://{addr}/from"))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::TEMPORARY_REDIRECT);
        let body = response.text().await.unwrap();
        assert!(!body.contains("followed"));
    }
}
