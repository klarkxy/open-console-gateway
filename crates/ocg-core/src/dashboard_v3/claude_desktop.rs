//! GET/PUT `/claude-desktop/models` — three-role Claude Desktop mapping.
//!
//! Reuses V2 `models::ClaudeDesktopModels` normalize/validate/resolved and
//! persists through `set_config` under `settings_update`. Host hooks and the
//! gateway listener are not touched.

use axum::Json;
use axum::body::Bytes;
use axum::extract::State;

use crate::models::ClaudeDesktopModels as AppClaudeDesktopModels;
use crate::state::CoreState;

use super::types::{ClaudeDesktopModels, ClaudeDesktopModelsUpdate};
use super::{V3ApiError, check_expectation, parse_mutation_json};

pub(super) async fn get_claude_desktop_models(
    State(state): State<CoreState>,
) -> Json<ClaudeDesktopModels> {
    let _settings_update = state.settings_update.lock();
    Json(models_from_state(&state))
}

pub(super) async fn put_claude_desktop_models(
    State(state): State<CoreState>,
    body: Bytes,
) -> Result<Json<ClaudeDesktopModels>, V3ApiError> {
    let update = parse_mutation_json::<ClaudeDesktopModelsUpdate>(&body)?;
    update_claude_desktop_models(&state, update).map(Json)
}

/// Validates the three-role mapping, then commits through `set_config`.
/// Bumps the unified revision exactly once on a real change. Identical
/// normalized mappings are a no-op. The primary Key and every unrelated
/// `AppConfig` field are preserved by cloning the live config.
fn update_claude_desktop_models(
    state: &CoreState,
    update: ClaudeDesktopModelsUpdate,
) -> Result<ClaudeDesktopModels, V3ApiError> {
    let _settings_update = state.settings_update.lock();
    check_expectation(state, &update.expectation)?;

    let mut next = AppClaudeDesktopModels {
        sonnet: update.sonnet,
        opus: update.opus,
        haiku: update.haiku,
    };
    next.normalize();
    next.validate()
        .map_err(|message| V3ApiError::invalid_request_at(state, message))?;

    let mut config = state.config();
    if config.claude_desktop_models == next {
        return Ok(models_from_state(state));
    }
    config.claude_desktop_models = next;
    state.set_config(config).map_err(V3ApiError::internal)?;
    Ok(models_from_state(state))
}

fn models_from_state(state: &CoreState) -> ClaudeDesktopModels {
    let resolved = state.config().claude_desktop_models.resolved();
    ClaudeDesktopModels {
        sonnet: resolved.sonnet,
        opus: resolved.opus,
        haiku: resolved.haiku,
        revision: state.settings_revision(),
        process_generation: state.process_generation(),
    }
}
