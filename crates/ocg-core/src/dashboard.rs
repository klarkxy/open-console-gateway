//! Dashboard HTTP surface: the preserved V2 auth family, the browser
//! session WebSocket proxy, and static assets. Every other V2 REST handler
//! is retired; `host_router` tombstones those paths with 401/410 before
//! they can reach this router.

use crate::dashboard_session;
use crate::state::CoreState;
use axum::{
    Json, Router,
    body::Body,
    extract::{
        Path, Request, State, WebSocketUpgrade,
        ws::{Message as AxumWsMessage, WebSocket},
    },
    http::{HeaderMap, Response as HttpResponse, StatusCode, header, uri::Authority},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use std::path::{Component, Path as FsPath, PathBuf};
use std::str::FromStr;

pub fn api_router(state: CoreState) -> Router<CoreState> {
    // Preserved V2 families only: the browser session WebSocket stays behind
    // the dashboard session middleware, exactly as before retirement.
    let protected = Router::new()
        .route(
            "/browser/sessions/{token}/ws",
            get(browser_session_websocket),
        )
        .route_layer(middleware::from_fn_with_state(
            state,
            require_dashboard_session,
        ));

    Router::new()
        .route("/auth/status", get(auth_status))
        .route("/auth/register", post(register_admin))
        .route("/auth/login", post(login_admin))
        .route("/auth/logout", post(logout_admin))
        .merge(protected)
}

pub fn dashboard_dir(state: &CoreState) -> PathBuf {
    if let Some(dir) = state.dashboard_dir() {
        return dir;
    }
    if let Ok(dir) = std::env::var("OCG_DASHBOARD_DIR") {
        return PathBuf::from(dir);
    }
    if let Ok(exe) = std::env::current_exe()
        && let Some(parent) = exe.parent()
    {
        return parent.join("dist");
    }
    std::env::current_dir()
        .unwrap_or_else(|_| PathBuf::from("."))
        .join("dist")
}

pub async fn serve_index(State(state): State<CoreState>) -> impl IntoResponse {
    serve_file(dashboard_dir(&state).join("index.html")).await
}

pub async fn serve_asset(
    State(state): State<CoreState>,
    Path(path): Path<String>,
) -> impl IntoResponse {
    match asset_path(&dashboard_dir(&state), &path) {
        Some(path) => serve_file(path).await,
        None => StatusCode::BAD_REQUEST.into_response(),
    }
}

fn asset_path(dashboard_dir: &FsPath, raw: &str) -> Option<PathBuf> {
    if raw.contains('\\') || raw.contains(':') {
        return None;
    }
    let mut path = dashboard_dir.join("assets");
    for component in FsPath::new(raw).components() {
        match component {
            Component::Normal(part) => path.push(part),
            _ => return None,
        }
    }
    Some(path)
}

async fn serve_file(path: PathBuf) -> Response {
    match tokio::fs::read(&path).await {
        Ok(bytes) => HttpResponse::builder()
            .status(StatusCode::OK)
            .header(
                header::CONTENT_TYPE,
                content_type(path.extension().and_then(|s| s.to_str())),
            )
            .body(Body::from(bytes))
            .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response()),
        Err(_) => (
            StatusCode::NOT_FOUND,
            format!("dashboard file not found: {}", path.display()),
        )
            .into_response(),
    }
}

fn content_type(ext: Option<&str>) -> &'static str {
    match ext.unwrap_or_default() {
        "css" => "text/css; charset=utf-8",
        "html" => "text/html; charset=utf-8",
        "js" => "text/javascript; charset=utf-8",
        "png" => "image/png",
        "svg" => "image/svg+xml",
        "ico" => "image/x-icon",
        "woff2" => "font/woff2",
        _ => "application/octet-stream",
    }
}

#[derive(Serialize)]
struct AuthStatus {
    local: bool,
    initialized: bool,
    authenticated: bool,
}

#[derive(Deserialize)]
struct AdminCredentials {
    username: String,
    password: String,
}

async fn auth_status(
    State(state): State<CoreState>,
    headers: HeaderMap,
) -> Result<Json<AuthStatus>, ApiError> {
    let snapshot = dashboard_session::status(
        state.dashboard_local_mode(),
        &state.db,
        &state.dashboard_session_token,
        &headers,
    )
    .map_err(ApiError::internal)?;
    Ok(Json(AuthStatus {
        local: snapshot.local,
        initialized: snapshot.initialized,
        authenticated: snapshot.authenticated,
    }))
}

async fn register_admin(
    State(state): State<CoreState>,
    headers: HeaderMap,
    Json(input): Json<AdminCredentials>,
) -> Result<Response, ApiError> {
    dashboard_session::register_admin(&state.db, &input.username, &input.password).map_err(
        |error| match error {
            dashboard_session::RegisterError::Invalid(message) => ApiError::bad_request(message),
            dashboard_session::RegisterError::AlreadyExists => {
                ApiError::status(StatusCode::CONFLICT, "管理员已创建，直接登录")
            }
            dashboard_session::RegisterError::Internal(message) => ApiError::internal(message),
        },
    )?;
    session_response(&state, &headers, StatusCode::CREATED).await
}

async fn login_admin(
    State(state): State<CoreState>,
    headers: HeaderMap,
    Json(input): Json<AdminCredentials>,
) -> Result<Response, ApiError> {
    let valid = dashboard_session::credentials_match(&state.db, &input.username, &input.password)
        .map_err(ApiError::internal)?;
    if !valid {
        return Err(ApiError::status(
            StatusCode::UNAUTHORIZED,
            "用户名或密码错误",
        ));
    }
    session_response(&state, &headers, StatusCode::OK).await
}

async fn logout_admin(
    State(state): State<CoreState>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    dashboard_session::logout(
        state.dashboard_local_mode(),
        &state.dashboard_session_token,
        &state.browser,
        &headers,
    )
    .await
    .map_err(|_| ApiError::status(StatusCode::UNAUTHORIZED, "dashboard session is required"))?;
    let mut response = StatusCode::NO_CONTENT.into_response();
    response.headers_mut().insert(
        header::SET_COOKIE,
        dashboard_session::cookie_header("", &headers, true).map_err(ApiError::internal)?,
    );
    Ok(response)
}

async fn require_dashboard_session(
    State(state): State<CoreState>,
    req: Request,
    next: Next,
) -> Result<Response, StatusCode> {
    let authorized = {
        let current = state.dashboard_session_token.lock();
        dashboard_session::is_authorized(
            state.dashboard_local_mode(),
            current.as_str(),
            req.headers(),
        )
    };
    if authorized {
        Ok(next.run(req).await)
    } else {
        Err(StatusCode::UNAUTHORIZED)
    }
}

async fn session_response(
    state: &CoreState,
    headers: &HeaderMap,
    status: StatusCode,
) -> Result<Response, ApiError> {
    let session_token =
        dashboard_session::issue_session(&state.browser, &state.dashboard_session_token).await;
    let mut response = (status, Json(serde_json::json!({ "ok": true }))).into_response();
    response.headers_mut().insert(
        header::SET_COOKIE,
        dashboard_session::cookie_header(&session_token, headers, false)
            .map_err(ApiError::internal)?,
    );
    Ok(response)
}

#[derive(Debug)]
struct ApiError {
    status: StatusCode,
    message: String,
}

impl ApiError {
    fn status(status: StatusCode, message: impl Into<String>) -> Self {
        Self {
            status,
            message: message.into(),
        }
    }

    fn bad_request(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            message: message.into(),
        }
    }

    fn internal(error: impl std::fmt::Display) -> Self {
        Self {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            message: error.to_string(),
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let body = serde_json::json!({ "error": self.message });
        (self.status, Json(body)).into_response()
    }
}

async fn browser_session_websocket(
    State(state): State<CoreState>,
    Path(token): Path<String>,
    headers: HeaderMap,
    websocket: WebSocketUpgrade,
) -> Result<Response, ApiError> {
    validate_websocket_origin(&headers)?;
    let binding = dashboard_session_binding(&state, &headers)?;
    let mut remote_session = state
        .browser
        .remote_websocket_session(&token, &binding)
        .map_err(|error| ApiError::status(StatusCode::GONE, error.to_string()))?;
    let worker = tokio::select! {
        _ = remote_session.cancellation.changed() => {
            return Err(ApiError::status(StatusCode::GONE, "browser session was replaced"));
        }
        result = tokio::time::timeout(
            std::time::Duration::from_secs(15),
            tokio_tungstenite::connect_async(&remote_session.worker_ws_url),
        ) => {
            let (worker, _) = result
                .map_err(|_| ApiError::status(
                    StatusCode::GATEWAY_TIMEOUT,
                    "timed out connecting to remote browser display",
                ))?
                .map_err(|error| ApiError::status(
                    StatusCode::BAD_GATEWAY,
                    format!("failed to connect to remote browser display: {error}"),
                ))?;
            worker
        }
    };
    Ok(websocket.on_upgrade(move |client| {
        proxy_browser_websocket(state, token, remote_session.cancellation, client, worker)
    }))
}

async fn proxy_browser_websocket(
    state: CoreState,
    token: String,
    mut cancellation: tokio::sync::watch::Receiver<bool>,
    client: WebSocket,
    worker: tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >,
) {
    use tokio_tungstenite::tungstenite::Message as WorkerWsMessage;

    let (mut client_tx, mut client_rx) = client.split();
    let (mut worker_tx, mut worker_rx) = worker.split();
    let mut expiry_check = tokio::time::interval(std::time::Duration::from_secs(1));
    expiry_check.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        tokio::select! {
            _ = cancellation.changed() => { break; }
            _ = expiry_check.tick() => {
                if !state.browser.remote_session_active(&token) { break; }
            }
            message = client_rx.next() => {
                let Some(Ok(message)) = message else { break };
                if !state.browser.touch_remote_session(&token) { break; }
                let message = match message {
                    AxumWsMessage::Text(value) => WorkerWsMessage::Text(value.as_str().into()),
                    AxumWsMessage::Binary(value) => WorkerWsMessage::Binary(value),
                    AxumWsMessage::Ping(value) => WorkerWsMessage::Ping(value),
                    AxumWsMessage::Pong(value) => WorkerWsMessage::Pong(value),
                    AxumWsMessage::Close(_) => break };
                if worker_tx.send(message).await.is_err() { break; }
            }
            message = worker_rx.next() => {
                let Some(Ok(message)) = message else { break };
                if !state.browser.remote_session_active(&token) { break; }
                let message = match message {
                    WorkerWsMessage::Text(value) => AxumWsMessage::Text(value.as_str().into()),
                    WorkerWsMessage::Binary(value) => AxumWsMessage::Binary(value),
                    WorkerWsMessage::Ping(value) => AxumWsMessage::Ping(value),
                    WorkerWsMessage::Pong(value) => AxumWsMessage::Pong(value),
                    WorkerWsMessage::Close(_) => break,
                    WorkerWsMessage::Frame(_) => continue };
                if client_tx.send(message).await.is_err() { break; }
            }
        }
    }
    let _ = worker_tx.close().await;
    let _ = client_tx.close().await;
}

fn dashboard_session_binding(state: &CoreState, headers: &HeaderMap) -> Result<String, ApiError> {
    if dashboard_session::is_local_dashboard_request(state.dashboard_local_mode(), headers) {
        return Ok("local-dashboard".to_string());
    }
    let current = state.dashboard_session_token.lock();
    if dashboard_session::has_dashboard_session(current.as_str(), headers) {
        return dashboard_session::session_cookie_value(headers)
            .map(str::to_string)
            .ok_or_else(|| {
                ApiError::status(StatusCode::UNAUTHORIZED, "dashboard session is required")
            });
    }
    Err(ApiError::status(
        StatusCode::UNAUTHORIZED,
        "dashboard session is required",
    ))
}

fn validate_websocket_origin(headers: &HeaderMap) -> Result<(), ApiError> {
    let origin = headers
        .get(header::ORIGIN)
        .and_then(|value| value.to_str().ok())
        .ok_or_else(|| ApiError::bad_request("browser WebSocket Origin is required"))?;
    let origin = reqwest::Url::parse(origin)
        .map_err(|_| ApiError::bad_request("browser WebSocket Origin is invalid"))?;
    if !matches!(origin.scheme(), "http" | "https") {
        return Err(ApiError::bad_request(
            "browser WebSocket Origin must use http or https",
        ));
    }
    let host = headers
        .get(header::HOST)
        .and_then(|value| value.to_str().ok())
        .ok_or_else(|| ApiError::bad_request("browser WebSocket Host is required"))?;
    let authority = Authority::from_str(host)
        .map_err(|_| ApiError::bad_request("browser WebSocket Host is invalid"))?;
    let default_port = if origin.scheme() == "https" { 443 } else { 80 };
    if !origin
        .host_str()
        .is_some_and(|value| value.eq_ignore_ascii_case(authority.host()))
        || origin.port_or_known_default() != Some(authority.port_u16().unwrap_or(default_port))
    {
        return Err(ApiError::status(
            StatusCode::FORBIDDEN,
            "browser WebSocket Origin does not match Host",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{asset_path, validate_websocket_origin};
    use axum::http::{HeaderMap, StatusCode, header};
    use std::path::Path;

    #[test]
    fn asset_path_rejects_escape_components() {
        let root = Path::new("dist");

        assert_eq!(
            asset_path(root, "index.js").unwrap(),
            root.join("assets").join("index.js")
        );
        assert_eq!(
            asset_path(root, "nested/index.js").unwrap(),
            root.join("assets").join("nested").join("index.js")
        );

        assert!(asset_path(root, "../secret.txt").is_none());
        assert!(asset_path(root, "/secret.txt").is_none());
        assert!(asset_path(root, r"nested\secret.txt").is_none());
        assert!(asset_path(root, "C:/secret.txt").is_none());
    }

    #[test]
    fn browser_websocket_origin_must_match_request_host() {
        let mut headers = HeaderMap::new();
        headers.insert(header::HOST, "manager.example:9443".parse().unwrap());
        headers.insert(
            header::ORIGIN,
            "https://manager.example:9443".parse().unwrap(),
        );
        validate_websocket_origin(&headers).expect("same origin should pass");

        headers.insert(header::ORIGIN, "https://evil.example".parse().unwrap());
        let error = validate_websocket_origin(&headers).expect_err("cross origin must fail");
        assert_eq!(error.status, StatusCode::FORBIDDEN);

        headers.remove(header::ORIGIN);
        assert!(validate_websocket_origin(&headers).is_err());
    }
}
