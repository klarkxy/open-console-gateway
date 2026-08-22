//! V3 dashboard authentication: public status plus CAS-gated register/login/logout.
//!
//! Session cookies, single-admin registration, Argon2 verification, rotation,
//! and forwarded-header trust are implemented in `dashboard_session` and shared
//! with V2. This module only owns the V3 envelope, CAS prechecks, and JSON
//! 401/409 mapping. Registration still does not bump `settings_revision`.
//! Credential hashing/verification runs outside synchronous locks. The async
//! browser-operation gate is then acquired before the final `settings_update`
//! CAS check and the synchronous persistence/session side effect, so a token
//! that becomes stale during either expensive phase has no side effects.

use axum::Json;
use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};

use crate::dashboard_session::{self, RegisterError};
use crate::state::CoreState;

use super::types::{AuthLogin, AuthLogout, AuthRegister, AuthStatus, MutationExpectation};
use super::{V3ApiError, check_expectation, parse_mutation_json};

pub(super) async fn auth_status(
    State(state): State<CoreState>,
    headers: HeaderMap,
) -> Result<Json<AuthStatus>, V3ApiError> {
    Ok(Json(status_from_request(&state, &headers)?))
}

pub(super) async fn register_admin(
    State(state): State<CoreState>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Response, V3ApiError> {
    let input = parse_mutation_json::<AuthRegister>(&body)?;
    // Preserve CAS precedence (including for malformed credentials), but
    // cheaply reject an already-initialized single-admin database before
    // paying the Argon2 cost.
    {
        let _settings_update = state.settings_update.lock();
        check_expectation(&state, &input.expectation)?;
        if dashboard_session::is_initialized(&state.db).map_err(V3ApiError::internal)? {
            return Err(V3ApiError::conflict_at(
                &state,
                "administrator is already registered",
            ));
        }
    }

    let admin = dashboard_session::prepare_admin(&input.username, &input.password)
        .map_err(|error| map_register_error(&state, error))?;
    let browser_operation = state.browser.operation().await;
    let token = {
        let _settings_update = state.settings_update.lock();
        check_expectation(&state, &input.expectation)?;
        dashboard_session::save_prepared_admin_if_absent(&state.db, &admin)
            .map_err(|error| map_register_error(&state, error))?;
        dashboard_session::rotate_session_under_operation(
            &state.browser,
            &state.dashboard_session_token,
        )
    };
    let response = session_json(
        &headers,
        StatusCode::CREATED,
        issued_status(&state, &headers)?,
        &token,
        false,
    );
    drop(browser_operation);
    response
}

pub(super) async fn login_admin(
    State(state): State<CoreState>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Response, V3ApiError> {
    let input = parse_mutation_json::<AuthLogin>(&body)?;
    require_expectation(&state, &input.expectation)?;
    let valid = dashboard_session::credentials_match(&state.db, &input.username, &input.password)
        .map_err(V3ApiError::internal)?;
    if !valid {
        return Err(V3ApiError::unauthorized_credentials());
    }
    let browser_operation = state.browser.operation().await;
    let token = {
        let _settings_update = state.settings_update.lock();
        check_expectation(&state, &input.expectation)?;
        dashboard_session::rotate_session_under_operation(
            &state.browser,
            &state.dashboard_session_token,
        )
    };
    let response = session_json(
        &headers,
        StatusCode::OK,
        issued_status(&state, &headers)?,
        &token,
        false,
    );
    drop(browser_operation);
    response
}

pub(super) async fn logout_admin(
    State(state): State<CoreState>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Response, V3ApiError> {
    let input = parse_mutation_json::<AuthLogout>(&body)?;
    require_expectation(&state, &input.expectation)?;
    if !dashboard_session::is_authorized(
        state.dashboard_local_mode(),
        state.dashboard_session_token.lock().as_str(),
        &headers,
    ) {
        return Err(V3ApiError::unauthorized());
    }
    let browser_operation = state.browser.operation().await;
    {
        let _settings_update = state.settings_update.lock();
        check_expectation(&state, &input.expectation)?;
        dashboard_session::rotate_session_if_authorized_under_operation(
            state.dashboard_local_mode(),
            &state.browser,
            &state.dashboard_session_token,
            &headers,
        )
        .map_err(|_| V3ApiError::unauthorized())?;
    }
    let response = session_json(
        &headers,
        StatusCode::OK,
        status_from_request(&state, &headers)?,
        "",
        true,
    );
    drop(browser_operation);
    response
}

fn require_expectation(
    state: &CoreState,
    expectation: &MutationExpectation,
) -> Result<(), V3ApiError> {
    let _settings_update = state.settings_update.lock();
    check_expectation(state, expectation)
}

fn status_from_request(state: &CoreState, headers: &HeaderMap) -> Result<AuthStatus, V3ApiError> {
    let snapshot = dashboard_session::status(
        state.dashboard_local_mode(),
        &state.db,
        &state.dashboard_session_token,
        headers,
    )
    .map_err(V3ApiError::internal)?;
    Ok(AuthStatus {
        local: snapshot.local,
        initialized: snapshot.initialized,
        authenticated: snapshot.authenticated,
        revision: state.settings_revision(),
        process_generation: state.process_generation(),
    })
}

fn map_register_error(state: &CoreState, error: RegisterError) -> V3ApiError {
    match error {
        RegisterError::Invalid(message) => V3ApiError::invalid_request_at(state, message),
        RegisterError::AlreadyExists => {
            V3ApiError::conflict_at(state, "administrator is already registered")
        }
        RegisterError::Internal(message) => V3ApiError::internal(message),
    }
}

fn issued_status(state: &CoreState, headers: &HeaderMap) -> Result<AuthStatus, V3ApiError> {
    let mut status = status_from_request(state, headers)?;
    status.initialized = true;
    status.authenticated = true;
    Ok(status)
}

fn session_json(
    headers: &HeaderMap,
    status: StatusCode,
    body: AuthStatus,
    cookie_value: &str,
    clear: bool,
) -> Result<Response, V3ApiError> {
    let mut response = (status, Json(body)).into_response();
    response.headers_mut().insert(
        header::SET_COOKIE,
        dashboard_session::cookie_header(cookie_value, headers, clear)
            .map_err(V3ApiError::internal)?,
    );
    Ok(response)
}

#[cfg(test)]
mod concurrency_tests {
    use super::*;
    use crate::crypto::{KeyCipher, StaticKeyCipher};
    use crate::db::Database;
    use crate::state::CoreStateInner;
    use serde_json::json;
    use std::fs;
    use std::future::Future;
    use std::sync::Arc;

    fn test_state(label: &str) -> CoreState {
        let dir = std::env::temp_dir().join(format!(
            "ocg-v3-auth-concurrency-{label}-{}",
            uuid::Uuid::new_v4().simple()
        ));
        fs::create_dir_all(&dir).unwrap();
        let db = Database::open(dir.clone()).unwrap();
        let cipher: Arc<dyn KeyCipher + Send + Sync> =
            Arc::new(StaticKeyCipher::new("v3-auth-concurrency"));
        Arc::new(CoreStateInner::new(db, dir, cipher).unwrap())
    }

    fn mutation_body(state: &CoreState, extra: serde_json::Value) -> Bytes {
        let mut object = extra.as_object().unwrap().clone();
        object.insert("expectedRevision".into(), json!(state.settings_revision()));
        object.insert(
            "processGeneration".into(),
            json!(state.process_generation()),
        );
        Bytes::from(serde_json::to_vec(&object).unwrap())
    }

    async fn poll_through_sync_phase<F, T>(future: &mut std::pin::Pin<Box<F>>)
    where
        F: Future<Output = T>,
    {
        // The test owns browser.operation(), so the auth handler's first await
        // cannot complete. A biased poll therefore executes the whole real
        // synchronous phase (including Argon2) and deterministically parks at
        // the browser gate before the timer branch wins.
        tokio::select! {
            biased;
            _ = future.as_mut() => panic!("auth mutation bypassed the browser-operation gate"),
            _ = tokio::time::sleep(std::time::Duration::from_millis(1)) => {}
        }
    }

    fn bump_revision(state: &CoreState) {
        let _settings_update = state.settings_update.lock();
        state.bump_settings_revision();
    }

    fn assert_revision_conflict(error: V3ApiError, state: &CoreState) {
        assert_eq!(error.status, StatusCode::CONFLICT);
        assert_eq!(error.body.code, super::super::ERROR_REVISION_CONFLICT);
        assert_eq!(error.body.current_revision, Some(state.settings_revision()));
    }

    #[tokio::test]
    async fn register_rechecks_cas_after_argon2_and_browser_wait() {
        let state = test_state("stale-register");
        let original_token = state.dashboard_session_token.lock().clone();
        let browser_operation = state.browser.operation().await;
        let mut request = Box::pin(register_admin(
            State(state.clone()),
            HeaderMap::new(),
            mutation_body(
                &state,
                json!({"username": "admin", "password": "password123"}),
            ),
        ));

        poll_through_sync_phase(&mut request).await;
        assert!(state.settings_update.try_lock().is_some());
        assert!(state.db.try_lock().is_some());
        bump_revision(&state);
        drop(browser_operation);

        assert_revision_conflict(request.await.unwrap_err(), &state);
        assert!(!dashboard_session::is_initialized(&state.db).unwrap());
        assert_eq!(
            state.dashboard_session_token.lock().as_str(),
            original_token
        );
    }

    #[tokio::test]
    async fn login_rechecks_cas_after_argon2_and_browser_wait() {
        let state = test_state("stale-login");
        dashboard_session::register_admin(&state.db, "admin", "password123").unwrap();
        let original_token = state.dashboard_session_token.lock().clone();
        let browser_operation = state.browser.operation().await;
        let mut request = Box::pin(login_admin(
            State(state.clone()),
            HeaderMap::new(),
            mutation_body(
                &state,
                json!({"username": "admin", "password": "password123"}),
            ),
        ));

        poll_through_sync_phase(&mut request).await;
        assert!(state.settings_update.try_lock().is_some());
        assert!(state.db.try_lock().is_some());
        bump_revision(&state);
        drop(browser_operation);

        assert_revision_conflict(request.await.unwrap_err(), &state);
        assert_eq!(
            state.dashboard_session_token.lock().as_str(),
            original_token
        );
    }

    #[tokio::test]
    async fn logout_rechecks_cas_after_browser_wait_without_rotating() {
        let state = test_state("stale-logout");
        let original_token = state.dashboard_session_token.lock().clone();
        let mut headers = HeaderMap::new();
        headers.insert(
            header::COOKIE,
            format!("{}={original_token}", dashboard_session::SESSION_COOKIE)
                .parse()
                .unwrap(),
        );
        let browser_operation = state.browser.operation().await;
        let mut request = Box::pin(logout_admin(
            State(state.clone()),
            headers,
            mutation_body(&state, json!({})),
        ));

        poll_through_sync_phase(&mut request).await;
        assert!(state.settings_update.try_lock().is_some());
        bump_revision(&state);
        drop(browser_operation);

        assert_revision_conflict(request.await.unwrap_err(), &state);
        assert_eq!(
            state.dashboard_session_token.lock().as_str(),
            original_token
        );
    }

    #[tokio::test]
    async fn concurrent_registration_has_one_commit_and_one_conflict() {
        let state = test_state("duplicate-register");
        let original_token = state.dashboard_session_token.lock().clone();
        let browser_operation = state.browser.operation().await;
        let mut first = Box::pin(register_admin(
            State(state.clone()),
            HeaderMap::new(),
            mutation_body(
                &state,
                json!({"username": "first", "password": "password123"}),
            ),
        ));
        let mut second = Box::pin(register_admin(
            State(state.clone()),
            HeaderMap::new(),
            mutation_body(
                &state,
                json!({"username": "second", "password": "password456"}),
            ),
        ));

        poll_through_sync_phase(&mut first).await;
        poll_through_sync_phase(&mut second).await;
        assert!(state.settings_update.try_lock().is_some());
        assert!(state.db.try_lock().is_some());
        drop(browser_operation);

        let (first, second) = tokio::join!(first, second);
        let statuses = [
            first
                .as_ref()
                .map(Response::status)
                .unwrap_or_else(|error| error.status),
            second
                .as_ref()
                .map(Response::status)
                .unwrap_or_else(|error| error.status),
        ];
        assert_eq!(
            statuses
                .iter()
                .filter(|status| **status == StatusCode::CREATED)
                .count(),
            1
        );
        assert_eq!(
            statuses
                .iter()
                .filter(|status| **status == StatusCode::CONFLICT)
                .count(),
            1
        );
        assert!(dashboard_session::is_initialized(&state.db).unwrap());
        assert_ne!(
            state.dashboard_session_token.lock().as_str(),
            original_token
        );
    }

    #[tokio::test]
    async fn simultaneous_logout_with_one_cookie_has_one_success_and_one_401() {
        let state = test_state("duplicate-logout");
        let original_token = state.dashboard_session_token.lock().clone();
        let headers = || {
            let mut headers = HeaderMap::new();
            headers.insert(
                header::COOKIE,
                format!("{}={original_token}", dashboard_session::SESSION_COOKIE)
                    .parse()
                    .unwrap(),
            );
            headers
        };
        let first = logout_admin(
            State(state.clone()),
            headers(),
            mutation_body(&state, json!({})),
        );
        let second = logout_admin(
            State(state.clone()),
            headers(),
            mutation_body(&state, json!({})),
        );
        let (first, second) = tokio::join!(first, second);
        let statuses = [
            first
                .as_ref()
                .map(Response::status)
                .unwrap_or_else(|error| error.status),
            second
                .as_ref()
                .map(Response::status)
                .unwrap_or_else(|error| error.status),
        ];
        assert_eq!(statuses.iter().filter(|s| **s == StatusCode::OK).count(), 1);
        assert_eq!(
            statuses
                .iter()
                .filter(|s| **s == StatusCode::UNAUTHORIZED)
                .count(),
            1
        );
    }
}
