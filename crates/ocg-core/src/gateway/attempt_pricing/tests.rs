use super::*;
use crate::crypto::{KeyCipher, StaticKeyCipher};
use crate::db::Database;
use crate::models::{
    Account, AccountCustomConfigInput, AccountModelCapabilityInput, AccountSetupStep, AccountType,
};
use crate::platform::{PlatformGroup, PlatformKind, PlatformPrice, PlatformSnapshot};
use crate::provider::{CUSTOM_PROVIDER_ID, ProviderAdapterKind, UpstreamProtocolKind};
use crate::state::{CoreState, CoreStateInner};
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
        "ocg-attempt-pricing-{}-{}",
        label,
        uuid::Uuid::new_v4()
    ));
    fs::create_dir_all(&dir).unwrap();
    dir
}

fn test_state(label: &str) -> (PathBuf, CoreState) {
    let dir = temp_dir(label);
    let cipher: Arc<dyn KeyCipher + Send + Sync> =
        Arc::new(StaticKeyCipher::new("attempt-pricing"));
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

fn goat_account(state: &CoreState) -> Account {
    let now = Utc::now();
    Account {
        id: "acct-1".into(),
        provider_id: crate::provider::COMMAND_CODE_PROVIDER_ID.into(),
        credential_kind: crate::provider::default_credential_kind(),
        quota_scope: crate::provider::default_quota_scope(),
        name: "acct-1".into(),
        username: None,
        password_cipher: None,
        key_cipher: state.encrypt_key("sk-guard").unwrap(),
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

#[test]
fn token_pricing_covers_plain_text_and_rejects_media_or_priority_tiers() {
    assert!(token_pricing_covers_request(
        br#"{"messages":[{"role":"user","content":"hello"}]}"#,
        None
    ));
    assert!(!token_pricing_covers_request(
        br#"{"messages":[{"content":[{"type":"image_url","image_url":{"url":"https://example.test/a.png"}}]}]}"#,
        None
    ));
    assert!(!token_pricing_covers_request(
        br#"{"input":"hello"}"#,
        Some("priority")
    ));
    assert!(token_pricing_covers_request(
        br#"{"tools":[{"type":"function","function":{"name":"save_record","parameters":{"type":"object","properties":{"inline_data":{"type":"string"}}}}}]}"#,
        None
    ));
}

#[test]
fn platform_price_does_not_freeze_a_hosted_tool_request() {
    use crate::gateway::protocol::RequestPlan;
    use crate::kernel::protocol::ApiFormat;
    use crate::models::UpstreamChannel;
    use bytes::Bytes;
    let plan = RequestPlan {
        client: ApiFormat::Responses,
        upstream: ApiFormat::Responses,
        model: "m".into(),
        client_model: "m".into(),
        stream: false,
        body: Bytes::from_static(br#"{"tools":[{"type":"web_search"}]}"#),
        channel: UpstreamChannel::Go,
        upstream_base_override: None,
        original_model: None,
        resolved_alias: None,
        custom_route: None,
        service_tier: None,
        custom_tools: Vec::new(),
        namespace_tools: Vec::new(),
        legacy_tool_compat: None,
        response_parallel_tool_calls: true,
        response_tool_choice: serde_json::json!("auto"),
        response_tools: Vec::new(),
    };
    let pricing =
        RequestPricingSnapshot::Platform(PlatformAttemptPrice::Frozen(FrozenPlatformPrice {
            provenance: "platform".into(),
            currency: "USD".into(),
            input: 1.0,
            output: 1.0,
            cache_read: None,
            cache_write: None,
        }));
    let restricted = restrict_platform_to_token_coverage(&plan, pricing);
    match restricted {
        RequestPricingSnapshot::Platform(PlatformAttemptPrice::Unknown { provenance }) => {
            assert_eq!(provenance.as_deref(), Some("platform:hosted_tool_unpriced"));
        }
        _ => panic!("hosted tools must not keep a frozen platform price"),
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
    // Linking moves the Key to the parent's configured inference routes.
    // The attempt identity check compares the exact route sent upstream.
    assert!(matches!(
        platform_price_for_attempt(
            &state,
            &(&account).into(),
            UPSTREAM,
            Some("https://api.example.com/v1/chat/completions")
        ),
        Some(PlatformAttemptPrice::Frozen(_))
    ));
    assert!(matches!(
        platform_price_for_attempt(
            &state,
            &(&account).into(),
            UPSTREAM,
            Some("https://api.example.com/v1/other")
        ),
        Some(PlatformAttemptPrice::Unknown { .. })
    ));
    assert!(matches!(
        platform_price_for_attempt(
            &state,
            &(&account).into(),
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
        platform_price_for_attempt(&state, &(&account).into(), UPSTREAM, None),
        Some(PlatformAttemptPrice::Unknown { .. })
    ));
    drop(state);
    let _ = fs::remove_dir_all(dir);
}

#[test]
fn command_code_requests_use_the_verified_provider_price_and_multiplier() {
    let (dir, state) = test_state("goat-pricing");
    let goat = goat_account(&state);
    let missing = RequestPricingSnapshot::for_account(
        &state,
        &(&goat).into(),
        ProviderAdapterKind::CommandCodeGoat,
        state.pricing_snapshot(),
    );
    let mut missing_metrics = pricing_metrics(
        &missing,
        "deepseek-v4-flash",
        1_000_000,
        100_000,
        0,
        0,
        None,
    );
    missing_metrics.scope_to_provider(Some(&goat.provider_id), true);
    assert_eq!(missing_metrics.cost_state, "unpriced");
    assert_eq!(missing_metrics.raw_cost_usd, None);
    assert_eq!(missing_metrics.pricing_revision_id, None);

    let snapshot = crate::pricing::ProviderScopedPricingSnapshot::new(
        crate::provider::COMMAND_CODE_PROVIDER_ID,
        "goat-runtime-test",
        "2030-01-01T00:00:00Z",
        None,
        crate::pricing::GOAT_SOURCE_URL,
        "goat-runtime-hash",
        crate::pricing::ProviderPricingEvidence::Verified,
        vec![
            crate::pricing::ProviderPricingValue::new(
                "deepseek-v4-flash",
                "DeepSeek V4 Flash (latest)",
                Some(0.22),
                Some(0.66),
                Some(0.007),
                None,
                Some(70.0),
                Some(60.0),
                Some(10.0),
                Some("USD".into()),
                None,
                None,
                crate::pricing::PricingTimeWindow::Always,
            )
            .unwrap(),
        ],
    )
    .unwrap();
    crate::pricing::store_provider_pricing_snapshot(&state.db.lock(), &snapshot).unwrap();

    let pricing = RequestPricingSnapshot::for_account(
        &state,
        &(&goat).into(),
        ProviderAdapterKind::CommandCodeGoat,
        state.pricing_snapshot(),
    );
    let mut metrics = pricing_metrics(
        &pricing,
        "deepseek/deepseek-v4-flash",
        1_000_000,
        100_000,
        0,
        0,
        None,
    );
    metrics.scope_to_provider(Some(&goat.provider_id), true);

    assert_eq!(metrics.cost_state, "priced");
    assert!((metrics.raw_cost_usd.unwrap() - 0.286).abs() < 1e-12);
    assert!((metrics.quota_multiplier.unwrap() - (70.0 / 60.0)).abs() < 1e-12);
    assert!((metrics.cost - (0.286 * 70.0 / 60.0)).abs() < 1e-12);
    assert_eq!(
        metrics.pricing_revision_id.as_deref(),
        Some("goat-runtime-test")
    );

    drop(state);
    let _ = fs::remove_dir_all(dir);
}
