use super::*;

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
use crate::http_client::RouteLabel;
use crate::kernel::protocol::ApiFormat;
use crate::models::{
    Account, AccountCustomConfigInput, AccountModelCapabilityInput, AccountSetupStep, AccountType,
};
use crate::platform::{PlatformGroup, PlatformKind, PlatformPrice, PlatformSnapshot};
use crate::provider::{CUSTOM_PROVIDER_ID, UpstreamProtocolKind};
use crate::state::CoreStateInner;
use chrono::Utc;
use std::fs;
use std::path::PathBuf;
use std::sync::Arc;

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
    assert!(matches!(
        platform_price_for_attempt(
            &state,
            &account,
            UPSTREAM,
            Some("https://api.example.com/v1/chat/completions")
        ),
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
        RequestPricingSnapshot::for_account(state, account, state.pricing_snapshot()),
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
fn o04_expired_price_is_not_used_for_new_request_estimate() {
    let mut expired = billable_price();
    expired.valid_until = Utc::now().timestamp() - 10;
    let (dir, state) = test_state("o04-expired");
    let account = custom_account(&state);
    persist_custom(&state, &account);
    link_with_snapshot(&state, pinned_group(), snapshot(vec![expired], false));
    let (pricing, context) = bind_for(&state, &account, UPSTREAM);
    let mut metrics = pricing_metrics(&pricing, UPSTREAM, 10, 5, 0, 0, None);
    metrics.scope_to_provider(Some(CUSTOM_PROVIDER_ID), true);
    assert_eq!(metrics.cost_state, "unknown");
    assert_usd_and_quota_null(&metrics);
    let id = persist_priced_row(&state, &account, &pricing, &context, 10, 5, 0, 0);
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
fn s02_secret_bearing_snapshot_requests_do_not_follow_redirects() {
    assert!(!crate::custom_http::follows_redirects_with_secret(
        true, true
    ));
    assert!(crate::custom_http::follows_redirects_with_secret(
        true, false
    ));
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
fn s01_forward_grant_refuses_a_cross_origin_user_override() {
    assert!(
        crate::custom_http::ensure_secret_origin_granted(
            "https://evil.example/v1/chat/completions",
            &["https://lab.example/v1".to_string()],
        )
        .is_err()
    );
    assert!(
        crate::custom_http::ensure_sealed_secret_origin(
            "https://api.commandcode.ai/provider/v1/chat/completions",
            crate::provider::COMMAND_CODE_GOAT_BASE_URL,
        )
        .is_ok()
    );
}
