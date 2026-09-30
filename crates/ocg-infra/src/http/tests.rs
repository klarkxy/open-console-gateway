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
fn direct_invalid_proxy_still_labels_direct() {
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
fn no_redirect_construction_succeeds_for_direct() {
    let client = build_no_redirect(&spec(ProxyMode::Direct, ProxyListDirection::Whitelist, ""))
        .expect("no-redirect client");
    let _ = client;
}
