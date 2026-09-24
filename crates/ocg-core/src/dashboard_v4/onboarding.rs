//! POST `/onboarding/commit` — idempotent CAS write for a user-defined
//! Provider connection and/or its first extra Key.

use axum::Json;
use axum::body::Bytes;
use axum::extract::State;
use chrono::Utc;
use hmac::{Hmac, Mac};
use ocg_domain::connection::{LegacyConnectionKind, connection_id_for_legacy, target_id_for};
use ocg_domain::credential::{
    assigned_endpoints_for_routes, credential_id_for_legacy_account, safe_default_grants,
};
use ocg_domain::destination::{AuthScheme, LegacyDestinationRef};
use ocg_domain::dynamic::DynamicAuthKind;
use ocg_domain::ids::CUSTOM_PROVIDER_ID;
use sha2::Sha256;
use std::collections::HashSet;

use crate::dashboard_v3::dynamic_providers::{
    first_account_key, runtime_from_definition, validate_draft_wire_definition,
    validate_wire_definition,
};
use crate::dashboard_v3::{
    ControlRevision, ProviderDefinitionModel, ProviderModelUpstreamOverride, V3ApiError,
    check_expectation, parse_mutation_json,
};
use crate::db::NewDashboardOperation;
use crate::dynamic::{DynamicProviderRuntime, collides_with_known_id, normalize_preset_id};
use crate::models::{
    Account as ModelAccount, AccountType as ModelAccountType, NEW_READY_KEY_ACCOUNT_ENABLED,
    normalize_account_notes,
};
use crate::provider::BUILTIN_PROVIDERS;
use crate::state::CoreState;
use serde::Serialize;

use super::types::{
    OnboardingAuthorization, OnboardingCommitMode, OnboardingCommitRequest, OnboardingCommitResult,
    OnboardingConnection, OnboardingConnectionExisting, OnboardingConnectionNew, OnboardingTarget,
    StoredOnboardingCommitResult,
};

const DIGEST_KEY_SETTING: &str = "dashboard_operation_digest_key";
const OPERATION_KIND: &str = "onboarding_commit";
const CUSTOM_HTTP_TEMPLATE: &str = "custom-http";
const NON_DYNAMIC_DRAFT_MESSAGE: &str =
    "only configurable HTTP draft connections can resume explicit onboarding";

type HmacSha256 = Hmac<Sha256>;

pub(super) async fn commit(
    State(state): State<CoreState>,
    body: Bytes,
) -> Result<Json<OnboardingCommitResult>, V3ApiError> {
    let input = parse_mutation_json::<OnboardingCommitRequest>(&body)?;
    if uuid::Uuid::parse_str(&input.operation_id).is_err() {
        return Err(V3ApiError::invalid_request_at(
            &state,
            "operationId must be a UUID",
        ));
    }
    commit_locked(&state, input).map(Json)
}

pub(crate) fn commit_locked(
    state: &CoreState,
    input: OnboardingCommitRequest,
) -> Result<OnboardingCommitResult, V3ApiError> {
    let _settings_update = state.settings_update.lock();
    let digest = payload_digest(state, &input)?;
    if let Some(existing) = {
        let db = state.db.lock();
        db.find_dashboard_operation(&input.operation_id)
            .map_err(V3ApiError::internal)?
    } {
        if existing.payload_digest != digest {
            return Err(V3ApiError::operation_payload_mismatch(
                state,
                "operationId was reused with a different payload",
            ));
        }
        return replay_stored(state, &existing.result_json);
    }

    check_expectation(state, &input.expectation)?;
    match input.connection {
        OnboardingConnection::New(connection) => commit_new(
            state,
            &input.operation_id,
            &digest,
            connection,
            input.authorization,
            input.targets,
            input.mode,
            input.authorize_current_endpoint,
        ),
        OnboardingConnection::Existing(connection) => commit_existing(
            state,
            &input.operation_id,
            &digest,
            connection,
            input.authorization,
            input.targets,
            input.mode,
            input.authorize_current_endpoint,
        ),
    }
}

/// CAS commit keeps connection, authorization, targets, and mode as separate facts.
#[allow(clippy::too_many_arguments)]
fn commit_new(
    state: &CoreState,
    operation_id: &str,
    digest: &str,
    connection: OnboardingConnectionNew,
    authorization: Option<OnboardingAuthorization>,
    targets: Vec<OnboardingTarget>,
    mode: Option<OnboardingCommitMode>,
    authorize_current_endpoint: bool,
) -> Result<OnboardingCommitResult, V3ApiError> {
    reject_legacy_authorize_flag(state, mode, authorize_current_endpoint)?;
    let draft = mode == Some(OnboardingCommitMode::Draft);
    if targets.is_empty() && !draft {
        return Err(V3ApiError::invalid_request_at(
            state,
            "new connections require at least one target",
        ));
    }
    let auth_kind = DynamicAuthKind::from(connection.auth_kind);
    let protocol_routes = connection.protocol_routes.as_ref().map(|routes| {
        routes
            .iter()
            .map(ocg_domain::destination::HttpProtocolRoute::from)
            .collect::<Vec<_>>()
    });
    let now = Utc::now();
    let models = to_definition_models(targets);
    let mut definition = if draft {
        validate_draft_wire_definition(
            uuid::Uuid::new_v4().to_string(),
            connection.name,
            connection.endpoint_url,
            connection.upstream_protocol,
            auth_kind,
            models,
        )
    } else {
        validate_wire_definition(
            uuid::Uuid::new_v4().to_string(),
            connection.name,
            connection.endpoint_url,
            connection.upstream_protocol,
            auth_kind,
            models,
        )
    }
    .map_err(|error| V3ApiError::invalid_request_at(state, error.to_string()))?;
    definition.preset_id = preset_from_template_id(state, &connection.template_id)?;
    let existing = {
        let db = state.db.lock();
        db.list_control_plane_dynamic_providers()
            .map_err(V3ApiError::internal)?
    };
    if collides_with_known_id(&definition.id, &existing) {
        return Err(V3ApiError::conflict_at(
            state,
            "generated provider id collided; retry",
        ));
    }

    let first_account = first_account_for_new(
        state,
        auth_kind,
        &definition.id,
        &definition.name,
        &authorization,
        now,
    )?;
    if mode == Some(OnboardingCommitMode::Complete)
        && auth_kind.requires_key()
        && first_account.is_none()
    {
        return Err(V3ApiError::invalid_request_at(
            state,
            "complete requires an api_key authorization",
        ));
    }
    let runtime = runtime_from_definition(definition, now, now);
    finish_new(
        state,
        operation_id,
        digest,
        runtime,
        first_account,
        draft,
        protocol_routes,
    )
}

fn finish_new(
    state: &CoreState,
    operation_id: &str,
    digest: &str,
    runtime: DynamicProviderRuntime,
    first_account: Option<ModelAccount>,
    onboarding_draft: bool,
    protocol_routes: Option<Vec<ocg_domain::destination::HttpProtocolRoute>>,
) -> Result<OnboardingCommitResult, V3ApiError> {
    let connection_id =
        connection_id_for_legacy(LegacyConnectionKind::DynamicProvider, &runtime.id);
    let (credential_id, account_id) = stored_commit_ids(first_account.as_ref());
    let target_ids = runtime
        .mappings
        .iter()
        .map(|mapping| target_id_for(&connection_id, &mapping.public_model).to_string())
        .collect::<Vec<_>>();
    let stored = StoredOnboardingCommitResult {
        connection_id: connection_id.to_string(),
        credential_id,
        target_ids,
        account_id,
    };
    let operation = ledger_row(operation_id, digest, &stored)?;
    let snapshot = {
        let db = state.db.lock();
        db.commit_onboarding_new_with_routes(
            &runtime,
            first_account.as_ref(),
            onboarding_draft,
            &operation,
            protocol_routes.as_deref(),
        )
        .map_err(|error| V3ApiError::invalid_request_at(state, error.to_string()))?
    };
    state
        .install_dynamic_providers_snapshot(snapshot)
        .map_err(V3ApiError::internal)?;
    Ok(committed_result(state, stored))
}

/// Existing-connection commit keeps the same explicit CAS facts as `commit_new`.
#[allow(clippy::too_many_arguments)]
fn commit_existing(
    state: &CoreState,
    operation_id: &str,
    digest: &str,
    connection: OnboardingConnectionExisting,
    authorization: Option<OnboardingAuthorization>,
    targets: Vec<OnboardingTarget>,
    mode: Option<OnboardingCommitMode>,
    authorize_current_endpoint: bool,
) -> Result<OnboardingCommitResult, V3ApiError> {
    if mode.is_none() {
        if connection.configuration.is_some() || authorize_current_endpoint {
            return Err(V3ApiError::invalid_request_at(
                state,
                "legacy existing onboarding rejects configuration and authorizeCurrentEndpoint",
            ));
        }
        return commit_existing_second_key(
            state,
            operation_id,
            digest,
            &connection.connection_id,
            authorization,
            targets,
        );
    }
    resume_existing_draft(
        state,
        operation_id,
        digest,
        connection,
        authorization,
        targets,
        mode,
        authorize_current_endpoint,
    )
}

fn commit_existing_second_key(
    state: &CoreState,
    operation_id: &str,
    digest: &str,
    connection_id: &str,
    authorization: Option<OnboardingAuthorization>,
    targets: Vec<OnboardingTarget>,
) -> Result<OnboardingCommitResult, V3ApiError> {
    if !targets.is_empty() {
        return Err(V3ApiError::invalid_request_at(
            state,
            "existing connections cannot change model targets on onboarding commit",
        ));
    }
    let connection = resolve_existing_key_connection(state, connection_id)?;
    if !connection.auth_kind.requires_key() {
        return Err(V3ApiError::invalid_request_at(
            state,
            "no-auth provider already has a singleton account",
        ));
    }
    let OnboardingAuthorization::ApiKey(api_key) = authorization.ok_or_else(|| {
        V3ApiError::invalid_request_at(
            state,
            "existing connections require an api_key authorization",
        )
    })?
    else {
        return Err(V3ApiError::invalid_request_at(
            state,
            "existing connections require an api_key authorization",
        ));
    };
    let secret = api_key.secret_input.trim();
    if secret.is_empty() {
        return Err(V3ApiError::invalid_request_at(
            state,
            "secretInput is required",
        ));
    }
    let now = Utc::now();
    let account_name = api_key
        .account_label
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or(connection.name.as_str())
        .to_string();
    let notes = match api_key.notes.as_deref() {
        Some(value) => normalize_account_notes(value)
            .map_err(|error| V3ApiError::invalid_request_at(state, error.to_string()))?,
        None => None,
    };
    let key_cipher = first_account_key(state, connection.auth_kind, Some(secret))?;
    let account = dynamic_provider_account(
        connection.auth_kind,
        &connection.provider_id,
        account_name,
        key_cipher,
        notes,
        now,
    );
    let stored = StoredOnboardingCommitResult {
        connection_id: connection_id.to_string(),
        credential_id: Some(credential_id_for_legacy_account(&account.id).to_string()),
        target_ids: Vec::new(),
        account_id: Some(account.id.clone()),
    };
    let operation = ledger_row(operation_id, digest, &stored)?;
    {
        let db = state.db.lock();
        db.commit_onboarding_existing_account(
            &account,
            connection.destination_id.as_deref(),
            &operation,
        )
        .map_err(|error| V3ApiError::invalid_request_at(state, error.to_string()))?;
    }
    state.bump_settings_revision();
    Ok(committed_result(state, stored))
}

struct ExistingKeyConnection {
    provider_id: String,
    name: String,
    auth_kind: DynamicAuthKind,
    destination_id: Option<String>,
}

fn resolve_existing_key_connection(
    state: &CoreState,
    connection_id: &str,
) -> Result<ExistingKeyConnection, V3ApiError> {
    if let Ok(runtime) = resolve_existing_dynamic(state, connection_id) {
        return Ok(ExistingKeyConnection {
            provider_id: runtime.id,
            name: runtime.name,
            auth_kind: runtime.auth_kind,
            destination_id: None,
        });
    }
    let projection = {
        let db = state.db.lock();
        crate::destination_projection::read_v4_projection(&db)
            .map_err(V3ApiError::internal)?
            .map_err(|_| V3ApiError::conflict_at(state, "destination projection refused"))?
    };
    for destination in projection.destinations {
        let LegacyDestinationRef::CustomAccount(legacy_id) = &destination.legacy else {
            continue;
        };
        if connection_id_for_legacy(LegacyConnectionKind::CustomAccount, legacy_id).as_str()
            != connection_id
        {
            continue;
        }
        let auth_kind = match destination.auth_scheme {
            AuthScheme::Bearer => DynamicAuthKind::Bearer,
            AuthScheme::XApiKey => DynamicAuthKind::XApiKey,
            AuthScheme::ApiKey => DynamicAuthKind::ApiKey,
            AuthScheme::None => DynamicAuthKind::None,
        };
        return Ok(ExistingKeyConnection {
            provider_id: CUSTOM_PROVIDER_ID.to_string(),
            name: destination.name,
            auth_kind,
            destination_id: Some(destination.id),
        });
    }
    Err(V3ApiError::not_found_at(state, "connection not found"))
}

/// Draft resume is one CAS write over configuration, auth, targets, and mode.
#[allow(clippy::too_many_arguments)]
fn resume_existing_draft(
    state: &CoreState,
    operation_id: &str,
    digest: &str,
    connection: OnboardingConnectionExisting,
    authorization: Option<OnboardingAuthorization>,
    targets: Vec<OnboardingTarget>,
    mode: Option<OnboardingCommitMode>,
    authorize_current_endpoint: bool,
) -> Result<OnboardingCommitResult, V3ApiError> {
    let Some(configuration) = connection.configuration else {
        return Err(V3ApiError::invalid_request_at(
            state,
            "explicit-mode existing connections require configuration",
        ));
    };
    let draft = mode == Some(OnboardingCommitMode::Draft);
    if targets.is_empty() && !draft {
        return Err(V3ApiError::invalid_request_at(
            state,
            "complete requires at least one target",
        ));
    }
    let existing = resolve_existing_dynamic(state, &connection.connection_id)?;
    let is_draft = {
        let db = state.db.lock();
        db.provider_is_onboarding_draft(&existing.id)
            .map_err(V3ApiError::internal)?
            .unwrap_or(false)
    };
    if !is_draft {
        return Err(V3ApiError::invalid_request_at(
            state,
            "explicit-mode existing onboarding can only resume a stored draft",
        ));
    }
    let protocol_routes = match configuration.protocol_routes.as_ref() {
        Some(routes) => Some(
            routes
                .iter()
                .map(ocg_domain::destination::HttpProtocolRoute::from)
                .collect::<Vec<_>>(),
        ),
        None => {
            let projection = crate::destination_projection::load_runtime(&state.db.lock())
                .map_err(V3ApiError::internal)?;
            projection
                .destinations
                .iter()
                .find(|destination| {
                    destination.id
                        == ocg_domain::destination::destination_id_for_dynamic(&existing.id)
                })
                .filter(|destination| !destination.protocol_routes.is_empty())
                .map(|destination| destination.protocol_routes.clone())
        }
    };
    let auth_kind = DynamicAuthKind::from(configuration.auth_kind);
    let now = Utc::now();
    let models = to_definition_models(targets);
    let mut definition = if draft {
        validate_draft_wire_definition(
            existing.id.clone(),
            configuration.name,
            configuration.endpoint_url,
            configuration.upstream_protocol,
            auth_kind,
            models,
        )
    } else {
        validate_wire_definition(
            existing.id.clone(),
            configuration.name,
            configuration.endpoint_url,
            configuration.upstream_protocol,
            auth_kind,
            models,
        )
    }
    .map_err(|error| V3ApiError::invalid_request_at(state, error.to_string()))?;
    definition.preset_id = preset_from_template_id(state, &configuration.template_id)?;
    let runtime = runtime_from_definition(definition, existing.created_at, now);
    let changing_from_none = existing.auth_kind.is_singleton() && !runtime.auth_kind.is_singleton();
    let changing_to_none = !existing.auth_kind.is_singleton() && runtime.auth_kind.is_singleton();
    if changing_to_none {
        return Err(V3ApiError::invalid_request_at(
            state,
            "cannot change a keyed connection to no-auth through onboarding",
        ));
    }
    let (accounts, snapshot) = {
        let db = state.db.lock();
        let accounts = db
            .list_accounts()
            .map_err(V3ApiError::internal)?
            .into_iter()
            .filter(|account| account.provider_id == runtime.id)
            .collect::<Vec<_>>();
        let snapshot = db.list_identity_model().map_err(V3ApiError::internal)?;
        (accounts, snapshot)
    };
    if accounts.len() > 1 {
        return Err(V3ApiError::invalid_request_at(
            state,
            "draft resume expects at most one saved Key",
        ));
    }
    let saved = accounts.into_iter().next();
    let saved_record = saved.as_ref().and_then(|account| {
        snapshot
            .accounts
            .iter()
            .find(|record| record.account.id == account.id)
    });
    if saved.is_some() && saved_record.is_none() {
        return Err(V3ApiError::invalid_request_at(
            state,
            "saved Key is missing identity, credential, or binding state",
        ));
    }
    reject_malformed_resume_authorization(
        state,
        runtime.auth_kind,
        saved.as_ref(),
        &authorization,
    )?;

    let mut create_account = None;
    let mut rotate = None;
    let mut account_meta = None;
    let mut sync_auth = None;
    let mut result_account_id = saved.as_ref().map(|account| account.id.clone());
    let mut result_credential_id = saved_record.map(|record| record.credential_id.clone());
    if let Some(account) = saved.as_ref() {
        let (key_cipher, label, notes) =
            resume_saved_key_update(state, runtime.auth_kind, &authorization)?;
        if changing_from_none {
            match key_cipher {
                Some(key_cipher) => {
                    rotate = Some((account.id.clone(), key_cipher));
                    sync_auth = Some((
                        account.id.clone(),
                        runtime.auth_kind.credential_kind().as_str().to_string(),
                        runtime.auth_kind.quota_scope().as_str().to_string(),
                    ));
                }
                None => {
                    return Err(V3ApiError::invalid_request_at(
                        state,
                        "changing a saved no-auth connection to keyed auth requires an api_key authorization",
                    ));
                }
            }
        } else if let Some(key_cipher) = key_cipher {
            rotate = Some((account.id.clone(), key_cipher));
        }
        if runtime.auth_kind != existing.auth_kind && (rotate.is_some() || !changing_from_none) {
            sync_auth = Some((
                account.id.clone(),
                runtime.auth_kind.credential_kind().as_str().to_string(),
                runtime.auth_kind.quota_scope().as_str().to_string(),
            ));
        }
        account_meta = Some((account.id.clone(), label, notes));
    } else {
        let first = first_account_for_new(
            state,
            runtime.auth_kind,
            &runtime.id,
            &runtime.name,
            &authorization,
            now,
        )?;
        if !draft && runtime.auth_kind.requires_key() && first.is_none() {
            return Err(V3ApiError::invalid_request_at(
                state,
                "complete requires an api_key authorization",
            ));
        }
        if let Some(account) = first {
            result_credential_id = Some(credential_id_for_legacy_account(&account.id).to_string());
            result_account_id = Some(account.id.clone());
            create_account = Some(account);
        }
    }

    if !draft {
        if let Some(record) = saved_record {
            if !record.binding_enabled {
                return Err(V3ApiError::invalid_request_at(
                    state,
                    "complete requires an enabled binding",
                ));
            }
            let has_required_key =
                !runtime.auth_kind.requires_key() || record.has_key_material || rotate.is_some();
            if !has_required_key {
                return Err(V3ApiError::invalid_request_at(
                    state,
                    "complete requires an api_key authorization",
                ));
            }
        } else if runtime.auth_kind.requires_key() && create_account.is_none() {
            return Err(V3ApiError::invalid_request_at(
                state,
                "complete requires an api_key authorization",
            ));
        }
    }

    let grant_union = if !draft {
        if let Some(account_id) = result_account_id.as_deref() {
            resolve_complete_grants(
                state,
                &runtime,
                account_id,
                create_account.is_some(),
                authorize_current_endpoint,
                saved_record,
                protocol_routes.as_deref().unwrap_or_default(),
            )?
        } else {
            None
        }
    } else {
        None
    };

    let connection_id =
        connection_id_for_legacy(LegacyConnectionKind::DynamicProvider, &runtime.id);
    let stored = StoredOnboardingCommitResult {
        connection_id: connection_id.to_string(),
        credential_id: result_credential_id,
        target_ids: runtime
            .mappings
            .iter()
            .map(|mapping| target_id_for(&connection_id, &mapping.public_model).to_string())
            .collect(),
        account_id: result_account_id,
    };
    let operation = ledger_row(operation_id, digest, &stored)?;
    let rotate_ref = rotate
        .as_ref()
        .map(|(id, cipher)| (id.as_str(), cipher.as_str()));
    let meta_ref = account_meta.as_ref().map(|(id, name, notes)| {
        (
            id.as_str(),
            name.as_deref(),
            notes.as_ref().map(|value| value.as_deref()),
        )
    });
    let grant_owned = grant_union;
    let grant_ref = grant_owned
        .as_ref()
        .map(|(id, ids, origins)| (id.as_str(), ids.as_slice(), origins.as_slice()));
    let sync_owned = sync_auth;
    let sync_ref = sync_owned
        .as_ref()
        .map(|(id, kind, quota)| (id.as_str(), kind.as_str(), quota.as_str()));
    let snapshot = {
        let db = state.db.lock();
        db.commit_onboarding_resume_with_routes(
            &runtime,
            draft,
            create_account.as_ref(),
            rotate_ref,
            meta_ref,
            grant_ref,
            sync_ref,
            &operation,
            protocol_routes.as_deref(),
        )
        .map_err(|error| V3ApiError::invalid_request_at(state, error.to_string()))?
    };
    state
        .install_dynamic_providers_snapshot(snapshot)
        .map_err(V3ApiError::internal)?;
    Ok(committed_result(state, stored))
}

fn reject_malformed_resume_authorization(
    state: &CoreState,
    auth_kind: DynamicAuthKind,
    saved: Option<&ModelAccount>,
    authorization: &Option<OnboardingAuthorization>,
) -> Result<(), V3ApiError> {
    match authorization {
        Some(OnboardingAuthorization::None {}) => {
            if auth_kind.requires_key()
                || saved.is_some_and(|account| {
                    account.credential_kind != ocg_domain::catalog::CredentialKind::None
                })
            {
                return Err(V3ApiError::invalid_request_at(
                    state,
                    "authorization none is only valid when authKind is none",
                ));
            }
        }
        Some(OnboardingAuthorization::ApiKey(_)) if !auth_kind.requires_key() => {
            return Err(V3ApiError::invalid_request_at(
                state,
                "api_key authorization is not valid when authKind is none",
            ));
        }
        Some(OnboardingAuthorization::ApiKey(_)) | None => {}
    }
    Ok(())
}

/// Cipher, label, and notes (`None` = leave, `Some(None)` = clear).
type ResumeKeyPatch = (Option<String>, Option<String>, Option<Option<String>>);

fn resume_saved_key_update(
    state: &CoreState,
    auth_kind: DynamicAuthKind,
    authorization: &Option<OnboardingAuthorization>,
) -> Result<ResumeKeyPatch, V3ApiError> {
    let Some(authorization) = authorization else {
        return Ok((None, None, None));
    };
    match authorization {
        OnboardingAuthorization::None {} => Ok((None, None, None)),
        OnboardingAuthorization::ApiKey(api_key) => {
            let secret = api_key.secret_input.trim();
            let key_cipher = if secret.is_empty() {
                None
            } else {
                Some(first_account_key(state, auth_kind, Some(secret))?)
            };
            let label = api_key
                .account_label
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_string);
            let notes =
                match api_key.notes.as_deref() {
                    Some(value) => Some(normalize_account_notes(value).map_err(|error| {
                        V3ApiError::invalid_request_at(state, error.to_string())
                    })?),
                    None => None,
                };
            Ok((key_cipher, label, notes))
        }
    }
}

/// Account id plus stored endpoint-id / origin grant arrays.
type BindingGrantUnion = (String, Vec<String>, Vec<String>);

fn resolve_complete_grants(
    state: &CoreState,
    runtime: &DynamicProviderRuntime,
    account_id: &str,
    newly_created: bool,
    authorize_current_endpoint: bool,
    saved_record: Option<&crate::db::identity::IdentityAccountRecord>,
    protocol_routes: &[ocg_domain::destination::HttpProtocolRoute],
) -> Result<Option<BindingGrantUnion>, V3ApiError> {
    if newly_created {
        return Ok(None);
    }
    let (safe_ids, safe_origins) = safe_grants_for_runtime(runtime, protocol_routes);
    let record = if let Some(record) = saved_record {
        record
    } else {
        return Err(V3ApiError::invalid_request_at(
            state,
            "saved Key is missing identity, credential, or binding state",
        ));
    };
    if record.account.id != account_id {
        return Err(V3ApiError::invalid_request_at(
            state,
            "saved Key is missing identity, credential, or binding state",
        ));
    }
    let connection_id =
        connection_id_for_legacy(LegacyConnectionKind::DynamicProvider, &runtime.id);
    let before = crate::destination_projection::load_persisted(&state.db.lock())
        .map_err(V3ApiError::internal)?
        .destinations
        .into_iter()
        .find(|d| d.id == ocg_domain::destination::destination_id_for_dynamic(&runtime.id))
        .ok_or_else(|| V3ApiError::invalid_request_at(state, "saved destination is missing"))?;
    let mut after = ocg_domain::destination::destination_from_legacy(
        &ocg_domain::destination::LegacyDestinationFacts::Dynamic {
            definition: runtime.definition(),
        },
    )
    .map_err(|error| V3ApiError::invalid_request_at(state, error.to_string()))?;
    after.protocol_routes = protocol_routes.to_vec();
    if !protocol_routes.is_empty() {
        after.protocols = protocol_routes.iter().map(|r| r.protocol).collect();
    }
    let preserved_ids = ocg_domain::credential::remap_route_grant_ids(
        &connection_id,
        &ocg_domain::destination::http_configured_routes(&before),
        &ocg_domain::destination::http_configured_routes(&after),
        &record.allowed_endpoint_ids,
    );
    let missing_destination = safe_ids
        .iter()
        .any(|id| !preserved_ids.iter().any(|got| got == id))
        || safe_origins
            .iter()
            .any(|origin| !record.allowed_origins.iter().any(|got| got == origin));
    if missing_destination && !authorize_current_endpoint {
        return Err(V3ApiError::invalid_request_at(
            state,
            "current endpoint is not granted; set authorizeCurrentEndpoint to add safe default and same-origin grants",
        ));
    }
    if !authorize_current_endpoint {
        return Ok(None);
    }
    Ok(Some((
        account_id.to_string(),
        union_unique(&preserved_ids, &safe_ids),
        union_unique(&record.allowed_origins, &safe_origins),
    )))
}

fn safe_grants_for_runtime(
    runtime: &DynamicProviderRuntime,
    protocol_routes: &[ocg_domain::destination::HttpProtocolRoute],
) -> (Vec<String>, Vec<String>) {
    use ocg_domain::destination::{
        LegacyDestinationFacts, destination_from_legacy, http_configured_routes,
    };
    let connection_id =
        connection_id_for_legacy(LegacyConnectionKind::DynamicProvider, &runtime.id);
    let mut destination = destination_from_legacy(&LegacyDestinationFacts::Dynamic {
        definition: runtime.definition(),
    })
    .expect("validated dynamic definition");
    destination.protocol_routes = protocol_routes.to_vec();
    if !protocol_routes.is_empty() {
        destination.protocols = protocol_routes.iter().map(|route| route.protocol).collect();
    }
    let routes = http_configured_routes(&destination);
    let assigned = assigned_endpoints_for_routes(&connection_id, &routes);
    safe_default_grants(&assigned)
}

fn union_unique(left: &[String], right: &[String]) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for value in left.iter().chain(right.iter()) {
        if seen.insert(value.clone()) {
            out.push(value.clone());
        }
    }
    out
}

fn stored_commit_ids(account: Option<&ModelAccount>) -> (Option<String>, Option<String>) {
    match account {
        Some(account) => (
            Some(credential_id_for_legacy_account(&account.id).to_string()),
            Some(account.id.clone()),
        ),
        None => (None, None),
    }
}

fn reject_legacy_authorize_flag(
    state: &CoreState,
    mode: Option<OnboardingCommitMode>,
    authorize_current_endpoint: bool,
) -> Result<(), V3ApiError> {
    if mode.is_none() && authorize_current_endpoint {
        return Err(V3ApiError::invalid_request_at(
            state,
            "authorizeCurrentEndpoint requires an explicit onboarding mode",
        ));
    }
    Ok(())
}

fn first_account_for_new(
    state: &CoreState,
    auth_kind: DynamicAuthKind,
    provider_id: &str,
    runtime_name: &str,
    authorization: &Option<OnboardingAuthorization>,
    now: chrono::DateTime<Utc>,
) -> Result<Option<ModelAccount>, V3ApiError> {
    match authorization {
        None => {
            if auth_kind.requires_key() {
                return Ok(None);
            }
            let key_cipher = first_account_key(state, auth_kind, None)?;
            Ok(Some(dynamic_provider_account(
                auth_kind,
                provider_id,
                runtime_name.to_string(),
                key_cipher,
                None,
                now,
            )))
        }
        Some(OnboardingAuthorization::None {}) => {
            if auth_kind.requires_key() {
                return Err(V3ApiError::invalid_request_at(
                    state,
                    "authorization none is only valid when authKind is none",
                ));
            }
            let key_cipher = first_account_key(state, auth_kind, None)?;
            Ok(Some(dynamic_provider_account(
                auth_kind,
                provider_id,
                runtime_name.to_string(),
                key_cipher,
                None,
                now,
            )))
        }
        Some(OnboardingAuthorization::ApiKey(api_key)) => {
            if !auth_kind.requires_key() {
                return Err(V3ApiError::invalid_request_at(
                    state,
                    "api_key authorization is not valid when authKind is none",
                ));
            }
            let secret = api_key.secret_input.trim();
            if secret.is_empty() {
                return Err(V3ApiError::invalid_request_at(
                    state,
                    "secretInput is required",
                ));
            }
            let account_name = api_key
                .account_label
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .unwrap_or(runtime_name)
                .to_string();
            let notes = match api_key.notes.as_deref() {
                Some(value) => normalize_account_notes(value)
                    .map_err(|error| V3ApiError::invalid_request_at(state, error.to_string()))?,
                None => None,
            };
            let key_cipher = first_account_key(state, auth_kind, Some(secret))?;
            Ok(Some(dynamic_provider_account(
                auth_kind,
                provider_id,
                account_name,
                key_cipher,
                notes,
                now,
            )))
        }
    }
}

fn dynamic_provider_account(
    auth_kind: DynamicAuthKind,
    provider_id: &str,
    name: String,
    key_cipher: String,
    notes: Option<String>,
    now: chrono::DateTime<Utc>,
) -> ModelAccount {
    ModelAccount {
        id: uuid::Uuid::new_v4().to_string(),
        provider_id: provider_id.to_string(),
        credential_kind: auth_kind.credential_kind(),
        quota_scope: auth_kind.quota_scope(),
        name,
        username: None,
        password_cipher: None,
        key_cipher,
        enabled: NEW_READY_KEY_ACCOUNT_ENABLED,
        account_type: ModelAccountType::Key,
        setup_step: crate::models::AccountSetupStep::Ready,
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
        notes,
        created_at: now,
        updated_at: now,
    }
}

fn resolve_existing_dynamic(
    state: &CoreState,
    connection_id: &str,
) -> Result<DynamicProviderRuntime, V3ApiError> {
    let runtimes = {
        let db = state.db.lock();
        db.list_control_plane_dynamic_providers()
            .map_err(V3ApiError::internal)?
    };
    for runtime in runtimes {
        if connection_id_for_legacy(LegacyConnectionKind::DynamicProvider, &runtime.id).as_str()
            == connection_id
        {
            return Ok(runtime);
        }
    }
    for plan in BUILTIN_PROVIDERS {
        if connection_id_for_legacy(LegacyConnectionKind::BuiltinProvider, plan.provider_id)
            .as_str()
            == connection_id
        {
            return Err(V3ApiError::invalid_request_at(
                state,
                NON_DYNAMIC_DRAFT_MESSAGE,
            ));
        }
    }
    let accounts = state
        .db
        .lock()
        .list_accounts()
        .map_err(V3ApiError::internal)?;
    for account in accounts {
        if account.provider_id == CUSTOM_PROVIDER_ID
            && connection_id_for_legacy(LegacyConnectionKind::CustomAccount, &account.id).as_str()
                == connection_id
        {
            return Err(V3ApiError::invalid_request_at(
                state,
                NON_DYNAMIC_DRAFT_MESSAGE,
            ));
        }
    }
    Err(V3ApiError::not_found_at(state, "connection not found"))
}

fn preset_from_template_id(
    state: &CoreState,
    template_id: &str,
) -> Result<Option<String>, V3ApiError> {
    if template_id.trim() == CUSTOM_HTTP_TEMPLATE {
        return Ok(None);
    }
    normalize_preset_id(Some(template_id.to_string()))
        .map_err(|error| V3ApiError::invalid_request_at(state, error.to_string()))
}

fn to_definition_models(targets: Vec<OnboardingTarget>) -> Vec<ProviderDefinitionModel> {
    targets
        .into_iter()
        .map(|target| ProviderDefinitionModel {
            public_model: target.public_model,
            upstream_model: target.upstream_model,
            upstream_override: target.upstream_override.map(|value| {
                ProviderModelUpstreamOverride {
                    protocol: value.protocol,
                    endpoint_url: value.endpoint_url,
                }
            }),
        })
        .collect()
}

/// Semantic fields covered by the operation digest.
///
/// CAS tokens (`expectedRevision`, `processGeneration`) are not payload: a
/// client whose first response was lost typically refreshes them from
/// `GET /contract` and retries the same `operationId`. Hashing those tokens
/// would turn a legitimate replay into `operationPayloadMismatch`.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct OnboardingDigestPayload<'a> {
    operation_id: &'a str,
    connection: &'a OnboardingConnection,
    authorization: &'a Option<OnboardingAuthorization>,
    targets: &'a [OnboardingTarget],
    #[serde(skip_serializing_if = "Option::is_none")]
    mode: &'a Option<OnboardingCommitMode>,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    authorize_current_endpoint: bool,
}

/// Canonical JSON of the semantic payload only: `operationId`, `connection`,
/// `authorization`, and `targets`. Field order is the struct declaration
/// order. CAS tokens are excluded so retries after a lost response still
/// match the stored digest.
pub(crate) fn digest_payload_bytes(
    input: &OnboardingCommitRequest,
) -> Result<Vec<u8>, serde_json::Error> {
    serde_json::to_vec(&OnboardingDigestPayload {
        operation_id: &input.operation_id,
        connection: &input.connection,
        authorization: &input.authorization,
        targets: &input.targets,
        mode: &input.mode,
        authorize_current_endpoint: input.authorize_current_endpoint,
    })
}

fn payload_digest(
    state: &CoreState,
    input: &OnboardingCommitRequest,
) -> Result<String, V3ApiError> {
    let canonical = digest_payload_bytes(input).map_err(V3ApiError::internal)?;
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

fn ledger_row(
    operation_id: &str,
    digest: &str,
    stored: &StoredOnboardingCommitResult,
) -> Result<NewDashboardOperation, V3ApiError> {
    Ok(NewDashboardOperation {
        operation_id: operation_id.to_string(),
        kind: OPERATION_KIND.to_string(),
        payload_digest: digest.to_string(),
        result_json: serde_json::to_string(stored).map_err(V3ApiError::internal)?,
    })
}

fn committed_result(
    state: &CoreState,
    stored: StoredOnboardingCommitResult,
) -> OnboardingCommitResult {
    OnboardingCommitResult {
        revision: ControlRevision::from_state(state),
        connection_id: stored.connection_id,
        credential_id: stored.credential_id,
        target_ids: stored.target_ids,
        replayed: false,
        account_id: stored.account_id,
    }
}

fn replay_stored(
    state: &CoreState,
    result_json: &str,
) -> Result<OnboardingCommitResult, V3ApiError> {
    let stored: StoredOnboardingCommitResult =
        serde_json::from_str(result_json).map_err(V3ApiError::internal)?;
    Ok(OnboardingCommitResult {
        revision: ControlRevision::from_state(state),
        connection_id: stored.connection_id,
        credential_id: stored.credential_id,
        target_ids: stored.target_ids,
        replayed: true,
        account_id: stored.account_id,
    })
}

#[cfg(test)]
mod tests;
