//! Merge bounded observations without treating an unrelated endpoint failure
//! as a failure of every successful component. This does not write routing.
use super::{PlatformAccount, PlatformSnapshot};

/// Private optimistic-concurrency token. Never return or log this value.
/// A parent observation changes its version, but does not change a child's
/// configuration. Child tokens bind to the actual parent origin/observer and
/// the child's link and Key instead of the parent's observation version.
pub(crate) fn refresh_identity(
    parent: &PlatformAccount,
    observer_cipher: Option<&str>,
    child: Option<(&str, i64, &str)>,
) -> anyhow::Result<String> {
    Ok(match child {
        Some(child) => serde_json::to_string(&(
            "child",
            &parent.id,
            parent.kind,
            &parent.base_url,
            observer_cipher,
            child,
        ))?,
        None => serde_json::to_string(&("parent", &parent.id, parent.version))?,
    })
}

fn failed(snapshot: &PlatformSnapshot, component: &str) -> bool {
    snapshot.errors.iter().any(|error| {
        error
            .strip_prefix(component)
            .is_some_and(|tail| tail.starts_with('.'))
    })
}

fn source_failed(snapshot: &PlatformSnapshot, source: &str) -> bool {
    match source {
        "sub2api.user.profile" => failed(snapshot, "sub2api.profile"),
        "sub2api.subscriptions.summary" => failed(snapshot, "sub2api.subscriptions"),
        "sub2api.v1.usage" => failed(snapshot, "sub2api.usage"),
        "v1_models" => failed(snapshot, "new_api.models") || failed(snapshot, "sub2api.models"),
        "token_limits" => failed(snapshot, "new_api.token_usage"),
        "new_api.pricing" => [
            "new_api.pricing",
            "new_api.status",
            "new_api.models",
            "new_api.token_usage",
        ]
        .iter()
        .any(|component| failed(snapshot, component)),
        "sub2api.official_pricing" | "sub2api.billed_pricing" => {
            ["sub2api.plaza", "sub2api.billing", "sub2api.models"]
                .iter()
                .any(|component| failed(snapshot, component))
        }
        _ => failed(snapshot, source),
    }
}

/// Keep last-known rows only for failed sources. Successful empty components
/// clear their previous rows. Kept prices retain their original expiry; any
/// partial failure remains globally stale for the existing fail-closed price
/// estimator. `observed_at` is the latest attempt, not proof every row is new.
pub(crate) fn merge_snapshot(
    previous: Option<&PlatformSnapshot>,
    incoming: &PlatformSnapshot,
) -> PlatformSnapshot {
    let Some(previous) = previous else {
        let mut saved = incoming.clone();
        saved.stale |= !saved.errors.is_empty();
        return saved;
    };
    let whole_read_failed = incoming.errors.iter().any(|error| {
        matches!(
            error.as_str(),
            "auth.missing" | "base_url.invalid" | "snapshot.secret_reflected"
        )
    }) || (incoming.stale && incoming.errors.is_empty());
    if whole_read_failed {
        let mut saved = previous.clone();
        saved.stale = true;
        saved.errors = incoming.errors.clone();
        return saved;
    }

    let mut saved = incoming.clone();
    saved.stale |= !saved.errors.is_empty();
    saved
        .quotas
        .retain(|row| !source_failed(incoming, &row.source));
    saved.quotas.extend(
        previous
            .quotas
            .iter()
            .filter(|row| source_failed(incoming, &row.source))
            .cloned(),
    );
    saved
        .models
        .retain(|row| !source_failed(incoming, &row.source));
    saved.models.extend(
        previous
            .models
            .iter()
            .filter(|row| source_failed(incoming, &row.source))
            .cloned(),
    );
    saved
        .prices
        .retain(|row| !source_failed(incoming, &row.source));
    saved.prices.extend(
        previous
            .prices
            .iter()
            .filter(|row| source_failed(incoming, &row.source))
            .cloned(),
    );

    // The legacy group DTO has no source field. Retain missing last-known
    // groups only when a group-producing endpoint failed, and keep the whole
    // observation stale rather than claiming those rows are newly verified.
    if [
        "new_api.user_self",
        "new_api.user_groups",
        "new_api.token_auto_groups",
        "new_api.token_usage",
        "sub2api.subscriptions",
        "sub2api.groups",
    ]
    .iter()
    .any(|component| failed(incoming, component))
    {
        for old in &previous.groups {
            if !saved
                .groups
                .iter()
                .any(|group| group.id == old.id && group.platform == old.platform)
            {
                saved.groups.push(old.clone());
            }
        }
    }
    if failed(incoming, "new_api.subscription_self") {
        saved.billing_preference = previous.billing_preference.clone();
        saved.wallet_overflow = previous.wallet_overflow;
    }
    saved
}

#[cfg(test)]
mod tests;
