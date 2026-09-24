//! Pure secret-free in-memory selection state machine.
//!
//! A later host adapter computes wall-clock, row, binding, auth, and Free-gate
//! eligibility once and supplies only [`BaseAvailability`]. This module never
//! sees host rows, stored secrets, or process I/O. [`std::time::Instant`] is
//! used only for conversation TTL and LRU recency, and must be supplied by
//! the caller as `now`.
//!
//! Candidate slice order is the authoritative card order. [`select_at`][SelectorState::select_at]
//! returns a [`Selection`] index into that slice; it never clones a candidate.
//! Duplicate account ids are rejected before any state mutation.
//!
//! Items are rust-public only as the cross-crate bridge; a later host facade
//! should keep historical routing-runtime paths crate-private.

use ocg_domain::account::UpstreamChannel;
use std::collections::{HashMap, VecDeque};
use std::time::{Duration, Instant};

/// Idle conversation bindings expire at this age, inclusive (`>=`).
pub const CONVERSATION_TTL: Duration = Duration::from_secs(30 * 60);

/// Maximum live conversation bindings. Hits refresh LRU recency.
pub const MAX_CONVERSATIONS: usize = 4096;

/// How the state machine walks base-available, non-excluded cards.
///
/// Public only as the cross-crate bridge.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[doc(hidden)]
pub enum SelectionPolicy {
    StrictPriority,
    StickyGlobal,
    RoundRobin,
}

/// Adapter-precomputed eligibility for one card. Transient request excludes
/// are a separate `&[&str]` of account ids; they are not encoded here.
///
/// Public only as the cross-crate bridge.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[doc(hidden)]
pub enum BaseAvailability {
    Available,
    Unavailable,
}

/// One already-filtered route target. Fields stay private so callers go
/// through [`Self::new`] and the accessors.
///
/// Public only as the cross-crate bridge.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[doc(hidden)]
pub struct Candidate<'a> {
    account_id: &'a str,
    channel: UpstreamChannel,
    resolved_model: &'a str,
    base_availability: BaseAvailability,
}

impl<'a> Candidate<'a> {
    pub fn new(
        account_id: &'a str,
        channel: UpstreamChannel,
        resolved_model: &'a str,
        base_availability: BaseAvailability,
    ) -> Self {
        Self {
            account_id,
            channel,
            resolved_model,
            base_availability,
        }
    }

    pub fn account_id(&self) -> &'a str {
        self.account_id
    }

    pub fn channel(&self) -> UpstreamChannel {
        self.channel
    }

    pub fn resolved_model(&self) -> &'a str {
        self.resolved_model
    }

    pub fn base_availability(&self) -> BaseAvailability {
        self.base_availability
    }

    fn is_selectable(&self, exclude_ids: &[&str]) -> bool {
        self.base_availability == BaseAvailability::Available
            && !exclude_ids.contains(&self.account_id)
    }
}

/// Index of the chosen candidate in the caller-supplied slice.
///
/// Public only as the cross-crate bridge.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[doc(hidden)]
pub struct Selection {
    candidate_index: usize,
}

impl Selection {
    pub fn candidate_index(&self) -> usize {
        self.candidate_index
    }
}

/// Recoverable selection failure. Duplicate ids are rejected with the first
/// and later slice indices; state is left unchanged.
///
/// Public only as the cross-crate bridge.
#[derive(Debug, Clone, PartialEq, Eq)]
#[doc(hidden)]
pub enum SelectionError {
    DuplicateAccountId { first: usize, duplicate: usize },
}

impl std::fmt::Display for SelectionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::DuplicateAccountId { first, duplicate } => write!(
                f,
                "duplicate account id at candidate index {duplicate} (first seen at {first})"
            ),
        }
    }
}

impl std::error::Error for SelectionError {}

/// Fresh conversation sticky binding. Fields stay private; use the accessors.
///
/// Public only as the cross-crate bridge.
#[derive(Debug, Clone, PartialEq, Eq)]
#[doc(hidden)]
pub struct BindingSnapshot {
    account_id: String,
    channel: UpstreamChannel,
    resolved_model: String,
}

impl BindingSnapshot {
    pub fn account_id(&self) -> &str {
        &self.account_id
    }

    pub fn channel(&self) -> UpstreamChannel {
        self.channel
    }

    pub fn resolved_model(&self) -> &str {
        &self.resolved_model
    }
}

#[derive(Debug, Clone)]
struct ConversationBinding {
    account_id: String,
    channel: UpstreamChannel,
    resolved_model: String,
    last_seen: Instant,
}

#[derive(Debug, Default, Clone)]
struct ConversationMap {
    entries: HashMap<String, ConversationBinding>,
    order: VecDeque<String>,
}

impl ConversationMap {
    fn get_fresh(&mut self, key: &str, now: Instant) -> Option<&ConversationBinding> {
        self.purge_expired(now);
        let expired = self
            .entries
            .get(key)
            .is_some_and(|binding| now.duration_since(binding.last_seen) >= CONVERSATION_TTL);
        if expired {
            self.remove(key);
            return None;
        }
        if let Some(binding) = self.entries.get_mut(key) {
            binding.last_seen = now;
            self.touch_order(key);
            self.entries.get(key)
        } else {
            None
        }
    }

    fn insert(
        &mut self,
        key: String,
        account_id: String,
        channel: UpstreamChannel,
        resolved_model: String,
        now: Instant,
    ) {
        self.purge_expired(now);
        if let Some(existing) = self.entries.get_mut(&key) {
            existing.account_id = account_id;
            existing.channel = channel;
            existing.resolved_model = resolved_model;
            existing.last_seen = now;
            self.touch_order(&key);
            return;
        }
        while self.entries.len() >= MAX_CONVERSATIONS {
            if let Some(oldest) = self.order.pop_front() {
                self.entries.remove(&oldest);
            } else {
                break;
            }
        }
        self.entries.insert(
            key.clone(),
            ConversationBinding {
                account_id,
                channel,
                resolved_model,
                last_seen: now,
            },
        );
        self.order.push_back(key);
    }

    fn remove(&mut self, key: &str) {
        self.entries.remove(key);
        if let Some(index) = self.order.iter().position(|item| item == key) {
            self.order.remove(index);
        }
    }

    fn touch_order(&mut self, key: &str) {
        if let Some(index) = self.order.iter().position(|item| item == key) {
            self.order.remove(index);
        }
        self.order.push_back(key.to_string());
    }

    fn purge_expired(&mut self, now: Instant) {
        let expired = self
            .entries
            .iter()
            .filter(|(_, binding)| now.duration_since(binding.last_seen) >= CONVERSATION_TTL)
            .map(|(key, _)| key.clone())
            .collect::<Vec<_>>();
        for key in expired {
            self.remove(&key);
        }
    }
}

/// Owned sticky / round-robin / conversation LRU slot. Mutation is `&mut self`;
/// the caller supplies any sharing wrapper.
///
/// Public only as the cross-crate bridge.
#[derive(Debug, Default, Clone)]
#[doc(hidden)]
pub struct SelectorState {
    global_account_id: Option<String>,
    round_robin_after: Option<String>,
    conversations: ConversationMap,
}

impl SelectorState {
    pub fn new() -> Self {
        Self::default()
    }

    /// Select one candidate index and update sticky / round-robin / conversation
    /// state. Duplicate account ids are rejected before any mutation.
    pub fn select_at(
        &mut self,
        candidates: &[Candidate<'_>],
        policy: SelectionPolicy,
        conversation_sticky: bool,
        conversation_key: Option<&str>,
        exclude_ids: &[&str],
        now: Instant,
    ) -> Result<Option<Selection>, SelectionError> {
        if let Some((first, duplicate)) = first_duplicate_account_id(candidates) {
            return Err(SelectionError::DuplicateAccountId { first, duplicate });
        }

        if conversation_sticky && let Some(key) = conversation_key {
            let bound = self.conversations.get_fresh(key, now).map(|binding| {
                (
                    binding.account_id.clone(),
                    binding.channel,
                    binding.resolved_model.clone(),
                )
            });
            if let Some((account_id, channel, resolved_model)) = bound
                && let Some(candidate_index) = find_available_index(
                    candidates,
                    &account_id,
                    channel,
                    &resolved_model,
                    exclude_ids,
                )
            {
                return Ok(Some(Selection { candidate_index }));
            }
        }

        let selected = match policy {
            SelectionPolicy::StrictPriority => first_available_index(candidates, exclude_ids),
            SelectionPolicy::StickyGlobal => self.select_sticky_global(candidates, exclude_ids),
            SelectionPolicy::RoundRobin => self.select_round_robin(candidates, exclude_ids),
        };

        if let Some(candidate_index) = selected
            && conversation_sticky
            && let Some(key) = conversation_key
        {
            let candidate = &candidates[candidate_index];
            self.conversations.insert(
                key.to_string(),
                candidate.account_id.to_string(),
                candidate.channel,
                candidate.resolved_model.to_string(),
                now,
            );
        }

        Ok(selected.map(|candidate_index| Selection { candidate_index }))
    }

    /// Clone-based selection preview. Sticky-global, round-robin, and
    /// conversation LRU on `self` stay unchanged, including when the cloned
    /// machine would have rewritten them.
    pub fn preview_at(
        &self,
        candidates: &[Candidate<'_>],
        policy: SelectionPolicy,
        conversation_sticky: bool,
        conversation_key: Option<&str>,
        exclude_ids: &[&str],
        now: Instant,
    ) -> Result<Option<Selection>, SelectionError> {
        let mut preview = self.clone();
        preview.select_at(
            candidates,
            policy,
            conversation_sticky,
            conversation_key,
            exclude_ids,
            now,
        )
    }

    /// Read a conversation binding if it is still fresh. A hit refreshes LRU
    /// recency using `now`, matching lookup during [`Self::select_at`].
    pub fn binding_at(&mut self, conversation_key: &str, now: Instant) -> Option<BindingSnapshot> {
        self.conversations
            .get_fresh(conversation_key, now)
            .map(|binding| BindingSnapshot {
                account_id: binding.account_id.clone(),
                channel: binding.channel,
                resolved_model: binding.resolved_model.clone(),
            })
    }

    pub fn reset(&mut self) {
        *self = Self::default();
    }

    fn select_sticky_global(
        &mut self,
        candidates: &[Candidate<'_>],
        exclude_ids: &[&str],
    ) -> Option<usize> {
        if let Some(current_id) = self.global_account_id.clone() {
            if let Some(index) = candidates.iter().position(|candidate| {
                candidate.account_id == current_id && candidate.is_selectable(exclude_ids)
            }) {
                return Some(index);
            }
            let persistently_available = candidates.iter().any(|candidate| {
                candidate.account_id == current_id && candidate.is_selectable(&[])
            });
            let selected = first_available_index(candidates, exclude_ids)?;
            if !persistently_available {
                self.global_account_id = Some(candidates[selected].account_id.to_string());
            }
            return Some(selected);
        }
        let selected = first_available_index(candidates, exclude_ids)?;
        self.global_account_id = Some(candidates[selected].account_id.to_string());
        Some(selected)
    }

    fn select_round_robin(
        &mut self,
        candidates: &[Candidate<'_>],
        exclude_ids: &[&str],
    ) -> Option<usize> {
        if candidates.is_empty() {
            return None;
        }
        let start = self
            .round_robin_after
            .as_ref()
            .and_then(|after| {
                candidates
                    .iter()
                    .position(|candidate| candidate.account_id == *after)
            })
            .map(|index| (index + 1) % candidates.len())
            .unwrap_or(0);
        for offset in 0..candidates.len() {
            let index = (start + offset) % candidates.len();
            if candidates[index].is_selectable(exclude_ids) {
                self.round_robin_after = Some(candidates[index].account_id.to_string());
                return Some(index);
            }
        }
        None
    }
}

fn first_duplicate_account_id(candidates: &[Candidate<'_>]) -> Option<(usize, usize)> {
    let mut first_by_id = HashMap::with_capacity(candidates.len());
    for (index, candidate) in candidates.iter().enumerate() {
        if let Some(first) = first_by_id.insert(candidate.account_id, index) {
            return Some((first, index));
        }
    }
    None
}

fn first_available_index(candidates: &[Candidate<'_>], exclude_ids: &[&str]) -> Option<usize> {
    candidates
        .iter()
        .position(|candidate| candidate.is_selectable(exclude_ids))
}

fn find_available_index(
    candidates: &[Candidate<'_>],
    account_id: &str,
    channel: UpstreamChannel,
    resolved_model: &str,
    exclude_ids: &[&str],
) -> Option<usize> {
    candidates.iter().position(|candidate| {
        candidate.account_id == account_id
            && candidate.channel == channel
            && candidate.resolved_model == resolved_model
            && candidate.is_selectable(exclude_ids)
    })
}

#[cfg(test)]
mod tests;
