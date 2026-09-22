use super::*;
use crate::crypto::{KeyCipher, StaticKeyCipher};
use crate::kernel::catalog::QuotaScope;
use crate::kernel::ids::ZEN_FREE_ACCOUNT_ID;
use crate::provider::ProviderAdapterKind;
use ocg_domain::destination::{
    AdapterKind, AuthScheme, Destination, LegacyDestinationRef, ModelResolution,
    destination_id_for_builtin, sealed_capabilities,
};
use ocg_gateway::selector::{CONVERSATION_TTL, MAX_CONVERSATIONS, SelectionError};
use std::sync::Arc;
use std::time::Duration;

fn account(id: &str, enabled: bool) -> Account {
    let cipher: Arc<dyn KeyCipher + Send + Sync> = Arc::new(StaticKeyCipher::new("test"));
    Account {
        id: id.into(),
        provider_id: crate::provider::default_provider_id(),

        credential_kind: crate::provider::default_credential_kind(),
        quota_scope: crate::provider::default_quota_scope(),
        name: id.into(),
        username: None,
        password_cipher: None,
        key_cipher: cipher.encrypt(id).unwrap(),
        enabled,
        account_type: crate::models::AccountType::Key,
        setup_step: crate::models::AccountSetupStep::Ready,
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
        created_at: Utc::now(),
        updated_at: Utc::now(),
    }
}

fn cooling(id: &str) -> Account {
    let mut item = account(id, true);
    item.cooldown_generic_until = Some(Utc::now() + chrono::Duration::hours(1));
    item.cooldown_until = item.cooldown_generic_until;
    item
}

fn frozen_wall() -> DateTime<Utc> {
    DateTime::from_naive_utc_and_offset(
        chrono::NaiveDate::from_ymd_opt(2024, 1, 2)
            .unwrap()
            .and_hms_opt(3, 4, 5)
            .unwrap(),
        Utc,
    )
}

fn cooling_at(id: &str, until: DateTime<Utc>) -> Account {
    let mut item = account(id, true);
    item.cooldown_generic_until = Some(until);
    item.cooldown_until = Some(until);
    item
}

fn zen_account(enabled: bool) -> Account {
    let mut item = account(ZEN_FREE_ACCOUNT_ID, enabled);
    item.provider_id = OPENCODE_ZEN_FREE_PROVIDER_ID.into();
    item.credential_kind = CredentialKind::None;
    item.quota_scope = QuotaScope::EgressIp;
    item.key_cipher.clear();
    item
}

fn routing_candidate(
    account: Account,
    channel: UpstreamChannel,
    resolved_model: &str,
) -> RoutingCandidate {
    let adapter = adapter_for_account(&account, None);
    routing_candidate_with_adapter(account, channel, resolved_model, adapter)
}

fn routing_candidate_with_adapter(
    account: Account,
    channel: UpstreamChannel,
    resolved_model: &str,
    adapter: ProviderAdapterKind,
) -> RoutingCandidate {
    RoutingCandidate {
        adapter,
        account,
        channel,
        resolved_model: resolved_model.to_string(),
    }
}

fn destination(adapter: AdapterKind, provider_id: &str) -> Destination {
    Destination {
        id: destination_id_for_builtin(provider_id),
        legacy: LegacyDestinationRef::Builtin(provider_id.to_string()),
        adapter,
        name: "dest".into(),
        brand_family: None,
        base_url: None,
        protocols: Vec::new(),
        protocol_routes: Vec::new(),
        auth_scheme: AuthScheme::None,
        model_resolution: ModelResolution::AdapterDefined,
        catalog: Vec::new(),
        capabilities: sealed_capabilities(adapter),
        plan: None,
        max_credentials: None,
        observer_credential_id: None,
        enabled: true,
    }
}

fn go_candidate(item: Account) -> RoutingCandidate {
    routing_candidate(item, UpstreamChannel::Go, "test-model")
}

#[allow(clippy::too_many_arguments)]
fn pick_index(
    runtime: &RoutingRuntime,
    candidates: &[RoutingCandidate],
    mode: RoutingMode,
    conversation_sticky: bool,
    conversation_key: Option<&str>,
    exclude_ids: &[&str],
    free_channel_available: bool,
    wall: DateTime<Utc>,
    mono: Instant,
) -> Option<usize> {
    runtime
        .try_select_candidate_index_at(
            candidates,
            mode,
            conversation_sticky,
            conversation_key,
            exclude_ids,
            free_channel_available,
            wall,
            mono,
        )
        .expect("candidates must not contain duplicate account ids")
}

#[test]
fn sticky_global_keeps_current_when_higher_priority_recovers() {
    let runtime = RoutingRuntime::new();
    let first = vec![cooling("a"), account("b", true)];
    assert_eq!(
        runtime
            .select_account(&first, RoutingMode::StickyGlobal, false, None, &[])
            .unwrap()
            .id,
        "b"
    );
    let recovered = vec![account("a", true), account("b", true)];
    assert_eq!(
        runtime
            .select_account(&recovered, RoutingMode::StickyGlobal, false, None, &[])
            .unwrap()
            .id,
        "b"
    );
}

#[test]
fn sticky_global_switches_when_current_persistently_unavailable() {
    let runtime = RoutingRuntime::new();
    let accounts = vec![account("a", true), account("b", true)];
    assert_eq!(
        runtime
            .select_account(&accounts, RoutingMode::StickyGlobal, false, None, &[])
            .unwrap()
            .id,
        "a"
    );
    let disabled = vec![account("a", false), account("b", true)];
    assert_eq!(
        runtime
            .select_account(&disabled, RoutingMode::StickyGlobal, false, None, &[])
            .unwrap()
            .id,
        "b"
    );
    assert_eq!(
        runtime
            .select_account(&accounts, RoutingMode::StickyGlobal, false, None, &[])
            .unwrap()
            .id,
        "b"
    );

    let runtime = RoutingRuntime::new();
    assert_eq!(
        runtime
            .select_account(&accounts, RoutingMode::StickyGlobal, false, None, &[])
            .unwrap()
            .id,
        "a"
    );
    let cooled = vec![cooling("a"), account("b", true)];
    assert_eq!(
        runtime
            .select_account(&cooled, RoutingMode::StickyGlobal, false, None, &[])
            .unwrap()
            .id,
        "b"
    );
    assert_eq!(
        runtime
            .select_account(&accounts, RoutingMode::StickyGlobal, false, None, &[])
            .unwrap()
            .id,
        "b"
    );
}

#[test]
fn round_robin_cycles_and_skips_unavailable() {
    let runtime = RoutingRuntime::new();
    let accounts = vec![account("a", true), cooling("b"), account("c", true)];
    assert_eq!(
        runtime
            .select_account(&accounts, RoutingMode::RoundRobin, false, None, &[])
            .unwrap()
            .id,
        "a"
    );
    assert_eq!(
        runtime
            .select_account(&accounts, RoutingMode::RoundRobin, false, None, &[])
            .unwrap()
            .id,
        "c"
    );
    assert_eq!(
        runtime
            .select_account(&accounts, RoutingMode::RoundRobin, false, None, &[])
            .unwrap()
            .id,
        "a"
    );
}

#[test]
fn round_robin_cursor_survives_reordering_and_missing_accounts_by_id() {
    let runtime = RoutingRuntime::new();
    let original = vec![account("a", true), account("b", true), account("c", true)];
    assert_eq!(
        runtime
            .select_account(&original, RoutingMode::RoundRobin, false, None, &[])
            .unwrap()
            .id,
        "a"
    );

    let reordered = vec![account("c", true), account("a", true), account("b", true)];
    assert_eq!(
        runtime
            .select_account(&reordered, RoutingMode::RoundRobin, false, None, &[])
            .unwrap()
            .id,
        "b"
    );

    let missing_cursor = vec![account("a", true), account("c", true)];
    assert_eq!(
        runtime
            .select_account(&missing_cursor, RoutingMode::RoundRobin, false, None, &[],)
            .unwrap()
            .id,
        "a"
    );
}

#[test]
fn concurrent_round_robin_selection_updates_one_shared_cursor() {
    let runtime = Arc::new(RoutingRuntime::new());
    let accounts = Arc::new(vec![account("a", true), account("b", true)]);
    let workers = (0..100)
        .map(|_| {
            let runtime = runtime.clone();
            let accounts = accounts.clone();
            std::thread::spawn(move || {
                runtime
                    .select_account(&accounts, RoutingMode::RoundRobin, false, None, &[])
                    .unwrap()
                    .id
            })
        })
        .collect::<Vec<_>>();
    let selected = workers
        .into_iter()
        .map(|worker| worker.join().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(selected.iter().filter(|id| id.as_str() == "a").count(), 50);
    assert_eq!(selected.iter().filter(|id| id.as_str() == "b").count(), 50);
    assert_eq!(
        runtime
            .select_account(&accounts, RoutingMode::RoundRobin, false, None, &[])
            .unwrap()
            .id,
        "a"
    );
}

#[test]
fn conversation_sticky_prefers_binding_without_advancing_round_robin() {
    let runtime = RoutingRuntime::new();
    let accounts = vec![account("a", true), account("b", true)];
    let key = "conv-1";
    assert_eq!(
        runtime
            .select_account(&accounts, RoutingMode::RoundRobin, true, Some(key), &[],)
            .unwrap()
            .id,
        "a"
    );
    assert_eq!(
        runtime
            .select_account(&accounts, RoutingMode::RoundRobin, true, Some(key), &[],)
            .unwrap()
            .id,
        "a"
    );
    assert_eq!(
        runtime
            .select_account(&accounts, RoutingMode::RoundRobin, false, None, &[])
            .unwrap()
            .id,
        "b",
        "conversation hits must not advance the round-robin cursor"
    );
}

#[test]
fn conversation_sticky_rebinds_when_bound_account_excluded() {
    let runtime = RoutingRuntime::new();
    let accounts = vec![account("a", true), account("b", true)];
    let key = "conv-2";
    assert_eq!(
        runtime
            .select_account(&accounts, RoutingMode::StrictPriority, true, Some(key), &[],)
            .unwrap()
            .id,
        "a"
    );
    assert_eq!(
        runtime
            .select_account(
                &accounts,
                RoutingMode::StrictPriority,
                true,
                Some(key),
                &["a"],
            )
            .unwrap()
            .id,
        "b"
    );
    assert_eq!(
        runtime
            .select_account(&accounts, RoutingMode::StrictPriority, true, Some(key), &[],)
            .unwrap()
            .id,
        "b"
    );
}

#[test]
fn conversation_capacity_evicts_least_recently_used() {
    let runtime = RoutingRuntime::new();
    let accounts = vec![account("a", true)];
    for index in 0..=MAX_CONVERSATIONS {
        let key = format!("k{index}");
        runtime
            .select_account(
                &accounts,
                RoutingMode::StrictPriority,
                true,
                Some(&key),
                &[],
            )
            .unwrap();
    }
    let now = Instant::now();
    assert!(runtime.sticky_binding_at("k0", now).is_none());
    assert!(
        runtime
            .sticky_binding_at(&format!("k{MAX_CONVERSATIONS}"), now)
            .is_some()
    );
}

#[test]
fn conversation_hit_refreshes_lru_order_before_capacity_eviction() {
    let runtime = RoutingRuntime::new();
    let accounts = vec![account("a", true)];
    for index in 0..MAX_CONVERSATIONS {
        let key = format!("k{index}");
        runtime
            .select_account(
                &accounts,
                RoutingMode::StrictPriority,
                true,
                Some(&key),
                &[],
            )
            .unwrap();
    }
    runtime
        .select_account(
            &accounts,
            RoutingMode::StrictPriority,
            true,
            Some("k0"),
            &[],
        )
        .unwrap();
    runtime
        .select_account(
            &accounts,
            RoutingMode::StrictPriority,
            true,
            Some("new"),
            &[],
        )
        .unwrap();

    let now = Instant::now();
    assert!(runtime.sticky_binding_at("k0", now).is_some());
    assert!(runtime.sticky_binding_at("k1", now).is_none());
    assert!(runtime.sticky_binding_at("new", now).is_some());
}

#[test]
fn reset_clears_runtime_state() {
    let runtime = RoutingRuntime::new();
    let accounts = vec![account("a", true), account("b", true)];
    runtime
        .select_account(&accounts, RoutingMode::RoundRobin, true, Some("c1"), &[])
        .unwrap();
    runtime.reset();
    assert!(runtime.sticky_binding("c1").is_none());
    assert_eq!(
        runtime
            .select_account(&accounts, RoutingMode::RoundRobin, false, None, &[])
            .unwrap()
            .id,
        "a"
    );
}

#[test]
fn disabling_conversation_sticky_ignores_existing_bindings() {
    let runtime = RoutingRuntime::new();
    let accounts = vec![account("a", true), account("b", true)];
    assert_eq!(
        runtime
            .select_account(&accounts, RoutingMode::RoundRobin, true, Some("bound"), &[],)
            .unwrap()
            .id,
        "a"
    );
    assert_eq!(
        runtime
            .select_account(
                &accounts,
                RoutingMode::RoundRobin,
                false,
                Some("bound"),
                &[],
            )
            .unwrap()
            .id,
        "b"
    );
}

#[test]
fn selection_cooldown_uses_injected_wall() {
    let runtime = RoutingRuntime::new();
    let wall = frozen_wall();
    let until = wall + chrono::Duration::hours(1);
    let accounts = vec![cooling_at("a", until), account("b", true)];
    let mono = Instant::now();
    assert_eq!(
        runtime
            .select_account_at(
                &accounts,
                RoutingMode::StrictPriority,
                false,
                None,
                &[],
                wall,
                mono,
            )
            .unwrap()
            .id,
        "b"
    );
    assert_eq!(
        runtime
            .select_account_at(
                &accounts,
                RoutingMode::StrictPriority,
                false,
                None,
                &[],
                until + chrono::Duration::seconds(1),
                mono,
            )
            .unwrap()
            .id,
        "a"
    );
    assert_eq!(
        runtime
            .select_account_at(
                &accounts,
                RoutingMode::StrictPriority,
                false,
                None,
                &[],
                until,
                mono,
            )
            .unwrap()
            .id,
        "a",
        "until == now must treat the cooled candidate as available"
    );
    let still_cooling = vec![cooling_at("a", until), account("b", true)];
    assert_eq!(
        runtime
            .select_account_at(
                &still_cooling,
                RoutingMode::StrictPriority,
                false,
                None,
                &[],
                until - chrono::Duration::seconds(1),
                mono,
            )
            .unwrap()
            .id,
        "b",
        "until > now must keep the candidate cooling"
    );
}

#[test]
fn conversation_ttl_uses_injected_mono_and_not_wall() {
    let runtime = RoutingRuntime::new();
    let wall = frozen_wall();
    let far_wall = wall + chrono::Duration::hours(24);
    let t0 = Instant::now();
    let accounts = vec![account("a", true), account("b", true)];
    let key = "ttl-mono";
    assert_eq!(
        runtime
            .select_account_at(
                &accounts,
                RoutingMode::StrictPriority,
                true,
                Some(key),
                &["a"],
                wall,
                t0,
            )
            .unwrap()
            .id,
        "b"
    );
    assert_eq!(
        runtime
            .select_account_at(
                &accounts,
                RoutingMode::StrictPriority,
                true,
                Some(key),
                &[],
                far_wall,
                t0 + Duration::from_secs(60),
            )
            .unwrap()
            .id,
        "b",
        "a large wall jump must not expire conversation TTL"
    );
    assert_eq!(
        runtime
            .select_account_at(
                &accounts,
                RoutingMode::StrictPriority,
                true,
                Some(key),
                &[],
                far_wall,
                t0 + Duration::from_secs(60) + CONVERSATION_TTL + Duration::from_secs(1),
            )
            .unwrap()
            .id,
        "a",
        "conversation TTL must expire from injected mono"
    );
}

#[test]
fn selection_policy_maps_every_routing_mode() {
    assert_eq!(
        selection_policy(RoutingMode::StrictPriority),
        SelectionPolicy::StrictPriority
    );
    assert_eq!(
        selection_policy(RoutingMode::StickyGlobal),
        SelectionPolicy::StickyGlobal
    );
    assert_eq!(
        selection_policy(RoutingMode::RoundRobin),
        SelectionPolicy::RoundRobin
    );
}

#[test]
fn typed_index_follows_card_order_for_all_three_modes() {
    let runtime = RoutingRuntime::new();
    let wall = frozen_wall();
    let mono = Instant::now();
    let candidates = vec![
        go_candidate(account("a", false)),
        go_candidate(account("b", true)),
        go_candidate(account("c", true)),
    ];

    assert_eq!(
        pick_index(
            &runtime,
            &candidates,
            RoutingMode::StrictPriority,
            false,
            None,
            &[],
            true,
            wall,
            mono,
        ),
        Some(1)
    );

    let runtime = RoutingRuntime::new();
    assert_eq!(
        pick_index(
            &runtime,
            &candidates,
            RoutingMode::StickyGlobal,
            false,
            None,
            &[],
            true,
            wall,
            mono,
        ),
        Some(1)
    );
    assert_eq!(
        pick_index(
            &runtime,
            &candidates,
            RoutingMode::StickyGlobal,
            false,
            None,
            &[],
            true,
            wall,
            mono,
        ),
        Some(1)
    );

    let runtime = RoutingRuntime::new();
    assert_eq!(
        pick_index(
            &runtime,
            &candidates,
            RoutingMode::RoundRobin,
            false,
            None,
            &[],
            true,
            wall,
            mono,
        ),
        Some(1)
    );
    assert_eq!(
        pick_index(
            &runtime,
            &candidates,
            RoutingMode::RoundRobin,
            false,
            None,
            &[],
            true,
            wall,
            mono,
        ),
        Some(2)
    );
    assert_eq!(
        pick_index(
            &runtime,
            &candidates,
            RoutingMode::RoundRobin,
            false,
            None,
            &[],
            true,
            wall,
            mono,
        ),
        Some(1)
    );
}

#[test]
fn typed_transient_excludes_do_not_rewrite_sticky_global() {
    let runtime = RoutingRuntime::new();
    let wall = frozen_wall();
    let mono = Instant::now();
    let candidates = vec![
        go_candidate(account("a", true)),
        go_candidate(account("b", true)),
    ];
    assert_eq!(
        pick_index(
            &runtime,
            &candidates,
            RoutingMode::StickyGlobal,
            false,
            None,
            &[],
            true,
            wall,
            mono,
        ),
        Some(0)
    );
    assert_eq!(
        pick_index(
            &runtime,
            &candidates,
            RoutingMode::StickyGlobal,
            false,
            None,
            &["a"],
            true,
            wall,
            mono,
        ),
        Some(1)
    );
    assert_eq!(
        pick_index(
            &runtime,
            &candidates,
            RoutingMode::StickyGlobal,
            false,
            None,
            &[],
            true,
            wall,
            mono,
        ),
        Some(0)
    );
}

#[test]
fn typed_duplicate_ids_error_before_state_mutation() {
    let runtime = RoutingRuntime::new();
    let wall = frozen_wall();
    let mono = Instant::now();
    let unique = vec![
        go_candidate(account("a", true)),
        go_candidate(account("b", true)),
    ];
    assert_eq!(
        pick_index(
            &runtime,
            &unique,
            RoutingMode::StickyGlobal,
            true,
            Some("dup-conv"),
            &[],
            true,
            wall,
            mono,
        ),
        Some(0)
    );
    let duplicates = vec![
        go_candidate(account("a", true)),
        go_candidate(account("a", true)),
    ];
    let error = runtime
        .try_select_candidate_index_at(
            &duplicates,
            RoutingMode::StickyGlobal,
            true,
            Some("dup-conv"),
            &[],
            true,
            wall,
            mono,
        )
        .expect_err("duplicate account ids must be a typed error");
    assert_eq!(
        error,
        SelectionError::DuplicateAccountId {
            first: 0,
            duplicate: 1
        }
    );
    assert_eq!(
        pick_index(
            &runtime,
            &unique,
            RoutingMode::StickyGlobal,
            true,
            Some("dup-conv"),
            &[],
            true,
            wall,
            mono,
        ),
        Some(0),
        "duplicate rejection must not rewrite sticky or conversation state"
    );
    assert_eq!(
        runtime
            .sticky_binding_at("dup-conv", mono)
            .map(|(id, _, _)| id)
            .as_deref(),
        Some("a")
    );
}

#[test]
fn legacy_option_wrappers_fail_closed_on_duplicates_and_preserve_state() {
    let runtime = RoutingRuntime::new();
    let wall = frozen_wall();
    let mono = Instant::now();
    let unique = vec![
        go_candidate(account("a", true)),
        go_candidate(account("b", true)),
    ];
    assert_eq!(
        runtime
            .select_candidate_at(
                &unique,
                RoutingMode::RoundRobin,
                false,
                None,
                &[],
                wall,
                mono,
            )
            .unwrap()
            .account
            .id,
        "a"
    );
    let duplicates = vec![
        go_candidate(account("b", true)),
        go_candidate(account("b", true)),
    ];
    assert!(
        runtime
            .select_candidate_at(
                &duplicates,
                RoutingMode::RoundRobin,
                false,
                None,
                &[],
                wall,
                mono,
            )
            .is_none()
    );
    assert_eq!(
        runtime
            .select_candidate_at(
                &unique,
                RoutingMode::RoundRobin,
                false,
                None,
                &[],
                wall,
                mono,
            )
            .unwrap()
            .account
            .id,
        "b",
        "legacy fail-closed must leave the round-robin cursor on the last successful pick"
    );
}

#[test]
fn disabled_zen_free_row_exhausts_free_until_inclusive_deadline() {
    let wall = frozen_wall();
    let mut cooled = zen_account(false);
    cooled.cooldown_free_until = Some(wall + chrono::Duration::hours(1));
    let accounts = vec![cooled, account("next", true)];

    assert!(
        free_channel_is_exhausted_at(&accounts, wall),
        "a disabled Zen row still exhausts Free while cooldown_free_until is in the future"
    );
    assert!(!free_channel_is_exhausted_at(
        &accounts,
        wall + chrono::Duration::hours(1),
    ));
}

#[test]
fn free_channel_exhaustion_ignores_go_accounts_and_uses_injected_wall() {
    let wall = frozen_wall();
    let mut cooled = zen_account(false);
    cooled.cooldown_free_until = Some(wall);
    let accounts = vec![cooled];

    assert!(
        free_channel_is_exhausted_at(&accounts, wall - chrono::Duration::seconds(1)),
        "until > now must exhaust Free even on a disabled Zen row"
    );
    assert!(
        !free_channel_is_exhausted_at(&accounts, wall),
        "until == now must expire Free exhaustion"
    );
    assert!(!free_channel_is_exhausted_at(
        &[account("go", true)],
        wall - chrono::Duration::seconds(1),
    ));
}

#[test]
fn free_channel_gate_closes_only_free_candidates() {
    let runtime = RoutingRuntime::new();
    let wall = frozen_wall();
    let mono = Instant::now();
    let mixed = vec![
        routing_candidate(zen_account(true), UpstreamChannel::Free, "m-free"),
        go_candidate(account("go", true)),
    ];
    assert_eq!(
        pick_index(
            &runtime,
            &mixed,
            RoutingMode::StrictPriority,
            false,
            None,
            &[],
            false,
            wall,
            mono,
        ),
        Some(1)
    );
    assert_eq!(
        pick_index(
            &runtime,
            &mixed,
            RoutingMode::StrictPriority,
            false,
            None,
            &[],
            true,
            wall,
            mono,
        ),
        Some(0)
    );

    let only_free = vec![routing_candidate(
        zen_account(true),
        UpstreamChannel::Free,
        "m-free",
    )];
    assert!(
        pick_index(
            &runtime,
            &only_free,
            RoutingMode::StrictPriority,
            false,
            None,
            &[],
            false,
            wall,
            mono,
        )
        .is_none()
    );
}

#[test]
fn conversation_ttl_expires_at_inclusive_boundary() {
    let wall = frozen_wall();
    let t0 = Instant::now();
    let accounts = vec![account("a", true), account("b", true)];

    let still = RoutingRuntime::new();
    assert_eq!(
        still
            .select_account_at(
                &accounts,
                RoutingMode::StrictPriority,
                true,
                Some("old"),
                &["a"],
                wall,
                t0,
            )
            .unwrap()
            .id,
        "b"
    );
    assert_eq!(
        still
            .select_account_at(
                &accounts,
                RoutingMode::StrictPriority,
                true,
                Some("old"),
                &[],
                wall,
                t0 + CONVERSATION_TTL - Duration::from_secs(1),
            )
            .unwrap()
            .id,
        "b"
    );

    let expired = RoutingRuntime::new();
    assert_eq!(
        expired
            .select_account_at(
                &accounts,
                RoutingMode::StrictPriority,
                true,
                Some("old"),
                &["a"],
                wall,
                t0,
            )
            .unwrap()
            .id,
        "b"
    );
    assert_eq!(
        expired
            .select_account_at(
                &accounts,
                RoutingMode::StrictPriority,
                true,
                Some("old"),
                &[],
                wall,
                t0 + CONVERSATION_TTL,
            )
            .unwrap()
            .id,
        "a",
        "duration_since == CONVERSATION_TTL must expire the binding"
    );

    let past_ttl = RoutingRuntime::new();
    assert_eq!(
        past_ttl
            .select_account_at(
                &accounts,
                RoutingMode::StrictPriority,
                true,
                Some("old"),
                &["a"],
                wall,
                t0,
            )
            .unwrap()
            .id,
        "b",
        "ttl-plus-one-bind"
    );
    assert_eq!(
        past_ttl
            .select_account_at(
                &accounts,
                RoutingMode::StrictPriority,
                true,
                Some("old"),
                &[],
                wall,
                t0 + CONVERSATION_TTL + Duration::from_secs(1),
            )
            .unwrap()
            .id,
        "a",
        "duration_since == CONVERSATION_TTL + 1s must expire the binding"
    );
}

#[test]
fn conversation_sticky_requires_account_channel_and_resolved_model() {
    let runtime = RoutingRuntime::new();
    let wall = frozen_wall();
    let t0 = Instant::now();
    let first = vec![
        routing_candidate(zen_account(true), UpstreamChannel::Free, "m1"),
        go_candidate(account("b", true)),
    ];
    assert_eq!(
        pick_index(
            &runtime,
            &first,
            RoutingMode::StrictPriority,
            true,
            Some("conv"),
            &[],
            true,
            wall,
            t0,
        ),
        Some(0)
    );

    let wrong_model = vec![
        routing_candidate(zen_account(true), UpstreamChannel::Free, "m2"),
        go_candidate(account("b", true)),
    ];
    assert_eq!(
        pick_index(
            &runtime,
            &wrong_model,
            RoutingMode::StrictPriority,
            true,
            Some("conv"),
            &[],
            true,
            wall,
            t0,
        ),
        Some(0)
    );
    assert_eq!(
        runtime
            .sticky_binding_at("conv", t0)
            .map(|(_, _, model)| model)
            .as_deref(),
        Some("m2")
    );

    let missing_triple = vec![go_candidate(account("b", true))];
    assert_eq!(
        pick_index(
            &runtime,
            &missing_triple,
            RoutingMode::StrictPriority,
            true,
            Some("conv"),
            &[],
            true,
            wall,
            t0,
        ),
        Some(0),
        "a conversation hit requires the bound account, channel, and resolved model"
    );
    assert_eq!(
        runtime.sticky_binding_at("conv", t0),
        Some((
            "b".to_string(),
            UpstreamChannel::Go,
            "test-model".to_string()
        ))
    );
}

#[test]
fn reserved_zen_account_id_is_not_the_free_gate() {
    let wall = frozen_wall();
    let mut reserved_go = account(ZEN_FREE_ACCOUNT_ID, true);
    reserved_go.cooldown_free_until = Some(wall + chrono::Duration::hours(1));
    assert!(
        !free_channel_is_exhausted_at(&[reserved_go], wall),
        "the reserved Zen account id on a Go catalog row must not exhaust Free"
    );

    let mut zen = account("not-the-reserved-zen-id", true);
    zen.provider_id = OPENCODE_ZEN_FREE_PROVIDER_ID.into();
    zen.credential_kind = CredentialKind::None;
    zen.quota_scope = QuotaScope::EgressIp;
    zen.key_cipher.clear();
    zen.cooldown_free_until = Some(wall + chrono::Duration::hours(1));
    assert!(
        free_channel_is_exhausted_at(&[zen], wall),
        "a Zen adapter catalog row exhausts Free without the reserved account id"
    );
}

#[test]
fn adapter_for_account_prefers_destination_adapter() {
    let mut go = account("go-looking", true);
    go.provider_id = crate::provider::default_provider_id();
    assert_eq!(
        adapter_for_account(&go, None),
        ProviderAdapterKind::OpenCodeGo
    );
    assert_eq!(
        adapter_for_account(
            &go,
            Some(&destination(
                AdapterKind::Zen,
                OPENCODE_ZEN_FREE_PROVIDER_ID
            ))
        ),
        ProviderAdapterKind::ZenFree
    );
    assert_eq!(
        account_channel_for(
            &go,
            Some(&destination(
                AdapterKind::Zen,
                OPENCODE_ZEN_FREE_PROVIDER_ID
            ))
        ),
        Some(UpstreamChannel::Free)
    );
}

#[test]
fn selector_uses_candidate_adapter_not_account_provider_id() {
    let runtime = RoutingRuntime::new();
    let wall = frozen_wall();
    let mono = Instant::now();
    let mut item = account("go-row", true);
    item.provider_id = crate::provider::default_provider_id();
    let candidates = vec![routing_candidate_with_adapter(
        item,
        UpstreamChannel::Free,
        "m-free",
        ProviderAdapterKind::ZenFree,
    )];
    assert_eq!(
        pick_index(
            &runtime,
            &candidates,
            RoutingMode::StrictPriority,
            false,
            None,
            &[],
            true,
            wall,
            mono,
        ),
        Some(0),
        "selector eligibility is candidate.adapter, not account.provider_id"
    );
    assert_eq!(
        pick_index(
            &runtime,
            &candidates,
            RoutingMode::StrictPriority,
            false,
            None,
            &[],
            false,
            wall,
            mono,
        ),
        None
    );
}

#[allow(clippy::too_many_arguments)]
fn preview_index(
    runtime: &RoutingRuntime,
    candidates: &[RoutingCandidate],
    mode: RoutingMode,
    conversation_sticky: bool,
    conversation_key: Option<&str>,
    exclude_ids: &[&str],
    free_channel_available: bool,
    wall: DateTime<Utc>,
    mono: Instant,
) -> Option<usize> {
    runtime
        .preview_candidate_index_at(
            candidates,
            mode,
            conversation_sticky,
            conversation_key,
            exclude_ids,
            free_channel_available,
            wall,
            mono,
        )
        .expect("candidates must not contain duplicate account ids")
}

#[test]
fn preview_candidate_index_does_not_advance_round_robin_or_sticky_global() {
    let runtime = RoutingRuntime::new();
    let wall = frozen_wall();
    let mono = Instant::now();
    let candidates = vec![
        go_candidate(account("a", true)),
        go_candidate(account("b", true)),
    ];
    assert_eq!(
        pick_index(
            &runtime,
            &candidates,
            RoutingMode::RoundRobin,
            false,
            None,
            &[],
            true,
            wall,
            mono,
        ),
        Some(0)
    );
    assert_eq!(
        preview_index(
            &runtime,
            &candidates,
            RoutingMode::RoundRobin,
            false,
            None,
            &[],
            true,
            wall,
            mono,
        ),
        Some(1)
    );
    assert_eq!(
        pick_index(
            &runtime,
            &candidates,
            RoutingMode::RoundRobin,
            false,
            None,
            &[],
            true,
            wall,
            mono,
        ),
        Some(1),
        "preview must leave the live round-robin cursor unmoved"
    );

    let sticky = RoutingRuntime::new();
    assert_eq!(
        pick_index(
            &sticky,
            &candidates,
            RoutingMode::StickyGlobal,
            false,
            None,
            &[],
            true,
            wall,
            mono,
        ),
        Some(0)
    );
    let disabled = vec![
        go_candidate(account("a", false)),
        go_candidate(account("b", true)),
    ];
    assert_eq!(
        preview_index(
            &sticky,
            &disabled,
            RoutingMode::StickyGlobal,
            false,
            None,
            &[],
            true,
            wall,
            mono,
        ),
        Some(1)
    );
    assert_eq!(
        pick_index(
            &sticky,
            &candidates,
            RoutingMode::StickyGlobal,
            false,
            None,
            &[],
            true,
            wall,
            mono,
        ),
        Some(0),
        "preview must not rewrite live sticky-global"
    );
}

#[test]
fn assess_candidate_availability_covers_current_gate_order() {
    let wall = frozen_wall();
    let available = go_candidate(account("ready", true));
    assert_eq!(
        assess_candidate_availability(&available, true, wall),
        CandidateAvailability::Available
    );

    let disabled = go_candidate(account("off", false));
    assert_eq!(
        assess_candidate_availability(&disabled, true, wall),
        CandidateAvailability::AccountDisabled
    );

    let mut setup = account("setup", true);
    setup.setup_step = crate::models::AccountSetupStep::KeyVerification;
    assert_eq!(
        assess_candidate_availability(&go_candidate(setup), true, wall),
        CandidateAvailability::SetupNotReady
    );

    let mismatched = routing_candidate(account("go", true), UpstreamChannel::Free, "m");
    assert_eq!(
        assess_candidate_availability(&mismatched, true, wall),
        CandidateAvailability::ChannelMismatch
    );

    let mut missing = account("missing", true);
    missing.key_cipher.clear();
    assert_eq!(
        assess_candidate_availability(&go_candidate(missing), true, wall),
        CandidateAvailability::CredentialMissing
    );

    let mut auth = account("auth", true);
    auth.auth_error = Some("invalid key".into());
    assert_eq!(
        assess_candidate_availability(&go_candidate(auth), true, wall),
        CandidateAvailability::AuthError
    );

    let cooling = go_candidate(cooling_at("cool", wall + chrono::Duration::hours(1)));
    assert_eq!(
        assess_candidate_availability(&cooling, true, wall),
        CandidateAvailability::CoolingDown
    );

    let free = routing_candidate(zen_account(true), UpstreamChannel::Free, "m-free");
    assert_eq!(
        assess_candidate_availability(&free, false, wall),
        CandidateAvailability::FreeChannelUnavailable
    );
    assert_eq!(
        assess_candidate_availability(&free, true, wall),
        CandidateAvailability::Available
    );
    assert_eq!(
        CandidateAvailability::AccountDisabled.as_str(),
        "account_disabled"
    );
    assert_eq!(
        CandidateAvailability::FreeChannelUnavailable.as_str(),
        "free_channel_unavailable"
    );
}
