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
pub struct RoutingCandidate<A = Account> {
    pub account: A,
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
    pub(crate) fn try_select_candidate_index_at<A: CandidateFacts>(
        &self,
        candidates: &[RoutingCandidate<A>],
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

    /// Clone-based selection preview. Sticky-global and round-robin on the
    /// live slot stay unchanged.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn preview_candidate_index_at<A: CandidateFacts>(
        &self,
        candidates: &[RoutingCandidate<A>],
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
        let mut preview = self.inner.lock().clone();
        Ok(preview
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

#[cfg(test)]
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

/// Why a materialized candidate is or is not base-available.
///
/// Order matches the historical `gateway_candidate` conjunction so selection
/// and explanation share one assessment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CandidateAvailability {
    Available,
    AccountDisabled,
    SetupNotReady,
    ChannelMismatch,
    CredentialMissing,
    AuthError,
    CoolingDown,
    FreeChannelUnavailable,
    QuotaWaiting,
    QuotaProbing,
}

impl CandidateAvailability {
    pub(crate) fn is_available(self) -> bool {
        matches!(self, Self::Available)
    }

    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Available => "available",
            Self::AccountDisabled => "account_disabled",
            Self::SetupNotReady => "setup_not_ready",
            Self::ChannelMismatch => "channel_mismatch",
            Self::CredentialMissing => "credential_missing",
            Self::AuthError => "auth_error",
            Self::CoolingDown => "cooling_down",
            Self::FreeChannelUnavailable => "free_channel_unavailable",
            Self::QuotaWaiting => "quota_waiting",
            Self::QuotaProbing => "quota_probing",
        }
    }
}

pub(crate) fn assess_candidate_availability<A: CandidateFacts>(
    candidate: &RoutingCandidate<A>,
    free_channel_available: bool,
    wall: DateTime<Utc>,
) -> CandidateAvailability {
    candidate.account.availability(
        candidate.adapter,
        candidate.channel,
        free_channel_available,
        wall,
    )
}

pub(crate) trait CandidateFacts {
    fn routing_id(&self) -> &str;
    fn availability(
        &self,
        adapter: ProviderAdapterKind,
        channel: UpstreamChannel,
        free: bool,
        wall: DateTime<Utc>,
    ) -> CandidateAvailability;
}

impl CandidateFacts for Account {
    fn routing_id(&self) -> &str {
        &self.id
    }
    fn availability(
        &self,
        adapter: ProviderAdapterKind,
        channel: UpstreamChannel,
        free: bool,
        wall: DateTime<Utc>,
    ) -> CandidateAvailability {
        if !self.enabled {
            CandidateAvailability::AccountDisabled
        } else if !self.setup_step.is_ready() {
            CandidateAvailability::SetupNotReady
        } else if channel_for_adapter(adapter) != channel {
            CandidateAvailability::ChannelMismatch
        } else if self.credential_kind == CredentialKind::ApiKey && self.key_cipher.is_empty() {
            CandidateAvailability::CredentialMissing
        } else if self.auth_error.is_some() {
            CandidateAvailability::AuthError
        } else if self.is_cooling_for(channel, wall) {
            CandidateAvailability::CoolingDown
        } else if channel == UpstreamChannel::Free && !free {
            CandidateAvailability::FreeChannelUnavailable
        } else {
            CandidateAvailability::Available
        }
    }
}

impl CandidateFacts for crate::routing_snapshot::ExecutionCredential {
    fn routing_id(&self) -> &str {
        &self.id
    }
    fn availability(
        &self,
        adapter: ProviderAdapterKind,
        channel: UpstreamChannel,
        free: bool,
        wall: DateTime<Utc>,
    ) -> CandidateAvailability {
        if !self.enabled {
            CandidateAvailability::AccountDisabled
        } else if !self.ready {
            CandidateAvailability::SetupNotReady
        } else if channel_for_adapter(adapter) != channel {
            CandidateAvailability::ChannelMismatch
        } else if self.auth_error.is_some() {
            CandidateAvailability::AuthError
        } else if self.is_cooling_for(channel, wall) {
            CandidateAvailability::CoolingDown
        } else if self.quota_probe {
            CandidateAvailability::QuotaProbing
        } else if self
            .quota_recovery
            .as_ref()
            .is_some_and(|recovery| !recovery.due_at(wall))
        {
            CandidateAvailability::QuotaWaiting
        } else if channel == UpstreamChannel::Free && !free {
            CandidateAvailability::FreeChannelUnavailable
        } else {
            CandidateAvailability::Available
        }
    }
}

fn gateway_candidate<'a, A: CandidateFacts>(
    candidate: &'a RoutingCandidate<A>,
    free_channel_available: bool,
    wall: DateTime<Utc>,
) -> GatewayCandidate<'a> {
    let available = assess_candidate_availability(candidate, free_channel_available, wall);
    GatewayCandidate::new(
        candidate.account.routing_id(),
        candidate.channel,
        candidate.resolved_model.as_str(),
        if available.is_available() {
            BaseAvailability::Available
        } else {
            BaseAvailability::Unavailable
        },
    )
}

#[cfg(test)]
mod tests;
