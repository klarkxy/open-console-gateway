//! Read-only V4 identity / credential / binding projection.

use axum::Json;
use axum::extract::State;
use chrono::Utc;
use std::collections::HashMap;

use crate::dashboard_v3::{ControlRevision, V3ApiError};
use crate::db::identity::{
    IdentityAccountRecord, IdentityModelSnapshot, PlatformIdentityRecord, cooldown_facts_for,
};
use crate::dynamic::DynamicProviderRuntime;
use crate::models::Account;
use crate::provider::{ConnectionVerificationStatus, builtin_provider};
use crate::redaction::redact_known_secret;
use crate::state::CoreState;
use ocg_domain::catalog::CredentialKind;
use ocg_domain::connection::{
    EndpointOperation, LegacyConnectionKind, connection_id_for_legacy, endpoint_id_for,
};
use ocg_domain::credential::{
    AssignedEndpoint, CredentialPurpose, LegacyAccountFacts, MaterialKind, OnboardingTaskKind,
    OnboardingTaskState, RuntimeSubjectKind, SubscriptionSource, cooldown_windows,
    identity_id_for_platform_account, legacy_account_objects,
    observer_credential_id_for_platform_account,
};
use ocg_domain::ids::CUSTOM_PROVIDER_ID;

use super::types::{
    AuthorityRefDto, BindingDto, CredentialDto, CredentialSummary, DeclaredRelationDto,
    IdentityLegacy, IdentityLegacyKind, IdentityList, IdentitySummary, OnboardingTaskDto,
    QuotaWindowDto, SubscriptionDto, UpstreamAccountDto,
};

pub(super) async fn list_accounts(
    State(state): State<CoreState>,
) -> Result<Json<IdentityList>, V3ApiError> {
    let now = Utc::now();
    let dynamic_providers = state.dynamic_providers();
    let (snapshot, custom_runtimes) = {
        let db = state.db.lock();
        let snapshot = db.list_identity_model().map_err(V3ApiError::internal)?;
        let custom_runtimes = db
            .list_custom_account_runtimes()
            .map_err(V3ApiError::internal)?;
        (snapshot, custom_runtimes)
    };

    let identities =
        project_identities(&state, snapshot, &dynamic_providers, &custom_runtimes, now)?;
    Ok(Json(IdentityList {
        revision: ControlRevision::from_state(&state),
        identities,
    }))
}

fn project_identities(
    state: &CoreState,
    snapshot: IdentityModelSnapshot,
    dynamic_providers: &[DynamicProviderRuntime],
    custom_runtimes: &[crate::custom::CustomAccountRuntime],
    now: chrono::DateTime<Utc>,
) -> Result<Vec<IdentitySummary>, V3ApiError> {
    let custom_by_id: HashMap<&str, &crate::custom::CustomAccountRuntime> = custom_runtimes
        .iter()
        .map(|runtime| (runtime.account_id.as_str(), runtime))
        .collect();
    let dynamic_by_id: HashMap<&str, &DynamicProviderRuntime> = dynamic_providers
        .iter()
        .map(|runtime| (runtime.id.as_str(), runtime))
        .collect();

    let mut identities = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for record in &snapshot.accounts {
        if !seen.insert(record.identity_id.clone()) {
            continue;
        }
        identities.push(project_account_identity(
            state,
            record,
            &dynamic_by_id,
            &custom_by_id,
            now,
        )?);
    }
    for parent in &snapshot.platform_parents {
        if !seen.insert(parent.identity.id.clone()) {
            continue;
        }
        identities.push(project_platform_identity(parent));
    }
    Ok(identities)
}

fn project_account_identity(
    state: &CoreState,
    record: &IdentityAccountRecord,
    dynamic_by_id: &HashMap<&str, &DynamicProviderRuntime>,
    custom_by_id: &HashMap<&str, &crate::custom::CustomAccountRuntime>,
    now: chrono::DateTime<Utc>,
) -> Result<IdentitySummary, V3ApiError> {
    let account = &record.account;
    let (connection_id, endpoints) = assigned_endpoints(account, dynamic_by_id, custom_by_id);
    let facts = LegacyAccountFacts {
        account_id: account.id.clone(),
        name: account.name.clone(),
        notes: account.notes.clone(),
        enabled: account.enabled,
        sort_order: u32::try_from(record.sort_order).unwrap_or(0),
        has_auth_error: account.auth_error.is_some(),
        verified: record.verification_status == ConnectionVerificationStatus::Verified,
        anonymous: account.credential_kind == CredentialKind::None,
        declared_relation: record.declared_relation.clone(),
    };
    let (identity, credential, binding) = legacy_account_objects(facts, &connection_id, &endpoints);
    let subject = if account.credential_kind == CredentialKind::None {
        RuntimeSubjectKind::Anonymous
    } else {
        RuntimeSubjectKind::AccountCredential
    };
    let last_error = redact_last_error(state, account);
    let quota_windows = cooldown_windows(&cooldown_facts_for(account), now)
        .into_iter()
        .map(|window| QuotaWindowDto {
            subject: window.subject,
            subject_ref: window.subject_ref,
            period: window.period,
            blocked_until: window.blocked_until.map(|until| until.to_rfc3339()),
            metric: None,
            relation_confidence: window.relation_confidence,
            policy_mode: window.policy_mode,
        })
        .collect();
    let onboarding_task = record.onboarding.as_ref().map(|task| OnboardingTaskDto {
        id: task.id.clone(),
        kind: OnboardingTaskKind::ManagedRegistration,
        step: task.step.clone(),
        state: if task.state == "completed" {
            OnboardingTaskState::Completed
        } else {
            OnboardingTaskState::InProgress
        },
    });
    let subscription = record.subscription.as_ref().map(|row| SubscriptionDto {
        source: if row.source == SubscriptionSource::ManagedPayment.as_str() {
            SubscriptionSource::ManagedPayment
        } else {
            SubscriptionSource::LegacyManual
        },
        purchase_date: row.purchase_date.clone(),
        expires_on: row.expires_on.clone(),
    });
    let material_kind = if account.credential_kind == CredentialKind::None {
        MaterialKind::ExternalReference
    } else {
        MaterialKind::ApiKey
    };
    let bindings = vec![BindingDto {
        id: record.binding_id.clone(),
        connection_id: binding.connection_id.to_string(),
        allowed_endpoint_ids: binding.allowed_endpoint_ids,
        allowed_origins: binding.allowed_origins,
        model_scope: binding.model_scope,
        enabled: binding.enabled,
        routing_rank: binding.routing_rank,
    }];
    let declared_relations = record
        .declared_relation
        .as_ref()
        .map(|relation| {
            vec![DeclaredRelationDto {
                platform_account_id: relation.platform_account_id.clone(),
                group: relation.group.clone(),
            }]
        })
        .unwrap_or_default();
    Ok(IdentitySummary {
        identity: UpstreamAccountDto {
            id: identity.id.to_string(),
            label: identity.label,
            authority_ref: identity.authority_ref.map(|authority| AuthorityRefDto {
                issuer_or_site: authority.issuer_or_site,
                tenant_or_subject: authority.tenant_or_subject,
            }),
            identity_confidence: identity.identity_confidence,
            enabled: identity.enabled,
            notes: identity.notes,
        },
        credentials: vec![CredentialSummary {
            credential: CredentialDto {
                id: credential.id.to_string(),
                purpose: credential.purpose,
                material_kind,
                secret_ref: credential.secret_ref,
                has_material: record.has_key_material,
                version: record.credential_version,
                enabled: credential.enabled,
                auth_state: credential.auth_state,
                auth_state_version: record.auth_state_version,
                expires_at: None,
            },
            subject,
            bindings,
            quota_windows,
            onboarding_task,
            subscription,
            last_error,
            legacy: IdentityLegacy {
                kind: IdentityLegacyKind::Account,
                id: account.id.clone(),
            },
        }],
        declared_relations,
        legacy: IdentityLegacy {
            kind: IdentityLegacyKind::Account,
            id: account.id.clone(),
        },
    })
}

fn project_platform_identity(parent: &PlatformIdentityRecord) -> IdentitySummary {
    let identity_id = identity_id_for_platform_account(&parent.platform_id);
    let credential_id = observer_credential_id_for_platform_account(&parent.platform_id);
    IdentitySummary {
        identity: UpstreamAccountDto {
            id: identity_id.to_string(),
            label: parent.name.clone(),
            authority_ref: Some(AuthorityRefDto {
                issuer_or_site: parent.base_url.clone(),
                tenant_or_subject: None,
            }),
            identity_confidence: ocg_domain::credential::IdentityConfidence::Declared,
            enabled: parent.identity.enabled,
            notes: parent.identity.notes.clone(),
        },
        credentials: vec![CredentialSummary {
            credential: CredentialDto {
                id: credential_id.to_string(),
                purpose: CredentialPurpose::PlatformObserver,
                material_kind: if parent.has_credential {
                    MaterialKind::ApiKey
                } else {
                    MaterialKind::ExternalReference
                },
                secret_ref: format!("platform:{}", parent.platform_id),
                has_material: parent.has_credential,
                version: 1,
                enabled: true,
                auth_state: ocg_domain::credential::AuthState::Unknown,
                auth_state_version: 1,
                expires_at: None,
            },
            subject: RuntimeSubjectKind::AccountCredential,
            bindings: Vec::new(),
            quota_windows: Vec::new(),
            onboarding_task: None,
            subscription: None,
            last_error: None,
            legacy: IdentityLegacy {
                kind: IdentityLegacyKind::PlatformAccount,
                id: parent.platform_id.clone(),
            },
        }],
        declared_relations: Vec::new(),
        legacy: IdentityLegacy {
            kind: IdentityLegacyKind::PlatformAccount,
            id: parent.platform_id.clone(),
        },
    }
}

fn assigned_endpoints(
    account: &Account,
    dynamic_by_id: &HashMap<&str, &DynamicProviderRuntime>,
    custom_by_id: &HashMap<&str, &crate::custom::CustomAccountRuntime>,
) -> (ocg_domain::connection::ConnectionId, Vec<AssignedEndpoint>) {
    if account.provider_id == CUSTOM_PROVIDER_ID {
        let connection_id =
            connection_id_for_legacy(LegacyConnectionKind::CustomAccount, &account.id);
        let Some(runtime) = custom_by_id.get(account.id.as_str()) else {
            return (connection_id, Vec::new());
        };
        let operation = EndpointOperation::from(runtime.config.upstream_protocol);
        let id = endpoint_id_for(&connection_id, operation);
        return (
            connection_id,
            vec![AssignedEndpoint {
                id: id.to_string(),
                url: Some(runtime.config.endpoint_url.clone()),
            }],
        );
    }
    if let Some(plan) = builtin_provider(&account.provider_id) {
        let connection_id =
            connection_id_for_legacy(LegacyConnectionKind::BuiltinProvider, plan.provider_id);
        let endpoints = plan
            .upstream_protocols
            .iter()
            .copied()
            .map(|protocol| {
                let operation = EndpointOperation::from(protocol);
                AssignedEndpoint {
                    id: endpoint_id_for(&connection_id, operation).to_string(),
                    url: None,
                }
            })
            .collect();
        return (connection_id, endpoints);
    }
    if let Some(runtime) = dynamic_by_id.get(account.provider_id.as_str()) {
        let connection_id =
            connection_id_for_legacy(LegacyConnectionKind::DynamicProvider, &runtime.id);
        let operation = EndpointOperation::from(runtime.upstream_protocol);
        return (
            connection_id.clone(),
            vec![AssignedEndpoint {
                id: endpoint_id_for(&connection_id, operation).to_string(),
                url: Some(runtime.endpoint_url.clone()),
            }],
        );
    }
    let connection_id =
        connection_id_for_legacy(LegacyConnectionKind::DynamicProvider, &account.provider_id);
    (connection_id, Vec::new())
}

fn redact_last_error(state: &CoreState, account: &Account) -> Option<String> {
    let error = account.last_error.as_ref()?;
    let secret = if account.key_cipher.is_empty() {
        String::new()
    } else {
        state.decrypt_key(&account.key_cipher).ok()?
    };
    Some(redact_known_secret(error, &secret))
}
