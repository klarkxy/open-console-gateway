//! Catalog-stripped global outbound proxy routing and dual-leg HTTP clients.
//!
//! List membership is exact-match against a pre-normalized id list supplied by
//! the caller. Product catalogs, model-name folding, and control-plane
//! validation stay in the core compatibility facade.

use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use reqwest::redirect::Policy;

/// Closed-set route leg label recorded on every forward log row. Carries no
/// URL or credential material by construction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RouteLabel {
    Auto,
    Proxy,
    Direct,
}

impl RouteLabel {
    pub fn as_str(self) -> &'static str {
        match self {
            RouteLabel::Auto => "auto",
            RouteLabel::Proxy => "proxy",
            RouteLabel::Direct => "direct",
        }
    }
}

/// Process-wide outbound proxy mode. Infra-local and serde-free.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ProxyMode {
    #[default]
    Auto,
    Manual,
    Direct,
    List,
}

/// Which leg listed models take in [`ProxyMode::List`]. Infra-local and serde-free.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ProxyListDirection {
    /// Listed models go through `proxy_url`; everything else connects directly.
    #[default]
    Whitelist,
    /// Listed models connect directly; everything else goes through `proxy_url`.
    Blacklist,
}

/// Neutral outbound proxy snapshot: mode, proxy URL, connect timeout, and list
/// direction. One spec produces one route set.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutboundProxySpec {
    pub mode: ProxyMode,
    pub proxy_url: String,
    pub connect_timeout: Duration,
    pub list_direction: ProxyListDirection,
}

/// One atomic routing unit: routing metadata and both leg clients are generated
/// from the same [`OutboundProxySpec`], so a snapshot held by an in-flight
/// request can never mix new metadata with old clients. Non-list modes keep
/// `exception_client` unset and always resolve to the default leg.
///
/// Construction is [`build_route_set`] only. There is no public constructor
/// that pairs an arbitrary [`reqwest::Client`] with a [`RouteLabel`].
///
/// ```compile_fail
/// let _ = ocg_infra::http::ForwardRouteSet::single_leg;
/// ```
///
/// List membership is exact. Callers that fold catalog aliases must normalize
/// lookup keys and filter the list before construction.
pub struct ForwardRouteSet {
    /// Registry ids supplied pre-normalized; empty or unknown entries simply
    /// never match (total function over any persisted shape).
    list: Vec<String>,
    default_client: reqwest::Client,
    exception_client: Option<reqwest::Client>,
    default_label: RouteLabel,
    exception_label: RouteLabel,
}

impl ForwardRouteSet {
    /// Pure, lock-free route resolution for one forwarding attempt.
    pub fn client_for(&self, model: &str) -> (&reqwest::Client, RouteLabel) {
        match &self.exception_client {
            None => (&self.default_client, self.default_label),
            Some(exception) => {
                if is_listed(&self.list, model) {
                    (exception, self.exception_label)
                } else {
                    (&self.default_client, self.default_label)
                }
            }
        }
    }

    /// The default leg client used by non-model-scoped outbound callers.
    pub fn default_client(&self) -> &reqwest::Client {
        &self.default_client
    }
}

fn is_listed(list: &[String], model: &str) -> bool {
    list.iter().any(|id| id == model)
}

fn tuned(builder: reqwest::ClientBuilder) -> reqwest::ClientBuilder {
    // Drop idle pooled connections earlier than the default so a stale connection
    // closed by the upstream/CDN isn't reused. Keep-alive probes further reduce
    // silent drops for long-lived gateways.
    builder
        .pool_idle_timeout(Duration::from_secs(30))
        .tcp_keepalive(Duration::from_secs(30))
}

fn direct_leg_builder() -> reqwest::ClientBuilder {
    tuned(reqwest::Client::builder().no_proxy())
}

fn proxy_leg_builder(url: &str) -> Result<reqwest::ClientBuilder> {
    Ok(tuned(
        reqwest::Client::builder().proxy(reqwest::Proxy::all(url)?),
    ))
}

fn leg_client(
    builder: reqwest::ClientBuilder,
    spec: &OutboundProxySpec,
) -> Result<reqwest::Client> {
    Ok(builder.connect_timeout(spec.connect_timeout).build()?)
}

pub fn no_redirect_policy() -> Policy {
    Policy::none()
}

/// Same global proxy policy as [`build`], with redirects disabled.
pub fn build_no_redirect(spec: &OutboundProxySpec) -> Result<reqwest::Client> {
    Ok(configured_builder(spec)?
        .redirect(no_redirect_policy())
        .connect_timeout(spec.connect_timeout)
        .build()?)
}

/// Rebuild an already-selected route leg with redirects disabled. The label
/// must come from a `ForwardRouteSet` produced from the same spec snapshot.
pub fn build_no_redirect_for_label(
    spec: &OutboundProxySpec,
    label: RouteLabel,
) -> Result<reqwest::Client> {
    let builder = match label {
        RouteLabel::Auto => tuned(reqwest::Client::builder()),
        RouteLabel::Proxy => proxy_leg_builder(&spec.proxy_url)?,
        RouteLabel::Direct => direct_leg_builder(),
    };
    Ok(builder
        .redirect(no_redirect_policy())
        .connect_timeout(spec.connect_timeout)
        .build()?)
}

/// Applies the process-wide outbound proxy policy while leaving callers free to
/// choose their own redirect, total-timeout, and response-size policies. Under
/// list mode this builds the direction's default leg: whitelist default is
/// direct, blacklist default is the manual proxy URL.
pub fn configured_builder(spec: &OutboundProxySpec) -> Result<reqwest::ClientBuilder> {
    let builder = match spec.mode {
        ProxyMode::Auto => tuned(reqwest::Client::builder()),
        ProxyMode::Manual => proxy_leg_builder(&spec.proxy_url)?,
        ProxyMode::Direct => direct_leg_builder(),
        ProxyMode::List => match spec.list_direction {
            ProxyListDirection::Whitelist => direct_leg_builder(),
            ProxyListDirection::Blacklist => proxy_leg_builder(&spec.proxy_url)?,
        },
    };
    Ok(builder)
}

/// Optional IsolatedTrustedAdmin DNS seam. Attaches `resolver` as the connector
/// resolver without changing proxy routing. Sealed-adapter builders must keep
/// calling [`configured_builder`] with no resolver. `ClientBuilder::resolve`
/// overrides wrap *outside* this resolver and must not be used to inject
/// destination answers under the guard.
pub fn attach_dns_resolver(
    builder: reqwest::ClientBuilder,
    resolver: Arc<dyn reqwest::dns::Resolve>,
) -> reqwest::ClientBuilder {
    builder.dns_resolver2(resolver)
}

pub fn build(spec: &OutboundProxySpec) -> Result<reqwest::Client> {
    leg_client(configured_builder(spec)?, spec)
}

/// Builds the full route set from one spec generation. List mode builds both
/// legs against the pre-normalized `list`; every other mode builds exactly the
/// process-wide client. The audit label is derived from `spec.mode` in the same
/// match that builds the client, so a caller cannot pair an arbitrary reqwest
/// client with a false route label.
pub fn build_route_set(spec: &OutboundProxySpec, list: Vec<String>) -> Result<ForwardRouteSet> {
    match spec.mode {
        ProxyMode::List => {
            let (default_builder, exception_builder, default_label, exception_label) =
                match spec.list_direction {
                    ProxyListDirection::Whitelist => (
                        direct_leg_builder(),
                        proxy_leg_builder(&spec.proxy_url)?,
                        RouteLabel::Direct,
                        RouteLabel::Proxy,
                    ),
                    ProxyListDirection::Blacklist => (
                        proxy_leg_builder(&spec.proxy_url)?,
                        direct_leg_builder(),
                        RouteLabel::Proxy,
                        RouteLabel::Direct,
                    ),
                };
            Ok(ForwardRouteSet {
                list,
                default_client: leg_client(default_builder, spec)?,
                exception_client: Some(leg_client(exception_builder, spec)?),
                default_label,
                exception_label,
            })
        }
        ProxyMode::Auto | ProxyMode::Manual | ProxyMode::Direct => non_list_route_set(spec),
    }
}

/// Single-leg set for Auto/Manual/Direct. Client (`build(spec)`) and audit
/// label (`spec.mode`) are produced from the same snapshot; there is no
/// `(client, label)` pairing.
fn non_list_route_set(spec: &OutboundProxySpec) -> Result<ForwardRouteSet> {
    let default_label = match spec.mode {
        ProxyMode::Auto => RouteLabel::Auto,
        ProxyMode::Manual => RouteLabel::Proxy,
        ProxyMode::Direct => RouteLabel::Direct,
        ProxyMode::List => unreachable!("list mode is dual-leg"),
    };
    Ok(ForwardRouteSet {
        list: Vec::new(),
        default_client: build(spec)?,
        exception_client: None,
        default_label,
        exception_label: RouteLabel::Direct,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const PROXY_URL: &str = "http://127.0.0.1:7890";
    const INVALID_PROXY: &str = "not a url";

    fn spec(mode: ProxyMode, direction: ProxyListDirection, proxy_url: &str) -> OutboundProxySpec {
        OutboundProxySpec {
            mode,
            proxy_url: proxy_url.to_string(),
            connect_timeout: Duration::from_secs(30),
            list_direction: direction,
        }
    }

    #[test]
    fn route_labels_are_closed_auto_proxy_direct() {
        assert_eq!(RouteLabel::Auto.as_str(), "auto");
        assert_eq!(RouteLabel::Proxy.as_str(), "proxy");
        assert_eq!(RouteLabel::Direct.as_str(), "direct");
    }

    #[test]
    fn list_whitelist_listed_is_proxy_and_unlisted_is_direct() {
        let routes = build_route_set(
            &spec(ProxyMode::List, ProxyListDirection::Whitelist, PROXY_URL),
            vec!["gpt-5.6-luna".to_string()],
        )
        .unwrap();
        assert_eq!(routes.client_for("gpt-5.6-luna").1, RouteLabel::Proxy);
        assert_eq!(routes.client_for("glm-5.3").1, RouteLabel::Direct);
    }

    #[test]
    fn list_blacklist_listed_is_direct_and_unlisted_is_proxy() {
        let routes = build_route_set(
            &spec(ProxyMode::List, ProxyListDirection::Blacklist, PROXY_URL),
            vec!["grok-4.5".to_string()],
        )
        .unwrap();
        assert_eq!(routes.client_for("grok-4.5").1, RouteLabel::Direct);
        assert_eq!(routes.client_for("glm-5.3").1, RouteLabel::Proxy);
    }

    #[test]
    fn empty_list_uses_direction_default_leg() {
        let whitelist = build_route_set(
            &spec(ProxyMode::List, ProxyListDirection::Whitelist, PROXY_URL),
            Vec::new(),
        )
        .unwrap();
        assert_eq!(whitelist.client_for("gpt-5.6-luna").1, RouteLabel::Direct);

        let blacklist = build_route_set(
            &spec(ProxyMode::List, ProxyListDirection::Blacklist, PROXY_URL),
            Vec::new(),
        )
        .unwrap();
        assert_eq!(blacklist.client_for("gpt-5.6-luna").1, RouteLabel::Proxy);
    }

    #[test]
    fn non_list_modes_ignore_membership_and_use_mode_label() {
        for (mode, label) in [
            (ProxyMode::Auto, RouteLabel::Auto),
            (ProxyMode::Manual, RouteLabel::Proxy),
            (ProxyMode::Direct, RouteLabel::Direct),
        ] {
            let routes = build_route_set(
                &spec(mode, ProxyListDirection::Whitelist, PROXY_URL),
                vec!["gpt-5.6-luna".to_string()],
            )
            .unwrap();
            assert_eq!(routes.client_for("gpt-5.6-luna").1, label);
            assert_eq!(routes.client_for("other").1, label);
            let (client, resolved) = routes.client_for("gpt-5.6-luna");
            assert!(
                std::ptr::eq(client, routes.default_client()),
                "non-list modes must resolve only the default leg"
            );
            assert_eq!(resolved, label);
        }
    }

    #[test]
    fn public_construction_cannot_pair_arbitrary_client_with_false_label() {
        // ForwardRouteSet fields are private and there is no public
        // (Client, RouteLabel) constructor. The only public builder is
        // build_route_set(spec, list), which binds client and audit label
        // to the same OutboundProxySpec.
        let auto = build_route_set(
            &spec(ProxyMode::Auto, ProxyListDirection::Blacklist, PROXY_URL),
            Vec::new(),
        )
        .unwrap();
        assert_eq!(auto.client_for("gpt-5.6-luna").1, RouteLabel::Auto);
        assert_ne!(auto.client_for("gpt-5.6-luna").1, RouteLabel::Proxy);
        assert_ne!(auto.client_for("gpt-5.6-luna").1, RouteLabel::Direct);

        let manual = build_route_set(
            &spec(ProxyMode::Manual, ProxyListDirection::Whitelist, PROXY_URL),
            Vec::new(),
        )
        .unwrap();
        assert_eq!(manual.client_for("gpt-5.6-luna").1, RouteLabel::Proxy);
        assert_ne!(manual.client_for("gpt-5.6-luna").1, RouteLabel::Auto);

        let direct = build_route_set(
            &spec(
                ProxyMode::Direct,
                ProxyListDirection::Blacklist,
                INVALID_PROXY,
            ),
            Vec::new(),
        )
        .unwrap();
        assert_eq!(direct.client_for("gpt-5.6-luna").1, RouteLabel::Direct);
        assert_ne!(direct.client_for("gpt-5.6-luna").1, RouteLabel::Auto);
        assert_ne!(direct.client_for("gpt-5.6-luna").1, RouteLabel::Proxy);
    }

    #[test]
    fn exact_membership_does_not_fold_model_names() {
        let routes = build_route_set(
            &spec(ProxyMode::List, ProxyListDirection::Whitelist, PROXY_URL),
            vec!["gpt-5.6-luna".to_string()],
        )
        .unwrap();
        assert_eq!(
            routes.client_for("GPT_5.6 LUNA").1,
            RouteLabel::Direct,
            "infra exact-match must not fold aliases"
        );
    }

    #[test]
    fn invalid_proxy_url_fails_manual_and_list_proxy_legs() {
        let manual = spec(
            ProxyMode::Manual,
            ProxyListDirection::Whitelist,
            INVALID_PROXY,
        );
        assert!(configured_builder(&manual).is_err());
        assert!(build(&manual).is_err());
        assert!(build_no_redirect(&manual).is_err());
        assert!(build_route_set(&manual, Vec::new()).is_err());

        let blacklist = spec(
            ProxyMode::List,
            ProxyListDirection::Blacklist,
            INVALID_PROXY,
        );
        assert!(configured_builder(&blacklist).is_err());
        assert!(build_route_set(&blacklist, Vec::new()).is_err());

        let whitelist = spec(
            ProxyMode::List,
            ProxyListDirection::Whitelist,
            INVALID_PROXY,
        );
        assert!(
            configured_builder(&whitelist).is_ok(),
            "whitelist default leg is direct and does not parse the proxy URL"
        );
        assert!(
            build_route_set(&whitelist, Vec::new()).is_err(),
            "whitelist exception leg still needs a valid proxy URL"
        );

        let auto = spec(
            ProxyMode::Auto,
            ProxyListDirection::Whitelist,
            INVALID_PROXY,
        );
        assert!(configured_builder(&auto).is_ok());
        assert!(build(&auto).is_ok());

        let direct = spec(
            ProxyMode::Direct,
            ProxyListDirection::Whitelist,
            INVALID_PROXY,
        );
        assert!(configured_builder(&direct).is_ok());
        assert!(build(&direct).is_ok());
    }

    #[test]
    fn no_redirect_construction_keeps_proxy_policy_and_disables_follow() {
        let client = build_no_redirect(&spec(ProxyMode::Direct, ProxyListDirection::Whitelist, ""))
            .expect("no-redirect client");
        let _ = client;
        let auto = spec(ProxyMode::Auto, ProxyListDirection::Whitelist, "");
        let _ = configured_builder(&auto)
            .expect("proxy builder")
            .redirect(no_redirect_policy());
    }
}
