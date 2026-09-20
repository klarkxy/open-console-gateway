use super::*;

fn availability(available: bool) -> BaseAvailability {
    if available {
        BaseAvailability::Available
    } else {
        BaseAvailability::Unavailable
    }
}

fn cand(id: &str, available: bool) -> Candidate<'_> {
    Candidate::new(
        id,
        UpstreamChannel::Go,
        "test-model",
        availability(available),
    )
}

fn cand_on<'a>(
    id: &'a str,
    channel: UpstreamChannel,
    model: &'a str,
    available: bool,
) -> Candidate<'a> {
    Candidate::new(id, channel, model, availability(available))
}

/// Far-future origin so accidental `Instant::now()` inside production
/// code cannot satisfy conversation hits or TTL arithmetic.
fn origin() -> Instant {
    Instant::now() + Duration::from_secs(86_400)
}

fn pick(
    state: &mut SelectorState,
    candidates: &[Candidate<'_>],
    policy: SelectionPolicy,
    conversation_sticky: bool,
    conversation_key: Option<&str>,
    exclude_ids: &[&str],
    now: Instant,
) -> Option<usize> {
    state
        .select_at(
            candidates,
            policy,
            conversation_sticky,
            conversation_key,
            exclude_ids,
            now,
        )
        .expect("candidates must not contain duplicate account ids")
        .map(|selection| selection.candidate_index())
}

fn id_at<'a>(candidates: &'a [Candidate<'a>], index: usize) -> &'a str {
    candidates[index].account_id()
}

#[test]
fn strict_priority_picks_first_available_in_card_order() {
    let mut state = SelectorState::new();
    let now = origin();
    let candidates = [cand("a", false), cand("b", true), cand("c", true)];
    let index = pick(
        &mut state,
        &candidates,
        SelectionPolicy::StrictPriority,
        false,
        None,
        &[],
        now,
    )
    .unwrap();
    assert_eq!(index, 1);
    assert_eq!(id_at(&candidates, index), "b");
}

#[test]
fn unavailable_and_excluded_cards_are_skipped() {
    let mut state = SelectorState::new();
    let now = origin();
    let candidates = [
        cand("disabled", false),
        cand("ready", true),
        cand("also-ready", true),
    ];
    let index = pick(
        &mut state,
        &candidates,
        SelectionPolicy::StrictPriority,
        false,
        None,
        &["ready"],
        now,
    )
    .unwrap();
    assert_eq!(id_at(&candidates, index), "also-ready");
}

#[test]
fn empty_or_all_unselectable_returns_none() {
    let mut state = SelectorState::new();
    let now = origin();
    assert!(
        pick(
            &mut state,
            &[],
            SelectionPolicy::StrictPriority,
            false,
            None,
            &[],
            now,
        )
        .is_none()
    );
    let candidates = [cand("a", false), cand("b", true)];
    assert!(
        pick(
            &mut state,
            &candidates,
            SelectionPolicy::RoundRobin,
            false,
            None,
            &["b"],
            now,
        )
        .is_none()
    );
    assert!(state.round_robin_after.is_none());
}

#[test]
fn sticky_global_keeps_current_when_higher_priority_recovers() {
    let mut state = SelectorState::new();
    let now = origin();
    let first = [cand("a", false), cand("b", true)];
    assert_eq!(
        id_at(
            &first,
            pick(
                &mut state,
                &first,
                SelectionPolicy::StickyGlobal,
                false,
                None,
                &[],
                now,
            )
            .unwrap()
        ),
        "b"
    );
    let recovered = [cand("a", true), cand("b", true)];
    assert_eq!(
        id_at(
            &recovered,
            pick(
                &mut state,
                &recovered,
                SelectionPolicy::StickyGlobal,
                false,
                None,
                &[],
                now,
            )
            .unwrap()
        ),
        "b"
    );
    assert_eq!(state.global_account_id.as_deref(), Some("b"));
}

#[test]
fn sticky_global_transient_exclude_does_not_rewrite_global() {
    let mut state = SelectorState::new();
    let now = origin();
    let candidates = [cand("a", true), cand("b", true)];
    assert_eq!(
        id_at(
            &candidates,
            pick(
                &mut state,
                &candidates,
                SelectionPolicy::StickyGlobal,
                false,
                None,
                &[],
                now,
            )
            .unwrap()
        ),
        "a"
    );
    assert_eq!(
        id_at(
            &candidates,
            pick(
                &mut state,
                &candidates,
                SelectionPolicy::StickyGlobal,
                false,
                None,
                &["a"],
                now,
            )
            .unwrap()
        ),
        "b"
    );
    assert_eq!(state.global_account_id.as_deref(), Some("a"));
    assert_eq!(
        id_at(
            &candidates,
            pick(
                &mut state,
                &candidates,
                SelectionPolicy::StickyGlobal,
                false,
                None,
                &[],
                now,
            )
            .unwrap()
        ),
        "a"
    );
}

#[test]
fn sticky_global_switches_when_current_persistently_unavailable() {
    let mut state = SelectorState::new();
    let now = origin();
    let candidates = [cand("a", true), cand("b", true)];
    pick(
        &mut state,
        &candidates,
        SelectionPolicy::StickyGlobal,
        false,
        None,
        &[],
        now,
    );
    let disabled = [cand("a", false), cand("b", true)];
    assert_eq!(
        id_at(
            &disabled,
            pick(
                &mut state,
                &disabled,
                SelectionPolicy::StickyGlobal,
                false,
                None,
                &[],
                now,
            )
            .unwrap()
        ),
        "b"
    );
    assert_eq!(state.global_account_id.as_deref(), Some("b"));
}

#[test]
fn sticky_global_switches_when_bound_account_is_missing() {
    let mut state = SelectorState::new();
    let now = origin();
    let original = [cand("a", true), cand("b", true)];
    pick(
        &mut state,
        &original,
        SelectionPolicy::StickyGlobal,
        false,
        None,
        &[],
        now,
    );
    let missing = [cand("b", true), cand("c", true)];
    assert_eq!(
        id_at(
            &missing,
            pick(
                &mut state,
                &missing,
                SelectionPolicy::StickyGlobal,
                false,
                None,
                &[],
                now,
            )
            .unwrap()
        ),
        "b"
    );
    assert_eq!(state.global_account_id.as_deref(), Some("b"));
}

#[test]
fn sticky_global_stores_account_only() {
    let mut state = SelectorState::new();
    let now = origin();
    let first = [cand_on("a", UpstreamChannel::Free, "m1", true)];
    pick(
        &mut state,
        &first,
        SelectionPolicy::StickyGlobal,
        false,
        None,
        &[],
        now,
    );
    let switched_identity = [
        cand_on("a", UpstreamChannel::Go, "m2", true),
        cand("b", true),
    ];
    let index = pick(
        &mut state,
        &switched_identity,
        SelectionPolicy::StickyGlobal,
        false,
        None,
        &[],
        now,
    )
    .unwrap();
    assert_eq!(index, 0);
    assert_eq!(switched_identity[index].channel(), UpstreamChannel::Go);
    assert_eq!(switched_identity[index].resolved_model(), "m2");
    assert_eq!(state.global_account_id.as_deref(), Some("a"));
}

#[test]
fn round_robin_cycles_and_skips_unavailable() {
    let mut state = SelectorState::new();
    let now = origin();
    let candidates = [cand("a", true), cand("b", false), cand("c", true)];
    assert_eq!(
        id_at(
            &candidates,
            pick(
                &mut state,
                &candidates,
                SelectionPolicy::RoundRobin,
                false,
                None,
                &[],
                now,
            )
            .unwrap()
        ),
        "a"
    );
    assert_eq!(
        id_at(
            &candidates,
            pick(
                &mut state,
                &candidates,
                SelectionPolicy::RoundRobin,
                false,
                None,
                &[],
                now,
            )
            .unwrap()
        ),
        "c"
    );
    assert_eq!(
        id_at(
            &candidates,
            pick(
                &mut state,
                &candidates,
                SelectionPolicy::RoundRobin,
                false,
                None,
                &[],
                now,
            )
            .unwrap()
        ),
        "a"
    );
}

#[test]
fn round_robin_cursor_survives_reordering_and_missing_cursor_by_account_id() {
    let mut state = SelectorState::new();
    let now = origin();
    let original = [cand("a", true), cand("b", true), cand("c", true)];
    assert_eq!(
        id_at(
            &original,
            pick(
                &mut state,
                &original,
                SelectionPolicy::RoundRobin,
                false,
                None,
                &[],
                now,
            )
            .unwrap()
        ),
        "a"
    );

    let reordered = [cand("c", true), cand("a", true), cand("b", true)];
    assert_eq!(
        id_at(
            &reordered,
            pick(
                &mut state,
                &reordered,
                SelectionPolicy::RoundRobin,
                false,
                None,
                &[],
                now,
            )
            .unwrap()
        ),
        "b"
    );

    let missing_cursor = [cand("a", true), cand("c", true)];
    assert_eq!(
        id_at(
            &missing_cursor,
            pick(
                &mut state,
                &missing_cursor,
                SelectionPolicy::RoundRobin,
                false,
                None,
                &[],
                now,
            )
            .unwrap()
        ),
        "a"
    );
}

#[test]
fn conversation_sticky_prefers_exact_binding_without_advancing_round_robin() {
    let mut state = SelectorState::new();
    let now = origin();
    let candidates = [cand("a", true), cand("b", true)];
    let key = "conv-1";
    assert_eq!(
        id_at(
            &candidates,
            pick(
                &mut state,
                &candidates,
                SelectionPolicy::RoundRobin,
                true,
                Some(key),
                &[],
                now,
            )
            .unwrap()
        ),
        "a"
    );
    assert_eq!(state.round_robin_after.as_deref(), Some("a"));
    let later = now + Duration::from_secs(1);
    assert_eq!(
        id_at(
            &candidates,
            pick(
                &mut state,
                &candidates,
                SelectionPolicy::RoundRobin,
                true,
                Some(key),
                &[],
                later,
            )
            .unwrap()
        ),
        "a"
    );
    assert_eq!(state.round_robin_after.as_deref(), Some("a"));
    let binding = state.binding_at(key, later).unwrap();
    assert_eq!(binding.account_id(), "a");
    assert_eq!(binding.channel(), UpstreamChannel::Go);
    assert_eq!(binding.resolved_model(), "test-model");
}

#[test]
fn conversation_sticky_requires_account_channel_and_resolved_model() {
    let mut state = SelectorState::new();
    let t0 = origin();
    let first = [
        cand_on("a", UpstreamChannel::Free, "m1", true),
        cand("b", true),
    ];
    pick(
        &mut state,
        &first,
        SelectionPolicy::StrictPriority,
        true,
        Some("conv"),
        &[],
        t0,
    );

    let wrong_model = [
        cand_on("a", UpstreamChannel::Free, "m2", true),
        cand("b", true),
    ];
    assert_eq!(
        id_at(
            &wrong_model,
            pick(
                &mut state,
                &wrong_model,
                SelectionPolicy::StrictPriority,
                true,
                Some("conv"),
                &[],
                t0,
            )
            .unwrap()
        ),
        "a"
    );
    assert_eq!(state.binding_at("conv", t0).unwrap().resolved_model(), "m2");

    let wrong_channel = [
        cand_on("a", UpstreamChannel::Go, "m2", true),
        cand("b", true),
    ];
    assert_eq!(
        id_at(
            &wrong_channel,
            pick(
                &mut state,
                &wrong_channel,
                SelectionPolicy::StrictPriority,
                true,
                Some("conv"),
                &[],
                t0,
            )
            .unwrap()
        ),
        "a"
    );
    assert_eq!(
        state.binding_at("conv", t0).unwrap().channel(),
        UpstreamChannel::Go
    );
}

#[test]
fn conversation_sticky_rebinds_when_bound_account_excluded() {
    let mut state = SelectorState::new();
    let now = origin();
    let candidates = [cand("a", true), cand("b", true)];
    let key = "conv-2";
    assert_eq!(
        id_at(
            &candidates,
            pick(
                &mut state,
                &candidates,
                SelectionPolicy::StrictPriority,
                true,
                Some(key),
                &[],
                now,
            )
            .unwrap()
        ),
        "a"
    );
    assert_eq!(
        id_at(
            &candidates,
            pick(
                &mut state,
                &candidates,
                SelectionPolicy::StrictPriority,
                true,
                Some(key),
                &["a"],
                now,
            )
            .unwrap()
        ),
        "b"
    );
    assert_eq!(
        id_at(
            &candidates,
            pick(
                &mut state,
                &candidates,
                SelectionPolicy::StrictPriority,
                true,
                Some(key),
                &[],
                now,
            )
            .unwrap()
        ),
        "b"
    );
}

#[test]
fn conversation_miss_falls_through_and_advances_round_robin() {
    let mut state = SelectorState::new();
    let now = origin();
    let candidates = [cand("a", true), cand("b", true)];
    pick(
        &mut state,
        &candidates,
        SelectionPolicy::RoundRobin,
        true,
        Some("conv"),
        &[],
        now,
    );
    assert_eq!(state.round_robin_after.as_deref(), Some("a"));
    let index = pick(
        &mut state,
        &candidates,
        SelectionPolicy::RoundRobin,
        true,
        Some("conv"),
        &["a"],
        now,
    )
    .unwrap();
    assert_eq!(id_at(&candidates, index), "b");
    assert_eq!(state.round_robin_after.as_deref(), Some("b"));
    assert_eq!(state.binding_at("conv", now).unwrap().account_id(), "b");
}

#[test]
fn conversation_ttl_expires_at_inclusive_boundary() {
    let t0 = origin();
    let bind_b = [cand("a", false), cand("b", true)];
    let both = [cand("a", true), cand("b", true)];

    let mut still = SelectorState::new();
    assert_eq!(
        id_at(
            &bind_b,
            pick(
                &mut still,
                &bind_b,
                SelectionPolicy::StrictPriority,
                true,
                Some("old"),
                &[],
                t0,
            )
            .unwrap()
        ),
        "b"
    );
    assert_eq!(
        id_at(
            &both,
            pick(
                &mut still,
                &both,
                SelectionPolicy::StrictPriority,
                true,
                Some("old"),
                &[],
                t0 + CONVERSATION_TTL - Duration::from_secs(1),
            )
            .unwrap()
        ),
        "b"
    );

    let mut expired = SelectorState::new();
    pick(
        &mut expired,
        &bind_b,
        SelectionPolicy::StrictPriority,
        true,
        Some("old"),
        &[],
        t0,
    );
    assert!(expired.binding_at("old", t0 + CONVERSATION_TTL).is_none());
    assert_eq!(
        id_at(
            &both,
            pick(
                &mut expired,
                &both,
                SelectionPolicy::StrictPriority,
                true,
                Some("old"),
                &[],
                t0 + CONVERSATION_TTL,
            )
            .unwrap()
        ),
        "a"
    );
}

#[test]
fn conversation_capacity_evicts_least_recently_used() {
    let mut state = SelectorState::new();
    let now = origin();
    let candidates = [cand("a", true)];
    for index in 0..=MAX_CONVERSATIONS {
        let key = format!("k{index}");
        pick(
            &mut state,
            &candidates,
            SelectionPolicy::StrictPriority,
            true,
            Some(&key),
            &[],
            now,
        );
    }
    assert_eq!(state.conversations.entries.len(), MAX_CONVERSATIONS);
    assert!(state.binding_at("k0", now).is_none());
    assert!(
        state
            .binding_at(&format!("k{MAX_CONVERSATIONS}"), now)
            .is_some()
    );
}

#[test]
fn conversation_lookup_refreshes_lru_before_availability_check() {
    let mut state = SelectorState::new();
    let now = origin();
    let candidates = [cand("a", true), cand("b", true)];
    for index in 0..MAX_CONVERSATIONS {
        let key = format!("k{index}");
        pick(
            &mut state,
            &candidates,
            SelectionPolicy::StrictPriority,
            true,
            Some(&key),
            &[],
            now,
        );
    }
    assert!(
        pick(
            &mut state,
            &candidates,
            SelectionPolicy::StrictPriority,
            true,
            Some("k0"),
            &["a", "b"],
            now,
        )
        .is_none()
    );
    pick(
        &mut state,
        &candidates,
        SelectionPolicy::StrictPriority,
        true,
        Some("new"),
        &[],
        now,
    );
    assert!(state.conversations.entries.contains_key("k0"));
    assert!(!state.conversations.entries.contains_key("k1"));
    assert!(state.conversations.entries.contains_key("new"));
}

#[test]
fn conversation_hit_refreshes_lru_order_before_capacity_eviction() {
    let mut state = SelectorState::new();
    let now = origin();
    let candidates = [cand("a", true)];
    for index in 0..MAX_CONVERSATIONS {
        let key = format!("k{index}");
        pick(
            &mut state,
            &candidates,
            SelectionPolicy::StrictPriority,
            true,
            Some(&key),
            &[],
            now,
        );
    }
    pick(
        &mut state,
        &candidates,
        SelectionPolicy::StrictPriority,
        true,
        Some("k0"),
        &[],
        now,
    );
    pick(
        &mut state,
        &candidates,
        SelectionPolicy::StrictPriority,
        true,
        Some("new"),
        &[],
        now,
    );

    assert!(state.conversations.entries.contains_key("k0"));
    assert!(!state.conversations.entries.contains_key("k1"));
    assert!(state.conversations.entries.contains_key("new"));
    assert_eq!(state.conversations.entries.len(), MAX_CONVERSATIONS);
}

#[test]
fn duplicate_account_ids_are_rejected_before_any_state_mutation() {
    let mut state = SelectorState::new();
    let t0 = origin();
    let unique = [cand("a", true), cand("b", true)];
    pick(
        &mut state,
        &unique,
        SelectionPolicy::RoundRobin,
        true,
        Some("alive"),
        &[],
        t0,
    );
    assert_eq!(state.round_robin_after.as_deref(), Some("a"));
    assert_eq!(state.binding_at("alive", t0).unwrap().account_id(), "a");

    let later = t0 + Duration::from_secs(20 * 60);
    let duplicates = [cand("a", true), cand("b", true), cand("a", true)];
    let error = state
        .select_at(
            &duplicates,
            SelectionPolicy::RoundRobin,
            true,
            Some("alive"),
            &[],
            later,
        )
        .unwrap_err();
    assert_eq!(
        error,
        SelectionError::DuplicateAccountId {
            first: 0,
            duplicate: 2
        }
    );

    assert_eq!(state.round_robin_after.as_deref(), Some("a"));
    assert_eq!(state.global_account_id.as_deref(), None);
    // Lookup was not refreshed at `later`; the original t0 last_seen still
    // expires at t0 + TTL rather than later + TTL.
    assert!(state.binding_at("alive", t0 + CONVERSATION_TTL).is_none());

    let mut sticky = SelectorState::new();
    pick(
        &mut sticky,
        &unique,
        SelectionPolicy::StickyGlobal,
        false,
        None,
        &[],
        t0,
    );
    assert_eq!(sticky.global_account_id.as_deref(), Some("a"));
    assert!(
        sticky
            .select_at(
                &[cand("x", true), cand("x", true)],
                SelectionPolicy::StickyGlobal,
                false,
                None,
                &["a"],
                t0,
            )
            .is_err()
    );
    assert_eq!(sticky.global_account_id.as_deref(), Some("a"));
}

#[test]
fn reset_clears_runtime_state() {
    let mut state = SelectorState::new();
    let now = origin();
    let candidates = [cand("a", true), cand("b", true)];
    pick(
        &mut state,
        &candidates,
        SelectionPolicy::RoundRobin,
        true,
        Some("c1"),
        &[],
        now,
    );
    pick(
        &mut state,
        &candidates,
        SelectionPolicy::StickyGlobal,
        false,
        None,
        &[],
        now,
    );
    state.reset();
    assert!(state.global_account_id.is_none());
    assert!(state.round_robin_after.is_none());
    assert_eq!(state.conversations.entries.len(), 0);
    assert_eq!(
        id_at(
            &candidates,
            pick(
                &mut state,
                &candidates,
                SelectionPolicy::RoundRobin,
                false,
                None,
                &[],
                now,
            )
            .unwrap()
        ),
        "a"
    );
}

#[test]
fn preview_at_does_not_advance_round_robin_or_rewrite_sticky_global() {
    let mut state = SelectorState::new();
    let now = origin();
    let candidates = [cand("a", true), cand("b", true)];
    assert_eq!(
        id_at(
            &candidates,
            pick(
                &mut state,
                &candidates,
                SelectionPolicy::RoundRobin,
                false,
                None,
                &[],
                now,
            )
            .unwrap()
        ),
        "a"
    );
    assert_eq!(state.round_robin_after.as_deref(), Some("a"));

    let preview = state
        .preview_at(
            &candidates,
            SelectionPolicy::RoundRobin,
            false,
            None,
            &[],
            now,
        )
        .expect("candidates must not contain duplicate account ids")
        .expect("preview should pick the next round-robin card");
    assert_eq!(id_at(&candidates, preview.candidate_index()), "b");
    assert_eq!(state.round_robin_after.as_deref(), Some("a"));

    assert_eq!(
        id_at(
            &candidates,
            pick(
                &mut state,
                &candidates,
                SelectionPolicy::RoundRobin,
                false,
                None,
                &[],
                now,
            )
            .unwrap()
        ),
        "b"
    );
    assert_eq!(state.round_robin_after.as_deref(), Some("b"));

    let mut sticky = SelectorState::new();
    pick(
        &mut sticky,
        &candidates,
        SelectionPolicy::StickyGlobal,
        false,
        None,
        &[],
        now,
    );
    assert_eq!(sticky.global_account_id.as_deref(), Some("a"));
    let disabled = [cand("a", false), cand("b", true)];
    let preview = sticky
        .preview_at(
            &disabled,
            SelectionPolicy::StickyGlobal,
            false,
            None,
            &[],
            now,
        )
        .expect("candidates must not contain duplicate account ids")
        .expect("preview should fall through when sticky is unavailable");
    assert_eq!(id_at(&disabled, preview.candidate_index()), "b");
    assert_eq!(sticky.global_account_id.as_deref(), Some("a"));
    assert_eq!(
        id_at(
            &candidates,
            pick(
                &mut sticky,
                &candidates,
                SelectionPolicy::StickyGlobal,
                false,
                None,
                &[],
                now,
            )
            .unwrap()
        ),
        "a"
    );
}

#[test]
fn preview_at_does_not_insert_or_refresh_conversation_bindings() {
    let mut state = SelectorState::new();
    let now = origin();
    let candidates = [cand("a", true), cand("b", true)];
    pick(
        &mut state,
        &candidates,
        SelectionPolicy::StrictPriority,
        true,
        Some("live"),
        &[],
        now,
    );
    assert_eq!(state.binding_at("live", now).unwrap().account_id(), "a");

    let later = now + Duration::from_secs(60);
    let preview = state
        .preview_at(
            &candidates,
            SelectionPolicy::RoundRobin,
            true,
            Some("live"),
            &[],
            later,
        )
        .expect("candidates must not contain duplicate account ids")
        .expect("preview should hit the conversation binding");
    assert_eq!(id_at(&candidates, preview.candidate_index()), "a");
    let inserted = state
        .preview_at(
            &candidates,
            SelectionPolicy::RoundRobin,
            true,
            Some("preview-only"),
            &[],
            later,
        )
        .expect("candidates must not contain duplicate account ids")
        .expect("preview should select a card");
    assert_eq!(id_at(&candidates, inserted.candidate_index()), "a");
    assert!(state.binding_at("preview-only", later).is_none());
    assert!(
        state.binding_at("live", now + CONVERSATION_TTL).is_none(),
        "preview must not refresh live conversation last_seen"
    );
}

#[test]
fn disabling_conversation_sticky_ignores_existing_bindings() {
    let mut state = SelectorState::new();
    let now = origin();
    let candidates = [cand("a", true), cand("b", true)];
    assert_eq!(
        id_at(
            &candidates,
            pick(
                &mut state,
                &candidates,
                SelectionPolicy::RoundRobin,
                true,
                Some("bound"),
                &[],
                now,
            )
            .unwrap()
        ),
        "a"
    );
    assert_eq!(
        id_at(
            &candidates,
            pick(
                &mut state,
                &candidates,
                SelectionPolicy::RoundRobin,
                false,
                Some("bound"),
                &[],
                now,
            )
            .unwrap()
        ),
        "b"
    );
}

#[test]
fn explicit_instant_drives_hits_not_wall_clock() {
    let t0 = origin();
    let bind_b = [cand("a", false), cand("b", true)];
    let both = [cand("a", true), cand("b", true)];

    let mut hit = SelectorState::new();
    pick(
        &mut hit,
        &bind_b,
        SelectionPolicy::StrictPriority,
        true,
        Some("timed"),
        &[],
        t0,
    );
    assert_eq!(
        id_at(
            &both,
            pick(
                &mut hit,
                &both,
                SelectionPolicy::StrictPriority,
                true,
                Some("timed"),
                &[],
                t0 + Duration::from_nanos(1),
            )
            .unwrap()
        ),
        "b"
    );
}

#[test]
fn free_channel_identity_has_no_special_exhaust_gate() {
    let mut state = SelectorState::new();
    let now = origin();
    let candidates = [
        cand_on("free-1", UpstreamChannel::Free, "m-free", true),
        cand_on("go-1", UpstreamChannel::Go, "m-go", true),
    ];
    assert_eq!(
        id_at(
            &candidates,
            pick(
                &mut state,
                &candidates,
                SelectionPolicy::StrictPriority,
                false,
                None,
                &[],
                now,
            )
            .unwrap()
        ),
        "free-1"
    );
    let gated = [
        cand_on("free-1", UpstreamChannel::Free, "m-free", false),
        cand_on("go-1", UpstreamChannel::Go, "m-go", true),
    ];
    assert_eq!(
        id_at(
            &gated,
            pick(
                &mut state,
                &gated,
                SelectionPolicy::StrictPriority,
                false,
                None,
                &[],
                now,
            )
            .unwrap()
        ),
        "go-1"
    );
}
