//! Manual account-owned platform control plane.
use super::{
    V3ApiError, check_expectation, parse_mutation_json,
    types::{MutationAck, MutationExpectation},
};
use crate::{platform::*, state::CoreState};
use axum::{
    Json,
    body::Bytes,
    extract::{Path, State},
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct PlatformAccounts {
    pub accounts: Vec<PlatformAccount>,
    pub links: Vec<PlatformLink>,
    pub revision: u64,
    pub process_generation: u64,
}

#[derive(Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct PlatformCreate {
    #[serde(flatten)]
    pub expectation: MutationExpectation,
    pub kind: PlatformKind,
    pub name: String,
    pub base_url: String,
    pub user_credential: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct PlatformUpdate {
    #[serde(flatten)]
    pub expectation: MutationExpectation,
    pub name: String,
    /// Omitted/null preserves, empty clears.
    pub user_credential: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct PlatformLinkWrite {
    #[serde(flatten)]
    pub expectation: MutationExpectation,
    pub platform_account_id: String,
    pub group: PlatformGroup,
}

#[derive(Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct PlatformRefresh {
    #[serde(flatten)]
    pub expectation: MutationExpectation,
    pub account_id: Option<String>,
}

fn view(state: &CoreState) -> Result<PlatformAccounts, V3ApiError> {
    let db = state.db.lock();
    Ok(PlatformAccounts {
        accounts: crate::destination_projection::list_platform_accounts_for_v3(&db)
            .map_err(V3ApiError::internal)?,
        links: db.list_platform_links().map_err(V3ApiError::internal)?,
        revision: state.settings_revision(),
        process_generation: state.process_generation(),
    })
}

pub(super) async fn list(
    State(state): State<CoreState>,
) -> Result<Json<PlatformAccounts>, V3ApiError> {
    let _lock = state.settings_update.lock();
    view(&state).map(Json)
}

pub(super) async fn create(
    State(state): State<CoreState>,
    body: Bytes,
) -> Result<Json<PlatformAccounts>, V3ApiError> {
    let mut op = super::settings::open_dashboard(&state, "platform.create", "platform", None);
    let mut effect = super::settings::CommittedEffect::Atomic;
    let result = create_inner(&state, body, &mut op, &mut effect).await;
    super::settings::record_effect(
        op,
        &state,
        &["kind"],
        (Some(1), Some(1), None),
        None,
        effect,
        result,
    )
}

async fn create_inner(
    state: &CoreState,
    body: Bytes,
    op: &mut crate::user_operation::UserOperation,
    effect: &mut super::settings::CommittedEffect,
) -> Result<Json<PlatformAccounts>, V3ApiError> {
    let input = parse_mutation_json::<PlatformCreate>(&body)?;
    let _lock = state.settings_update.lock();
    check_expectation(state, &input.expectation)?;
    let credential = input
        .user_credential
        .as_deref()
        .filter(|s| !s.trim().is_empty())
        .map(|s| state.encrypt_key(s.trim()))
        .transpose()
        .map_err(V3ApiError::internal)?;
    let id = uuid::Uuid::new_v4().to_string();
    op.subject(id.clone());
    state
        .db
        .lock()
        .create_platform_account(
            &id,
            input.kind,
            &input.name,
            &input.base_url,
            credential.as_deref(),
        )
        .map_err(|e| V3ApiError::invalid_request_at(state, e.to_string()))?;
    state.bump_settings_revision();
    effect.note_follow_up(view(state).map(Json))
}

pub(super) async fn update(
    State(state): State<CoreState>,
    Path(id): Path<String>,
    body: Bytes,
) -> Result<Json<PlatformAccounts>, V3ApiError> {
    let op = super::settings::open_dashboard(
        &state,
        "platform.update",
        "platform",
        super::settings::known_subject(&id),
    );
    let mut effect = super::settings::CommittedEffect::Atomic;
    let result = update_inner(state.clone(), id, body, &mut effect).await;
    super::settings::record_effect(
        op,
        &state,
        &[],
        (Some(1), Some(1), None),
        None,
        effect,
        result,
    )
}

async fn update_inner(
    state: CoreState,
    id: String,
    body: Bytes,
    effect: &mut super::settings::CommittedEffect,
) -> Result<Json<PlatformAccounts>, V3ApiError> {
    let input = parse_mutation_json::<PlatformUpdate>(&body)?;
    let _lock = state.settings_update.lock();
    check_expectation(&state, &input.expectation)?;
    let credential = input
        .user_credential
        .as_deref()
        .filter(|s| !s.trim().is_empty())
        .map(|s| state.encrypt_key(s.trim()))
        .transpose()
        .map_err(V3ApiError::internal)?;
    state
        .db
        .lock()
        .update_platform_account(
            &id,
            &input.name,
            input
                .user_credential
                .as_ref()
                .map(|_| credential.as_deref()),
        )
        .map_err(|e| V3ApiError::invalid_request_at(&state, e.to_string()))?;
    state.bump_settings_revision();
    effect.note_follow_up(view(&state).map(Json))
}

pub(super) async fn delete(
    State(state): State<CoreState>,
    Path(id): Path<String>,
    body: Bytes,
) -> Result<Json<MutationAck>, V3ApiError> {
    let op = super::settings::open_dashboard(
        &state,
        "platform.delete",
        "platform",
        super::settings::known_subject(&id),
    );
    let result = delete_inner(state.clone(), id, body).await;
    super::settings::record_after(op, &state, &[], (Some(1), Some(1), None), None, result)
}

async fn delete_inner(
    state: CoreState,
    id: String,
    body: Bytes,
) -> Result<Json<MutationAck>, V3ApiError> {
    let input = parse_mutation_json::<MutationExpectation>(&body)?;
    let _lock = state.settings_update.lock();
    check_expectation(&state, &input)?;
    state
        .db
        .lock()
        .delete_platform_account(&id)
        .map_err(|e| V3ApiError::invalid_request_at(&state, e.to_string()))?;
    Ok(Json(MutationAck {
        revision: state.bump_settings_revision(),
        process_generation: state.process_generation(),
    }))
}

pub(super) async fn link(
    State(state): State<CoreState>,
    Path(id): Path<String>,
    body: Bytes,
) -> Result<Json<PlatformAccounts>, V3ApiError> {
    let op = super::settings::open_dashboard(
        &state,
        "platform.link",
        "platform",
        super::settings::known_subject(&id),
    );
    let mut effect = super::settings::CommittedEffect::Atomic;
    let result = link_inner(state.clone(), id, body, &mut effect).await;
    super::settings::record_effect(
        op,
        &state,
        &["group"],
        (Some(1), Some(1), None),
        None,
        effect,
        result,
    )
}

async fn link_inner(
    state: CoreState,
    id: String,
    body: Bytes,
    effect: &mut super::settings::CommittedEffect,
) -> Result<Json<PlatformAccounts>, V3ApiError> {
    let input = parse_mutation_json::<PlatformLinkWrite>(&body)?;
    let _lock = state.settings_update.lock();
    check_expectation(&state, &input.expectation)?;
    state
        .db
        .lock()
        .link_platform_account(&id, &input.platform_account_id, &input.group)
        .map_err(|e| V3ApiError::invalid_request_at(&state, e.to_string()))?;
    state.bump_settings_revision();
    effect.note_follow_up(
        state
            .reload_provider_contracts()
            .map_err(V3ApiError::internal),
    )?;
    effect.note_follow_up(view(&state).map(Json))
}

pub(super) async fn unlink(
    State(state): State<CoreState>,
    Path(id): Path<String>,
    body: Bytes,
) -> Result<Json<PlatformAccounts>, V3ApiError> {
    let op = super::settings::open_dashboard(
        &state,
        "platform.unlink",
        "platform",
        super::settings::known_subject(&id),
    );
    let mut effect = super::settings::CommittedEffect::Atomic;
    let result = unlink_inner(state.clone(), id, body, &mut effect).await;
    super::settings::record_effect(
        op,
        &state,
        &[],
        (Some(1), Some(1), None),
        None,
        effect,
        result,
    )
}

async fn unlink_inner(
    state: CoreState,
    id: String,
    body: Bytes,
    effect: &mut super::settings::CommittedEffect,
) -> Result<Json<PlatformAccounts>, V3ApiError> {
    let input = parse_mutation_json::<MutationExpectation>(&body)?;
    let _lock = state.settings_update.lock();
    check_expectation(&state, &input)?;
    state
        .db
        .lock()
        .unlink_platform_account(&id)
        .map_err(V3ApiError::internal)?;
    state.bump_settings_revision();
    effect.note_follow_up(view(&state).map(Json))
}

pub(super) async fn refresh(
    State(state): State<CoreState>,
    Path(id): Path<String>,
    body: Bytes,
) -> Result<Json<PlatformAccounts>, V3ApiError> {
    let op = super::settings::open_dashboard(
        &state,
        "platform.refresh",
        "platform",
        super::settings::known_subject(&id),
    );
    let result = refresh_inner(state.clone(), id, body).await;
    let (ok_outcome, counts) = match &result {
        Ok((_, errors)) if *errors > 0 => (
            Some((
                crate::log_types::OperationOutcome::Partial,
                "outboundFailed",
            )),
            (Some(1), Some(1), Some(*errors)),
        ),
        Ok(_) => (None, (Some(1), Some(1), None)),
        Err(_) => (None, (Some(1), None, Some(1))),
    };
    let result = result.map(|(view, _)| view);
    super::settings::record_after(op, &state, &[], counts, ok_outcome, result)
}

async fn refresh_inner(
    state: CoreState,
    id: String,
    body: Bytes,
) -> Result<(Json<PlatformAccounts>, u32), V3ApiError> {
    let input = parse_mutation_json::<PlatformRefresh>(&body)?;
    let (parent, group, credential, key, token) = {
        let _lock = state.settings_update.lock();
        check_expectation(&state, &input.expectation)?;
        let db = state.db.lock();
        let parent = db
            .platform_account(&id)
            .map_err(V3ApiError::internal)?
            .ok_or_else(|| V3ApiError::not_found_at(&state, "platform account not found"))?;
        let token = db
            .platform_refresh_token(&id, input.account_id.as_deref())
            .map_err(|_| {
                V3ApiError::invalid_request_at(&state, "Key is not linked to this platform account")
            })?;
        let credential = db
            .platform_credential_cipher(&id)
            .map_err(V3ApiError::internal)?
            .map(|s| state.decrypt_key(&s))
            .transpose()
            .map_err(V3ApiError::internal)?;
        let (group, key) = if let Some(account_id) = &input.account_id {
            let link = db
                .list_platform_links()
                .map_err(V3ApiError::internal)?
                .into_iter()
                .find(|l| l.account_id == *account_id)
                .ok_or_else(|| V3ApiError::not_found_at(&state, "link not found"))?;
            let account = db
                .get_account(account_id)
                .map_err(V3ApiError::internal)?
                .ok_or_else(|| V3ApiError::not_found_at(&state, "Key not found"))?;
            (
                link.group,
                Some(
                    state
                        .decrypt_key(&account.key_cipher)
                        .map_err(V3ApiError::internal)?,
                ),
            )
        } else {
            (PlatformGroup::default(), None)
        };
        (parent, group, credential, key, token)
    };
    let client =
        crate::http_client::build_no_redirect(&state.config()).map_err(V3ApiError::internal)?;
    let mut snapshot = reader::read(
        &client,
        &PlatformReadRequest {
            kind: parent.kind,
            base_url: &parent.base_url,
            user_credential: credential.as_deref(),
            key: key.as_deref(),
            group: &group,
            now: chrono::Utc::now().timestamp(),
        },
    )
    .await;
    let error_count = u32::try_from(snapshot.errors.len()).unwrap_or(u32::MAX);
    if error_count > 0 {
        snapshot.stale = true;
    }
    // Do not copy user-credential balances into every child. New API Key
    // refresh also skips those user-scoped fetches. Key-authenticated
    // observations remain on that Key; a manual parent link is not ownership proof.
    if input.account_id.is_some() {
        snapshot.quotas.retain(|q| {
            matches!(q.kind, PlatformQuotaKind::KeyLimit) || q.source == "sub2api.v1.usage"
        });
    } else {
        snapshot
            .quotas
            .retain(|q| !matches!(q.kind, PlatformQuotaKind::KeyLimit));
    }
    let _lock = state.settings_update.lock();
    if !state
        .db
        .lock()
        .save_platform_refresh(&id, input.account_id.as_deref(), &token, &snapshot)
        .map_err(V3ApiError::internal)?
    {
        return Err(V3ApiError::conflict_at(
            &state,
            "platform account or Key changed during refresh; retry",
        ));
    }
    // The snapshot is an observation. Inference reads it from SQLite on the
    // next attempt, and platform_version / link_version already reject a stale
    // refresh. Leave the configuration CAS token unchanged.
    view(&state).map(|view| (Json(view), error_count))
}

#[cfg(test)]
mod tests;
