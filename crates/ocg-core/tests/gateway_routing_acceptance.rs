//! Real-network routing acceptance: production Gateway HTTP to independent
//! loopback upstreams, with a shared chronological arrival journal.
//!
//! Dummy keys are explicit test credentials (`dummy-*`). Listeners bind
//! `127.0.0.1` only and shut down with the harness.
//!
//! Shadow compare stays default-off. This binary never mutates process
//! environment. To replay with shadow, set `OCG_SHADOW_COMPARE=1` in the
//! launching shell before `cargo test`. Journal asserts still catch extra
//! origin hits whether shadow is on or off.

use axum::http::StatusCode;
use ocg_core::models::{ProxyListDirection, ProxyMode, RoutingMode};
use ocg_core::provider::{
    COMMAND_CODE_GOAT_DEEPSEEK_V4_FLASH_ALIAS, COMMAND_CODE_GOAT_DEEPSEEK_V4_FLASH_UPSTREAM,
    COMMAND_CODE_PROVIDER_ID, CUSTOM_PROVIDER_ID, OPENCODE_PROVIDER_ID,
};
use ocg_domain::credential::ModelScope;

#[path = "fixtures/gateway_fallback.rs"]
mod fallback_fix;

use fallback_fix::*;

const ROUTE_MODEL: &str = "route-lab";
const UPSTREAM_MODEL: &str = "vendor/route-lab";
const DUMMY_A: &str = "dummy-key-a";
const DUMMY_B: &str = "dummy-key-b";
const DUMMY_C: &str = "dummy-key-c";
const DUMMY_GO: &str = "dummy-go-key";
const DUMMY_GOAT: &str = "dummy-goat-key";

struct ThreeLabs {
    h: FallbackHarness,
    journal: SharedJournal,
    ids: [String; 3],
}

async fn bind_three_dynamic_labs(
    a: MockReply,
    b: MockReply,
    c: MockReply,
    routing: RoutingMode,
    sticky: bool,
) -> ThreeLabs {
    let journal = SharedJournal::new();
    let labs = [
        start_journaled_lab(&journal, "lab-a", DUMMY_A, &[a]).await,
        start_journaled_lab(&journal, "lab-b", DUMMY_B, &[b]).await,
        start_journaled_lab(&journal, "lab-c", DUMMY_C, &[c]).await,
    ];
    let (state, dir) = build_state_with_routing("http://127.0.0.1:1".into(), &[], routing, sticky);
    let mut h = FallbackHarness::from_state(state, dir).await;
    let mut ids = Vec::new();
    for (lab, key) in labs.iter().zip([DUMMY_A, DUMMY_B, DUMMY_C]) {
        let created = create_dynamic_lab(
            h.port,
            &h.state,
            &lab.label,
            &lab.url,
            Some(key),
            ROUTE_MODEL,
            UPSTREAM_MODEL,
            "chat_completions",
        )
        .await;
        ids.push(
            created
                .account_id
                .expect("keyed dynamic lab creates an account"),
        );
    }
    for lab in labs {
        h.push_stop(lab.stop);
    }
    let ids = [ids[0].clone(), ids[1].clone(), ids[2].clone()];
    reorder_first(&h.state, &ids);
    ThreeLabs { h, journal, ids }
}

#[tokio::test]
async fn proxy_list_matches_the_materialized_upstream_id_in_both_directions() {
    let journal = SharedJournal::new();
    let origin = start_journaled_lab(&journal, "origin", DUMMY_A, &[ok()]).await;
    let proxy = start_journaled_lab(&journal, "proxy", DUMMY_A, &[ok(), ok()]).await;
    let (state, dir) = build_state_with_routing(
        "http://127.0.0.1:1".into(),
        &[],
        RoutingMode::StrictPriority,
        false,
    );
    let mut h = FallbackHarness::from_state(state, dir).await;
    create_dynamic_lab(
        h.port,
        &h.state,
        "proxy-id-lab",
        &origin.url,
        Some(DUMMY_A),
        ROUTE_MODEL,
        UPSTREAM_MODEL,
        "chat_completions",
    )
    .await;
    let (settings_status, settings) =
        dashboard_json(h.port, reqwest::Method::GET, "v4", "/settings", None).await;
    assert_eq!(settings_status, StatusCode::OK, "{settings}");
    assert!(
        settings["proxySupportedModels"]
            .as_array()
            .unwrap()
            .iter()
            .any(|row| row["id"] == UPSTREAM_MODEL),
        "dynamic upstream id is missing from proxy candidates: {settings}"
    );

    let mut config = h.state.config();
    config.proxy_mode = ProxyMode::List;
    config.proxy_url = proxy.url.clone();
    config.proxy_list_models = vec![UPSTREAM_MODEL.into()];
    config.proxy_list_direction = ProxyListDirection::Whitelist;
    h.state.set_config(config.clone()).unwrap();
    let (status, body) = h.protocol("/v1/chat/completions", ROUTE_MODEL).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let proxy_log = sorted_logs(&h.state).pop().unwrap();
    let proxy_attribution = h
        .state
        .db
        .lock()
        .forward_log_native_attribution(proxy_log.id)
        .unwrap()
        .unwrap();
    assert_eq!(
        proxy_attribution.upstream_model.as_deref(),
        Some(UPSTREAM_MODEL)
    );
    assert_eq!(proxy_log.route, "proxy");
    assert_eq!(journal.listeners(), ["proxy"]);

    config.proxy_list_direction = ProxyListDirection::Blacklist;
    h.state.set_config(config).unwrap();
    let (status, body) = h.protocol("/v1/chat/completions", ROUTE_MODEL).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(journal.listeners(), ["proxy", "origin"]);
    let direct_log = sorted_logs(&h.state).pop().unwrap();
    let direct_attribution = h
        .state
        .db
        .lock()
        .forward_log_native_attribution(direct_log.id)
        .unwrap()
        .unwrap();
    assert_eq!(
        direct_attribution.upstream_model.as_deref(),
        Some(UPSTREAM_MODEL)
    );

    h.push_stop(origin.stop);
    h.push_stop(proxy.stop);
    h.stop();
}

fn evidence(
    scenario: &str,
    expected_listeners: &[&str],
    expected_keys: &[&str],
    journal: &SharedJournal,
    logs: &[ocg_core::models::ForwardLog],
    client_status: u16,
) {
    write_routing_evidence(
        scenario,
        serde_json::json!({
            "scenario": scenario,
            "dummyKeys": "explicit test credentials dummy-key-* / dummy-go-key / dummy-goat-key",
            "clientStatus": client_status,
            "expectedListeners": expected_listeners,
            "observedListeners": journal.listeners(),
            "expectedDummyKeys": expected_keys,
            "observedDummyKeys": journal.keys(),
            "observedPaths": journal.paths(),
            "arrivals": arrivals_json(journal),
            "forwardLogs": logs_json(logs),
        }),
    );
}

#[tokio::test]
async fn strict_priority_and_reorder_across_three_upstreams_stops_on_success() {
    let ThreeLabs { h, journal, ids } =
        bind_three_dynamic_labs(ok(), ok(), ok(), RoutingMode::StrictPriority, false).await;

    let (status, body) = h.protocol("/v1/chat/completions", ROUTE_MODEL).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(journal.listeners(), ["lab-a"]);
    assert_eq!(journal.keys(), [DUMMY_A]);
    let first_logs = sorted_logs(&h.state);
    assert_eq!(first_logs.len(), 1);
    assert_eq!(first_logs[0].attempt, Some(1));
    assert_eq!(first_logs[0].account_id, ids[0]);
    assert_eq!(first_logs[0].status, "success_unpriced");
    evidence(
        "strict-priority-success-stops",
        &["lab-a"],
        &[DUMMY_A],
        &journal,
        &first_logs,
        status.as_u16(),
    );

    reorder_first(&h.state, &[ids[2].clone(), ids[0].clone(), ids[1].clone()]);
    let (status, body) = h.protocol("/v1/chat/completions", ROUTE_MODEL).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(journal.listeners(), ["lab-a", "lab-c"]);
    assert_eq!(journal.keys(), [DUMMY_A, DUMMY_C]);
    let logs = sorted_logs(&h.state);
    let second = logs
        .iter()
        .find(|log| log.account_id == ids[2] && log.status.starts_with("success"))
        .expect("reordered card should succeed");
    assert_eq!(second.attempt, Some(1));
    evidence(
        "strict-priority-reordered-cards",
        &["lab-a", "lab-c"],
        &[DUMMY_A, DUMMY_C],
        &journal,
        &logs,
        status.as_u16(),
    );
}

#[tokio::test]
async fn fallthrough_429_then_403_then_success_across_three_upstreams() {
    let ThreeLabs { h, journal, ids } = bind_three_dynamic_labs(
        limited(),
        forbidden(),
        ok(),
        RoutingMode::StrictPriority,
        false,
    )
    .await;

    let (status, body) = h.protocol("/v1/chat/completions", ROUTE_MODEL).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(journal.listeners(), ["lab-a", "lab-b", "lab-c"]);
    assert_eq!(journal.keys(), [DUMMY_A, DUMMY_B, DUMMY_C]);
    let logs = sorted_logs(&h.state);
    let attempts: Vec<_> = logs.iter().filter_map(|log| log.attempt).collect();
    assert_eq!(attempts, [1, 2, 3], "{logs:?}");
    assert_eq!(logs[0].account_id, ids[0]);
    assert_eq!(logs[0].http_status, Some(429));
    assert_eq!(logs[1].account_id, ids[1]);
    assert_eq!(logs[1].http_status, Some(403));
    assert_eq!(logs[2].account_id, ids[2]);
    assert!(logs[2].status.starts_with("success"), "{logs:?}");
    let request_ids = logs
        .iter()
        .map(|log| log.request_id.clone())
        .collect::<std::collections::HashSet<_>>();
    assert_eq!(request_ids.len(), 1, "one client request: {logs:?}");
    evidence(
        "fallthrough-429-403-success",
        &["lab-a", "lab-b", "lab-c"],
        &[DUMMY_A, DUMMY_B, DUMMY_C],
        &journal,
        &logs,
        status.as_u16(),
    );
}

#[tokio::test]
async fn http_5xx_does_not_fall_through_across_distinct_upstreams() {
    let ThreeLabs { h, journal, ids } = bind_three_dynamic_labs(
        server_error(),
        ok(),
        ok(),
        RoutingMode::StrictPriority,
        false,
    )
    .await;

    let (status, _body) = h.protocol("/v1/chat/completions", ROUTE_MODEL).await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(journal.listeners(), ["lab-a"]);
    assert_eq!(journal.keys(), [DUMMY_A]);
    let logs = sorted_logs(&h.state);
    assert_eq!(logs.len(), 1);
    assert_eq!(logs[0].account_id, ids[0]);
    assert_eq!(logs[0].http_status, Some(500));
    evidence(
        "http-5xx-no-fallthrough",
        &["lab-a"],
        &[DUMMY_A],
        &journal,
        &logs,
        status.as_u16(),
    );
}

#[tokio::test]
async fn disabled_account_and_model_scope_never_call_upstream() {
    let ThreeLabs { h, journal, ids } =
        bind_three_dynamic_labs(ok(), ok(), ok(), RoutingMode::StrictPriority, false).await;
    h.set_enabled(&ids[0], false);
    set_binding_gate(
        &h.state,
        &ids[1],
        Some(true),
        Some(ModelScope::Only {
            models: vec!["other-model".into()],
        }),
    );

    let (status, body) = h.protocol("/v1/chat/completions", ROUTE_MODEL).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(journal.listeners(), ["lab-c"]);
    assert_eq!(journal.keys(), [DUMMY_C]);
    let logs = sorted_logs(&h.state);
    assert!(
        logs.iter().all(|log| log.account_id == ids[2]),
        "disabled/model-scope cards must not produce upstream attempts: {logs:?}"
    );
    evidence(
        "disabled-and-model-scope-skipped",
        &["lab-c"],
        &[DUMMY_C],
        &journal,
        &logs,
        status.as_u16(),
    );
}

#[tokio::test]
async fn unknown_custom_429_does_not_invent_shared_pool_exhaustion() {
    let journal = SharedJournal::new();
    let labs = [
        start_journaled_lab(&journal, "lab-a", DUMMY_A, &[limited()]).await,
        start_journaled_lab(&journal, "lab-b", DUMMY_B, &[ok()]).await,
        start_journaled_lab(&journal, "lab-c", DUMMY_C, &[ok()]).await,
    ];
    let (state, dir) = build_state("http://127.0.0.1:1".into(), &[]);
    let mut h = FallbackHarness::from_state(state, dir).await;
    let first = create_dynamic_lab(
        h.port,
        &h.state,
        "lab-a",
        &labs[0].url,
        Some(DUMMY_A),
        ROUTE_MODEL,
        UPSTREAM_MODEL,
        "chat_completions",
    )
    .await;
    let first_id = first.account_id.expect("first lab account");
    let sibling_def = create_dynamic_lab(
        h.port,
        &h.state,
        "lab-b",
        &labs[1].url,
        None,
        ROUTE_MODEL,
        UPSTREAM_MODEL,
        "chat_completions",
    )
    .await;
    let third = create_dynamic_lab(
        h.port,
        &h.state,
        "lab-c",
        &labs[2].url,
        Some(DUMMY_C),
        ROUTE_MODEL,
        UPSTREAM_MODEL,
        "chat_completions",
    )
    .await;
    let third_id = third.account_id.expect("third lab account");
    for lab in labs {
        h.push_stop(lab.stop);
    }

    let refs = identity_refs_for(&h.state, &first_id);
    let (status, connections) = v4_get(h.port, "/connections").await;
    assert_eq!(status, StatusCode::OK, "{connections}");
    let sibling_connection = connections["connections"]
        .as_array()
        .unwrap()
        .iter()
        .find(|connection| {
            connection["legacy"]["kind"] == "dynamic_provider"
                && connection["legacy"]["id"] == sibling_def.provider_id
        })
        .unwrap_or_else(|| panic!("missing sibling connection: {connections}"));
    let (status, created) = v4_mutate(
        h.port,
        &h.state,
        &format!("/identities/{}/credentials", refs.identity_id),
        serde_json::json!({
            "connectionId": sibling_connection["id"],
            "secretInput": DUMMY_B,
            "quotaSharing": {
                "kind": "shared",
                "credentialId": refs.credential_id
            }
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{created}");
    let sibling_id = created["accountId"]
        .as_str()
        .unwrap_or_else(|| panic!("sibling account id missing: {created}"))
        .to_string();
    h.set_enabled(&sibling_id, true);
    let pool = h
        .state
        .db
        .lock()
        .shared_pool_account_ids(&first_id)
        .unwrap();
    assert!(pool.contains(&first_id), "{pool:?}");
    assert!(pool.contains(&sibling_id), "{pool:?}");
    assert!(!pool.contains(&third_id), "{pool:?}");
    reorder_first(
        &h.state,
        &[first_id.clone(), sibling_id.clone(), third_id.clone()],
    );

    let (status, body) = h.protocol("/v1/chat/completions", ROUTE_MODEL).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        journal.listeners(),
        ["lab-a", "lab-b"],
        "unknown Custom 429 must not claim shared quota exhaustion: {:?}",
        journal.snapshot()
    );
    assert_eq!(journal.keys(), [DUMMY_A, DUMMY_B]);
    let logs = sorted_logs(&h.state);
    assert!(
        logs.iter()
            .any(|log| log.account_id == sibling_id && log.http_status == Some(200)),
        "eligible sibling should serve after an unknown request-local rejection: {logs:?}"
    );
    evidence(
        "unknown-429-keeps-shared-pool-eligible",
        &["lab-a", "lab-b"],
        &[DUMMY_A, DUMMY_B],
        &journal,
        &logs,
        status.as_u16(),
    );
}

#[tokio::test]
async fn round_robin_and_sticky_current_behavior_across_three_upstreams() {
    let rr = bind_three_dynamic_labs(ok(), ok(), ok(), RoutingMode::RoundRobin, false).await;
    for _ in 0..3 {
        let (status, body) = rr.h.protocol("/v1/chat/completions", ROUTE_MODEL).await;
        assert_eq!(status, StatusCode::OK, "{body}");
    }
    assert_eq!(rr.journal.listeners(), ["lab-a", "lab-b", "lab-c"]);
    assert_eq!(rr.journal.keys(), [DUMMY_A, DUMMY_B, DUMMY_C]);
    evidence(
        "round-robin-three-upstreams",
        &["lab-a", "lab-b", "lab-c"],
        &[DUMMY_A, DUMMY_B, DUMMY_C],
        &rr.journal,
        &sorted_logs(&rr.h.state),
        200,
    );

    let sticky = bind_three_dynamic_labs(ok(), ok(), ok(), RoutingMode::StickyGlobal, false).await;
    assert_eq!(
        sticky
            .h
            .protocol("/v1/chat/completions", ROUTE_MODEL)
            .await
            .0,
        StatusCode::OK
    );
    sticky.h.set_enabled(&sticky.ids[0], false);
    assert_eq!(
        sticky
            .h
            .protocol("/v1/chat/completions", ROUTE_MODEL)
            .await
            .0,
        StatusCode::OK
    );
    sticky.h.set_enabled(&sticky.ids[0], true);
    assert_eq!(
        sticky
            .h
            .protocol("/v1/chat/completions", ROUTE_MODEL)
            .await
            .0,
        StatusCode::OK
    );
    assert_eq!(
        sticky.journal.listeners(),
        ["lab-a", "lab-b", "lab-b"],
        "sticky should keep the failover card after the higher-priority card recovers"
    );
    evidence(
        "sticky-global-three-upstreams",
        &["lab-a", "lab-b", "lab-b"],
        &[DUMMY_A, DUMMY_B, DUMMY_B],
        &sticky.journal,
        &sorted_logs(&sticky.h.state),
        200,
    );
}

#[tokio::test]
async fn custom_chat_responses_messages_use_configured_protocol_and_auth() {
    let journal = SharedJournal::new();
    let labs = [
        start_journaled_lab(&journal, "custom-chat", DUMMY_A, &[ok()]).await,
        start_journaled_lab(&journal, "custom-responses", DUMMY_B, &[ok_responses()]).await,
        start_journaled_lab(&journal, "custom-messages", DUMMY_C, &[ok_messages()]).await,
    ];
    let (state, dir) = build_state("http://127.0.0.1:1".into(), &[]);
    let mut h = FallbackHarness::from_state(state, dir).await;
    let chat_endpoint = format!("{}/v1/chat/completions", labs[0].url);
    let responses_endpoint = format!("{}/v1/responses", labs[1].url);
    let messages_endpoint = format!("{}/v1/messages", labs[2].url);
    let _chat_id = create_custom_lab(
        h.port,
        &h.state,
        "custom-chat",
        &chat_endpoint,
        DUMMY_A,
        "lab-chat",
        "chat_completions",
    )
    .await;
    let _responses_id = create_custom_lab(
        h.port,
        &h.state,
        "custom-responses",
        &responses_endpoint,
        DUMMY_B,
        "lab-responses",
        "responses",
    )
    .await;
    let _messages_id = create_custom_lab(
        h.port,
        &h.state,
        "custom-messages",
        &messages_endpoint,
        DUMMY_C,
        "lab-messages",
        "messages",
    )
    .await;
    for lab in labs {
        h.push_stop(lab.stop);
    }

    let (status, body) = h.protocol("/v1/chat/completions", "lab-chat").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (status, body) = h.protocol("/v1/responses", "lab-responses").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (status, body) = h.protocol("/v1/messages", "lab-messages").await;
    assert_eq!(status, StatusCode::OK, "{body}");

    assert_eq!(
        journal.listeners(),
        ["custom-chat", "custom-responses", "custom-messages"]
    );
    let arrivals = journal.snapshot();
    assert_eq!(arrivals[0].path, "/v1/chat/completions");
    assert_eq!(
        arrivals[0].authorization.as_deref(),
        Some("Bearer dummy-key-a")
    );
    assert!(arrivals[0].x_api_key.is_none());
    assert_eq!(arrivals[1].path, "/v1/responses");
    assert_eq!(
        arrivals[1].authorization.as_deref(),
        Some("Bearer dummy-key-b")
    );
    assert_eq!(arrivals[2].path, "/v1/messages");
    assert_eq!(arrivals[2].x_api_key.as_deref(), Some(DUMMY_C));
    assert!(arrivals[2].authorization.is_none());
    let logs = sorted_logs(&h.state);
    assert!(
        logs.iter()
            .all(|log| log.provider_id.as_deref() == Some(CUSTOM_PROVIDER_ID)),
        "{logs:?}"
    );
    evidence(
        "custom-chat-responses-messages",
        &["custom-chat", "custom-responses", "custom-messages"],
        &[DUMMY_A, DUMMY_B, DUMMY_C],
        &journal,
        &logs,
        200,
    );
}

#[tokio::test]
async fn mixed_go_and_goat_loopback_chain_uses_distinct_origins() {
    let journal = SharedJournal::new();
    let (go_url, _, stop_go) =
        start_fake_upstream_on_journal("go", script(&[(DUMMY_GO, &[ok()])]), journal.clone()).await;
    let (goat_url, _, stop_goat) = start_fake_upstream_on_journal(
        "goat",
        script(&[(DUMMY_GOAT, &[limited()])]),
        journal.clone(),
    )
    .await;
    let (state, dir) = build_state(go_url, &[DUMMY_GO]);
    let goat_id = prepare_goat(
        &state,
        DUMMY_GOAT,
        &[COMMAND_CODE_GOAT_DEEPSEEK_V4_FLASH_UPSTREAM],
        true,
    );
    let mut h = FallbackHarness::from_state(state, dir).await;
    h.attach_goat_route(goat_id.clone(), goat_url);
    h.push_stop(stop_go);
    h.push_stop(stop_goat);

    let (status, body) = h
        .protocol(
            "/v1/chat/completions",
            COMMAND_CODE_GOAT_DEEPSEEK_V4_FLASH_ALIAS,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(journal.listeners(), ["goat", "go"]);
    assert_eq!(journal.keys(), [DUMMY_GOAT, DUMMY_GO]);
    let arrivals = journal.snapshot();
    assert_eq!(arrivals[0].path, "/provider/v1/chat/completions");
    assert_eq!(
        arrivals[0].authorization.as_deref(),
        Some("Bearer dummy-goat-key")
    );
    assert_eq!(arrivals[1].path, "/v1/chat/completions");
    assert_eq!(
        arrivals[1].authorization.as_deref(),
        Some("Bearer dummy-go-key")
    );
    let logs = sorted_logs(&h.state);
    assert_eq!(logs.len(), 2, "{logs:?}");
    assert_eq!(
        logs[0].provider_id.as_deref(),
        Some(COMMAND_CODE_PROVIDER_ID)
    );
    assert_eq!(logs[1].provider_id.as_deref(), Some(OPENCODE_PROVIDER_ID));
    assert_eq!(logs[0].http_status, Some(429));
    assert!(logs[1].status.starts_with("success"), "{logs:?}");
    evidence(
        "mixed-go-goat-loopback-chain",
        &["goat", "go"],
        &[DUMMY_GOAT, DUMMY_GO],
        &journal,
        &logs,
        status.as_u16(),
    );
}
