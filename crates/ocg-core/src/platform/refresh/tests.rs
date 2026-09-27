use super::*;
use crate::platform::{PlatformKind, PlatformPrice, PlatformQuota, PlatformQuotaKind};

fn quota(source: &str, remaining: f64) -> PlatformQuota {
    PlatformQuota {
        kind: if source.contains("subscription") {
            PlatformQuotaKind::Subscription
        } else {
            PlatformQuotaKind::Wallet
        },
        scope_id: source.into(),
        unit: "usd".into(),
        used: None,
        remaining: Some(remaining),
        limit: None,
        unlimited: false,
        period: None,
        resets_at: None,
        expires_at: None,
        source: source.into(),
    }
}

fn parent() -> PlatformAccount {
    PlatformAccount {
        id: "parent".into(),
        kind: PlatformKind::Sub2api,
        name: "Site".into(),
        base_url: "https://example.test/chat".into(),
        has_user_credential: true,
        version: 1,
        snapshot: None,
    }
}

#[test]
fn fresh_wallet_survives_unrelated_price_failure() {
    let old = PlatformSnapshot {
        observed_at: 1,
        quotas: vec![quota("new_api.user_self", 100.0)],
        ..Default::default()
    };
    let new = PlatformSnapshot {
        observed_at: 2,
        stale: true,
        errors: vec!["new_api.pricing.http_status".into()],
        quotas: vec![quota("new_api.user_self", 50.0)],
        ..Default::default()
    };
    let saved = merge_snapshot(Some(&old), &new);
    assert_eq!(saved.quotas.len(), 1);
    assert_eq!(saved.quotas[0].remaining, Some(50.0));
    assert_eq!(saved.observed_at, 2);
    assert!(saved.stale);
}

#[test]
fn only_failed_sources_keep_their_last_known_values() {
    let old = PlatformSnapshot {
        quotas: vec![
            quota("sub2api.user.profile", 100.0),
            quota("sub2api.subscriptions.summary", 8.0),
        ],
        ..Default::default()
    };
    let new = PlatformSnapshot {
        errors: vec!["sub2api.subscriptions.timeout".into()],
        quotas: vec![quota("sub2api.user.profile", 50.0)],
        ..Default::default()
    };
    let saved = merge_snapshot(Some(&old), &new);
    assert_eq!(saved.quotas.len(), 2);
    assert_eq!(saved.quotas[0].remaining, Some(50.0));
    assert_eq!(saved.quotas[1].remaining, Some(8.0));
    assert!(saved.stale);
}

#[test]
fn successful_empty_subscription_list_clears_old_rows_even_if_wallet_fails() {
    let old = PlatformSnapshot {
        quotas: vec![
            quota("sub2api.user.profile", 100.0),
            quota("sub2api.subscriptions.summary", 8.0),
        ],
        ..Default::default()
    };
    let new = PlatformSnapshot {
        errors: vec!["sub2api.profile.timeout".into()],
        ..Default::default()
    };
    let saved = merge_snapshot(Some(&old), &new);
    assert_eq!(saved.quotas.len(), 1);
    assert_eq!(saved.quotas[0].source, "sub2api.user.profile");
}

#[test]
fn failed_component_partial_rows_do_not_replace_the_last_complete_component() {
    let old = PlatformSnapshot {
        quotas: vec![quota("sub2api.v1.usage", 20.0)],
        ..Default::default()
    };
    let new = PlatformSnapshot {
        errors: vec!["sub2api.usage.parse".into()],
        quotas: vec![quota("sub2api.v1.usage", 0.0)],
        ..Default::default()
    };
    assert_eq!(
        merge_snapshot(Some(&old), &new).quotas[0].remaining,
        Some(20.0)
    );
}

#[test]
fn retained_price_expiry_is_never_renewed() {
    let price = PlatformPrice {
        model: "model".into(),
        group_id: None,
        currency: "USD".into(),
        input: Some(1e-6),
        output: Some(2e-6),
        cache_read: None,
        cache_write: None,
        source: "sub2api.billed_pricing".into(),
        official_reference: false,
        unavailable_reason: None,
        valid_until: 123,
    };
    let old = PlatformSnapshot {
        prices: vec![price],
        ..Default::default()
    };
    let new = PlatformSnapshot {
        observed_at: 1000,
        errors: vec!["sub2api.billing.timeout".into()],
        ..Default::default()
    };
    let saved = merge_snapshot(Some(&old), &new);
    assert_eq!(saved.prices[0].valid_until, 123);
    assert!(saved.stale);
}

#[test]
fn first_partial_snapshot_keeps_successful_data_and_is_stale() {
    let new = PlatformSnapshot {
        errors: vec!["sub2api.plaza.forbidden".into()],
        quotas: vec![quota("sub2api.v1.usage", 20.0)],
        ..Default::default()
    };
    let saved = merge_snapshot(None, &new);
    assert_eq!(saved.quotas[0].remaining, Some(20.0));
    assert!(saved.stale);
}

#[test]
fn whole_read_failure_retains_old_data_without_renewing_its_timestamp() {
    let old = PlatformSnapshot {
        observed_at: 1,
        quotas: vec![quota("new_api.user_self", 100.0)],
        ..Default::default()
    };
    let new = PlatformSnapshot {
        observed_at: 2,
        errors: vec!["auth.missing".into()],
        ..Default::default()
    };
    let saved = merge_snapshot(Some(&old), &new);
    assert_eq!(saved.observed_at, 1);
    assert_eq!(saved.errors, new.errors);
    assert!(saved.stale);
}

#[test]
fn parent_observation_does_not_invalidate_child_refresh() {
    let mut parent = parent();
    let child = refresh_identity(
        &parent,
        Some("encrypted-observer"),
        Some(("key", 3, "encrypted-key")),
    )
    .unwrap();
    let before = refresh_identity(&parent, None, None).unwrap();
    parent.version += 1;
    assert_eq!(
        child,
        refresh_identity(
            &parent,
            Some("encrypted-observer"),
            Some(("key", 3, "encrypted-key"))
        )
        .unwrap()
    );
    assert_ne!(before, refresh_identity(&parent, None, None).unwrap());
}

#[test]
fn child_refresh_is_still_bound_to_origin_observer_link_and_key() {
    let mut parent = parent();
    let before = refresh_identity(&parent, Some("observer"), Some(("key", 3, "cipher"))).unwrap();
    assert_ne!(
        before,
        refresh_identity(&parent, Some("rotated"), Some(("key", 3, "cipher"))).unwrap()
    );
    assert_ne!(
        before,
        refresh_identity(&parent, Some("observer"), Some(("key", 4, "cipher"))).unwrap()
    );
    assert_ne!(
        before,
        refresh_identity(&parent, Some("observer"), Some(("key", 3, "rotated"))).unwrap()
    );
    parent.base_url.push_str("/different");
    assert_ne!(
        before,
        refresh_identity(&parent, Some("observer"), Some(("key", 3, "cipher"))).unwrap()
    );
}
