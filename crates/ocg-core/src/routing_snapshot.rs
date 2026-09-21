//! Persisted facts captured once for a logical inference request.
use crate::destination_projection::DestinationProjection;
use crate::models::UpstreamChannel;
use crate::quota_recovery::{PersistedQuotaRecovery, QuotaEpisode};
use chrono::{DateTime, Utc};
use ocg_domain::credential::ModelScope;
use ocg_domain::destination::{Cooldowns, Grants};
use std::collections::HashMap;

/// Private execution identity. This is deliberately not an Account or an API DTO.
/// Ciphertext is retained solely to reject rotation and guard late state writes.
#[derive(Clone, Debug)]
pub(crate) struct ExecutionCredential {
    pub id: String,
    pub credential_id: String,
    pub destination_id: String,
    pub provider_id: String,
    pub official_pricing_kind: Option<crate::official_api::OfficialApiKind>,
    pub name: String,
    pub key_cipher: String,
    pub enabled: bool,
    pub ready: bool,
    pub auth_error: Option<String>,
    pub cooldowns: Cooldowns,
    pub binding_id: String,
    pub binding_enabled: bool,
    pub credential_version: u64,
    pub authorization_connection_id: String,
    pub scope: ModelScope,
    pub grants: Grants,
    pub quota_recovery: Option<PersistedQuotaRecovery>,
    /// Process-local probing overlay. Not persisted.
    pub quota_probe: bool,
}

impl ExecutionCredential {
    pub(crate) fn is_cooling_for(&self, channel: UpstreamChannel, now: DateTime<Utc>) -> bool {
        self.cooldown_ends_at_for(channel, now).is_some()
    }

    pub(crate) fn cooldown_ends_at_for(
        &self,
        channel: UpstreamChannel,
        now: DateTime<Utc>,
    ) -> Option<DateTime<Utc>> {
        let c = &self.cooldowns;
        let windows = match channel {
            UpstreamChannel::Go => [
                c.generic_until,
                c.five_hour_until,
                c.week_until,
                c.month_until,
            ],
            UpstreamChannel::Free => [c.generic_until, c.free_until, None, None],
        };
        windows
            .into_iter()
            .flatten()
            .filter(|until| *until > now)
            .max()
    }

    pub(crate) fn matches_quota_episode(&self, episode: &QuotaEpisode) -> bool {
        let Some(recovery) = self.quota_recovery.as_ref() else {
            return false;
        };
        quota_episode_matches(
            episode,
            &self.credential_id,
            self.credential_version,
            &self.key_cipher,
            recovery.epoch,
        )
    }
}

pub(crate) fn quota_episode_matches(
    episode: &QuotaEpisode,
    credential_id: &str,
    credential_version: u64,
    key_cipher: &str,
    epoch: u64,
) -> bool {
    episode.credential_id == credential_id
        && episode.credential_version == credential_version
        && episode.key_cipher == key_cipher
        && episode.epoch == epoch
}

#[derive(Clone, Debug)]
pub(crate) struct RoutingSnapshot {
    pub projection: DestinationProjection,
    pub credentials: Vec<ExecutionCredential>,
    pub ollama_pinned: Vec<String>,
}

impl RoutingSnapshot {
    pub(crate) fn load(db: &crate::db::Database) -> anyhow::Result<Self> {
        let projection = crate::destination_projection::load_runtime(db)?;
        let credentials = crate::db::routing_credentials::load(db, &projection)?;
        let ollama_pinned = crate::db::routing_credentials::load_ollama_pins(db)?;
        Ok(Self {
            projection,
            credentials,
            ollama_pinned,
        })
    }

    pub(crate) fn apply_quota_probes(&mut self, probes: &HashMap<String, QuotaEpisode>) {
        for credential in &mut self.credentials {
            credential.quota_probe = probes
                .get(&credential.credential_id)
                .is_some_and(|episode| credential.matches_quota_episode(episode));
        }
    }
}

#[cfg(test)]
impl From<&crate::models::Account> for ExecutionCredential {
    fn from(account: &crate::models::Account) -> Self {
        Self {
            id: account.id.clone(),
            credential_id: String::new(),
            destination_id: String::new(),
            provider_id: account.provider_id.clone(),
            official_pricing_kind: None,
            name: account.name.clone(),
            key_cipher: account.key_cipher.clone(),
            enabled: account.enabled,
            ready: account.setup_step.is_ready(),
            auth_error: account.auth_error.clone(),
            cooldowns: Cooldowns {
                generic_until: account.cooldown_generic_until,
                five_hour_until: account.cooldown_5h_until,
                week_until: account.cooldown_week_until,
                month_until: account.cooldown_month_until,
                free_until: account.cooldown_free_until,
            },
            binding_id: String::new(),
            binding_enabled: true,
            credential_version: 0,
            authorization_connection_id: String::new(),
            scope: ModelScope::All,
            grants: Grants {
                allowed_endpoint_ids: vec![],
                allowed_origins: vec![],
            },
            quota_recovery: None,
            quota_probe: false,
        }
    }
}

#[cfg(test)]
mod tests;
