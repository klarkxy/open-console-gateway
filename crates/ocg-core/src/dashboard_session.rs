//! Shared Dashboard V3 session and single-admin authentication policy.
//!
//! Cookie attributes, loopback trust, forwarded-header fail-closed checks,
//! Argon2 register/login, session rotation, and remote-browser invalidation
//! live here. Wire envelopes stay in the dashboard_v3 module. Callers pass a
//! database mutex, browser runtime, session token mutex, local-mode flag, and
//! request headers — this module does not import host state.
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

pub(crate) const SESSION_COOKIE: &str = "ocg_dashboard_session";

const FORWARDED_TRUST_HEADERS: [&str; 5] = [
    "forwarded",
    "x-forwarded-for",
    "x-forwarded-proto",
    "x-forwarded-host",
    "x-real-ip",
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DashboardAuthStatus {
    pub local: bool,
    pub initialized: bool,
    pub authenticated: bool,
}

#[derive(Debug)]
pub(crate) enum RegisterError {
    Invalid(String),
    AlreadyExists,
    Internal(String),
}

#[derive(Debug)]
pub(crate) struct Unauthorized;

/// A validated, Argon2-hashed administrator ready for the short persistence
/// critical section. Constructing this value is deliberately separate from
/// persistence so password hashing never runs while a DB or control-plane
/// mutation lock is held.
pub(crate) struct PreparedAdmin(auth::DashboardAdmin);

pub(crate) fn is_local_dashboard_request(dashboard_local_mode: bool, headers: &HeaderMap) -> bool {
    if !dashboard_local_mode
        || FORWARDED_TRUST_HEADERS
            .iter()
            .any(|name| headers.contains_key(*name))
    {
        return false;
    }
    has_local_dashboard_authority(headers)
}

pub(crate) fn has_local_dashboard_authority(headers: &HeaderMap) -> bool {
    let Some(host) = single_header(headers, "host")
        .and_then(|value| value.parse::<axum::http::uri::Authority>().ok())
    else {
        return false;
    };
    let hostname = host.host().trim_start_matches('[').trim_end_matches(']');
    if host.as_str().contains('@')
        || (host.as_str().len() != host.host().len() && host.port_u16().is_none())
        || !(hostname.eq_ignore_ascii_case("localhost")
            || hostname
                .parse::<std::net::IpAddr>()
                .is_ok_and(|ip| ip.is_loopback()))
    {
        return false;
    }
    if headers.contains_key("sec-fetch-site")
        && !matches!(
            single_header(headers, "sec-fetch-site"),
            Some("same-origin" | "same-site" | "none")
        )
    {
        return false;
    }
    // Native clients omit Origin. Browser requests must name this exact
    // loopback authority; Vite preserves its original Host when proxying.
    if !headers.contains_key(header::ORIGIN) {
        return true;
    }
    let Some(origin) = single_header(headers, "origin")
        .filter(|value| !value.contains('#'))
        .and_then(|value| value.parse::<axum::http::Uri>().ok())
    else {
        return false;
    };
    let default_port = match origin.scheme_str() {
        Some("http") => 80,
        Some("https") => 443,
        _ => return false,
    };
    let Some(authority) = origin.authority() else {
        return false;
    };
    !authority.as_str().contains('@')
        && (authority.as_str().len() == authority.host().len() || authority.port_u16().is_some())
        && authority.host().eq_ignore_ascii_case(host.host())
        && authority.port_u16().unwrap_or(default_port) == host.port_u16().unwrap_or(default_port)
        && matches!(origin.path(), "" | "/")
        && origin.query().is_none()
}

fn single_header<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    let mut values = headers.get_all(name).iter();
    let value = values.next()?.to_str().ok()?;
    values.next().is_none().then_some(value)
}

pub(crate) fn has_dashboard_session(current_token: &str, headers: &HeaderMap) -> bool {
    session_cookie_value(headers)
        .map(|value| value == current_token)
        .unwrap_or(false)
}

pub(crate) fn is_authorized(
    dashboard_local_mode: bool,
    current_token: &str,
    headers: &HeaderMap,
) -> bool {
    is_local_dashboard_request(dashboard_local_mode, headers)
        || has_dashboard_session(current_token, headers)
}

pub(crate) fn session_cookie_value(headers: &HeaderMap) -> Option<&str> {
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

pub(crate) fn is_initialized(db: &Mutex<Database>) -> Result<bool> {
    let db = db.lock();
    Ok(auth::load_admin(&db)?.is_some())
}

pub(crate) fn status(
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

pub(crate) fn prepare_admin(
    username: &str,
    password: &str,
) -> std::result::Result<PreparedAdmin, RegisterError> {
    auth::build_admin(username, password)
        .map(PreparedAdmin)
        .map_err(|error| RegisterError::Invalid(error.to_string()))
}

pub(crate) fn save_prepared_admin_if_absent(
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

pub(crate) fn register_admin(
    db: &Mutex<Database>,
    username: &str,
    password: &str,
) -> std::result::Result<(), RegisterError> {
    let admin = prepare_admin(username, password)?;
    save_prepared_admin_if_absent(db, &admin)
}

pub(crate) fn credentials_match(
    db: &Mutex<Database>,
    username: &str,
    password: &str,
) -> Result<bool> {
    let admin = {
        let db = db.lock();
        auth::load_admin(&db)?
    };
    Ok(admin
        .as_ref()
        .map(|admin| auth::verify_admin(admin, username, password))
        .unwrap_or(false))
}

pub(crate) async fn issue_session(
    browser: &BrowserRuntime,
    session_token: &Mutex<String>,
) -> String {
    let _browser_operation = browser.operation().await;
    rotate_session_under_operation(browser, session_token)
}

pub(crate) async fn logout(
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

pub(crate) fn cookie_header(
    value: &str,
    request_headers: &HeaderMap,
    clear: bool,
) -> Result<HeaderValue> {
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
mod tests;
