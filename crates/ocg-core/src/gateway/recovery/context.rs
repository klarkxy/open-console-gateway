//! Host snapshot of an authorized route and explicitly stored quota membership.
//! Identity digests contain no plaintext Key and are never serialized to logs.
use super::{ResourceKey, ResourceKind, kind_for};
use crate::db::Database;
use crate::gateway::failure::FailureFacts;
use crate::gateway::policy::RestrictionScope;
use crate::routing_snapshot::{ExecutionCredential, RoutingSnapshot};
use anyhow::Result;
use sha2::{Digest, Sha256};

/// Exact send identity used by recovery admission. Callers must pass the URL,
/// route leg, protocol and proxy flag actually used on the outbound attempt.
/// An empty URL is missing model identity: model-scoped rules are skipped.
pub(crate) fn restriction_endpoint_identity(
    url: &str,
    route: impl std::fmt::Debug,
    protocol: impl std::fmt::Debug,
    proxy_identity: Option<&str>,
) -> String {
    format!("{url}|{route:?}|{protocol:?}|{proxy_identity:?}")
}

#[derive(Clone)]
pub(crate) struct ResourceSet {
    endpoint: [u8; 32],
    quota: [u8; 32],
    credits: [u8; 32],
    credential: [u8; 32],
    policy_credential: [u8; 32],
    policy_model: Option<[u8; 32]>,
    owner: String,
    pub(super) members: Vec<String>,
    free_contract: bool,
    pub(super) destination_id: String,
    pub(super) credential_id: String,
    pub(super) upstream_model: Option<String>,
}
impl ResourceSet {
    pub(crate) fn capture(
        db: &Database,
        account: &ExecutionCredential,
        endpoint: &str,
        model: &str,
        free_contract: bool,
    ) -> Result<Self> {
        let snapshot = RoutingSnapshot::load(db)?;
        Self::from_snapshot(&snapshot, account, endpoint, model, free_contract)
    }
    pub(crate) fn from_snapshot(
        snapshot: &RoutingSnapshot,
        account: &ExecutionCredential,
        endpoint: &str,
        model: &str,
        free_contract: bool,
    ) -> Result<Self> {
        let selected = snapshot
            .projection
            .credentials
            .iter()
            .find(|row| row.id == account.credential_id)
            .ok_or_else(|| anyhow::anyhow!("recovery credential no longer exists"))?;
        let mut rows = snapshot
            .credentials
            .iter()
            .filter(|row| {
                row.credential_id == selected.id
                    || selected.quota_pool_id.as_ref().is_some_and(|pool| {
                        snapshot.projection.credentials.iter().any(|candidate| {
                            candidate.id == row.credential_id
                                && candidate.quota_pool_id.as_ref() == Some(pool)
                        })
                    })
            })
            .collect::<Vec<_>>();
        rows.sort_by(|left, right| left.id.cmp(&right.id));
        let members = rows.iter().map(|row| row.id.clone()).collect();
        let identity = |row: &ExecutionCredential| {
            serde_json::json!({
                "id": row.credential_id, "provider": row.provider_id, "cipher": row.key_cipher,
                "enabled": row.enabled, "ready": row.ready,
                "binding": (&row.binding_id, row.credential_version, row.binding_enabled,
                    &row.scope, &row.grants, &row.authorization_connection_id),
                "destination": snapshot.projection.destinations.iter()
                    .find(|d| d.id == row.destination_id)
                    .map(|destination| destination_identity(destination, free_contract)),
            })
        };
        let current = rows
            .iter()
            .find(|row| row.credential_id == account.credential_id)
            .ok_or_else(|| anyhow::anyhow!("recovery credential no longer routes"))?;
        let credential: [u8; 32] = Sha256::digest(serde_json::to_vec(&identity(current))?).into();
        let identities = rows.iter().map(|row| identity(row)).collect::<Vec<_>>();
        let quota: [u8; 32] = Sha256::digest(serde_json::to_vec(&identities)?).into();
        let credits = digest(&[&quota, model.as_bytes()]);
        let policy_credential = credential;
        let policy_model = if endpoint.is_empty() || model.is_empty() {
            None
        } else {
            Some(digest(&[
                &policy_credential,
                endpoint.as_bytes(),
                model.as_bytes(),
            ]))
        };
        Ok(Self {
            endpoint: digest(&[endpoint.as_bytes(), model.as_bytes()]),
            quota,
            credits,
            credential,
            policy_credential,
            policy_model,
            owner: account.id.clone(),
            members,
            free_contract,
            destination_id: account.destination_id.clone(),
            credential_id: account.credential_id.clone(),
            upstream_model: if model.is_empty() {
                None
            } else {
                Some(model.to_string())
            },
        })
    }
    pub(super) fn owner_generation(&self, key: &ResourceKey) -> [u8; 32] {
        match key.kind {
            ResourceKind::CredentialRetry | ResourceKind::PolicyCredential => self.credential,
            ResourceKind::PolicyCredentialModel => self.policy_credential,
            _ => self.quota,
        }
    }
    pub(super) fn owners(&self, key: &ResourceKey) -> &[String] {
        match key.kind {
            ResourceKind::CredentialRetry
            | ResourceKind::PolicyCredential
            | ResourceKind::PolicyCredentialModel => std::slice::from_ref(&self.owner),
            _ => &self.members,
        }
    }
    pub(super) fn keys(&self) -> Vec<ResourceKey> {
        let mut keys: Vec<_> = [
            ResourceKind::CredentialRetry,
            ResourceKind::EndpointModel,
            ResourceKind::Credits,
            ResourceKind::FiveHours,
            ResourceKind::Week,
            ResourceKind::Month,
            ResourceKind::FreeEgress,
            ResourceKind::PolicyCredential,
        ]
        .into_iter()
        .map(|kind| self.key(kind))
        .collect();
        if self.policy_model.is_some() {
            keys.push(self.key(ResourceKind::PolicyCredentialModel));
        }
        keys
    }
    pub(super) fn enforces(&self, key: &ResourceKey) -> bool {
        match key.kind {
            ResourceKind::EndpointModel | ResourceKind::CredentialRetry => true,
            ResourceKind::PolicyCredential | ResourceKind::PolicyCredentialModel => true,
            ResourceKind::FreeEgress => self.free_contract,
            _ => !self.free_contract,
        }
    }
    pub(super) fn key(&self, kind: ResourceKind) -> ResourceKey {
        ResourceKey {
            kind,
            generation: match kind {
                ResourceKind::EndpointModel => self.endpoint,
                ResourceKind::Credits => self.credits,
                ResourceKind::CredentialRetry => self.credential,
                ResourceKind::PolicyCredential => self.policy_credential,
                ResourceKind::PolicyCredentialModel => self.policy_model.unwrap_or([0; 32]),
                ResourceKind::FreeEgress => [0; 32],
                _ => self.quota,
            },
        }
    }
    pub(super) fn policy_key(&self, scope: RestrictionScope) -> Option<ResourceKey> {
        match scope {
            RestrictionScope::Credential => Some(self.key(ResourceKind::PolicyCredential)),
            RestrictionScope::CredentialModel => self
                .policy_model
                .is_some()
                .then(|| self.key(ResourceKind::PolicyCredentialModel)),
        }
    }
    pub(super) fn for_facts(&self, facts: &FailureFacts) -> ResourceKey {
        self.key(kind_for(facts))
    }
    pub(super) fn same_generation(&self, other: &Self) -> bool {
        self.quota == other.quota
            && self.endpoint == other.endpoint
            && self.free_contract == other.free_contract
            && self.credential == other.credential
            && self.policy_credential == other.policy_credential
            && self.policy_model == other.policy_model
    }
    pub(super) fn same_policy_identity(&self, other: &Self) -> bool {
        self.policy_credential == other.policy_credential
            && self.policy_model == other.policy_model
            && self.destination_id == other.destination_id
            && self.credential_id == other.credential_id
            && self.free_contract == other.free_contract
    }
    #[cfg(test)]
    pub(super) fn with_credential(mut self, generation: u8, owner: &str) -> Self {
        self.credential = [generation; 32];
        self.policy_credential = [generation; 32];
        self.policy_model = self
            .policy_model
            .map(|_| digest(&[&[generation], &self.endpoint[..8], &[1]]));
        self.owner = owner.into();
        self.credential_id = format!("c{generation}-{owner}");
        self
    }
    #[cfg(test)]
    pub(super) fn with_quota_pool(mut self, generation: u8) -> Self {
        self.quota = [generation; 32];
        self
    }
    #[cfg(test)]
    pub(super) fn without_model(mut self) -> Self {
        self.policy_model = None;
        self
    }
    #[cfg(test)]
    pub(super) fn unique(index: u16, members: &[&str]) -> Self {
        let mut credential = [0u8; 32];
        credential[0] = (index >> 8) as u8;
        credential[1] = index as u8;
        let owner = members
            .first()
            .map(|name| format!("{name}-{index}"))
            .unwrap_or_else(|| format!("fixture-{index}"));
        Self {
            quota: credential,
            credential,
            policy_credential: credential,
            policy_model: Some(digest(&[&credential, &[1], &[1]])),
            owner: owner.clone(),
            credits: digest(&[&credential, &[1]]),
            endpoint: digest(&[&credential, &[1]]),
            members: vec![owner],
            free_contract: false,
            destination_id: format!("dest-{index}"),
            credential_id: format!("cred-{index}"),
            upstream_model: Some("model".into()),
        }
    }
    #[cfg(test)]
    pub(super) fn fixture(
        credential: u8,
        endpoint: u8,
        model: u8,
        members: &[&str],
        free_contract: bool,
    ) -> Self {
        let credential_id = [credential; 32];
        Self {
            quota: [credential; 32],
            credential: credential_id,
            policy_credential: credential_id,
            policy_model: Some(digest(&[&[credential], &[endpoint], &[model]])),
            owner: members.first().copied().unwrap_or("fixture").into(),
            credits: digest(&[&[credential], &[model]]),
            endpoint: digest(&[&[endpoint], &[model]]),
            members: members.iter().map(|v| (*v).into()).collect(),
            free_contract,
            destination_id: format!("dest-{endpoint}"),
            credential_id: format!("cred-{credential}"),
            upstream_model: Some(format!("model-{model}")),
        }
    }
}
fn destination_identity(
    destination: &ocg_domain::destination::Destination,
    free_contract: bool,
) -> ocg_domain::destination::Destination {
    use ocg_domain::destination::{AdapterKind, AuthScheme};
    let mut identity = destination.clone();
    if free_contract
        && identity.adapter == AdapterKind::Zen
        && identity.auth_scheme == AuthScheme::None
    {
        // Anonymous Free limits belong to the shared egress. A catalog refresh
        // does not invalidate evidence from an already dispatched request.
        // Future sends still check the complete live destination/model row.
        identity.catalog.clear();
    }
    identity
}

fn digest(parts: &[&[u8]]) -> [u8; 32] {
    let mut hasher = Sha256::new();
    for part in parts {
        hasher.update((part.len() as u64).to_le_bytes());
        hasher.update(part);
    }
    hasher.finalize().into()
}

#[cfg(test)]
mod tests;
