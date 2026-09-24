//! Compatibility facade for process-wide outbound proxy routing.
//!
//! Catalog-stripped client construction lives in [`ocg_infra::http`]. This
//! module maps [`AppConfig`] onto that spec, filters list membership through
//! the caller-supplied exact upstream ids, and matches list membership by
//! [`model_identity_key`] before infra exact-match.

use crate::kernel::ids::{model_identity_key, normalize_model_name};
use crate::models::{AppConfig, ProxyListDirection, ProxyMode};
use std::time::Duration;

pub(crate) use ocg_infra::http::{RouteLabel, no_redirect_policy};

pub(crate) fn outbound_proxy_spec(config: &AppConfig) -> ocg_infra::http::OutboundProxySpec {
    ocg_infra::http::OutboundProxySpec {
        mode: match config.proxy_mode {
            ProxyMode::Auto => ocg_infra::http::ProxyMode::Auto,
            ProxyMode::Manual => ocg_infra::http::ProxyMode::Manual,
            ProxyMode::Direct => ocg_infra::http::ProxyMode::Direct,
            ProxyMode::List => ocg_infra::http::ProxyMode::List,
        },
        proxy_url: config.proxy_url.clone(),
        connect_timeout: Duration::from_secs(config.connect_timeout_secs),
        list_direction: match config.proxy_list_direction {
            ProxyListDirection::Whitelist => ocg_infra::http::ProxyListDirection::Whitelist,
            ProxyListDirection::Blacklist => ocg_infra::http::ProxyListDirection::Blacklist,
        },
    }
}

/// One atomic routing unit: the routing metadata and both leg clients are
/// generated from the same `AppConfig` generation, so a snapshot held by an
/// in-flight request can never mix new metadata with old clients. Non-list
/// modes keep `exception_client` unset and always resolve to the default leg.
///
/// Lookup keys use real model identity: trimmed, case-folded, separators kept.
pub(crate) struct ForwardRouteSet(ocg_infra::http::ForwardRouteSet);

impl ForwardRouteSet {
    /// Pure, lock-free route resolution for one forwarding attempt.
    pub(crate) fn client_for(&self, model: &str) -> (&reqwest::Client, RouteLabel) {
        self.0.client_for(&model_identity_key(model))
    }

    /// The default leg client used by non-model-scoped outbound callers
    /// (`upstream_context` and friends).
    pub(crate) fn default_client(&self) -> &reqwest::Client {
        self.0.default_client()
    }
}

/// Registry ids normalized once at build time; empty or stale entries
/// simply never match (total function over any persisted shape).
///
/// Stale entries — ids a newer candidate set no longer contains — are dropped
/// here so they stay inert even if a client explicitly requests that exact id.
fn normalized_known_list(models: &[String], known_models: &[String]) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    let mut resolved = Vec::new();
    for model in models {
        let Some(identity) = resolve_proxy_list_entry(model, known_models) else {
            continue;
        };
        if seen.insert(identity.clone()) {
            resolved.push(identity);
        }
    }
    resolved
}

/// Exact identity wins. A historical folded name is kept only when it resolves
/// to one current model; a collision is dropped instead of matching every
/// lookalike.
fn resolve_proxy_list_entry(saved: &str, known_models: &[String]) -> Option<String> {
    let identity = model_identity_key(saved);
    if identity.is_empty() {
        return None;
    }
    let exact = known_models
        .iter()
        .filter(|model| model_identity_key(model) == identity)
        .count();
    if exact == 1 {
        return Some(identity);
    }
    if exact > 1 {
        return None;
    }
    let folded = normalize_model_name(saved);
    let mut hits = known_models
        .iter()
        .filter(|model| normalize_model_name(model) == folded);
    let first = hits.next()?;
    if hits.next().is_some() {
        return None;
    }
    Some(model_identity_key(first))
}

/// Same global proxy policy as [`ocg_infra::http::build`], with redirects
/// disabled. Command Code GOAT inference uses this seam; it must not follow
/// Location hop-off.
pub(crate) fn build_no_redirect(config: &AppConfig) -> crate::Result<reqwest::Client> {
    ocg_infra::http::build_no_redirect(&outbound_proxy_spec(config))
}

pub(crate) fn build_no_redirect_for_route(
    config: &AppConfig,
    route: RouteLabel,
) -> crate::Result<reqwest::Client> {
    ocg_infra::http::build_no_redirect_for_label(&outbound_proxy_spec(config), route)
}

/// Applies the process-wide outbound proxy policy while leaving callers free to
/// choose their own redirect, total-timeout, and response-size policies. Under
/// list mode this builds the direction's default leg: whitelist default is
/// direct, blacklist default is the manual proxy URL.
pub(crate) fn configured_builder(config: &AppConfig) -> crate::Result<reqwest::ClientBuilder> {
    ocg_infra::http::configured_builder(&outbound_proxy_spec(config))
}

#[cfg(test)]
pub(crate) fn build(config: &AppConfig) -> crate::Result<reqwest::Client> {
    ocg_infra::http::build(&outbound_proxy_spec(config))
}

/// Builds the full route set from one config generation. List mode builds both
/// legs against the candidate-filtered membership list; every other mode builds
/// exactly the process-wide client. All modes go through
/// [`ocg_infra::http::build_route_set`] so the reqwest client and audit label
/// are generated atomically from one [`ocg_infra::http::OutboundProxySpec`].
#[cfg(test)]
pub(crate) fn build_route_set(
    config: &AppConfig,
    known_models: &[String],
) -> crate::Result<ForwardRouteSet> {
    build_route_set_from_known_models(config, known_models)
}

pub(crate) fn build_route_set_from_known_models(
    config: &AppConfig,
    known_models: &[String],
) -> crate::Result<ForwardRouteSet> {
    let list = match config.proxy_mode {
        ProxyMode::List => normalized_known_list(&config.proxy_list_models, known_models),
        ProxyMode::Auto | ProxyMode::Manual | ProxyMode::Direct => Vec::new(),
    };
    Ok(ForwardRouteSet(ocg_infra::http::build_route_set(
        &outbound_proxy_spec(config),
        list,
    )?))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::Router;
    use axum::http::StatusCode;
    use axum::response::Redirect;
    use axum::routing::get;

    fn known(ids: &[&str]) -> Vec<String> {
        ids.iter().map(|id| (*id).to_string()).collect()
    }

    #[test]
    fn no_redirect_builder_succeeds_for_direct() {
        let config = AppConfig {
            proxy_mode: ProxyMode::Direct,
            ..AppConfig::default()
        };
        let client = build_no_redirect(&config).expect("no-redirect client");
        let _ = client;
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
        let config = AppConfig {
            proxy_mode: ProxyMode::Direct,
            ..AppConfig::default()
        };
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

    #[tokio::test]
    async fn s02_no_redirect_client_does_not_follow_with_authorization() {
        let app = Router::new()
            .route("/from", get(|| async { Redirect::temporary("/to") }))
            .route("/to", get(|| async { "followed-with-secret" }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let config = AppConfig {
            proxy_mode: ProxyMode::Direct,
            ..AppConfig::default()
        };
        let client = build_no_redirect(&config).unwrap();
        let response = client
            .get(format!("http://{addr}/from"))
            .header(reqwest::header::AUTHORIZATION, "Bearer sk-secret")
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::TEMPORARY_REDIRECT);
        let body = response.text().await.unwrap();
        assert!(!body.contains("followed-with-secret"));
    }

    fn list_config(direction: ProxyListDirection, models: &[&str]) -> AppConfig {
        AppConfig {
            gateway_key: "k".to_string(),
            proxy_mode: ProxyMode::List,
            proxy_url: "http://127.0.0.1:7890".to_string(),
            proxy_list_direction: direction,
            proxy_list_models: models.iter().map(|model| model.to_string()).collect(),
            ..AppConfig::default()
        }
    }

    #[test]
    fn client_for_resolves_both_directions_and_tolerates_stale_entries() {
        // Whitelist: listed -> proxy leg, unlisted/unknown/stale -> direct leg.
        let whitelist = build_route_set(
            &list_config(
                ProxyListDirection::Whitelist,
                &["gpt-5.6-luna", "removed-model"],
            ),
            &known(&["gpt-5.6-luna"]),
        )
        .unwrap();
        assert_eq!(whitelist.client_for("gpt-5.6-luna").1, RouteLabel::Proxy);
        assert_eq!(
            whitelist.client_for("GPT-5.6-Luna").1,
            RouteLabel::Proxy,
            "matching folds case and keeps separators"
        );
        assert_eq!(
            whitelist.client_for("gpt_5.6 luna").1,
            RouteLabel::Direct,
            "a different separator is a different model"
        );
        assert_eq!(whitelist.client_for("glm-5.3").1, RouteLabel::Direct);
        assert_eq!(
            whitelist.client_for("removed-model").1,
            RouteLabel::Direct,
            "stale ids stored in old configs never match"
        );

        // Blacklist inverts both legs.
        let blacklist = build_route_set(
            &list_config(ProxyListDirection::Blacklist, &["grok-4.5"]),
            &known(&["grok-4.5", "glm-5.3"]),
        )
        .unwrap();
        assert_eq!(blacklist.client_for("grok-4.5").1, RouteLabel::Direct);
        assert_eq!(blacklist.client_for("glm-5.3").1, RouteLabel::Proxy);

        let distinct = build_route_set(
            &list_config(ProxyListDirection::Whitelist, &["vendor/model"]),
            &known(&["vendor/model", "vendor-model"]),
        )
        .unwrap();
        assert_eq!(distinct.client_for("vendor/model").1, RouteLabel::Proxy);
        assert_eq!(distinct.client_for("vendor-model").1, RouteLabel::Direct);

        let migrated = build_route_set(
            &list_config(ProxyListDirection::Whitelist, &["vendor-model"]),
            &known(&["vendor/model"]),
        )
        .unwrap();
        assert_eq!(
            migrated.client_for("vendor/model").1,
            RouteLabel::Proxy,
            "a folded legacy name that resolves to one model stays on that model"
        );
        let collided = build_route_set(
            &list_config(ProxyListDirection::Whitelist, &["vendor_model"]),
            &known(&["vendor/model", "vendor-model"]),
        )
        .unwrap();
        assert_eq!(collided.client_for("vendor/model").1, RouteLabel::Direct);
        assert_eq!(collided.client_for("vendor-model").1, RouteLabel::Direct);

        // Empty list: whitelist = all direct, blacklist = all proxy.
        let empty_whitelist = build_route_set(
            &list_config(ProxyListDirection::Whitelist, &[]),
            &known(&["gpt-5.6-luna"]),
        )
        .unwrap();
        assert_eq!(
            empty_whitelist.client_for("gpt-5.6-luna").1,
            RouteLabel::Direct
        );
        let empty_blacklist = build_route_set(
            &list_config(ProxyListDirection::Blacklist, &[]),
            &known(&["gpt-5.6-luna"]),
        )
        .unwrap();
        assert_eq!(
            empty_blacklist.client_for("gpt-5.6-luna").1,
            RouteLabel::Proxy
        );
    }

    #[test]
    fn non_list_modes_use_infra_spec_derived_route_labels() {
        // Auto/Manual/Direct go through ocg_infra::http::build_route_set, so
        // the audit label is bound to the spec rather than a public
        // (client, label) pairing. List membership on the config is ignored.
        for (mode, label) in [
            (ProxyMode::Auto, RouteLabel::Auto),
            (ProxyMode::Manual, RouteLabel::Proxy),
            (ProxyMode::Direct, RouteLabel::Direct),
        ] {
            let config = AppConfig {
                gateway_key: "k".to_string(),
                proxy_mode: mode,
                proxy_url: "http://127.0.0.1:7890".to_string(),
                proxy_list_direction: ProxyListDirection::Whitelist,
                proxy_list_models: vec!["gpt-5.6-luna".to_string()],
                ..AppConfig::default()
            };
            let route_set = build_route_set(&config, &known(&["gpt-5.6-luna"])).unwrap();
            assert_eq!(route_set.client_for("gpt-5.6-luna").1, label);
            assert_eq!(route_set.client_for("glm-5.3").1, label);
            let (client, resolved) = route_set.client_for("gpt-5.6-luna");
            assert!(
                std::ptr::eq(client, route_set.default_client()),
                "non-list modes must resolve only the default leg"
            );
            assert_eq!(resolved, label);
        }
    }

    #[test]
    fn refreshed_zen_models_are_known_and_removed_models_become_inert() {
        let config = list_config(
            ProxyListDirection::Whitelist,
            &["brand-new-promo-free", "mimo-v2.5-free"],
        );
        let routes = build_route_set(&config, &known(&["brand-new-promo-free"])).unwrap();
        assert_eq!(
            routes.client_for("brand-new-promo-free").1,
            RouteLabel::Proxy
        );
        assert_eq!(routes.client_for("mimo-v2.5-free").1, RouteLabel::Direct);
    }

    #[test]
    fn sealed_cn_contract_models_participate_in_list_routing() {
        let config = AppConfig {
            proxy_mode: ProxyMode::List,
            proxy_url: "http://127.0.0.1:7890".to_string(),
            proxy_list_direction: ProxyListDirection::Whitelist,
            proxy_list_models: vec!["MiniMax-M3".to_string(), "kimi-for-coding".to_string()],
            ..AppConfig::default()
        };
        let routes =
            build_route_set_from_known_models(&config, &known(&["MiniMax-M3", "kimi-for-coding"]))
                .unwrap();
        assert_eq!(routes.client_for("MiniMax-M3").1, RouteLabel::Proxy);
        assert_eq!(routes.client_for("kimi-for-coding").1, RouteLabel::Proxy);
    }

    #[tokio::test]
    async fn list_mode_builds_the_direction_default_leg_into_configured_builder() {
        // A tiny upstream that accepts one request and answers 204.
        async fn spawn_upstream() -> std::net::SocketAddr {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let address = listener.local_addr().unwrap();
            tokio::spawn(async move {
                use tokio::io::{AsyncReadExt, AsyncWriteExt};
                if let Ok((mut stream, _)) = listener.accept().await {
                    let mut buffer = vec![0_u8; 4096];
                    let _ = stream.read(&mut buffer).await;
                    let _ = stream
                        .write_all(
                            b"HTTP/1.1 204 No Content\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                        )
                        .await;
                }
            });
            address
        }

        let upstream = spawn_upstream().await;
        // Blacklist default leg goes through an unreachable proxy URL and must
        // fail instead of silently connecting to the reachable upstream.
        let closed_proxy = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let closed_proxy_url = format!("http://{}", closed_proxy.local_addr().unwrap());
        drop(closed_proxy);

        let mut blacklist = list_config(ProxyListDirection::Blacklist, &[]);
        blacklist.proxy_url = closed_proxy_url;
        let client = crate::http_client::build(&blacklist).unwrap();
        let error = client
            .get(format!("http://{upstream}"))
            .timeout(Duration::from_secs(5))
            .send()
            .await
            .expect_err("blacklist default leg must route through the proxy URL");
        assert!(error.is_connect() || error.is_request(), "{error}");

        // Whitelist default leg connects directly: reachable upstream answers.
        let upstream = spawn_upstream().await;
        let whitelist = list_config(ProxyListDirection::Whitelist, &[]);
        let client = crate::http_client::build(&whitelist).unwrap();
        let response = client
            .get(format!("http://{upstream}"))
            .timeout(Duration::from_secs(5))
            .send()
            .await
            .expect("whitelist default leg must connect directly")
            .status();
        assert_eq!(response.as_u16(), 204);
    }
}
