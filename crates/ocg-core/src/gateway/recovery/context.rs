//! Host snapshot of an authorized route and explicitly stored quota membership.
//! Identity digests contain no plaintext Key and are never serialized to logs.
use super::policies::PolicyResource;
use super::{ResourceKey, ResourceKind, kind_for};
use crate::db::Database;
use crate::db::temporary_policy::SavedRules;
use crate::gateway::failure::FailureFacts;
use crate::routing_snapshot::{ExecutionCredential, RoutingSnapshot};
use crate::temporary_policy::TemporaryRuleScope;
use anyhow::Result;
use sha2::{Digest, Sha256};

#[derive(Clone)]
pub(crate) struct ResourceSet {
    endpoint: [u8; 32],
    quota: [u8; 32],
    credits: [u8; 32],
    credential: [u8; 32],
    owner: String,
    pub(super) members: Vec<String>,
    free_contract: bool,
    pub(super) policies: Vec<PolicyResource>,
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
        let mut resources =
            Self::from_snapshot(&snapshot, account, endpoint, model, free_contract)?;
        let rules = crate::db::temporary_policy::load_on(&db.conn)?;
        resources.add_policies(&rules, account, endpoint, model);
        Ok(resources)
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
        let credential = Sha256::digest(serde_json::to_vec(&identity(current))?).into();
        let identities = rows.iter().map(|row| identity(row)).collect::<Vec<_>>();
        let quota: [u8; 32] = Sha256::digest(serde_json::to_vec(&identities)?).into();
        let credits = digest(&[&quota, model.as_bytes()]);
        Ok(Self {
            endpoint: digest(&[endpoint.as_bytes(), model.as_bytes()]),
            quota,
            credits,
            credential,
            owner: account.id.clone(),
            members,
            free_contract,
            policies: Vec::new(),
        })
    }
    pub(super) fn add_policies(
        &mut self,
        saved: &SavedRules,
        account: &ExecutionCredential,
        endpoint: &str,
        model: &str,
    ) {
        // Anonymous shared Free keeps its existing declared egress policy.
        if self.free_contract {
            return;
        }
        self.policies = saved
            .effective(&account.destination_id)
            .into_iter()
            .filter_map(|rule| {
                if rule.rule.scope == TemporaryRuleScope::CredentialModel && model.is_empty() {
                    return None; // Missing model never widens the configured scope.
                }
                let kind = match rule.rule.scope {
                    TemporaryRuleScope::Credential => ResourceKind::PolicyCredential,
                    TemporaryRuleScope::CredentialModel => ResourceKind::PolicyCredentialModel,
                };
                let version = rule.revision.to_le_bytes();
                let route = if kind == ResourceKind::PolicyCredentialModel {
                    &self.endpoint[..]
                } else {
                    &[]
                };
                let generation = digest(&[
                    &self.credential,
                    route,
                    rule.rule.id.as_bytes(),
                    rule.rule.destination_id.as_deref().unwrap_or("").as_bytes(),
                    &version,
                ]);
                Some(PolicyResource {
                    key: ResourceKey { kind, generation },
                    retry_key: self.key(ResourceKind::CredentialModelRetry),
                    credential_key: self.key(ResourceKind::CredentialRetry),
                    rule,
                    credential_id: account.credential_id.clone(),
                    account_id: account.id.clone(),
                    destination_id: account.destination_id.clone(),
                    endpoint: endpoint.into(),
                    model: model.into(),
                })
            })
            .collect();
    }

    pub(super) fn credential_generation(&self) -> [u8; 32] {
        self.credential
    }

    pub(super) fn policy_for(&self, key: &ResourceKey) -> Option<&PolicyResource> {
        self.policies.iter().find(|policy| policy.key == *key)
    }

    pub(super) fn owner_generation(&self, key: &ResourceKey) -> [u8; 32] {
        if key.kind.credential_scoped() {
            self.credential
        } else {
            self.quota
        }
    }
    pub(super) fn owners(&self, key: &ResourceKey) -> &[String] {
        if key.kind.credential_scoped() {
            std::slice::from_ref(&self.owner)
        } else {
            &self.members
        }
    }
    pub(super) fn keys(&self) -> Vec<ResourceKey> {
        [
            ResourceKind::CredentialRetry,
            ResourceKind::CredentialModelRetry,
            ResourceKind::EndpointModel,
            ResourceKind::Credits,
            ResourceKind::FiveHours,
            ResourceKind::Week,
            ResourceKind::Month,
            ResourceKind::FreeEgress,
        ]
        .into_iter()
        .map(|kind| self.key(kind))
        .chain(self.policies.iter().map(|policy| policy.key.clone()))
        .collect()
    }
    pub(super) fn enforces(&self, key: &ResourceKey) -> bool {
        match key.kind {
            ResourceKind::EndpointModel
            | ResourceKind::CredentialRetry
            | ResourceKind::CredentialModelRetry
            | ResourceKind::PolicyCredential
            | ResourceKind::PolicyCredentialModel => true,
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
                ResourceKind::CredentialRetry | ResourceKind::PolicyCredential => self.credential,
                ResourceKind::CredentialModelRetry | ResourceKind::PolicyCredentialModel => {
                    digest(&[&self.credential, &self.endpoint])
                }
                ResourceKind::FreeEgress => [0; 32],
                _ => self.quota,
            },
        }
    }
    pub(super) fn for_facts(&self, facts: &FailureFacts) -> ResourceKey {
        self.key(kind_for(facts))
    }
    pub(super) fn same_generation(&self, other: &Self) -> bool {
        self.quota == other.quota
            && self.endpoint == other.endpoint
            && self.free_contract == other.free_contract
    }
    #[cfg(test)]
    pub(super) fn with_credential(mut self, generation: u8, owner: &str) -> Self {
        self.credential = [generation; 32];
        self.owner = owner.into();
        self
    }
    #[cfg(test)]
    pub(super) fn fixture(
        credential: u8,
        endpoint: u8,
        model: u8,
        members: &[&str],
        free_contract: bool,
    ) -> Self {
        Self {
            quota: [credential; 32],
            credential: [credential; 32],
            owner: members.first().copied().unwrap_or("fixture").into(),
            credits: digest(&[&[credential], &[model]]),
            endpoint: digest(&[&[endpoint], &[model]]),
            members: members.iter().map(|v| (*v).into()).collect(),
            free_contract,
            policies: Vec::new(),
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
