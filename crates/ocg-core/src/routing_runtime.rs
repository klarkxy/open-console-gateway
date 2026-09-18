//! Host adapter around [`ocg_gateway::selector::SelectorState`].
//!
//! Owned outside `gateway` so `state` can hold the process slot without a
//! `state -> gateway` edge. Account eligibility, wall-clock cooling, Free dual
//! gates, and provider fail-closed stay in Core. Conversation-key parsing stays
//! in `gateway::routing` and is re-exported from there.

use crate::kernel::catalog::CredentialKind;
use crate::models::{Account, RoutingMode, UpstreamChannel};
use crate::provider::ProviderAdapterKind;
use chrono::{DateTime, Utc};
use ocg_domain::destination::Destination;
use ocg_gateway::selector::{BaseAvailability, Candidate as GatewayCandidate, SelectionPolicy};
use parking_lot::Mutex;
use std::time::Instant;

#[cfg(test)]
use crate::kernel::ids::OPENCODE_ZEN_FREE_PROVIDER_ID;

#[derive(Debug, Default)]
pub struct RoutingRuntime {
    inner: Mutex<ocg_gateway::selector::SelectorState>,
}

#[derive(Debug, Clone)]
pub struct RoutingCandidate {
    pub account: Account,
    pub channel: UpstreamChannel,
    pub resolved_model: String,
    pub adapter: ProviderAdapterKind,
}

impl RoutingRuntime {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn reset(&self) {
        self.inner.lock().reset();
    }

    /// Select an account for Go channel requests (test and legacy callers).
    pub fn select_account(
        &self,
        accounts: &[Account],
        mode: RoutingMode,
        conversation_sticky: bool,
        conversation_key: Option<&str>,
        exclude_ids: &[&str],
    ) -> Option<Account> {
        self.select_account_at(
            accounts,
            mode,
            conversation_sticky,
            conversation_key,
            exclude_ids,
            Utc::now(),
            Instant::now(),
        )
    }

    /// Select an account for Go channel requests against an explicit wall/mono pair.
    #[allow(clippy::too_many_arguments)]
    pub fn select_account_at(
        &self,
        accounts: &[Account],
        mode: RoutingMode,
        conversation_sticky: bool,
        conversation_key: Option<&str>,
        exclude_ids: &[&str],
        wall: DateTime<Utc>,
        mono: Instant,
    ) -> Option<Account> {
        self.select_account_for_at(
            accounts,
            mode,
            conversation_sticky,
            conversation_key,
            UpstreamChannel::Go,
            "",
            exclude_ids,
            wall,
            mono,
        )
    }

    /// Select an account against an explicit wall/mono pair.
    #[allow(clippy::too_many_arguments)]
    pub fn select_account_for_at(
        &self,
        accounts: &[Account],
        mode: RoutingMode,
        conversation_sticky: bool,
        conversation_key: Option<&str>,
        channel: UpstreamChannel,
        resolved_model: &str,
        exclude_ids: &[&str],
        wall: DateTime<Utc>,
        mono: Instant,
    ) -> Option<Account> {
        let candidates = accounts
            .iter()
            .cloned()
            .map(|account| RoutingCandidate {
                adapter: adapter_for_account(&account, None),
                account,
                channel,
                resolved_model: resolved_model.to_string(),
            })
            .collect::<Vec<_>>();
        self.select_candidate_at(
            &candidates,
            mode,
            conversation_sticky,
            conversation_key,
            exclude_ids,
            wall,
            mono,
        )
        .map(|candidate| candidate.account)
    }

    /// Select one capability-filtered route target against an explicit wall/mono pair.
    /// Wall drives cooldown/availability; mono drives conversation TTL.
    ///
    /// Duplicate account ids fail closed to `None` and leave sticky / round-robin
    /// / conversation state unchanged.
    #[allow(clippy::too_many_arguments)]
    pub fn select_candidate_at(
        &self,
        candidates: &[RoutingCandidate],
        mode: RoutingMode,
        conversation_sticky: bool,
        conversation_key: Option<&str>,
        exclude_ids: &[&str],
        wall: DateTime<Utc>,
        mono: Instant,
    ) -> Option<RoutingCandidate> {
        match self.try_select_candidate_index_at(
            candidates,
            mode,
            conversation_sticky,
            conversation_key,
            exclude_ids,
            true,
            wall,
            mono,
        ) {
            Ok(Some(index)) => candidates.get(index).cloned(),
            Ok(None) | Err(_) => None,
        }
    }

    /// Typed production selection. Returns a slice index into `candidates`.
    ///
    /// Base availability is computed with no transient excludes. Free candidates
    /// are closed when `free_channel_available` is false (durable SQLite gate and
    /// disabled-Zen-row exhaustion combined by the caller). Duplicate account
    /// ids error before any state mutation.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn try_select_candidate_index_at(
        &self,
        candidates: &[RoutingCandidate],
        mode: RoutingMode,
        conversation_sticky: bool,
        conversation_key: Option<&str>,
        exclude_ids: &[&str],
        free_channel_available: bool,
        wall: DateTime<Utc>,
        mono: Instant,
    ) -> Result<Option<usize>, ocg_gateway::selector::SelectionError> {
        let gateway_candidates = candidates
            .iter()
            .map(|candidate| gateway_candidate(candidate, free_channel_available, wall))
            .collect::<Vec<_>>();
        let mut state = self.inner.lock();
        Ok(state
            .select_at(
                &gateway_candidates,
                selection_policy(mode),
                conversation_sticky,
                conversation_key,
                exclude_ids,
                mono,
            )?
            .map(|selection| selection.candidate_index()))
    }

    /// Read sticky binding for a conversation if still fresh.
    pub fn sticky_binding(
        &self,
        conversation_key: &str,
    ) -> Option<(String, UpstreamChannel, String)> {
        self.sticky_binding_at(conversation_key, Instant::now())
    }

    /// Read sticky binding against an explicit monotonic instant.
    pub fn sticky_binding_at(
        &self,
        conversation_key: &str,
        now: Instant,
    ) -> Option<(String, UpstreamChannel, String)> {
        let mut state = self.inner.lock();
        state.binding_at(conversation_key, now).map(|binding| {
            (
                binding.account_id().to_string(),
                binding.channel(),
                binding.resolved_model().to_string(),
            )
        })
    }
}

pub(crate) fn account_is_available_for(
    account: &Account,
    channel: UpstreamChannel,
    exclude_ids: &[&str],
) -> bool {
    account_is_available_for_at(account, channel, exclude_ids, Utc::now())
}

pub(crate) fn account_is_available_for_at(
    account: &Account,
    channel: UpstreamChannel,
    exclude_ids: &[&str],
    now: DateTime<Utc>,
) -> bool {
    account.enabled
        && account.setup_step.is_ready()
        && account_matches_channel(account, channel)
        && match account.credential_kind {
            CredentialKind::ApiKey => !account.key_cipher.is_empty(),
            CredentialKind::None => true,
        }
        && account.auth_error.is_none()
        && !exclude_ids.iter().any(|excluded| account.id == *excluded)
        && !account.is_cooling_for(channel, now)
}

/// Runtime channel owned by one adapter. Zen is Free; every other adapter is Go.
pub(crate) fn channel_for_adapter(kind: ProviderAdapterKind) -> UpstreamChannel {
    match kind {
        ProviderAdapterKind::ZenFree => UpstreamChannel::Free,
        ProviderAdapterKind::OpenCodeGo
        | ProviderAdapterKind::CommandCodeGoat
        | ProviderAdapterKind::MiniMaxCn
        | ProviderAdapterKind::KimiCn
        | ProviderAdapterKind::OllamaCloud
        | ProviderAdapterKind::Cpa
        | ProviderAdapterKind::ConfigurableHttp => UpstreamChannel::Go,
    }
}

/// Adapter for a routing row: destination projection when present, otherwise
/// the catalog kind for `account.provider_id` (a catalog key, not a reserved
/// account-id gate).
pub(crate) fn adapter_for_account(
    account: &Account,
    destination: Option<&Destination>,
) -> ProviderAdapterKind {
    destination
        .map(|destination| ProviderAdapterKind::from(destination.adapter))
        .or_else(|| crate::dynamic::adapter_kind_for(&account.provider_id, &[]))
        .unwrap_or(ProviderAdapterKind::ConfigurableHttp)
}

/// Runtime channel owned by one adapter. Dashboard probes without a
/// destination still resolve the catalog kind; reserved account ids are not
/// consulted.
pub(crate) fn account_channel(account: &Account) -> Option<UpstreamChannel> {
    account_channel_for(account, None)
}

pub(crate) fn account_channel_for(
    account: &Account,
    destination: Option<&Destination>,
) -> Option<UpstreamChannel> {
    Some(channel_for_adapter(adapter_for_account(
        account,
        destination,
    )))
}

pub(crate) fn free_channel_is_exhausted_at(accounts: &[Account], now: DateTime<Utc>) -> bool {
    accounts.iter().any(|account| {
        adapter_for_account(account, None) == ProviderAdapterKind::ZenFree
            && account.cooldown_free_until.is_some_and(|until| until > now)
    })
}

fn account_matches_channel(account: &Account, channel: UpstreamChannel) -> bool {
    account_channel(account) == Some(channel)
}

fn selection_policy(mode: RoutingMode) -> SelectionPolicy {
    match mode {
        RoutingMode::StrictPriority => SelectionPolicy::StrictPriority,
        RoutingMode::StickyGlobal => SelectionPolicy::StickyGlobal,
        RoutingMode::RoundRobin => SelectionPolicy::RoundRobin,
    }
}

fn gateway_candidate<'a>(
    candidate: &'a RoutingCandidate,
    free_channel_available: bool,
    wall: DateTime<Utc>,
) -> GatewayCandidate<'a> {
    let available = candidate.account.enabled
        && candidate.account.setup_step.is_ready()
        && channel_for_adapter(candidate.adapter) == candidate.channel
        && match candidate.account.credential_kind {
            CredentialKind::ApiKey => !candidate.account.key_cipher.is_empty(),
            CredentialKind::None => true,
        }
        && candidate.account.auth_error.is_none()
        && !candidate.account.is_cooling_for(candidate.channel, wall)
        && (candidate.channel != UpstreamChannel::Free || free_channel_available);
    GatewayCandidate::new(
        candidate.account.id.as_str(),
        candidate.channel,
        candidate.resolved_model.as_str(),
        if available {
            BaseAvailability::Available
        } else {
            BaseAvailability::Unavailable
        },
    )
}

#[cfg(test)]
mod tests;
