//! Saved account operation choices. Mutations still recheck current state and CAS.
use super::super::types::*;
use super::*;
use ocg_domain::credential::{
    CredentialPurpose, MaterialKind, RuntimeSubjectKind, normalize_origin,
};
use std::collections::HashSet;

pub(super) fn detail(
    s: &PageSnapshot,
    account: &dashboard_v3::Account,
    destination: Option<&DestinationDto>,
    identity: Option<&IdentitySummary>,
) -> AccountOperationDetail {
    let selected = identity.and_then(|i| {
        i.credentials.iter().find(|c| {
            c.legacy.id == account.id && c.credential.purpose == CredentialPurpose::Inference
        })
    });
    let binding = selected.and_then(|c| c.bindings.first());
    let connection = binding.and_then(|b| s.connections.iter().find(|c| c.id == b.connection_id));
    let reason = if destination
        .is_some_and(|d| d.account_controls.toggle_write == AccountToggleWriteDto::ProviderSettings)
    {
        Some("provider_settings")
    } else if destination.is_some_and(|d| d.capabilities.external_integration) {
        Some("external_integration")
    } else if account.credential_kind == dashboard_v3::AccountCredentialKind::None {
        Some("no_authentication")
    } else if account.setup_step != dashboard_v3::AccountSetupStep::Ready {
        Some("setup_required")
    } else if selected.is_none() {
        Some("credential_missing")
    } else if selected.is_some_and(|c| c.subject == RuntimeSubjectKind::Anonymous) {
        Some("no_authentication")
    } else if selected.is_some_and(|c| c.credential.material_kind != MaterialKind::ApiKey) {
        Some("unsupported_material")
    } else {
        None
    };
    let writable = reason.is_none();
    let allows_create = |c: &ConnectionSummary| {
        c.credential_create.allowed
            && c.credential_create
                .material_kinds
                .contains(&MaterialKind::ApiKey)
    };
    let create = writable && identity.is_some() && connection.is_some_and(allows_create);
    let unsupported_reason = reason.or_else(|| {
        (connection.is_some_and(|c| {
            c.credential_create.reason
                == Some(CredentialCreateUnavailableReasonDto::DedicatedAccountFlow)
        }))
        .then_some("dedicated_account_flow")
    });
    let share_targets = identity
        .into_iter()
        .flat_map(|i| &i.credentials)
        .filter(|c| {
            c.credential.purpose == CredentialPurpose::Inference
                && c.credential.material_kind == MaterialKind::ApiKey
                && c.subject != RuntimeSubjectKind::Anonymous
        })
        .map(|c| AccountCredentialShareTarget {
            id: c.credential.id.clone(),
            label: s
                .accounts
                .iter()
                .find(|a| a.id == c.legacy.id)
                .map(|a| a.name.clone())
                .filter(|name| !name.is_empty())
                .unwrap_or_else(|| c.legacy.id.clone()),
        })
        .collect();
    let (granted_endpoint_ids, stale_endpoint_ids, stale_origins) =
        saved_grants(binding, connection);
    AccountOperationDetail {
        rotate: writable,
        binding: writable && binding.is_some(),
        create,
        unsupported_reason: unsupported_reason.map(str::to_string),
        credential_id: selected.map(|c| c.credential.id.clone()),
        binding_id: binding.map(|b| b.id.clone()),
        identity_id: identity.map(|i| i.identity.id.clone()),
        allowed_connections: if create {
            s.connections
                .iter()
                .filter(|c| allows_create(c))
                .cloned()
                .collect()
        } else {
            vec![]
        },
        share_targets,
        test_models: test_models(s, destination),
        granted_endpoint_ids,
        stale_endpoint_ids,
        stale_origins,
    }
}

fn saved_grants(
    binding: Option<&BindingDto>,
    connection: Option<&ConnectionSummary>,
) -> (Vec<String>, Vec<String>, Vec<String>) {
    let Some(binding) = binding else {
        return (vec![], vec![], vec![]);
    };
    let endpoints = connection
        .map(|c| c.endpoints.as_slice())
        .unwrap_or_default();
    let origins: HashSet<_> = binding
        .allowed_origins
        .iter()
        .filter_map(|v| normalize_origin(v))
        .collect();
    let current: HashSet<_> = endpoints
        .iter()
        .filter_map(|e| e.url.as_deref().and_then(normalize_origin))
        .collect();
    let granted = endpoints
        .iter()
        .filter(|e| {
            binding.allowed_endpoint_ids.contains(&e.id)
                && e.url
                    .as_deref()
                    .is_none_or(|url| normalize_origin(url).is_some_and(|o| origins.contains(&o)))
        })
        .map(|e| e.id.clone())
        .collect();
    let stale_ids = binding
        .allowed_endpoint_ids
        .iter()
        .filter(|id| !endpoints.iter().any(|e| &e.id == *id))
        .cloned()
        .collect();
    let mut seen = HashSet::new();
    let stale_origins = binding
        .allowed_origins
        .iter()
        .map(|raw| normalize_origin(raw).unwrap_or_else(|| raw.trim().to_string()))
        .filter(|origin| {
            !origin.is_empty() && !current.contains(origin) && seen.insert(origin.clone())
        })
        .collect();
    (granted, stale_ids, stale_origins)
}

fn test_models(
    s: &PageSnapshot,
    destination: Option<&DestinationDto>,
) -> Vec<AccountTestModelChoice> {
    let Some(destination) =
        destination.filter(|d| d.capabilities.testable && !d.capabilities.external_integration)
    else {
        return vec![];
    };
    // The same scope projection supplies builtins, persisted Custom API routes,
    // and dynamic destinations; it already resolves preferred HTTP fallbacks.
    let Some((_, models)) = providers::scope(s, destination) else {
        return vec![];
    };
    let mut seen = HashSet::new();
    let mut choices: Vec<_> = models
        .into_iter()
        .filter(|m| m.routable)
        .filter_map(|m| {
            let id = m.model_id.trim();
            if id.is_empty() || !seen.insert(id.to_lowercase()) {
                return None;
            }
            Some(AccountTestModelChoice {
                model_id: id.into(),
                alias: m.alias.trim().into(),
                protocol: m.preferred_protocol,
            })
        })
        .collect();
    choices.sort_by_cached_key(|m| m.model_id.to_lowercase());
    choices
}

#[cfg(test)]
mod tests;
