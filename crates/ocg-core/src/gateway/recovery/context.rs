//! Host snapshot of an authorized route and explicitly stored quota membership.
//! Identity digests contain no plaintext Key and are never serialized to logs.
use super::{ResourceKey, ResourceKind, kind_for};
use crate::db::Database;
use crate::gateway::failure::FailureFacts;
use crate::models::Account;
use anyhow::Result;
use sha2::{Digest, Sha256};

#[derive(Clone)]
pub(crate) struct ResourceSet {
    endpoint: [u8; 32],
    quota: [u8; 32],
    credits: [u8; 32],
    pub(super) members: Vec<String>,
    free_contract: bool,
}
impl ResourceSet {
    pub(crate) fn capture(
        db: &Database,
        account: &Account,
        endpoint: &str,
        model: &str,
        free_contract: bool,
    ) -> Result<Self> {
        let mut members = db.shared_pool_account_ids(&account.id)?;
        if members.is_empty() {
            members.push(account.id.clone());
        }
        members.sort();
        members.dedup();
        let bindings = db.list_inference_bindings()?;
        let mut identities = Vec::new();
        for id in &members {
            if let Some(row) = db.get_account(id)? {
                let binding = bindings.iter().find(|b| b.account_id == *id);
                identities.push(serde_json::json!({
                    "id": row.id, "provider": row.provider_id, "cipher": row.key_cipher,
                    "enabled": row.enabled,
                    "binding": binding.map(|b| (&b.binding_id, b.credential_version, b.enabled, &b.model_scope, &b.allowed_endpoint_ids, &b.allowed_origins)),
                }));
            }
        }
        if identities.is_empty() {
            identities.push(serde_json::json!({"id":account.id,"provider":account.provider_id,"cipher":account.key_cipher}));
        }
        let quota: [u8; 32] = Sha256::digest(serde_json::to_vec(&identities)?).into();
        let credits = digest(&[&quota, model.as_bytes()]);
        Ok(Self {
            endpoint: digest(&[endpoint.as_bytes(), model.as_bytes()]),
            quota,
            credits,
            members,
            free_contract,
        })
    }
    pub(super) fn quota_generation(&self) -> [u8; 32] {
        self.quota
    }
    pub(super) fn keys(&self) -> Vec<ResourceKey> {
        [
            ResourceKind::EndpointModel,
            ResourceKind::Credits,
            ResourceKind::FiveHours,
            ResourceKind::Week,
            ResourceKind::Month,
            ResourceKind::FreeEgress,
        ]
        .into_iter()
        .map(|kind| self.key(kind))
        .collect()
    }
    pub(super) fn enforces(&self, key: &ResourceKey) -> bool {
        match key.kind {
            ResourceKind::EndpointModel => true,
            ResourceKind::FreeEgress => self.free_contract,
            _ => !self.free_contract,
        }
    }
    fn key(&self, kind: ResourceKind) -> ResourceKey {
        ResourceKey {
            kind,
            generation: match kind {
                ResourceKind::EndpointModel => self.endpoint,
                ResourceKind::Credits => self.credits,
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
    pub(super) fn fixture(
        credential: u8,
        endpoint: u8,
        model: u8,
        members: &[&str],
        free_contract: bool,
    ) -> Self {
        Self {
            quota: [credential; 32],
            credits: digest(&[&[credential], &[model]]),
            endpoint: digest(&[&[endpoint], &[model]]),
            members: members.iter().map(|v| (*v).into()).collect(),
            free_contract,
        }
    }
}
fn digest(parts: &[&[u8]]) -> [u8; 32] {
    let mut hasher = Sha256::new();
    for part in parts {
        hasher.update((part.len() as u64).to_le_bytes());
        hasher.update(part);
    }
    hasher.finalize().into()
}
