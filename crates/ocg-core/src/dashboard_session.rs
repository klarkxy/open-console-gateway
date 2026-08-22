//! Shared Dashboard V2/V3 session and single-admin authentication policy.
//!
//! Cookie attributes, loopback trust, forwarded-header fail-closed checks,
//! Argon2 register/login, session rotation, and remote-browser invalidation
//! live here so V2 and V3 cannot diverge. Wire envelopes stay in each
//! dashboard module. Callers pass a database mutex, browser runtime, session
//! token mutex, local-mode flag, and request headers — this module does not
//! import host state.
//!
//! Registration persists the first administrator only and does **not** bump
//! `settings_revision`, matching historical V2 `/auth/register` semantics.
//! Login and logout are session-only and also leave the control revision
//! unchanged.

use anyhow::Result;
use axum::http::{HeaderMap, HeaderValue, header};
use parking_lot::Mutex;

use crate::auth;
use crate::browser::BrowserRuntime;
use crate::db::Database;

pub const SESSION_COOKIE: &str = "ocg_dashboard_session";

const FORWARDED_TRUST_HEADERS: [&str; 4] = [
    "forwarded",
    "x-forwarded-for",
    "x-forwarded-proto",
    "x-real-ip",
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DashboardAuthStatus {
    pub local: bool,
    pub initialized: bool,
    pub authenticated: bool,
}

#[derive(Debug)]
pub enum RegisterError {
    Invalid(String),
    AlreadyExists,
    Internal(String),
}

#[derive(Debug)]
pub struct Unauthorized;

/// A validated, Argon2-hashed administrator ready for the short persistence
/// critical section. Constructing this value is deliberately separate from
/// persistence so password hashing never runs while a DB or control-plane
/// mutation lock is held.
pub struct PreparedAdmin(auth::DashboardAdmin);

pub fn is_local_dashboard_request(dashboard_local_mode: bool, headers: &HeaderMap) -> bool {
    dashboard_local_mode
        && FORWARDED_TRUST_HEADERS
            .iter()
            .all(|name| !headers.contains_key(*name))
}

pub fn has_dashboard_session(current_token: &str, headers: &HeaderMap) -> bool {
    session_cookie_value(headers)
        .map(|value| value == current_token)
        .unwrap_or(false)
}

pub fn is_authorized(dashboard_local_mode: bool, current_token: &str, headers: &HeaderMap) -> bool {
    is_local_dashboard_request(dashboard_local_mode, headers)
        || has_dashboard_session(current_token, headers)
}

pub fn session_cookie_value(headers: &HeaderMap) -> Option<&str> {
    headers
        .get(header::COOKIE)
        .and_then(|value| value.to_str().ok())
        .and_then(|cookies| {
            cookies.split(';').find_map(|cookie| {
                let (name, value) = cookie.trim().split_once('=')?;
                (name == SESSION_COOKIE).then_some(value)
            })
        })
}

pub fn is_initialized(db: &Mutex<Database>) -> Result<bool> {
    let db = db.lock();
    Ok(auth::load_admin(&db)?.is_some())
}

pub fn status(
    dashboard_local_mode: bool,
    db: &Mutex<Database>,
    session_token: &Mutex<String>,
    headers: &HeaderMap,
) -> Result<DashboardAuthStatus> {
    let local = is_local_dashboard_request(dashboard_local_mode, headers);
    // Preserve the historical V2 linearization order: initialization is read
    // first, then the live token. In particular, never clone a token and wait
    // on the DB afterwards, because a concurrent logout could then complete
    // while this response still reports the old cookie as authenticated.
    let initialized = is_initialized(db)?;
    let authenticated = {
        let current_token = session_token.lock();
        local || has_dashboard_session(current_token.as_str(), headers)
    };
    Ok(DashboardAuthStatus {
        local,
        initialized,
        authenticated,
    })
}

pub fn prepare_admin(
    username: &str,
    password: &str,
) -> std::result::Result<PreparedAdmin, RegisterError> {
    auth::build_admin(username, password)
        .map(PreparedAdmin)
        .map_err(|error| RegisterError::Invalid(error.to_string()))
}

pub fn save_prepared_admin_if_absent(
    db: &Mutex<Database>,
    admin: &PreparedAdmin,
) -> std::result::Result<(), RegisterError> {
    let db = db.lock();
    if auth::load_admin(&db)
        .map_err(|error| RegisterError::Internal(error.to_string()))?
        .is_some()
    {
        return Err(RegisterError::AlreadyExists);
    }
    auth::save_admin(&db, &admin.0).map_err(|error| RegisterError::Internal(error.to_string()))
}

pub fn register_admin(
    db: &Mutex<Database>,
    username: &str,
    password: &str,
) -> std::result::Result<(), RegisterError> {
    let admin = prepare_admin(username, password)?;
    save_prepared_admin_if_absent(db, &admin)
}

pub fn credentials_match(db: &Mutex<Database>, username: &str, password: &str) -> Result<bool> {
    let admin = {
        let db = db.lock();
        auth::load_admin(&db)?
    };
    Ok(admin
        .as_ref()
        .map(|admin| auth::verify_admin(admin, username, password))
        .unwrap_or(false))
}

pub async fn issue_session(browser: &BrowserRuntime, session_token: &Mutex<String>) -> String {
    let _browser_operation = browser.operation().await;
    rotate_session_under_operation(browser, session_token)
}

pub async fn logout(
    dashboard_local_mode: bool,
    session_token: &Mutex<String>,
    browser: &BrowserRuntime,
    headers: &HeaderMap,
) -> std::result::Result<(), Unauthorized> {
    if !authorized_now(dashboard_local_mode, session_token, headers) {
        return Err(Unauthorized);
    }
    let _browser_operation = browser.operation().await;
    rotate_session_if_authorized_under_operation(
        dashboard_local_mode,
        browser,
        session_token,
        headers,
    )?;
    Ok(())
}

fn authorized_now(
    dashboard_local_mode: bool,
    session_token: &Mutex<String>,
    headers: &HeaderMap,
) -> bool {
    let current = session_token.lock();
    is_authorized(dashboard_local_mode, current.as_str(), headers)
}

/// Rotate the Dashboard session while the caller owns `browser.operation()`.
/// Keeping this synchronous lets V3 place its final CAS check and the whole
/// side effect in one short `settings_update` critical section without ever
/// holding a synchronous lock across an await.
pub(crate) fn rotate_session_under_operation(
    browser: &BrowserRuntime,
    session_token: &Mutex<String>,
) -> String {
    let token = uuid::Uuid::new_v4().simple().to_string();
    let mut current = session_token.lock();
    browser.invalidate_remote_sessions();
    current.clone_from(&token);
    token
}

/// Authorize against and replace the same live token while the caller owns
/// `browser.operation()`. This makes concurrent logout linearizable: exactly
/// one request using an old cookie may rotate it; a loser observes 401 and can
/// never invalidate a newer session.
pub(crate) fn rotate_session_if_authorized_under_operation(
    dashboard_local_mode: bool,
    browser: &BrowserRuntime,
    session_token: &Mutex<String>,
    headers: &HeaderMap,
) -> std::result::Result<String, Unauthorized> {
    let mut current = session_token.lock();
    if !is_authorized(dashboard_local_mode, current.as_str(), headers) {
        return Err(Unauthorized);
    }
    browser.invalidate_remote_sessions();
    let token = uuid::Uuid::new_v4().simple().to_string();
    current.clone_from(&token);
    Ok(token)
}

pub fn cookie_header(value: &str, request_headers: &HeaderMap, clear: bool) -> Result<HeaderValue> {
    let mut cookie =
        format!("{SESSION_COOKIE}={value}; HttpOnly; SameSite=Strict; Path=/dashboard");
    if clear {
        cookie.push_str("; Max-Age=0");
    }
    if request_headers
        .get("x-forwarded-proto")
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.eq_ignore_ascii_case("https"))
    {
        cookie.push_str("; Secure");
    }
    HeaderValue::from_str(&cookie)
        .map_err(|error| anyhow::anyhow!("invalid session cookie header: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;

    #[test]
    fn cookie_is_httponly_strict_and_scoped_to_dashboard() {
        let headers = HeaderMap::new();
        assert_eq!(
            cookie_header("abc123", &headers, false)
                .unwrap()
                .to_str()
                .unwrap(),
            "ocg_dashboard_session=abc123; HttpOnly; SameSite=Strict; Path=/dashboard"
        );
    }

    #[test]
    fn cleared_cookie_sets_max_age_zero_and_empty_value() {
        let headers = HeaderMap::new();
        assert_eq!(
            cookie_header("", &headers, true).unwrap().to_str().unwrap(),
            "ocg_dashboard_session=; HttpOnly; SameSite=Strict; Path=/dashboard; Max-Age=0"
        );
    }

    #[test]
    fn secure_is_inferred_only_from_https_forwarded_proto() {
        let mut headers = HeaderMap::new();
        headers.insert("x-forwarded-proto", HeaderValue::from_static("https"));
        assert!(
            cookie_header("tok", &headers, false)
                .unwrap()
                .to_str()
                .unwrap()
                .ends_with("; Secure")
        );

        headers.insert("x-forwarded-proto", HeaderValue::from_static("HTTPS"));
        assert!(
            cookie_header("tok", &headers, false)
                .unwrap()
                .to_str()
                .unwrap()
                .contains("; Secure")
        );

        headers.insert("x-forwarded-proto", HeaderValue::from_static("http"));
        assert!(
            !cookie_header("tok", &headers, false)
                .unwrap()
                .to_str()
                .unwrap()
                .contains("Secure")
        );
    }

    #[test]
    fn loopback_trust_requires_local_mode_and_no_forwarded_headers() {
        let mut headers = HeaderMap::new();
        assert!(is_local_dashboard_request(true, &headers));
        assert!(!is_local_dashboard_request(false, &headers));
        headers.insert("x-forwarded-for", HeaderValue::from_static("203.0.113.10"));
        assert!(!is_local_dashboard_request(true, &headers));
    }

    #[test]
    fn session_cookie_matches_the_current_token_exactly() {
        let mut headers = HeaderMap::new();
        headers.insert(
            header::COOKIE,
            HeaderValue::from_static("theme=dark; ocg_dashboard_session=abc123"),
        );
        assert!(has_dashboard_session("abc123", &headers));
        assert!(!has_dashboard_session("other", &headers));
        assert!(is_authorized(false, "abc123", &headers));
        assert!(!is_authorized(false, "other", &headers));
        assert!(is_authorized(true, "other", &headers));
    }
}
