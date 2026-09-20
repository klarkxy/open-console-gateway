use super::*;

#[test]
fn mixed_sse_line_endings_preserve_usage_and_done_at_every_chunk_split() {
    for (first, second) in [("\r\n\r\n", "\n\n"), ("\n\n", "\r\n\r\n")] {
        let payload = format!(
            "data: {{\"usage\":{{\"prompt_tokens\":7,\"completion_tokens\":11}}}}{first}data: [DONE]{second}"
        );
        for split in 0..=payload.len() {
            let mut state = StreamState::default();
            for chunk in [&payload.as_bytes()[..split], &payload.as_bytes()[split..]] {
                process_chunk_for_usage(
                    &mut state,
                    ApiFormat::ChatCompletions,
                    &Bytes::copy_from_slice(chunk),
                    None,
                );
            }
            assert!(state.has_usage, "usage lost at split {split}");
            assert_eq!(token_counts(state.usage), (7, 11, 0, 0));
            assert!(state.terminal, "DONE lost at split {split}");
            assert!(state.buf.is_empty());
        }
    }
}

#[test]
fn mixed_sse_line_endings_keep_the_first_error_terminal() {
    let mut state = StreamState::default();
    process_chunk_for_usage(
        &mut state,
        ApiFormat::ChatCompletions,
        &Bytes::from_static(
            b"data: {\"error\":{\"message\":\"mock failure\"}}\r\n\r\ndata: {\"usage\":{\"prompt_tokens\":999}}\n\n",
        ),
        None,
    );
    assert!(state.error);
    assert!(state.terminal);
    assert_eq!(state.error_message.as_deref(), Some("mock failure"));
    assert!(
        !state.has_usage,
        "later events must not override a terminal error"
    );
    assert!(state.buf.is_empty());
}

#[test]
fn platform_media_and_service_tiers_do_not_use_plain_text_rates() {
    assert!(!platform_request_has_variable_cost(
        br#"{"messages":[{"role":"user","content":"hello"}]}"#,
        None
    ));
    assert!(platform_request_has_variable_cost(br#"{"messages":[{"content":[{"type":"image_url","image_url":{"url":"https://example.test/a.png"}}]}]}"#,None));
    assert!(platform_request_has_variable_cost(
        br#"{"input":"hello"}"#,
        Some("priority")
    ));
}
use crate::crypto::{KeyCipher, StaticKeyCipher};
use crate::db::Database;
use crate::gateway::diagnostics::RequestTrace;
use crate::gateway::protocol::{CustomRouteSpec, RequestPlan};
use crate::http_client::RouteLabel;
use crate::kernel::protocol::ApiFormat;
use crate::models::{
    Account, AccountCustomConfigInput, AccountModelCapabilityInput, AccountSetupStep, AccountType,
    ProxyMode, UpstreamChannel,
};
use crate::platform::{PlatformGroup, PlatformKind, PlatformPrice, PlatformSnapshot};
use crate::provider::{
    CUSTOM_PROVIDER_ID, OPENCODE_PROVIDER_ID, ProviderAdapterKind, UpstreamProtocolKind,
};
use crate::state::CoreStateInner;
use bytes::Bytes;
use chrono::Utc;
use ocg_domain::connection::{
    EndpointOperation, LegacyConnectionKind, connection_id_for_legacy, endpoint_id_for,
};
use ocg_domain::credential::{
    ModelScope, RouteSpec, assigned_endpoints_for_routes, normalize_origin,
};
use serde_json::json;
use std::fs;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

const UPSTREAM: &str = "gpt-4o";
const GROUP: &str = "default";
const PARENT: &str = "parent-1";
const ACCOUNT: &str = "custom-1";

fn temp_dir(label: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "ocg-platform-price-{}-{}",
        label,
        uuid::Uuid::new_v4()
    ));
    fs::create_dir_all(&dir).unwrap();
    dir
}

fn test_state(label: &str) -> (PathBuf, CoreState) {
    let dir = temp_dir(label);
    let cipher: Arc<dyn KeyCipher + Send + Sync> = Arc::new(StaticKeyCipher::new("platform-price"));
    let db = Database::open(dir.clone()).unwrap();
    let state = Arc::new(CoreStateInner::new(db, dir.clone(), cipher).unwrap());
    (dir, state)
}

fn custom_account(state: &CoreState) -> Account {
    let now = Utc::now();
    Account {
        id: ACCOUNT.into(),
        provider_id: CUSTOM_PROVIDER_ID.into(),
        credential_kind: crate::provider::default_credential_kind(),
        quota_scope: crate::provider::default_quota_scope(),
        name: "custom".into(),
        username: None,
        password_cipher: None,
        key_cipher: state.encrypt_key("sk-custom").unwrap(),
        enabled: true,
        account_type: AccountType::Key,
        setup_step: AccountSetupStep::Ready,
        referral_code: None,
        purchase_date: String::new(),
        expires_on: String::new(),
        cooldown_until: None,
        cooldown_generic_until: None,
        cooldown_5h_until: None,
        cooldown_week_until: None,
        cooldown_month_until: None,
        cooldown_free_until: None,
        last_error: None,
        auth_error: None,
        notes: None,
        created_at: now,
        updated_at: now,
    }
}

fn persist_custom(state: &CoreState, account: &Account) {
    state
        .db
        .lock()
        .create_account_with_contract(
            account,
            Some(&AccountCustomConfigInput {
                endpoint_url: "https://api.example.com/v1/chat/completions".into(),
                upstream_protocol: UpstreamProtocolKind::ChatCompletions,
            }),
            &[AccountModelCapabilityInput {
                public_model: UPSTREAM.into(),
                upstream_model: UPSTREAM.into(),
                protocol: UpstreamProtocolKind::ChatCompletions,
                source: None,
            }],
        )
        .unwrap();
}

fn billable_price() -> PlatformPrice {
    PlatformPrice {
        model: UPSTREAM.into(),
        group_id: Some(GROUP.into()),
        currency: "CNY".into(),
        input: Some(0.002),
        output: Some(0.008),
        cache_read: Some(0.001),
        cache_write: Some(0.003),
        source: "platform".into(),
        official_reference: false,
        unavailable_reason: None,
        valid_until: Utc::now().timestamp() + 3_600,
    }
}

fn snapshot(prices: Vec<PlatformPrice>, stale: bool) -> PlatformSnapshot {
    PlatformSnapshot {
        observed_at: Utc::now().timestamp(),
        stale,
        prices,
        ..PlatformSnapshot::default()
    }
}

fn link_with_snapshot(state: &CoreState, group: PlatformGroup, snapshot: PlatformSnapshot) {
    let db = state.db.lock();
    db.create_platform_account(
        PARENT,
        PlatformKind::NewApi,
        "New API",
        "https://api.example.com",
        None,
    )
    .unwrap();
    db.link_platform_account(ACCOUNT, PARENT, &group).unwrap();
    let token = db.platform_refresh_token(PARENT, Some(ACCOUNT)).unwrap();
    assert!(
        db.save_platform_refresh(PARENT, Some(ACCOUNT), &token, &snapshot)
            .unwrap()
    );
}

fn pinned_group() -> PlatformGroup {
    PlatformGroup {
        subscription_type: None,
        id: Some(GROUP.into()),
        platform: Some("openai".into()),
        auto_groups: Vec::new(),
        verified: true,
    }
}

#[test]
fn platform_attempt_rejects_old_key_or_endpoint_and_keeps_billed_row() {
    let (dir, state) = test_state("platform-identity");
    let account = custom_account(&state);
    persist_custom(&state, &account);
    let mut official = billable_price();
    official.official_reference = true;
    link_with_snapshot(
        &state,
        pinned_group(),
        snapshot(vec![billable_price(), official], false),
    );
    // Linking rewrites the Key's endpoint to the parent-owned site root, so
    // the attempt identity check compares against that root.
    assert!(matches!(
        platform_price_for_attempt(&state, &account, UPSTREAM, Some("https://api.example.com")),
        Some(PlatformAttemptPrice::Frozen(_))
    ));
    assert!(matches!(
        platform_price_for_attempt(
            &state,
            &account,
            UPSTREAM,
            Some("https://old.example/v1/chat/completions")
        ),
        Some(PlatformAttemptPrice::Unknown { .. })
    ));
    state
        .db
        .lock()
        .update_account(
            &account.id,
            &crate::models::AccountUpdate::default(),
            Some("new-key-cipher"),
            None,
        )
        .unwrap();
    assert!(matches!(
        platform_price_for_attempt(&state, &account, UPSTREAM, None),
        Some(PlatformAttemptPrice::Unknown { .. })
    ));
    drop(state);
    let _ = fs::remove_dir_all(dir);
}

fn attempt_context(upstream: &str) -> ForwardAttemptContext {
    ForwardAttemptContext {
        trace: RequestTrace::new(),
        client_body_bytes: 0,
        upstream_body_bytes: 0,
        attempt: 1,
        client_format: ApiFormat::ChatCompletions,
        upstream_format: ApiFormat::ChatCompletions,
        model: upstream.into(),
        requested_model: upstream.into(),
        resolved_alias: None,
        upstream_model: upstream.into(),
        stream: false,
        route: RouteLabel::Direct,
        known_secret: None,
        route_account_id: Some(ACCOUNT.into()),
        provider_id: Some(CUSTOM_PROVIDER_ID.into()),
        credential_account_id: Some(ACCOUNT.into()),
        client_key_id: None,
        client_key_name: None,
        platform_price: None,
        official_price: None,
    }
}

fn bind_for(
    state: &CoreState,
    account: &Account,
    upstream: &str,
) -> (RequestPricingSnapshot, ForwardAttemptContext) {
    let mut context = attempt_context(upstream);
    let pricing = bind_platform_attempt_price(
        state,
        account,
        &mut context,
        RequestPricingSnapshot::for_account(
            state,
            account,
            crate::routing_runtime::adapter_for_account(account, None),
            state.pricing_snapshot(),
        ),
        None,
    );
    (pricing, context)
}

fn assert_usd_and_quota_null(metrics: &ForwardMetrics) {
    assert_eq!(metrics.raw_cost_usd, None);
    assert_eq!(metrics.quota_debit, None);
    assert_eq!(metrics.effective_paid_cost_usd, None);
    assert_eq!(metrics.cost, 0.0);
    assert_ne!(metrics.cost_state, "priced");
}

/// Priced forward-log row needs the live token counts for the insert.
#[allow(clippy::too_many_arguments)]
fn persist_priced_row(
    state: &CoreState,
    account: &Account,
    pricing: &RequestPricingSnapshot,
    context: &ForwardAttemptContext,
    prompt: i64,
    completion: i64,
    cached: i64,
    cache_creation: i64,
) -> i64 {
    let mut metrics = pricing_metrics(
        pricing,
        UPSTREAM,
        prompt,
        completion,
        cached,
        cache_creation,
        None,
    );
    metrics.scope_to_provider(Some(account.provider_id.as_str()), true);
    DbAttemptSink::new(&state.db.lock())
        .insert(
            account,
            UPSTREAM,
            success_status_for_cost(metrics.cost_state),
            Some(200),
            metrics,
            None,
            context,
            None,
        )
        .unwrap()
}

#[test]
fn explicit_opencode_identity_is_copied_from_the_original_client_map() {
    let mut client = HeaderMap::new();
    client.insert("x-opencode-client", "desktop".parse().unwrap());
    client.insert("x-opencode-request", "req_keep".parse().unwrap());
    client.insert("x-opencode-project", "proj_keep".parse().unwrap());
    client.insert("x-session-id", "ses_not_identity".parse().unwrap());
    let mut upstream = reqwest::header::HeaderMap::new();
    copy_explicit_opencode_identity_headers(&mut upstream, &client);
    assert_eq!(upstream.get("x-opencode-client").unwrap(), "desktop");
    assert_eq!(upstream.get("x-opencode-request").unwrap(), "req_keep");
    assert_eq!(upstream.get("x-opencode-project").unwrap(), "proj_keep");
    assert!(upstream.get("x-session-id").is_none());
    assert!(upstream.get("x-opencode-session").is_none());
}

#[test]
fn frozen_exact_model_and_group_writes_native_cost_without_usd() {
    let (dir, state) = test_state("frozen");
    let account = custom_account(&state);
    persist_custom(&state, &account);
    link_with_snapshot(
        &state,
        pinned_group(),
        snapshot(vec![billable_price()], false),
    );
    let (pricing, context) = bind_for(&state, &account, UPSTREAM);
    let mut metrics = pricing_metrics(&pricing, "ignored-alias", 10, 5, 0, 0, None);
    metrics.scope_to_provider(Some(CUSTOM_PROVIDER_ID), true);
    assert_eq!(metrics.cost_state, "unknown");
    assert_usd_and_quota_null(&metrics);
    assert!(
        metrics
            .pricing_revision_id
            .as_deref()
            .is_some_and(|id| id.contains(PARENT) && id.contains(UPSTREAM) && id.contains(GROUP))
    );

    let id = persist_priced_row(&state, &account, &pricing, &context, 10, 5, 0, 0);
    let log = state.db.lock().list_forward_logs(1).unwrap().remove(0);
    assert_eq!(log.cost_state, "unknown");
    assert_eq!(log.raw_cost_usd, None);
    assert_eq!(log.quota_debit, None);
    assert_eq!(log.effective_paid_cost_usd, None);
    assert_eq!(log.cost, None);
    let native = state
        .db
        .lock()
        .forward_log_native_attribution(id)
        .unwrap()
        .unwrap();
    assert!((native.native_cost_value.unwrap() - (10.0 * 0.002 + 5.0 * 0.008)).abs() < 1e-12);
    assert_eq!(native.native_cost_unit.as_deref(), Some("CNY"));
    assert_eq!(native.native_cost_currency.as_deref(), Some("CNY"));
    drop(state);
    let _ = fs::remove_dir_all(dir);
}

#[test]
fn cache_arithmetic_applies_only_when_rates_are_known() {
    let (dir, state) = test_state("cache-known");
    let account = custom_account(&state);
    persist_custom(&state, &account);
    link_with_snapshot(
        &state,
        pinned_group(),
        snapshot(vec![billable_price()], false),
    );
    let (pricing, context) = bind_for(&state, &account, UPSTREAM);
    let id = persist_priced_row(&state, &account, &pricing, &context, 10, 2, 4, 1);
    let native = state
        .db
        .lock()
        .forward_log_native_attribution(id)
        .unwrap()
        .unwrap();
    let expected = 5.0 * 0.002 + 2.0 * 0.008 + 4.0 * 0.001 + 1.0 * 0.003;
    assert!((native.native_cost_value.unwrap() - expected).abs() < 1e-12);
    drop(state);
    let _ = fs::remove_dir_all(dir);
}

#[test]
fn cache_tokens_without_known_rate_stay_unknown_and_do_not_use_input() {
    let (dir, state) = test_state("cache-unknown");
    let account = custom_account(&state);
    persist_custom(&state, &account);
    let mut price = billable_price();
    price.cache_read = None;
    price.cache_write = None;
    link_with_snapshot(&state, pinned_group(), snapshot(vec![price], false));
    let (pricing, context) = bind_for(&state, &account, UPSTREAM);
    let mut metrics = pricing_metrics(&pricing, UPSTREAM, 10, 2, 4, 0, None);
    metrics.scope_to_provider(Some(CUSTOM_PROVIDER_ID), true);
    assert_eq!(metrics.cost_state, "unknown");
    assert_usd_and_quota_null(&metrics);
    let id = persist_priced_row(&state, &account, &pricing, &context, 10, 2, 4, 0);
    let native = state
        .db
        .lock()
        .forward_log_native_attribution(id)
        .unwrap()
        .unwrap();
    assert_eq!(native.native_cost_value, None);
    drop(state);
    let _ = fs::remove_dir_all(dir);
}

#[test]
fn auto_group_stale_expired_incomplete_unavailable_and_official_are_unknown() {
    let cases: &[(&str, PlatformGroup, PlatformSnapshot)] = &[
        (
            "auto",
            PlatformGroup {
                subscription_type: None,
                id: None,
                auto_groups: vec!["a".into()],
                ..PlatformGroup::default()
            },
            snapshot(vec![billable_price()], false),
        ),
        (
            "stale",
            pinned_group(),
            snapshot(vec![billable_price()], true),
        ),
    ];
    for (label, group, snap) in cases {
        let (dir, state) = test_state(label);
        let account = custom_account(&state);
        persist_custom(&state, &account);
        link_with_snapshot(&state, group.clone(), snap.clone());
        let (pricing, context) = bind_for(&state, &account, UPSTREAM);
        let mut metrics = pricing_metrics(&pricing, UPSTREAM, 10, 5, 0, 0, None);
        metrics.scope_to_provider(Some(CUSTOM_PROVIDER_ID), true);
        assert_eq!(metrics.cost_state, "unknown", "{label}");
        assert_usd_and_quota_null(&metrics);
        let id = persist_priced_row(&state, &account, &pricing, &context, 10, 5, 0, 0);
        let native = state
            .db
            .lock()
            .forward_log_native_attribution(id)
            .unwrap()
            .unwrap();
        assert_eq!(native.native_cost_value, None, "{label}");
        drop(state);
        let _ = fs::remove_dir_all(dir);
    }

    let mut expired = billable_price();
    expired.valid_until = Utc::now().timestamp() - 10;
    let mut incomplete = billable_price();
    incomplete.output = None;
    let mut unavailable = billable_price();
    unavailable.unavailable_reason = Some("quota".into());
    let mut official = billable_price();
    official.official_reference = true;
    let mut other_model = billable_price();
    other_model.model = "other".into();
    let mut other_group = billable_price();
    other_group.group_id = Some("other".into());

    for (label, price) in [
        ("expired", expired),
        ("incomplete", incomplete),
        ("unavailable", unavailable),
        ("official", official),
        ("model-mismatch", other_model),
        ("group-mismatch", other_group),
    ] {
        let (dir, state) = test_state(label);
        let account = custom_account(&state);
        persist_custom(&state, &account);
        link_with_snapshot(&state, pinned_group(), snapshot(vec![price], false));
        let (pricing, context) = bind_for(&state, &account, UPSTREAM);
        if label == "expired" {
            let mut metrics = pricing_metrics(&pricing, UPSTREAM, 10, 5, 0, 0, None);
            metrics.scope_to_provider(Some(CUSTOM_PROVIDER_ID), true);
            assert_eq!(metrics.cost_state, "unknown", "{label} estimate");
            assert_usd_and_quota_null(&metrics);
        }
        let id = persist_priced_row(&state, &account, &pricing, &context, 10, 5, 0, 0);
        let native = state
            .db
            .lock()
            .forward_log_native_attribution(id)
            .unwrap()
            .unwrap();
        assert_eq!(native.native_cost_value, None, "{label}");
        let log = state.db.lock().list_forward_logs(1).unwrap().remove(0);
        assert_eq!(log.cost_state, "unknown", "{label}");
        assert_eq!(log.raw_cost_usd, None, "{label}");
        drop(state);
        let _ = fs::remove_dir_all(dir);
    }
}

#[test]
fn linked_unknown_does_not_inherit_go_provider_prices() {
    let (dir, state) = test_state("no-go-fallback");
    let account = custom_account(&state);
    persist_custom(&state, &account);
    let mut official = billable_price();
    official.official_reference = true;
    link_with_snapshot(&state, pinned_group(), snapshot(vec![official], false));
    let (pricing, _) = bind_for(&state, &account, UPSTREAM);
    assert!(matches!(pricing, RequestPricingSnapshot::Platform(_)));
    let mut metrics = pricing_metrics(&pricing, "gpt-5", 1_000_000, 1_000_000, 0, 0, None);
    metrics.scope_to_provider(Some(CUSTOM_PROVIDER_ID), true);
    assert_eq!(metrics.cost_state, "unknown");
    assert_usd_and_quota_null(&metrics);
    drop(state);
    let _ = fs::remove_dir_all(dir);
}

#[test]
fn streaming_finalize_retains_the_attempt_frozen_price() {
    let (dir, state) = test_state("stream-retain");
    let account = custom_account(&state);
    persist_custom(&state, &account);
    link_with_snapshot(
        &state,
        pinned_group(),
        snapshot(vec![billable_price()], false),
    );
    let (pricing, context) = bind_for(&state, &account, UPSTREAM);
    let id = {
        let db = state.db.lock();
        DbAttemptSink::new(&db)
            .insert(
                &account,
                UPSTREAM,
                "streaming",
                Some(200),
                metadata_metrics(&pricing, None, "not_applicable"),
                None,
                &context,
                None,
            )
            .unwrap()
    };
    let preliminary = state
        .db
        .lock()
        .forward_log_native_attribution(id)
        .unwrap()
        .unwrap();
    assert_eq!(preliminary.native_cost_value, None);

    let mut later = billable_price();
    later.input = Some(9.0);
    later.output = Some(9.0);
    let token = state
        .db
        .lock()
        .platform_refresh_token(PARENT, Some(ACCOUNT))
        .unwrap();
    assert!(
        state
            .db
            .lock()
            .save_platform_refresh(PARENT, Some(ACCOUNT), &token, &snapshot(vec![later], false),)
            .unwrap()
    );

    let metrics = pricing_metrics(&pricing, UPSTREAM, 10, 5, 0, 0, None);
    DbAttemptSink::new(&state.db.lock())
        .finalize(
            id,
            success_status_for_cost(metrics.cost_state),
            Some(200),
            metrics,
            None,
            None,
            &context,
        )
        .unwrap();
    let native = state
        .db
        .lock()
        .forward_log_native_attribution(id)
        .unwrap()
        .unwrap();
    assert!((native.native_cost_value.unwrap() - (10.0 * 0.002 + 5.0 * 0.008)).abs() < 1e-12);
    let log = state.db.lock().list_forward_logs(1).unwrap().remove(0);
    assert_eq!(log.raw_cost_usd, None);
    assert_eq!(log.quota_debit, None);
    drop(state);
    let _ = fs::remove_dir_all(dir);
}

#[test]
fn fallback_attempt_rebinds_from_the_live_link_snapshot() {
    let (dir, state) = test_state("fallback-rebind");
    let account = custom_account(&state);
    persist_custom(&state, &account);
    link_with_snapshot(
        &state,
        pinned_group(),
        snapshot(vec![billable_price()], false),
    );
    let (first, first_ctx) = bind_for(&state, &account, UPSTREAM);
    let first_id = persist_priced_row(&state, &account, &first, &first_ctx, 10, 0, 0, 0);

    let mut next = billable_price();
    next.input = Some(0.05);
    next.output = Some(0.05);
    let token = state
        .db
        .lock()
        .platform_refresh_token(PARENT, Some(ACCOUNT))
        .unwrap();
    assert!(
        state
            .db
            .lock()
            .save_platform_refresh(PARENT, Some(ACCOUNT), &token, &snapshot(vec![next], false))
            .unwrap()
    );

    let (second, second_ctx) = bind_for(&state, &account, UPSTREAM);
    let second_id = persist_priced_row(&state, &account, &second, &second_ctx, 10, 0, 0, 0);
    let first_native = state
        .db
        .lock()
        .forward_log_native_attribution(first_id)
        .unwrap()
        .unwrap();
    let second_native = state
        .db
        .lock()
        .forward_log_native_attribution(second_id)
        .unwrap()
        .unwrap();
    assert!((first_native.native_cost_value.unwrap() - 0.02).abs() < 1e-12);
    assert!((second_native.native_cost_value.unwrap() - 0.50).abs() < 1e-12);
    drop(state);
    let _ = fs::remove_dir_all(dir);
}

#[test]
fn o05_fallback_from_a_to_b_keeps_each_attempts_native_rate() {
    let (dir, state) = test_state("o05-ab");
    let mut account_a = custom_account(&state);
    account_a.id = "custom-a".into();
    account_a.name = "A".into();
    persist_custom(&state, &account_a);

    let mut account_b = custom_account(&state);
    account_b.id = "custom-b".into();
    account_b.name = "B".into();
    account_b.key_cipher = state.encrypt_key("sk-custom-b").unwrap();
    persist_custom(&state, &account_b);

    {
        let db = state.db.lock();
        db.create_platform_account(
            "parent-a",
            PlatformKind::NewApi,
            "Parent A",
            "https://api.example.com",
            None,
        )
        .unwrap();
        db.create_platform_account(
            "parent-b",
            PlatformKind::NewApi,
            "Parent B",
            "https://api.example.com",
            None,
        )
        .unwrap();
        db.link_platform_account("custom-a", "parent-a", &pinned_group())
            .unwrap();
        db.link_platform_account("custom-b", "parent-b", &pinned_group())
            .unwrap();
        let token_a = db
            .platform_refresh_token("parent-a", Some("custom-a"))
            .unwrap();
        let mut price_a = billable_price();
        price_a.currency = "CNY".into();
        price_a.input = Some(0.002);
        price_a.output = Some(0.008);
        assert!(
            db.save_platform_refresh(
                "parent-a",
                Some("custom-a"),
                &token_a,
                &snapshot(vec![price_a], false)
            )
            .unwrap()
        );
        let token_b = db
            .platform_refresh_token("parent-b", Some("custom-b"))
            .unwrap();
        let mut price_b = billable_price();
        price_b.currency = "USD".into();
        price_b.input = Some(0.01);
        price_b.output = Some(0.03);
        assert!(
            db.save_platform_refresh(
                "parent-b",
                Some("custom-b"),
                &token_b,
                &snapshot(vec![price_b], false)
            )
            .unwrap()
        );
    }

    let (pricing_a, mut ctx_a) = bind_for(&state, &account_a, UPSTREAM);
    ctx_a.route_account_id = Some("custom-a".into());
    ctx_a.credential_account_id = Some("custom-a".into());
    let (pricing_b, mut ctx_b) = bind_for(&state, &account_b, UPSTREAM);
    ctx_b.route_account_id = Some("custom-b".into());
    ctx_b.credential_account_id = Some("custom-b".into());

    let id_a = persist_priced_row(&state, &account_a, &pricing_a, &ctx_a, 10, 5, 0, 0);
    let id_b = persist_priced_row(&state, &account_b, &pricing_b, &ctx_b, 10, 5, 0, 0);
    let native_a = state
        .db
        .lock()
        .forward_log_native_attribution(id_a)
        .unwrap()
        .unwrap();
    let native_b = state
        .db
        .lock()
        .forward_log_native_attribution(id_b)
        .unwrap()
        .unwrap();
    assert!((native_a.native_cost_value.unwrap() - (10.0 * 0.002 + 5.0 * 0.008)).abs() < 1e-12);
    assert_eq!(native_a.native_cost_currency.as_deref(), Some("CNY"));
    assert_eq!(native_a.native_cost_unit.as_deref(), Some("CNY"));
    assert!((native_b.native_cost_value.unwrap() - (10.0 * 0.01 + 5.0 * 0.03)).abs() < 1e-12);
    assert_eq!(native_b.native_cost_currency.as_deref(), Some("USD"));
    assert_eq!(native_b.native_cost_unit.as_deref(), Some("USD"));
    assert_ne!(native_a.native_cost_currency, native_b.native_cost_currency);
    drop(state);
    let _ = fs::remove_dir_all(dir);
}

#[test]
fn s02_secret_bearing_headers_are_detected() {
    let mut secret = reqwest::header::HeaderMap::new();
    secret.insert(
        reqwest::header::AUTHORIZATION,
        reqwest::header::HeaderValue::from_static("Bearer sk-secret"),
    );
    assert!(headers_carry_upstream_secret(&secret));
    assert!(!crate::custom_http::follows_redirects_with_secret(
        true,
        headers_carry_upstream_secret(&secret)
    ));
    assert!(!headers_carry_upstream_secret(
        &reqwest::header::HeaderMap::new()
    ));
}

#[test]
fn s01_forward_grant_allows_sealed_goat_origin() {
    assert!(
        crate::custom_http::ensure_sealed_secret_origin(
            "https://api.commandcode.ai/provider/v1/chat/completions",
            crate::provider::COMMAND_CODE_GOAT_BASE_URL,
        )
        .is_ok()
    );
}

#[tokio::test]
async fn p09_forward_attempt_emits_carried_legacy_tool_compat() {
    use crate::gateway::diagnostics::{RequestTrace, take_legacy_tool_compat_emissions};
    use crate::gateway::protocol::{
        CustomRouteSpec, LEGACY_TOOL_COMPAT_PROFILE, LEGACY_TOOL_COMPAT_VERSION, MaterializeSpec,
        materialize_parsed_request, parse_client_request,
    };
    use crate::http_client::RouteLabel;
    use crate::kernel::protocol::ApiFormat;
    use crate::models::UpstreamChannel;
    use axum::http::HeaderMap;
    use bytes::Bytes;
    use serde_json::{Value, json};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    let _ = take_legacy_tool_compat_emissions();

    let hits = Arc::new(AtomicUsize::new(0));
    let app = axum::Router::new()
        .fallback(axum::routing::any(p09_count_ok))
        .with_state(hits.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let (stop_tx, stop_rx) = tokio::sync::oneshot::channel();
    tokio::spawn(async move {
        let _ = axum::serve(listener, app)
            .with_graceful_shutdown(async {
                let _ = stop_rx.await;
            })
            .await;
    });
    let endpoint_url = format!("http://{addr}/v1/chat/completions");

    let (dir, state) = test_state("p09-legacy-compat");
    let mut config = state.config();
    config.proxy_mode = ProxyMode::Direct;
    state.set_config(config.clone()).unwrap();

    let account = custom_account(&state);
    persist_custom_at(&state, &account, &endpoint_url);
    grant_binding(
        &state,
        &account.id,
        &[RouteSpec {
            operation: EndpointOperation::ChatCreate,
            url: Some(endpoint_url.clone()),
        }],
        LegacyConnectionKind::CustomAccount,
        &account.id,
    );

    let secret = "sk-p09-must-not-leak";
    let client_body = json!({
        "model": "local-custom",
        "store": false,
        "input": format!("secret {secret}"),
        "tools": [
            {"type": "function", "name": "local", "parameters": {"type": "object"}},
            {"type": "web_search"}
        ],
        "tool_choice": "auto"
    });
    let client_bytes = Bytes::from(serde_json::to_vec(&client_body).unwrap());
    let parsed = parse_client_request(ApiFormat::Responses, client_bytes.clone()).unwrap();
    let plan = materialize_parsed_request(
        &parsed,
        &MaterializeSpec {
            client_model: "local-custom".into(),
            upstream_model: "local-custom".into(),
            resolved_alias: None,
            channel: UpstreamChannel::Go,
            upstream_base_override: None,
            original_model: None,
            forced_upstream: Some(ApiFormat::ChatCompletions),
            custom_route: Some(CustomRouteSpec {
                endpoint_url: endpoint_url.clone(),
                auth_kind: ocg_domain::dynamic::DynamicAuthKind::Bearer,
            }),
        },
    )
    .expect("legacy_compat conversion must produce a request plan");
    let carried = plan
        .legacy_tool_compat
        .as_ref()
        .expect("materialize must carry the converter marker to the forward consumer");
    assert_eq!(carried.profile, LEGACY_TOOL_COMPAT_PROFILE);
    assert_eq!(carried.version, LEGACY_TOOL_COMPAT_VERSION);
    assert_eq!(carried.dropped_hosted_tools, vec!["web_search".to_string()]);

    let trace = RequestTrace::new();
    let request_id = trace.request_id.clone();
    let client = reqwest::Client::builder().no_proxy().build().unwrap();
    let selection = live_send_selection(&state, &account, &plan);
    let result = forward_request(
        &client,
        RouteLabel::Direct,
        &state,
        &account,
        ProviderAdapterKind::ConfigurableHttp,
        &config,
        &plan,
        &trace,
        &client_bytes,
        1,
        false,
        HeaderMap::new(),
        state.pricing_snapshot(),
        None,
        &[],
        &selection,
    )
    .await
    .expect("local custom forward should complete");
    assert_eq!(hits.load(Ordering::SeqCst), 1, "{:?}", result.error_message);

    let emissions = take_legacy_tool_compat_emissions();
    assert_eq!(
        emissions.len(),
        1,
        "forward attempt must emit the carried marker"
    );
    let payload = &emissions[0];
    assert_eq!(payload["request_id"], request_id);
    assert_eq!(payload["profile"], LEGACY_TOOL_COMPAT_PROFILE);
    assert_eq!(payload["version"], LEGACY_TOOL_COMPAT_VERSION);
    assert_eq!(payload["dropped_hosted_tools"], json!(["web_search"]));
    let encoded = payload.to_string();
    assert!(!encoded.contains(secret), "{encoded}");
    assert!(!encoded.contains("local-custom"), "{encoded}");
    let body: Value = serde_json::from_slice(&plan.body).unwrap();
    assert!(!encoded.contains(&body.to_string()), "{encoded}");

    let _ = stop_tx.send(());
    drop(state);
    let _ = fs::remove_dir_all(dir);
}

async fn p09_count_ok(
    axum::extract::State(hits): axum::extract::State<
        std::sync::Arc<std::sync::atomic::AtomicUsize>,
    >,
) -> impl axum::response::IntoResponse {
    hits.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    (
        axum::http::StatusCode::OK,
        [("content-type", "application/json")],
        r#"{"id":"ok","object":"chat.completion","model":"local-custom","choices":[{"index":0,"message":{"role":"assistant","content":"ok"},"finish_reason":"stop"}],"usage":{"prompt_tokens":1,"completion_tokens":1}}"#,
    )
}

fn live_send_selection(
    state: &CoreState,
    account: &Account,
    plan: &crate::gateway::protocol::RequestPlan,
) -> crate::gateway::forwarder::LiveSendSelection {
    let db = state.db.lock();
    let binding = db
        .list_inference_bindings()
        .unwrap()
        .into_iter()
        .find(|row| row.account_id == account.id);
    crate::gateway::forwarder::LiveSendSelection::from_binding(
        account,
        binding.as_ref(),
        &plan.client_model,
        &plan.model,
        &plan.model,
    )
}

fn persist_custom_at(state: &CoreState, account: &Account, endpoint_url: &str) {
    state
        .db
        .lock()
        .create_account_with_contract(
            account,
            Some(&AccountCustomConfigInput {
                endpoint_url: endpoint_url.into(),
                upstream_protocol: UpstreamProtocolKind::ChatCompletions,
            }),
            &[AccountModelCapabilityInput {
                public_model: "local-custom".into(),
                upstream_model: "local-custom".into(),
                protocol: UpstreamProtocolKind::ChatCompletions,
                source: None,
            }],
        )
        .unwrap();
}

async fn spawn_hit_counter() -> (
    std::net::SocketAddr,
    Arc<AtomicUsize>,
    tokio::sync::oneshot::Sender<()>,
) {
    let hits = Arc::new(AtomicUsize::new(0));
    let app = axum::Router::new()
        .fallback(axum::routing::any(p09_count_ok))
        .with_state(hits.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let (stop_tx, stop_rx) = tokio::sync::oneshot::channel();
    tokio::spawn(async move {
        let _ = axum::serve(listener, app)
            .with_graceful_shutdown(async {
                let _ = stop_rx.await;
            })
            .await;
    });
    (addr, hits, stop_tx)
}

fn chat_plan(model: &str, custom_endpoint: Option<&str>) -> RequestPlan {
    RequestPlan {
        client: ApiFormat::ChatCompletions,
        upstream: ApiFormat::ChatCompletions,
        model: model.into(),
        client_model: model.into(),
        stream: false,
        body: Bytes::from(
            serde_json::to_vec(&json!({
                "model": model,
                "messages": [{"role": "user", "content": "hi"}]
            }))
            .unwrap(),
        ),
        channel: UpstreamChannel::Go,
        upstream_base_override: None,
        original_model: None,
        resolved_alias: Some(model.into()),
        custom_route: custom_endpoint.map(|endpoint_url| CustomRouteSpec {
            endpoint_url: endpoint_url.to_string(),
            auth_kind: ocg_domain::dynamic::DynamicAuthKind::Bearer,
        }),
        service_tier: None,
        custom_tools: Vec::new(),
        namespace_tools: Vec::new(),
        legacy_tool_compat: None,
        response_parallel_tool_calls: true,
        response_tool_choice: json!("auto"),
        response_tools: Vec::new(),
    }
}

fn grant_binding(
    state: &CoreState,
    account_id: &str,
    routes: &[RouteSpec],
    connection_kind: LegacyConnectionKind,
    legacy_id: &str,
) {
    let assigned = assigned_endpoints_for_routes(
        &connection_id_for_legacy(connection_kind, legacy_id),
        routes,
    );
    let ids: Vec<String> = assigned
        .iter()
        .map(|endpoint| endpoint.id.clone())
        .collect();
    let origins: Vec<String> = assigned
        .iter()
        .filter_map(|endpoint| endpoint.url.as_deref().and_then(normalize_origin))
        .collect();
    let db = state.db.lock();
    let binding = db
        .list_inference_bindings()
        .unwrap()
        .into_iter()
        .find(|row| row.account_id == account_id)
        .expect("stored binding");
    db.update_credential_binding(
        &binding.binding_id,
        None,
        None,
        Some(ids.as_slice()),
        Some(origins.as_slice()),
    )
    .unwrap();
}

fn assert_no_secret_leak(text: &str, secrets: &[&str]) {
    for secret in secrets {
        assert!(!text.contains(secret), "error leaked secret material");
    }
}

async fn forward_once(
    state: &CoreState,
    account: &Account,
    plan: &RequestPlan,
    selection: &crate::gateway::forwarder::LiveSendSelection,
    dynamics: &[crate::dynamic::DynamicProviderRuntime],
) -> crate::gateway::forwarder::ForwardResult {
    let config = state.config();
    let client = reqwest::Client::builder().no_proxy().build().unwrap();
    crate::gateway::forwarder::forward_request(
        &client,
        RouteLabel::Direct,
        state,
        account,
        crate::routing_runtime::adapter_for_account(account, None),
        &config,
        plan,
        &RequestTrace::new(),
        &plan.body,
        1,
        true,
        axum::http::HeaderMap::new(),
        state.pricing_snapshot(),
        None,
        dynamics,
        selection,
    )
    .await
    .expect("forward should complete locally")
}

#[tokio::test]
async fn r06_granted_same_origin_custom_sends_once() {
    let (addr, hits, stop_tx) = spawn_hit_counter().await;
    let endpoint = format!("http://{addr}/v1/chat/completions");
    let (dir, state) = test_state("r06-granted-same");
    let mut config = state.config();
    config.proxy_mode = ProxyMode::Direct;
    state.set_config(config).unwrap();
    let account = custom_account(&state);
    persist_custom_at(&state, &account, &endpoint);
    grant_binding(
        &state,
        &account.id,
        &[RouteSpec {
            operation: EndpointOperation::ChatCreate,
            url: Some(endpoint.clone()),
        }],
        LegacyConnectionKind::CustomAccount,
        &account.id,
    );
    let plan = chat_plan("local-custom", Some(&endpoint));
    let selection = live_send_selection(&state, &account, &plan);
    let result = forward_once(&state, &account, &plan, &selection, &[]).await;
    assert_eq!(hits.load(Ordering::SeqCst), 1, "{:?}", result.error_message);
    let _ = stop_tx.send(());
    drop(state);
    let _ = fs::remove_dir_all(dir);
}

#[tokio::test]
async fn r06_shared_custom_second_key_uses_owner_grant_and_model_override() {
    let (default_addr, default_hits, default_stop) = spawn_hit_counter().await;
    let (override_addr, override_hits, override_stop) = spawn_hit_counter().await;
    let default_url = format!("http://{default_addr}/v1/chat/completions");
    let override_url = format!("http://{override_addr}/v1/chat/completions");
    let (dir, state) = test_state("r06-shared-custom-override");
    let mut config = state.config();
    config.proxy_mode = ProxyMode::Direct;
    state.set_config(config).unwrap();

    let mut owner = custom_account(&state);
    owner.id = uuid::Uuid::new_v4().to_string();
    owner.name = "Owner".into();
    persist_custom_at(&state, &owner, &default_url);
    let destination_id = ocg_domain::destination::destination_id_for_custom_account(&owner.id);
    let mut second = custom_account(&state);
    second.id = uuid::Uuid::new_v4().to_string();
    second.name = "Second".into();
    second.key_cipher = state.encrypt_key("sk-shared-second").unwrap();
    state
        .db
        .lock()
        .commit_onboarding_existing_account(
            &second,
            Some(&destination_id),
            &crate::db::NewDashboardOperation {
                operation_id: uuid::Uuid::new_v4().to_string(),
                kind: "onboarding_commit".into(),
                payload_digest: "1".repeat(64),
                result_json: "{}".into(),
            },
        )
        .unwrap();
    let definition = ocg_domain::dynamic::DynamicProviderDefinition {
        preset_id: None,
        id: owner.id.clone(),
        name: "Shared".into(),
        endpoint_url: default_url.clone(),
        upstream_protocol: UpstreamProtocolKind::ChatCompletions,
        auth_kind: ocg_domain::dynamic::DynamicAuthKind::Bearer,
        mappings: vec![ocg_domain::dynamic::DynamicModelMapping {
            public_model: "shared-model".into(),
            upstream_model: "vendor/shared-model".into(),
            upstream_override: Some(ocg_domain::dynamic::DynamicModelUpstreamOverride {
                protocol: UpstreamProtocolKind::ChatCompletions,
                endpoint_url: override_url.clone(),
            }),
        }],
    };
    state
        .db
        .lock()
        .replace_custom_destination(
            &destination_id,
            &definition,
            &[ocg_domain::credential::credential_id_for_legacy_account(&second.id).to_string()],
        )
        .unwrap();

    let mut plan = chat_plan("vendor/shared-model", Some(&override_url));
    plan.resolved_alias = Some("shared-model".into());
    let selection = live_send_selection(&state, &second, &plan);
    let result = forward_once(&state, &second, &plan, &selection, &[]).await;
    assert_eq!(
        override_hits.load(Ordering::SeqCst),
        1,
        "{:?}",
        result.error_message
    );
    assert_eq!(default_hits.load(Ordering::SeqCst), 0);

    let _ = default_stop.send(());
    let _ = override_stop.send(());
    drop(state);
    let _ = fs::remove_dir_all(dir);
}

#[tokio::test]
async fn r06_ungranted_and_edited_destination_send_zero_times() {
    let old_key = "sk-test-old-grant";
    let (foreign_addr, foreign_hits, foreign_stop) = spawn_hit_counter().await;
    let (granted_addr, granted_hits, granted_stop) = spawn_hit_counter().await;
    let granted = format!("http://{granted_addr}/v1/chat/completions");
    let foreign = format!("http://{foreign_addr}/v1/chat/completions");
    let (dir, state) = test_state("r06-ungranted-edit");
    let mut config = state.config();
    config.proxy_mode = ProxyMode::Direct;
    state.set_config(config).unwrap();
    let mut account = custom_account(&state);
    account.id = "custom-edit".into();
    account.key_cipher = state.encrypt_key(old_key).unwrap();
    persist_custom_at(&state, &account, &granted);
    grant_binding(
        &state,
        &account.id,
        &[RouteSpec {
            operation: EndpointOperation::ChatCreate,
            url: Some(granted.clone()),
        }],
        LegacyConnectionKind::CustomAccount,
        &account.id,
    );
    let plan = chat_plan("local-custom", Some(&foreign));
    let selection = live_send_selection(&state, &account, &plan);
    let result = forward_once(&state, &account, &plan, &selection, &[]).await;
    assert_eq!(
        foreign_hits.load(Ordering::SeqCst),
        0,
        "{:?}",
        result.error_message
    );
    assert_eq!(granted_hits.load(Ordering::SeqCst), 0);
    let message = result.error_message.unwrap_or_default();
    assert!(
        message.contains("not authorized") || message.contains("refusing"),
        "{message}"
    );
    assert_no_secret_leak(&message, &[old_key]);

    state
        .db
        .lock()
        .upsert_account_custom_config(
            &account.id,
            &AccountCustomConfigInput {
                endpoint_url: foreign.clone(),
                upstream_protocol: UpstreamProtocolKind::ChatCompletions,
            },
        )
        .unwrap();
    let edited_plan = chat_plan("local-custom", Some(&foreign));
    let edited = forward_once(&state, &account, &edited_plan, &selection, &[]).await;
    assert_eq!(
        foreign_hits.load(Ordering::SeqCst),
        0,
        "{:?}",
        edited.error_message
    );
    assert_no_secret_leak(&edited.error_message.unwrap_or_default(), &[old_key]);

    let _ = foreign_stop.send(());
    let _ = granted_stop.send(());
    drop(state);
    let _ = fs::remove_dir_all(dir);
}

#[tokio::test]
async fn r06_granted_foreign_dynamic_override_sends_and_ungranted_does_not() {
    let (granted_addr, granted_hits, granted_stop) = spawn_hit_counter().await;
    let (foreign_addr, foreign_hits, foreign_stop) = spawn_hit_counter().await;
    let default_url = format!("http://{granted_addr}/v1");
    let foreign_url = format!("http://{foreign_addr}/v1");
    let (dir, state) = test_state("r06-foreign-override");
    let mut config = state.config();
    config.proxy_mode = ProxyMode::Direct;
    state.set_config(config).unwrap();
    let now = Utc::now();
    let provider_id = uuid::Uuid::new_v4().to_string();
    let runtime = crate::dynamic::DynamicProviderRuntime {
        preset_id: None,
        id: provider_id.clone(),
        name: "Lab".into(),
        endpoint_url: default_url.clone(),
        upstream_protocol: UpstreamProtocolKind::ChatCompletions,
        auth_kind: ocg_domain::dynamic::DynamicAuthKind::Bearer,
        mappings: vec![ocg_domain::dynamic::DynamicModelMapping {
            public_model: "lab".into(),
            upstream_model: "vendor/lab".into(),
            upstream_override: Some(ocg_domain::dynamic::DynamicModelUpstreamOverride {
                protocol: UpstreamProtocolKind::ChatCompletions,
                endpoint_url: foreign_url.clone(),
            }),
        }],
        created_at: now,
        updated_at: now,
        origin: crate::provider::ProviderOrigin::Custom,
        offering: "api".into(),
    };
    let mut account = custom_account(&state);
    account.id = "dyn-live".into();
    account.provider_id = provider_id.clone();
    account.name = "dyn".into();
    state
        .db
        .lock()
        .create_dynamic_provider(&runtime, &account)
        .unwrap();

    let plan = chat_plan("vendor/lab", None);
    let selection = live_send_selection(&state, &account, &plan);
    let ungranted = forward_once(
        &state,
        &account,
        &plan,
        &selection,
        std::slice::from_ref(&runtime),
    )
    .await;
    assert_eq!(
        foreign_hits.load(Ordering::SeqCst),
        0,
        "{:?}",
        ungranted.error_message
    );
    assert_eq!(granted_hits.load(Ordering::SeqCst), 0);

    grant_binding(
        &state,
        &account.id,
        &[
            RouteSpec {
                operation: EndpointOperation::ChatCreate,
                url: Some(default_url.clone()),
            },
            RouteSpec {
                operation: EndpointOperation::ChatCreate,
                url: Some(foreign_url.clone()),
            },
        ],
        LegacyConnectionKind::DynamicProvider,
        &provider_id,
    );
    let granted = forward_once(
        &state,
        &account,
        &plan,
        &selection,
        std::slice::from_ref(&runtime),
    )
    .await;
    assert_eq!(
        foreign_hits.load(Ordering::SeqCst),
        1,
        "{:?}",
        granted.error_message
    );
    assert_eq!(granted_hits.load(Ordering::SeqCst), 0);

    let _ = foreign_stop.send(());
    let _ = granted_stop.send(());
    drop(state);
    let _ = fs::remove_dir_all(dir);
}

#[tokio::test]
async fn r06_selected_then_rotate_disable_or_narrow_does_not_emit_old_key() {
    let old_key = "sk-test-live-old";
    let new_key = "sk-test-live-new";
    let (addr, hits, stop_tx) = spawn_hit_counter().await;
    let endpoint = format!("http://{addr}/v1/chat/completions");
    let (dir, state) = test_state("r06-rotate-disable");
    let mut config = state.config();
    config.proxy_mode = ProxyMode::Direct;
    state.set_config(config).unwrap();
    let mut account = custom_account(&state);
    account.id = "custom-r06".into();
    account.key_cipher = state.encrypt_key(old_key).unwrap();
    persist_custom_at(&state, &account, &endpoint);
    grant_binding(
        &state,
        &account.id,
        &[RouteSpec {
            operation: EndpointOperation::ChatCreate,
            url: Some(endpoint.clone()),
        }],
        LegacyConnectionKind::CustomAccount,
        &account.id,
    );
    let plan = chat_plan("local-custom", Some(&endpoint));
    let selection = live_send_selection(&state, &account, &plan);

    let rotated_cipher = state.encrypt_key(new_key).unwrap();
    state
        .db
        .lock()
        .rotate_account_credential(&account.id, &rotated_cipher)
        .unwrap();
    let rotated = forward_once(&state, &account, &plan, &selection, &[]).await;
    assert_eq!(
        hits.load(Ordering::SeqCst),
        0,
        "{:?}",
        rotated.error_message
    );
    assert_no_secret_leak(
        &rotated.error_message.unwrap_or_default(),
        &[old_key, new_key],
    );

    let live_account = state.db.lock().get_account(&account.id).unwrap().unwrap();
    let fresh = live_send_selection(&state, &live_account, &plan);
    let first = forward_once(&state, &live_account, &plan, &fresh, &[]).await;
    assert_eq!(hits.load(Ordering::SeqCst), 1, "{:?}", first.error_message);

    let binding_id = state
        .db
        .lock()
        .list_inference_bindings()
        .unwrap()
        .into_iter()
        .find(|row| row.account_id == account.id)
        .unwrap()
        .binding_id;
    state
        .db
        .lock()
        .update_credential_binding(&binding_id, None, Some(false), None, None)
        .unwrap();
    let disabled_binding = forward_once(&state, &live_account, &plan, &fresh, &[]).await;
    assert_eq!(
        hits.load(Ordering::SeqCst),
        1,
        "{:?}",
        disabled_binding.error_message
    );
    state
        .db
        .lock()
        .update_credential_binding(&binding_id, None, Some(true), None, None)
        .unwrap();
    crate::account_control::set_account_enabled(&state, &account.id, false).unwrap();
    let disabled_row = state.db.lock().get_account(&account.id).unwrap().unwrap();
    let disabled_account = forward_once(&state, &disabled_row, &plan, &fresh, &[]).await;
    assert_eq!(
        hits.load(Ordering::SeqCst),
        1,
        "{:?}",
        disabled_account.error_message
    );
    crate::account_control::set_account_enabled(&state, &account.id, true).unwrap();
    state
        .db
        .lock()
        .update_credential_binding(
            &binding_id,
            Some(&ModelScope::Only {
                models: vec!["other-model".into()],
            }),
            None,
            None,
            None,
        )
        .unwrap();
    let enabled_row = state.db.lock().get_account(&account.id).unwrap().unwrap();
    let narrowed = forward_once(&state, &enabled_row, &plan, &fresh, &[]).await;
    assert_eq!(
        hits.load(Ordering::SeqCst),
        1,
        "{:?}",
        narrowed.error_message
    );
    assert_no_secret_leak(
        &narrowed.error_message.unwrap_or_default(),
        &[old_key, new_key],
    );

    let _ = stop_tx.send(());
    drop(state);
    let _ = fs::remove_dir_all(dir);
}

#[tokio::test]
async fn r06_same_account_retry_repeats_live_checks_after_rotation() {
    let old_key = "sk-test-retry-old";
    let new_key = "sk-test-retry-new";
    let (addr, hits, stop_tx) = spawn_hit_counter().await;
    let endpoint = format!("http://{addr}/v1/chat/completions");
    let (dir, state) = test_state("r06-retry");
    let mut config = state.config();
    config.proxy_mode = ProxyMode::Direct;
    state.set_config(config).unwrap();
    let mut account = custom_account(&state);
    account.id = "custom-retry".into();
    account.key_cipher = state.encrypt_key(old_key).unwrap();
    persist_custom_at(&state, &account, &endpoint);
    grant_binding(
        &state,
        &account.id,
        &[RouteSpec {
            operation: EndpointOperation::ChatCreate,
            url: Some(endpoint.clone()),
        }],
        LegacyConnectionKind::CustomAccount,
        &account.id,
    );
    let plan = chat_plan("local-custom", Some(&endpoint));
    let selection = live_send_selection(&state, &account, &plan);
    let first = forward_once(&state, &account, &plan, &selection, &[]).await;
    assert_eq!(hits.load(Ordering::SeqCst), 1, "{:?}", first.error_message);

    let rotated = state.encrypt_key(new_key).unwrap();
    state
        .db
        .lock()
        .rotate_account_credential(&account.id, &rotated)
        .unwrap();
    let second = forward_once(&state, &account, &plan, &selection, &[]).await;
    assert_eq!(hits.load(Ordering::SeqCst), 1, "{:?}", second.error_message);
    assert_no_secret_leak(
        &second.error_message.unwrap_or_default(),
        &[old_key, new_key],
    );

    let _ = stop_tx.send(());
    drop(state);
    let _ = fs::remove_dir_all(dir);
}

fn sealed_chat_endpoint_id() -> String {
    endpoint_id_for(
        &connection_id_for_legacy(LegacyConnectionKind::BuiltinProvider, OPENCODE_PROVIDER_ID),
        EndpointOperation::ChatCreate,
    )
    .to_string()
}

fn persist_go_account(state: &CoreState, account: &Account) {
    state.db.lock().create_account(account).unwrap();
}

fn clear_binding_grants(state: &CoreState, account_id: &str) {
    let db = state.db.lock();
    let binding = db
        .list_inference_bindings()
        .unwrap()
        .into_iter()
        .find(|row| row.account_id == account_id)
        .expect("stored binding");
    db.update_credential_binding(&binding.binding_id, None, None, Some(&[]), Some(&[]))
        .unwrap();
}

#[tokio::test]
async fn r06_equivalent_custom_base_and_full_endpoint_sends() {
    let (addr, hits, stop_tx) = spawn_hit_counter().await;
    let base = format!("http://{addr}/v1");
    let (dir, state) = test_state("r06-equivalent-url");
    let mut config = state.config();
    config.proxy_mode = ProxyMode::Direct;
    state.set_config(config).unwrap();
    let mut account = custom_account(&state);
    account.id = "custom-equiv".into();
    persist_custom_at(&state, &account, &base);
    grant_binding(
        &state,
        &account.id,
        &[RouteSpec {
            operation: EndpointOperation::ChatCreate,
            url: Some(base.clone()),
        }],
        LegacyConnectionKind::CustomAccount,
        &account.id,
    );
    let plan = chat_plan("local-custom", Some(&base));
    let selection = live_send_selection(&state, &account, &plan);
    let result = forward_once(&state, &account, &plan, &selection, &[]).await;
    assert_eq!(hits.load(Ordering::SeqCst), 1, "{:?}", result.error_message);
    let _ = stop_tx.send(());
    drop(state);
    let _ = fs::remove_dir_all(dir);
}

#[tokio::test]
async fn r06_same_origin_path_edit_does_not_authorize_captured_url() {
    let (addr, hits, stop_tx) = spawn_hit_counter().await;
    let original = format!("http://{addr}/v1/chat/completions");
    let edited = format!("http://{addr}/other/v1/chat/completions");
    let (dir, state) = test_state("r06-path-edit");
    let mut config = state.config();
    config.proxy_mode = ProxyMode::Direct;
    state.set_config(config).unwrap();
    let mut account = custom_account(&state);
    account.id = "custom-path".into();
    persist_custom_at(&state, &account, &original);
    grant_binding(
        &state,
        &account.id,
        &[RouteSpec {
            operation: EndpointOperation::ChatCreate,
            url: Some(original.clone()),
        }],
        LegacyConnectionKind::CustomAccount,
        &account.id,
    );
    let plan = chat_plan("local-custom", Some(&original));
    let selection = live_send_selection(&state, &account, &plan);
    state
        .db
        .lock()
        .upsert_account_custom_config(
            &account.id,
            &AccountCustomConfigInput {
                endpoint_url: edited.clone(),
                upstream_protocol: UpstreamProtocolKind::ChatCompletions,
            },
        )
        .unwrap();
    grant_binding(
        &state,
        &account.id,
        &[RouteSpec {
            operation: EndpointOperation::ChatCreate,
            url: Some(edited),
        }],
        LegacyConnectionKind::CustomAccount,
        &account.id,
    );
    let result = forward_once(&state, &account, &plan, &selection, &[]).await;
    assert_eq!(hits.load(Ordering::SeqCst), 0, "{:?}", result.error_message);
    let message = result.error_message.unwrap_or_default();
    assert!(
        message.contains("not the current granted route") || message.contains("refusing"),
        "{message}"
    );
    let _ = stop_tx.send(());
    drop(state);
    let _ = fs::remove_dir_all(dir);
}

#[tokio::test]
async fn r06_cleared_sealed_endpoint_grant_zero_sends_until_restored() {
    let (addr, hits, stop_tx) = spawn_hit_counter().await;
    let (dir, state) = test_state("r06-sealed-grant");
    let mut config = state.config();
    config.proxy_mode = ProxyMode::Direct;
    config.upstream_base_url = format!("http://{addr}");
    state.set_config(config).unwrap();
    let mut account = custom_account(&state);
    account.id = "go-sealed".into();
    account.provider_id = OPENCODE_PROVIDER_ID.into();
    persist_go_account(&state, &account);
    clear_binding_grants(&state, &account.id);
    let plan = chat_plan("deepseek-v4-flash", None);
    let selection = live_send_selection(&state, &account, &plan);
    let revoked = forward_once(&state, &account, &plan, &selection, &[]).await;
    assert_eq!(
        hits.load(Ordering::SeqCst),
        0,
        "{:?}",
        revoked.error_message
    );

    grant_binding(
        &state,
        &account.id,
        &[RouteSpec {
            operation: EndpointOperation::ChatCreate,
            url: None,
        }],
        LegacyConnectionKind::BuiltinProvider,
        OPENCODE_PROVIDER_ID,
    );
    let restored = forward_once(&state, &account, &plan, &selection, &[]).await;
    assert_eq!(
        hits.load(Ordering::SeqCst),
        1,
        "{:?}",
        restored.error_message
    );
    assert_eq!(sealed_chat_endpoint_id().len(), 36);

    let _ = stop_tx.send(());
    drop(state);
    let _ = fs::remove_dir_all(dir);
}

#[tokio::test]
async fn r06_persisted_draft_does_not_send_from_captured_configured_snapshot() {
    let (addr, hits, stop_tx) = spawn_hit_counter().await;
    let default_url = format!("http://{addr}/v1");
    let (dir, state) = test_state("r06-draft");
    let mut config = state.config();
    config.proxy_mode = ProxyMode::Direct;
    state.set_config(config).unwrap();
    let now = Utc::now();
    let provider_id = uuid::Uuid::new_v4().to_string();
    let runtime = crate::dynamic::DynamicProviderRuntime {
        preset_id: None,
        id: provider_id.clone(),
        name: "Lab".into(),
        endpoint_url: default_url.clone(),
        upstream_protocol: UpstreamProtocolKind::ChatCompletions,
        auth_kind: ocg_domain::dynamic::DynamicAuthKind::Bearer,
        mappings: vec![ocg_domain::dynamic::DynamicModelMapping {
            public_model: "lab".into(),
            upstream_model: "vendor/lab".into(),
            upstream_override: None,
        }],
        created_at: now,
        updated_at: now,
        origin: crate::provider::ProviderOrigin::Custom,
        offering: "api".into(),
    };
    let mut account = custom_account(&state);
    account.id = "dyn-draft".into();
    account.provider_id = provider_id.clone();
    account.name = "dyn".into();
    state
        .db
        .lock()
        .create_dynamic_provider(&runtime, &account)
        .unwrap();
    grant_binding(
        &state,
        &account.id,
        &[RouteSpec {
            operation: EndpointOperation::ChatCreate,
            url: Some(default_url.clone()),
        }],
        LegacyConnectionKind::DynamicProvider,
        &provider_id,
    );
    let plan = chat_plan("vendor/lab", None);
    let selection = live_send_selection(&state, &account, &plan);
    state
        .db
        .lock()
        .commit_onboarding_resume(
            &runtime,
            true,
            None,
            None,
            None,
            None,
            None,
            &crate::db::NewDashboardOperation {
                operation_id: uuid::Uuid::new_v4().to_string(),
                kind: "onboarding_commit".into(),
                payload_digest: "0".repeat(64),
                result_json: "{}".into(),
            },
        )
        .unwrap();
    let result = forward_once(
        &state,
        &account,
        &plan,
        &selection,
        std::slice::from_ref(&runtime),
    )
    .await;
    assert_eq!(hits.load(Ordering::SeqCst), 0, "{:?}", result.error_message);

    let _ = stop_tx.send(());
    drop(state);
    let _ = fs::remove_dir_all(dir);
}

#[test]
fn official_api_attempt_pricing_is_native_frozen_and_never_attaches_to_foreign_routes() {
    use crate::official_api::{OfficialApiKind, pricing};
    let (dir, state) = test_state("official-api-cost");
    for (kind, model, amount, currency) in [
        (OfficialApiKind::Deepseek, "deepseek-flash", 0.0015, "USD"),
        (OfficialApiKind::Zhipu, "glm-5.3", 0.036, "CNY"),
    ] {
        let runtime = crate::official_api::tests::runtime(kind);
        let mut account = custom_account(&state);
        account.provider_id = runtime.id.clone();
        let body = Bytes::from(
            serde_json::to_vec(
                &json!({"model":model,"messages":[{"role":"user","content":"hello"}]}),
            )
            .unwrap(),
        );
        let parsed =
            crate::gateway::protocol::parse_client_request(ApiFormat::ChatCompletions, body)
                .unwrap();
        let mut plan = crate::gateway::protocol::materialize_parsed_request(
            &parsed,
            &crate::gateway::protocol::MaterializeSpec {
                client_model: model.into(),
                upstream_model: model.into(),
                resolved_alias: None,
                channel: UpstreamChannel::Go,
                upstream_base_override: None,
                original_model: None,
                forced_upstream: Some(ApiFormat::ChatCompletions),
                custom_route: Some(CustomRouteSpec {
                    endpoint_url: runtime.endpoint_url.clone(),
                    auth_kind: ocg_domain::dynamic::DynamicAuthKind::Bearer,
                }),
            },
        )
        .unwrap();
        let now = chrono::DateTime::parse_from_rfc3339("2026-09-17T01:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        // Exercise the shared estimator and real log-finalization path at a
        // deterministic observation time, rather than relying on CI wall time.
        let frozen = crate::official_api::OfficialAttemptPrice {
            provider_id: runtime.id.clone(),
            sheet: pricing::seed(kind),
            model: model.into(),
            at: now,
        };
        let price = RequestPricingSnapshot::OfficialApi(frozen.clone());
        let metrics = pricing_metrics(&price, model, 1000, 1000, 0, 0, None);
        assert_eq!(metrics.quota_debit, None);
        assert_eq!(metrics.effective_paid_cost_usd, None);
        if currency == "CNY" {
            assert_eq!(metrics.raw_cost_usd, None);
        } else {
            assert_eq!(metrics.raw_cost_usd, Some(amount));
        }
        let mut context = attempt_context(model);
        context.provider_id = Some(runtime.id.clone());
        context.official_price = Some(frozen.clone());
        let id = DbAttemptSink::new(&state.db.lock())
            .insert(
                &account,
                model,
                "success",
                Some(200),
                metrics.clone(),
                None,
                &context,
                None,
            )
            .unwrap();
        let native = state
            .db
            .lock()
            .forward_log_native_attribution(id)
            .unwrap()
            .unwrap();
        assert!((native.native_cost_value.unwrap() - amount).abs() < 1e-12);
        assert_eq!(native.native_cost_currency.as_deref(), Some(currency));
        // Finalizing a stream keeps the captured prices, not a later sheet.
        DbAttemptSink::new(&state.db.lock())
            .finalize(id, "success", Some(200), metrics, None, None, &context)
            .unwrap();
        assert_eq!(
            state
                .db
                .lock()
                .forward_log_native_attribution(id)
                .unwrap()
                .unwrap()
                .native_cost_value,
            Some(amount)
        );
        let mut positive = attempt_context(model);
        assert!(matches!(
            bind_official_attempt_price(
                &state,
                &account,
                &plan,
                std::slice::from_ref(&runtime),
                &mut positive,
                RequestPricingSnapshot::Unpriced
            ),
            RequestPricingSnapshot::OfficialApi(_)
        ));
        assert!(positive.official_price.is_some());
        for endpoint in [
            "https://attacker.test/chat/completions",
            "http://127.0.0.1:9/chat/completions",
        ] {
            plan.custom_route = Some(CustomRouteSpec {
                endpoint_url: endpoint.into(),
                auth_kind: ocg_domain::dynamic::DynamicAuthKind::Bearer,
            });
            let mut context = attempt_context(model);
            assert!(matches!(
                bind_official_attempt_price(
                    &state,
                    &account,
                    &plan,
                    std::slice::from_ref(&runtime),
                    &mut context,
                    RequestPricingSnapshot::Unpriced
                ),
                RequestPricingSnapshot::Unpriced
            ));
            assert!(context.official_price.is_none());
        }
        plan.custom_route = Some(CustomRouteSpec {
            endpoint_url: runtime.endpoint_url.clone(),
            auth_kind: ocg_domain::dynamic::DynamicAuthKind::Bearer,
        });
        plan.body = Bytes::from_static(br#"{"tools":[{"type":"web_search"}]}"#);
        let mut context = attempt_context(model);
        assert!(matches!(
            bind_official_attempt_price(
                &state,
                &account,
                &plan,
                std::slice::from_ref(&runtime),
                &mut context,
                RequestPricingSnapshot::Unpriced
            ),
            RequestPricingSnapshot::Unpriced
        ));
        context.official_price = Some(frozen.clone());
        let missing = metadata_metrics(&price, None, "usage_missing");
        let id = DbAttemptSink::new(&state.db.lock())
            .insert(
                &account,
                model,
                "success_no_usage",
                Some(200),
                missing,
                None,
                &context,
                None,
            )
            .unwrap();
        assert!(
            state
                .db
                .lock()
                .forward_log_native_attribution(id)
                .unwrap()
                .unwrap()
                .native_cost_value
                .is_none()
        );
    }
    drop(state);
    let _ = fs::remove_dir_all(dir);
}
