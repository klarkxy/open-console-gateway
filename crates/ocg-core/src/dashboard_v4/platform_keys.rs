//! POST `/platform-accounts/{id}/import-keys` — copy New API tokens locally.
//!
//! Outbound reads use the stored management credential, the same class of
//! request as V3 platform refresh. The response is secret-free.

use axum::Json;
use axum::body::Bytes;
use axum::extract::{Path, State};
use chrono::Utc;
use std::collections::HashSet;

use crate::custom;
use crate::dashboard_v3::{ControlRevision, V3ApiError, check_expectation, parse_mutation_json};
use crate::models::{
    Account as ModelAccount, AccountCustomConfigInput, AccountModelCapabilityInput,
    AccountSetupStep as ModelSetupStep, AccountType as ModelAccountType,
    NEW_READY_KEY_ACCOUNT_ENABLED,
};
use crate::platform::hosted_endpoint;
use crate::platform::{PlatformGroup, PlatformKind, import};
use crate::provider::{UpstreamProtocolKind, builtin_provider};
use crate::redaction::redact_known_secret;
use crate::state::CoreState;
use ocg_domain::ids::CUSTOM_PROVIDER_ID;

use super::types::{PlatformKeyImportFailure, PlatformKeyImportRequest, PlatformKeyImportResult};

pub(super) async fn import_keys(
    State(state): State<CoreState>,
    Path(id): Path<String>,
    body: Bytes,
) -> Result<Json<PlatformKeyImportResult>, V3ApiError> {
    let input = parse_mutation_json::<PlatformKeyImportRequest>(&body)?;
    let (base_url, credential) = {
        let _lock = state.settings_update.lock();
        check_expectation(&state, &input.expectation)?;
        let db = state.db.lock();
        let parent = db
            .platform_account(&id)
            .map_err(V3ApiError::internal)?
            .ok_or_else(|| V3ApiError::not_found_at(&state, "platform account not found"))?;
        if parent.kind != PlatformKind::NewApi {
            return Err(V3ApiError::invalid_request_at(
                &state,
                "key import is only available for New API",
            ));
        }
        let credential = db
            .platform_credential_cipher(&id)
            .map_err(V3ApiError::internal)?
            .map(|cipher| state.decrypt_key(&cipher))
            .transpose()
            .map_err(V3ApiError::internal)?
            .filter(|value| !value.trim().is_empty());
        let Some(credential) = credential else {
            return Err(V3ApiError::invalid_request_at(
                &state,
                "user credential required",
            ));
        };
        (parent.base_url, credential)
    };

    let hosted = hosted_endpoint(&base_url)
        .map_err(|error| V3ApiError::invalid_request_at(&state, error.to_string()))?;
    let client =
        crate::http_client::build_no_redirect(&state.config()).map_err(V3ApiError::internal)?;
    let origin = reqwest::Url::parse(&hosted)
        .map_err(|error| V3ApiError::invalid_request_at(&state, error.to_string()))?;
    let (secrets, skipped_disabled, mut failed) =
        import::collect_remote_secrets(&client, &origin, &credential)
            .await
            .map_err(|error| {
                V3ApiError::invalid_request_at(&state, redact_known_secret(&error, &credential))
            })?;

    let config = state.config();
    let mut pending: Vec<(String, String, Vec<AccountModelCapabilityInput>)> = Vec::new();
    for secret in secrets {
        match custom::discover_custom_models(
            &config,
            &AccountCustomConfigInput {
                endpoint_url: hosted.clone(),
                upstream_protocol: UpstreamProtocolKind::ChatCompletions,
            },
            &secret.key,
        )
        .await
        {
            Ok(discovery) if discovery.models.is_empty() => {
                failed.push((secret.name, "no_models".to_string()));
            }
            Ok(discovery) => {
                let capabilities = discovery
                    .models
                    .into_iter()
                    .map(|model| AccountModelCapabilityInput {
                        public_model: model.clone(),
                        upstream_model: model,
                        protocol: UpstreamProtocolKind::ChatCompletions,
                        source: Some("discovery".to_string()),
                    })
                    .collect();
                pending.push((secret.name, secret.key, capabilities));
            }
            Err(_) => failed.push((secret.name, "discover".to_string())),
        }
    }

    let plan = builtin_provider(CUSTOM_PROVIDER_ID)
        .ok_or_else(|| V3ApiError::internal(anyhow::anyhow!("custom provider is required")))?;
    let now = Utc::now();
    let mut imported = 0_u32;
    let mut skipped_existing = 0_u32;

    {
        let _lock = state.settings_update.lock();
        if state
            .db
            .lock()
            .platform_account(&id)
            .map_err(V3ApiError::internal)?
            .is_none()
        {
            return Err(V3ApiError::not_found_at(
                &state,
                "platform account not found",
            ));
        }
        let mut existing_keys = local_custom_keys(&state)?;
        for (name, key, capabilities) in pending {
            if existing_keys.contains(&key) {
                skipped_existing += 1;
                continue;
            }
            let account_id = uuid::Uuid::new_v4().to_string();
            let account = ModelAccount {
                id: account_id.clone(),
                provider_id: plan.provider_id.to_string(),
                credential_kind: plan.credential_kind,
                quota_scope: plan.quota_scope,
                name,
                username: None,
                password_cipher: None,
                key_cipher: state.encrypt_key(&key).map_err(V3ApiError::internal)?,
                enabled: NEW_READY_KEY_ACCOUNT_ENABLED,
                account_type: ModelAccountType::Key,
                setup_step: ModelSetupStep::Ready,
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
            let custom_config = AccountCustomConfigInput {
                endpoint_url: hosted.clone(),
                upstream_protocol: UpstreamProtocolKind::ChatCompletions,
            };
            {
                let db = state.db.lock();
                if let Err(error) = db.create_account_with_contract_and_billing(
                    &account,
                    Some(&custom_config),
                    &capabilities,
                    None,
                ) {
                    if error.to_string().contains("duplicate") {
                        skipped_existing += 1;
                    } else {
                        failed.push((account.name, "create".to_string()));
                    }
                    continue;
                }
                if db
                    .link_platform_account(&account_id, &id, &PlatformGroup::default())
                    .is_err()
                {
                    failed.push((account.name, "link".to_string()));
                    continue;
                }
            }
            existing_keys.insert(key);
            imported += 1;
        }
        if imported > 0 {
            state.bump_settings_revision();
            state
                .reload_provider_contracts()
                .map_err(V3ApiError::internal)?;
        }
    }

    let failed = failed
        .into_iter()
        .map(|(name, code)| PlatformKeyImportFailure {
            name: redact_known_secret(&name, &credential),
            code,
        })
        .collect();
    Ok(Json(PlatformKeyImportResult {
        imported,
        skipped_existing,
        skipped_disabled: u32::try_from(skipped_disabled).unwrap_or(u32::MAX),
        failed,
        revision: ControlRevision::from_state(&state),
    }))
}

fn local_custom_keys(state: &CoreState) -> Result<HashSet<String>, V3ApiError> {
    let db = state.db.lock();
    let mut keys = HashSet::new();
    for account in db.list_accounts().map_err(V3ApiError::internal)? {
        if account.provider_id != CUSTOM_PROVIDER_ID {
            continue;
        }
        if let Ok(plain) = state.decrypt_key(&account.key_cipher)
            && !plain.is_empty()
        {
            keys.insert(plain);
        }
    }
    Ok(keys)
}
