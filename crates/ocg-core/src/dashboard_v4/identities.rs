//! V4 identity / credential / binding projection and second-credential writes.

use axum::Json;
use axum::body::Bytes;
use axum::extract::{Path, State};
use chrono::Utc;
use hmac::{Hmac, Mac};
use serde::Serialize;
use sha2::Sha256;
use std::collections::HashMap;

use crate::dashboard_v3::dynamic_providers::first_account_key;
use crate::dashboard_v3::{ControlRevision, V3ApiError, check_expectation, parse_mutation_json};
use crate::db::identity::{
    IdentityAccountRecord, IdentityModelSnapshot, PlatformIdentityRecord, QuotaSharingJoin,
    StoredInferenceBinding, cooldown_facts_for,
};
use crate::dynamic::DynamicProviderRuntime;
use crate::models::{Account, AccountSetupStep, AccountType, local_today};
use crate::provider::{
    CPA_PROVIDER_ID, ConnectionVerificationStatus, CreationAvailability, ProviderRegistry,
    builtin_provider, validate_plan_key,
};
use crate::redaction::redact_known_secret;
use crate::state::CoreState;
use ocg_domain::catalog::CredentialKind;
use ocg_domain::connection::{
    EndpointOperation, LegacyConnectionKind, connection_id_for_legacy, endpoint_id_for,
};
use ocg_domain::credential::{
    AssignedEndpoint, CredentialPurpose, LegacyAccountFacts, MaterialKind, OnboardingTaskKind,
    OnboardingTaskState, QuotaPolicyMode, RelationConfidence, RouteSpec, RuntimeSubjectKind,
    SubscriptionSource, assigned_endpoints_for_routes, cooldown_windows,
    identity_id_for_platform_account, legacy_account_objects, normalize_origin,
    observer_credential_id_for_platform_account,
};
use ocg_domain::dynamic::DynamicAuthKind;
use ocg_domain::ids::CUSTOM_PROVIDER_ID;
use ocg_domain::provider::ProviderOrigin;

use super::types::{
    AuthorityRefDto, BindingDto, CredentialDto, CredentialSummary, DeclaredRelationDto,
    IdentityCredentialCreateRequest, IdentityCredentialCreateResult, IdentityLegacy,
    IdentityLegacyKind, IdentityList, IdentitySummary, OnboardingTaskDto, QuotaSharing,
    QuotaWindowDto, SubscriptionDto, UpstreamAccountDto,
};

const DIGEST_KEY_SETTING: &str = "dashboard_operation_digest_key";
type HmacSha256 = Hmac<Sha256>;

pub(super) async fn list_accounts(
    State(state): State<CoreState>,
) -> Result<Json<IdentityList>, V3ApiError> {
    let _settings_update = state.settings_update.lock();
    let now = Utc::now();
    let (snapshot, custom_runtimes, dynamic_providers) = {
        let db = state.db.lock();
        let snapshot = db.list_identity_model().map_err(V3ApiError::internal)?;
        let custom_runtimes = db
            .list_custom_account_runtimes()
            .map_err(V3ApiError::internal)?;
        let dynamic_providers = db
            .list_control_plane_dynamic_providers()
            .map_err(V3ApiError::internal)?;
        (snapshot, custom_runtimes, dynamic_providers)
    };

    let identities =
        project_identities(&state, snapshot, &dynamic_providers, &custom_runtimes, now)?;
    Ok(Json(IdentityList {
        revision: ControlRevision::from_state(&state),
        identities,
    }))
}

pub(super) async fn create_credential(
    State(state): State<CoreState>,
    Path(id): Path<String>,
    body: Bytes,
) -> Result<Json<IdentityCredentialCreateResult>, V3ApiError> {
    let input = parse_mutation_json::<IdentityCredentialCreateRequest>(&body)?;
    create_credential_locked(&state, &id, input).map(Json)
}

fn create_credential_locked(
    state: &CoreState,
    identity_id: &str,
    input: IdentityCredentialCreateRequest,
) -> Result<IdentityCredentialCreateResult, V3ApiError> {
    let _settings_update = state.settings_update.lock();
    if let Some(operation_id) = input.operation_id.as_deref() {
        if uuid::Uuid::parse_str(operation_id).is_err() {
            return Err(V3ApiError::invalid_request_at(
                state,
                "operationId must be a UUID",
            ));
        }
        let digest = credential_create_digest(state, identity_id, &input)?;
        if let Some(existing) = {
            let db = state.db.lock();
            db.find_dashboard_operation(operation_id)
                .map_err(V3ApiError::internal)?
        } {
            if existing.payload_digest != digest {
                return Err(V3ApiError::operation_payload_mismatch(
                    state,
                    "operationId was reused with a different payload",
                ));
            }
            return replay_stored_credential(state, &existing.result_json);
        }
    }

    check_expectation(state, &input.expectation)?;

    let secret = input.secret_input.trim();
    if secret.is_empty() {
        return Err(V3ApiError::invalid_request_at(
            state,
            "secretInput is required",
        ));
    }

    let snapshot = {
        let db = state.db.lock();
        db.list_identity_model().map_err(V3ApiError::internal)?
    };
    if snapshot
        .platform_parents
        .iter()
        .any(|parent| parent.identity.id == identity_id)
    {
        return Err(V3ApiError::invalid_request_at(
            state,
            "platform observer identities cannot receive inference credentials here",
        ));
    }
    let existing = snapshot
        .accounts
        .iter()
        .find(|record| record.identity_id == identity_id)
        .ok_or_else(|| V3ApiError::not_found_at(state, "identity not found"))?;

    let target = resolve_connection_target(state, &snapshot, &input.connection_id)?;
    let quota_sharing = resolve_quota_sharing(state, &snapshot, identity_id, &input.quota_sharing)?;
    let key_cipher = encrypt_connection_secret(state, &target, secret)?;
    let now = Utc::now();
    let account_id = uuid::Uuid::new_v4().to_string();
    let label = input
        .account_label
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or(existing.account.name.as_str());
    let account = Account {
        id: account_id.clone(),
        provider_id: target.provider_id.clone(),
        credential_kind: target.credential_kind,
        quota_scope: target.quota_scope,
        name: label.to_string(),
        username: None,
        password_cipher: None,
        key_cipher,
        enabled: target.enabled,
        account_type: AccountType::Key,
        setup_step: AccountSetupStep::Ready,
        referral_code: None,
        purchase_date: String::new(),
        expires_on: String::new(),
        cooldown_until: None,
        cooldown_generic_until: None,
        cooldown_5h_until: None,
        cooldown_week_until: None,
        cooldown_month_until: None,
        cooldown_free_until: None,
        last_error: None,
        auth_error: None,
        notes: None,
        created_at: now,
        updated_at: now,
    };
    let digest = if input.operation_id.is_some() {
        Some(credential_create_digest(state, identity_id, &input)?)
    } else {
        None
    };
    let created = {
        let db = state.db.lock();
        let operation = match (input.operation_id.as_deref(), digest.as_deref()) {
            (Some(operation_id), Some(digest)) => Some((operation_id, digest)),
            _ => None,
        };
        db.create_account_for_identity(
            identity_id,
            &account,
            &local_today(),
            target.verification_status,
            quota_sharing,
            operation,
        )
        .map_err(|error| V3ApiError::invalid_request_at(state, error.to_string()))?
    };
    state.bump_settings_revision();
    Ok(IdentityCredentialCreateResult {
        revision: ControlRevision::from_state(state),
        identity_id: created.identity_id,
        credential_id: created.credential_id,
        binding_id: created.binding_id,
        account_id: created.account_id,
        connection_id: target.connection_id,
        version: created.version,
        auth_state_version: created.auth_state_version,
        replayed: false,
    })
}

struct ConnectionTarget {
    connection_id: String,
    provider_id: String,
    credential_kind: CredentialKind,
    quota_scope: ocg_domain::catalog::QuotaScope,
    enabled: bool,
    verification_status: ConnectionVerificationStatus,
    auth_kind: Option<DynamicAuthKind>,
}

fn resolve_connection_target(
    state: &CoreState,
    snapshot: &IdentityModelSnapshot,
    connection_id: &str,
) -> Result<ConnectionTarget, V3ApiError> {
    use crate::provider::{BUILTIN_PROVIDERS, default_verification_status};

    for plan in BUILTIN_PROVIDERS {
        let id = connection_id_for_legacy(LegacyConnectionKind::BuiltinProvider, plan.provider_id);
        if id.as_str() != connection_id {
            continue;
        }
        if plan.product_surface.is_external_integration()
            || plan.provider_id == CPA_PROVIDER_ID
            || super::templates::is_cpa_id(plan.provider_id)
        {
            return Err(V3ApiError::invalid_request_at(
                state,
                "CPA Subscription Pool settings must use the external-integration endpoint",
            ));
        }
        if plan.creation_availability == CreationAvailability::Unavailable
            || plan.singleton_account_id.is_some()
        {
            return Err(V3ApiError::invalid_request_at(
                state,
                plan.creation_unavailable_reason
                    .unwrap_or("this Plan cannot receive another Key through this endpoint")
                    .to_string(),
            ));
        }
        if plan.credential_kind == CredentialKind::None {
            return Err(V3ApiError::invalid_request_at(
                state,
                "anonymous and no-auth credentials cannot be created",
            ));
        }
        if plan.provider_id == CUSTOM_PROVIDER_ID {
            return Err(V3ApiError::invalid_request_at(
                state,
                "Custom API connections require the dedicated account endpoint",
            ));
        }
        let enable_requires_verification = ProviderRegistry::get(plan.provider_id)
            .is_some_and(|descriptor| descriptor.card_actions.enable_requires_verification);
        let enabled = crate::provider::provider_allows_enablement(plan.provider_id)
            && !enable_requires_verification;
        return Ok(ConnectionTarget {
            connection_id: connection_id.to_string(),
            provider_id: plan.provider_id.to_string(),
            credential_kind: plan.credential_kind,
            quota_scope: plan.quota_scope,
            enabled,
            verification_status: default_verification_status(plan),
            auth_kind: None,
        });
    }

    let dynamic_providers = {
        let db = state.db.lock();
        db.list_control_plane_dynamic_providers()
            .map_err(V3ApiError::internal)?
    };
    if let Some(runtime) = dynamic_providers.iter().find(|runtime| {
        connection_id_for_legacy(LegacyConnectionKind::DynamicProvider, &runtime.id).as_str()
            == connection_id
    }) {
        let runtime = runtime.clone();
        if runtime.auth_kind.is_singleton() || !runtime.auth_kind.requires_key() {
            return Err(V3ApiError::invalid_request_at(
                state,
                "anonymous and no-auth credentials cannot be created",
            ));
        }
        if runtime.origin == ProviderOrigin::Builtin {
            return Err(V3ApiError::invalid_request_at(
                state,
                "builtin provider definitions cannot receive Keys here",
            ));
        }
        return Ok(ConnectionTarget {
            connection_id: connection_id.to_string(),
            provider_id: runtime.id,
            credential_kind: runtime.auth_kind.credential_kind(),
            quota_scope: runtime.auth_kind.quota_scope(),
            enabled: true,
            verification_status: ConnectionVerificationStatus::NotRequired,
            auth_kind: Some(runtime.auth_kind),
        });
    }

    if snapshot.accounts.iter().any(|record| {
        record.account.provider_id == CUSTOM_PROVIDER_ID
            && connection_id_for_legacy(LegacyConnectionKind::CustomAccount, &record.account.id)
                .as_str()
                == connection_id
    }) {
        return Err(V3ApiError::invalid_request_at(
            state,
            "Custom API connections require the dedicated account endpoint",
        ));
    }

    Err(V3ApiError::not_found_at(state, "connection not found"))
}

fn encrypt_connection_secret(
    state: &CoreState,
    target: &ConnectionTarget,
    secret: &str,
) -> Result<String, V3ApiError> {
    if let Some(auth_kind) = target.auth_kind {
        return first_account_key(state, auth_kind, Some(secret));
    }
    if let Some(plan) = builtin_provider(&target.provider_id) {
        validate_plan_key(plan, secret)
            .map_err(|error| V3ApiError::invalid_request_at(state, error.to_string()))?;
    }
    state.encrypt_key(secret).map_err(V3ApiError::internal)
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
        let records: Vec<_> = snapshot
            .accounts
            .iter()
            .filter(|candidate| candidate.identity_id == record.identity_id)
            .collect();
        identities.push(project_account_identity(
            state,
            &records,
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
    records: &[&IdentityAccountRecord],
    dynamic_by_id: &HashMap<&str, &DynamicProviderRuntime>,
    custom_by_id: &HashMap<&str, &crate::custom::CustomAccountRuntime>,
    now: chrono::DateTime<Utc>,
) -> Result<IdentitySummary, V3ApiError> {
    let primary = records
        .first()
        .copied()
        .ok_or_else(|| V3ApiError::internal("identity has no credentials"))?;
    let mut credentials = Vec::new();
    let mut declared_relations = Vec::new();
    let mut seen_relations = std::collections::HashSet::new();
    for record in records {
        credentials.push(project_credential(
            state,
            record,
            dynamic_by_id,
            custom_by_id,
            now,
        )?);
        if let Some(relation) = &record.declared_relation {
            let key = (relation.platform_account_id.clone(), relation.group.clone());
            if seen_relations.insert(key) {
                declared_relations.push(DeclaredRelationDto {
                    platform_account_id: relation.platform_account_id.clone(),
                    group: relation.group.clone(),
                });
            }
        }
    }
    let account = &primary.account;
    let (connection_id, endpoints) = assigned_endpoints(account, dynamic_by_id, custom_by_id);
    let facts = LegacyAccountFacts {
        account_id: account.id.clone(),
        name: account.name.clone(),
        notes: account.notes.clone(),
        enabled: account.enabled,
        sort_order: u32::try_from(primary.sort_order).unwrap_or(0),
        has_auth_error: account.auth_error.is_some(),
        verified: primary.verification_status == ConnectionVerificationStatus::Verified,
        anonymous: account.credential_kind == CredentialKind::None,
        declared_relation: primary.declared_relation.clone(),
    };
    let (identity, _, _) = legacy_account_objects(facts, &connection_id, &endpoints);
    Ok(IdentitySummary {
        identity: UpstreamAccountDto {
            id: primary.identity_id.clone(),
            label: identity.label,
            authority_ref: identity.authority_ref.map(|authority| AuthorityRefDto {
                issuer_or_site: authority.issuer_or_site,
                tenant_or_subject: authority.tenant_or_subject,
            }),
            identity_confidence: identity.identity_confidence,
            enabled: identity.enabled,
            notes: identity.notes,
        },
        credentials,
        declared_relations,
        legacy: IdentityLegacy {
            kind: IdentityLegacyKind::Account,
            id: account.id.clone(),
        },
    })
}

fn project_credential(
    state: &CoreState,
    record: &IdentityAccountRecord,
    dynamic_by_id: &HashMap<&str, &DynamicProviderRuntime>,
    custom_by_id: &HashMap<&str, &crate::custom::CustomAccountRuntime>,
    now: chrono::DateTime<Utc>,
) -> Result<CredentialSummary, V3ApiError> {
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
    let (_, credential, binding) = legacy_account_objects(facts, &connection_id, &endpoints);
    let subject = if account.credential_kind == CredentialKind::None {
        RuntimeSubjectKind::Anonymous
    } else {
        RuntimeSubjectKind::AccountCredential
    };
    let last_error = redact_last_error(state, account);
    let pool_confidence =
        record
            .quota_relation_confidence
            .as_deref()
            .and_then(|value| match value {
                "declared" => Some(RelationConfidence::Declared),
                "unknown" => Some(RelationConfidence::Unknown),
                _ => None,
            });
    let pool_policy = record
        .quota_policy_mode
        .as_deref()
        .and_then(|value| match value {
            "authoritative_limit" => Some(QuotaPolicyMode::AuthoritativeLimit),
            "observe_only" => Some(QuotaPolicyMode::ObserveOnly),
            _ => None,
        });
    let quota_windows = cooldown_windows(&cooldown_facts_for(account, &record.credential_id), now)
        .into_iter()
        .map(|window| {
            let (relation_confidence, policy_mode) =
                if window.subject == ocg_domain::credential::QuotaSubject::Egress {
                    (window.relation_confidence, window.policy_mode)
                } else {
                    (
                        pool_confidence.unwrap_or(window.relation_confidence),
                        pool_policy.unwrap_or(window.policy_mode),
                    )
                };
            QuotaWindowDto {
                subject: window.subject,
                subject_ref: window.subject_ref,
                period: window.period,
                blocked_until: window.blocked_until.map(|until| until.to_rfc3339()),
                metric: None,
                relation_confidence,
                policy_mode,
            }
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
    Ok(CredentialSummary {
        credential: CredentialDto {
            id: record.credential_id.clone(),
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
        bindings: vec![BindingDto {
            id: record.binding_id.clone(),
            connection_id: binding.connection_id.to_string(),
            allowed_endpoint_ids: record.allowed_endpoint_ids.clone(),
            allowed_origins: record.allowed_origins.clone(),
            model_scope: record.binding_model_scope.clone(),
            enabled: record.binding_enabled,
            routing_rank: binding.routing_rank,
        }],
        quota_windows,
        quota_pool_id: record.quota_pool_id.clone(),
        onboarding_task,
        subscription,
        last_error,
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
            quota_pool_id: None,
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

pub(super) fn project_binding_dto(
    record: &IdentityAccountRecord,
    connection_id: &ocg_domain::connection::ConnectionId,
    endpoints: &[AssignedEndpoint],
    stored: &StoredInferenceBinding,
) -> BindingDto {
    let facts = LegacyAccountFacts {
        account_id: record.account.id.clone(),
        name: record.account.name.clone(),
        notes: record.account.notes.clone(),
        enabled: record.account.enabled,
        sort_order: u32::try_from(record.sort_order).unwrap_or(0),
        has_auth_error: record.account.auth_error.is_some(),
        verified: record.verification_status == ConnectionVerificationStatus::Verified,
        anonymous: record.account.credential_kind == CredentialKind::None,
        declared_relation: record.declared_relation.clone(),
    };
    let (_, _, binding) = legacy_account_objects(facts, connection_id, endpoints);
    BindingDto {
        id: stored.binding_id.clone(),
        connection_id: connection_id.to_string(),
        allowed_endpoint_ids: stored.allowed_endpoint_ids.clone(),
        allowed_origins: stored.allowed_origins.clone(),
        model_scope: stored.model_scope.clone(),
        enabled: stored.enabled,
        routing_rank: binding.routing_rank,
    }
}

pub(super) fn assigned_endpoints(
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
        let mut routes = vec![RouteSpec {
            operation: EndpointOperation::from(runtime.upstream_protocol),
            url: Some(runtime.endpoint_url.clone()),
        }];
        let mut seen = std::collections::HashSet::from([(
            runtime.upstream_protocol,
            runtime.endpoint_url.clone(),
        )]);
        for mapping in &runtime.mappings {
            let Some(override_route) = &mapping.upstream_override else {
                continue;
            };
            if !seen.insert((override_route.protocol, override_route.endpoint_url.clone())) {
                continue;
            }
            routes.push(RouteSpec {
                operation: EndpointOperation::from(override_route.protocol),
                url: Some(override_route.endpoint_url.clone()),
            });
        }
        return (
            connection_id.clone(),
            assigned_endpoints_for_routes(&connection_id, &routes),
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

fn resolve_quota_sharing(
    state: &CoreState,
    snapshot: &IdentityModelSnapshot,
    identity_id: &str,
    sharing: &QuotaSharing,
) -> Result<QuotaSharingJoin, V3ApiError> {
    match sharing {
        QuotaSharing::Independent => Ok(QuotaSharingJoin::Independent),
        QuotaSharing::Shared { credential_id } => {
            let source = snapshot.accounts.iter().find(|record| {
                record.identity_id == identity_id && record.credential_id == *credential_id
            });
            let Some(source) = source else {
                return Err(V3ApiError::invalid_request_at(
                    state,
                    "quota sharing requires an inference credential on the same identity",
                ));
            };
            if source.account.credential_kind == CredentialKind::None {
                return Err(V3ApiError::invalid_request_at(
                    state,
                    "anonymous and no-auth credentials cannot share quota",
                ));
            }
            Ok(QuotaSharingJoin::Shared {
                source_credential_id: credential_id.clone(),
            })
        }
    }
}

#[derive(Debug, Clone, Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct StoredCredentialCreateResult {
    identity_id: String,
    credential_id: String,
    binding_id: String,
    account_id: String,
    connection_id: String,
    version: u64,
    auth_state_version: u64,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct CredentialCreateDigestPayload<'a> {
    operation_id: &'a str,
    identity_id: &'a str,
    connection_id: &'a str,
    secret_input: &'a str,
    quota_sharing: &'a QuotaSharing,
    account_label: &'a Option<String>,
}

fn credential_create_digest(
    state: &CoreState,
    identity_id: &str,
    input: &IdentityCredentialCreateRequest,
) -> Result<String, V3ApiError> {
    let operation_id = input
        .operation_id
        .as_deref()
        .ok_or_else(|| V3ApiError::internal("operationId is required to digest"))?;
    let canonical = serde_json::to_vec(&CredentialCreateDigestPayload {
        operation_id,
        identity_id,
        connection_id: &input.connection_id,
        secret_input: &input.secret_input,
        quota_sharing: &input.quota_sharing,
        account_label: &input.account_label,
    })
    .map_err(V3ApiError::internal)?;
    let key = digest_key(state)?;
    let mut mac = HmacSha256::new_from_slice(&key).map_err(V3ApiError::internal)?;
    mac.update(&canonical);
    Ok(hex::encode(mac.finalize().into_bytes()))
}

fn digest_key(state: &CoreState) -> Result<[u8; 32], V3ApiError> {
    let db = state.db.lock();
    if let Some(existing) = db
        .get_setting(DIGEST_KEY_SETTING)
        .map_err(V3ApiError::internal)?
    {
        return parse_digest_key(&existing);
    }
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes).map_err(V3ApiError::internal)?;
    db.set_setting(DIGEST_KEY_SETTING, &hex::encode(bytes))
        .map_err(V3ApiError::internal)?;
    Ok(bytes)
}

fn parse_digest_key(value: &str) -> Result<[u8; 32], V3ApiError> {
    let decoded = hex::decode(value).map_err(V3ApiError::internal)?;
    decoded
        .try_into()
        .map_err(|_| V3ApiError::internal("dashboard operation digest key is not 32 bytes"))
}

fn replay_stored_credential(
    state: &CoreState,
    result_json: &str,
) -> Result<IdentityCredentialCreateResult, V3ApiError> {
    let stored: StoredCredentialCreateResult =
        serde_json::from_str(result_json).map_err(V3ApiError::internal)?;
    Ok(IdentityCredentialCreateResult {
        revision: ControlRevision::from_state(state),
        identity_id: stored.identity_id,
        credential_id: stored.credential_id,
        binding_id: stored.binding_id,
        account_id: stored.account_id,
        connection_id: stored.connection_id,
        version: stored.version,
        auth_state_version: stored.auth_state_version,
        replayed: true,
    })
}

pub(super) fn validate_binding_grants(
    endpoints: &[AssignedEndpoint],
    allowed_endpoint_ids: &[String],
    allowed_origins: &[String],
) -> Result<(Vec<String>, Vec<String>), String> {
    let configured_ids: std::collections::HashSet<&str> = endpoints
        .iter()
        .map(|endpoint| endpoint.id.as_str())
        .collect();
    let mut ids = Vec::new();
    let mut seen_ids = std::collections::HashSet::new();
    for id in allowed_endpoint_ids {
        let id = id.trim();
        if id.is_empty() || !seen_ids.insert(id) || !configured_ids.contains(id) {
            return Err("allowedEndpointIds contains a foreign, empty, or duplicate id".into());
        }
        ids.push(id.to_string());
    }
    let configured_origins: std::collections::HashSet<String> = endpoints
        .iter()
        .filter_map(|endpoint| endpoint.url.as_deref().and_then(normalize_origin))
        .collect();
    let mut origins = Vec::new();
    let mut seen_origins = std::collections::HashSet::new();
    for origin in allowed_origins {
        let Some(normalized) = normalize_origin(origin) else {
            return Err("allowedOrigins contains a malformed origin".into());
        };
        if !seen_origins.insert(normalized.clone()) || !configured_origins.contains(&normalized) {
            return Err(
                "allowedOrigins contains a nonconfigured, duplicate, or malformed origin".into(),
            );
        }
        origins.push(normalized);
    }
    let granted_origins: std::collections::HashSet<String> = endpoints
        .iter()
        .filter(|endpoint| ids.iter().any(|id| id == &endpoint.id))
        .filter_map(|endpoint| endpoint.url.as_deref().and_then(normalize_origin))
        .collect();
    if granted_origins != seen_origins {
        return Err(
            "allowedOrigins must match the origins of the granted configured endpoints".into(),
        );
    }
    Ok((ids, origins))
}
