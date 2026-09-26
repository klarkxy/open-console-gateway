//! Loopback HTTP client for a running DSH plugin-manager Host.
//!
//! Callers supply a session cookie (including one minted in memory from a
//! local browser-session grant). The client never follows redirects, never
//! uses a proxy, and never prints request URLs, cookies, tokens, or raw
//! response bodies.

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use hmac::{Hmac, Mac};
use reqwest::header::{CONTENT_TYPE, COOKIE, HOST};
use reqwest::{Method, StatusCode, Url, blocking::Client};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};
use std::fmt;
use std::io::Read;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use uuid::Uuid;

const COOKIE_PREFIX: &str = "dsh-auth-";
const TOKEN_QUERY: &str = "token";
const COOKIE_TTL: Duration = Duration::from_secs(5 * 60);
const DEFAULT_MAX_BODY_BYTES: usize = 1024 * 1024;
const DEFAULT_EXCHANGE_TIMEOUT: Duration = Duration::from_secs(10);
const DEFAULT_RPC_TIMEOUT: Duration = Duration::from_secs(20);
const DEFAULT_MUTATION_TIMEOUT: Duration = Duration::from_secs(120);

const MSG_INVALID_URL: &str = "DSH runtime URL is not a permitted loopback HTTP origin";
const MSG_INVALID_COOKIE: &str = "DSH runtime cookie is invalid";
const MSG_UNAUTHENTICATED: &str = "DSH runtime authentication was refused";
const MSG_FORBIDDEN: &str = "DSH runtime rejected the request";
const MSG_REMOTE: &str = "DSH runtime refused the operation";
const MSG_TIMEOUT: &str = "DSH runtime request timed out";
const MSG_TRANSPORT: &str = "DSH runtime transport failed";
const MSG_PROTOCOL: &str = "DSH runtime returned an invalid response";
const MSG_BOUNDED: &str = "DSH runtime response exceeded the size limit";
const MSG_RUNTIME: &str = "DSH runtime client could not start";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DshRuntimeErrorKind {
    Invalid,
    Unauthenticated,
    Forbidden,
    Remote,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DshRuntimeUnknownKind {
    Timeout,
    Transport,
    Protocol,
}

#[derive(Clone, PartialEq, Eq)]
pub(crate) struct DshRuntimeError {
    pub kind: DshRuntimeErrorKind,
    pub message: &'static str,
    pub remote_code: Option<String>,
    pub request_id: Option<String>,
    pub unknown_kind: Option<DshRuntimeUnknownKind>,
}

impl DshRuntimeError {
    pub fn is_unknown(&self) -> bool {
        self.kind == DshRuntimeErrorKind::Unknown
    }

    pub fn request_id(&self) -> Option<&str> {
        self.request_id.as_deref()
    }

    fn invalid() -> Self {
        Self {
            kind: DshRuntimeErrorKind::Invalid,
            message: MSG_INVALID_URL,
            remote_code: None,
            request_id: None,
            unknown_kind: None,
        }
    }

    fn invalid_cookie() -> Self {
        Self {
            kind: DshRuntimeErrorKind::Invalid,
            message: MSG_INVALID_COOKIE,
            remote_code: None,
            request_id: None,
            unknown_kind: None,
        }
    }

    fn unauthenticated(request_id: Option<String>) -> Self {
        Self {
            kind: DshRuntimeErrorKind::Unauthenticated,
            message: MSG_UNAUTHENTICATED,
            remote_code: None,
            request_id,
            unknown_kind: None,
        }
    }

    fn forbidden(request_id: Option<String>) -> Self {
        Self {
            kind: DshRuntimeErrorKind::Forbidden,
            message: MSG_FORBIDDEN,
            remote_code: None,
            request_id,
            unknown_kind: None,
        }
    }

    fn remote(code: Option<String>, request_id: Option<String>) -> Self {
        Self {
            kind: DshRuntimeErrorKind::Remote,
            message: MSG_REMOTE,
            remote_code: code,
            request_id,
            unknown_kind: None,
        }
    }

    fn unknown(
        kind: DshRuntimeUnknownKind,
        message: &'static str,
        request_id: Option<String>,
    ) -> Self {
        Self {
            kind: DshRuntimeErrorKind::Unknown,
            message,
            remote_code: None,
            request_id,
            unknown_kind: Some(kind),
        }
    }
}

impl fmt::Debug for DshRuntimeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DshRuntimeError")
            .field("kind", &self.kind)
            .field("message", &self.message)
            .field("remote_code", &self.remote_code)
            .field("request_id", &self.request_id)
            .field("unknown_kind", &self.unknown_kind)
            .finish()
    }
}

impl fmt::Display for DshRuntimeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.message)
    }
}

impl std::error::Error for DshRuntimeError {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct DshRuntimeLimits {
    pub max_body_bytes: usize,
    pub exchange_timeout: Duration,
    pub rpc_timeout: Duration,
    pub mutation_timeout: Duration,
}

impl Default for DshRuntimeLimits {
    fn default() -> Self {
        Self {
            max_body_bytes: DEFAULT_MAX_BODY_BYTES,
            exchange_timeout: DEFAULT_EXCHANGE_TIMEOUT,
            rpc_timeout: DEFAULT_RPC_TIMEOUT,
            mutation_timeout: DEFAULT_MUTATION_TIMEOUT,
        }
    }
}

#[derive(Clone)]
pub(crate) struct DshRuntimeOrigin {
    display: String,
    host_header: String,
}

impl DshRuntimeOrigin {
    pub fn parse(raw: &str) -> Result<Self, DshRuntimeError> {
        parse_session_origin(raw)
    }

    pub fn as_str(&self) -> &str {
        &self.display
    }

    #[cfg(test)]
    pub fn host_header(&self) -> &str {
        &self.host_header
    }
}

impl fmt::Debug for DshRuntimeOrigin {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("DshRuntimeOrigin")
            .field(&self.display)
            .finish()
    }
}

pub(crate) struct DshRuntimeCookie {
    pub(crate) name: String,
    pub(crate) value: String,
}

impl DshRuntimeCookie {
    #[cfg(test)]
    pub fn parse(origin: &DshRuntimeOrigin, header: &str) -> Result<Self, DshRuntimeError> {
        parse_cookie_header(origin, header)
    }
}

impl fmt::Debug for DshRuntimeCookie {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("DshRuntimeCookie([redacted])")
    }
}

pub(crate) struct DshRuntimeClient {
    origin: DshRuntimeOrigin,
    cookie: DshRuntimeCookie,
    http: Client,
    limits: DshRuntimeLimits,
}

impl fmt::Debug for DshRuntimeClient {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DshRuntimeClient")
            .field("origin", &self.origin.display)
            .field("cookie", &self.cookie)
            .finish_non_exhaustive()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DshRuntimeApplication {
    Applied,
    RestartRequired,
    Overridden,
    Failed,
    Cancelled,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DshChangeResult {
    pub application: DshRuntimeApplication,
    pub changed: Option<bool>,
    pub stage: Option<String>,
    pub target: Option<String>,
    pub error_code: Option<String>,
    pub pending_builds: Option<Vec<String>>,
    pub bundle: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DshInstallResult {
    pub request_id: String,
    pub change: DshChangeResult,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct DshInstallOptions {
    pub enabled: Option<bool>,
    pub request_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DshRuntimeBundle {
    pub name: String,
    pub enabled: bool,
    pub installed: bool,
    pub removable: bool,
    pub version: Option<String>,
    pub error_code: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DshRuntimePlugin {
    pub module_name: String,
    pub enabled: Option<bool>,
    pub fiber_phase: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum DshSpecInspection {
    Accepted {
        kind: String,
        name: Option<String>,
        version: Option<String>,
        bundle: Option<bool>,
    },
    Refused {
        problem: String,
    },
}

#[derive(Clone, Copy)]
enum DshRpc {
    ListBundles,
    ListPlugins,
    Inspect,
    InstallBundle,
    RemoveBundle,
    WaitForInstall,
}

impl DshRpc {
    fn endpoint(self) -> &'static str {
        match self {
            Self::ListBundles => "pluginManager/listBundles",
            Self::ListPlugins => "pluginManager/listPlugins",
            Self::Inspect => "pluginManager/inspect",
            Self::InstallBundle => "pluginManager/installBundle",
            Self::RemoveBundle => "pluginManager/removeBundle",
            Self::WaitForInstall => "pluginManager/waitForInstall",
        }
    }

    fn is_mutation(self) -> bool {
        matches!(
            self,
            Self::InstallBundle | Self::RemoveBundle | Self::WaitForInstall
        )
    }
}

#[derive(Serialize)]
struct RpcRequest {
    #[serde(rename = "type")]
    kind: &'static str,
    #[serde(rename = "rpcId")]
    rpc_id: String,
    method: String,
    payload: RpcPayload,
}

#[derive(Serialize)]
struct RpcPayload {
    args: Value,
}

#[derive(Deserialize)]
struct RpcResponse {
    #[serde(rename = "type")]
    kind: String,
    #[serde(rename = "rpcId")]
    rpc_id: String,
    result: RpcResult,
}

#[derive(Deserialize)]
struct RpcResult {
    ok: bool,
    #[serde(default)]
    value: Value,
    error: Option<RpcErrorBody>,
}

#[derive(Deserialize)]
struct RpcErrorBody {
    code: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct BundleDto {
    name: String,
    enabled: bool,
    installed: bool,
    removable: bool,
    version: Option<String>,
    error: Option<CodedErrorDto>,
}

#[derive(Deserialize)]
struct CodedErrorDto {
    code: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct PluginDto {
    module_name: String,
    enabled: Option<bool>,
    fiber_phase: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ChangeDto {
    application: String,
    changed: Option<bool>,
    stage: Option<String>,
    target: Option<String>,
    error: Option<CodedErrorDto>,
    pending_builds: Option<Vec<String>>,
    bundle: Option<String>,
}

#[derive(Deserialize)]
struct InspectionDto {
    status: String,
    kind: Option<String>,
    name: Option<String>,
    version: Option<String>,
    bundle: Option<bool>,
    problem: Option<String>,
}

#[derive(Serialize)]
struct BrowserCookiePayload {
    version: u8,
    authority: String,
    #[serde(rename = "issuedAt")]
    issued_at: u64,
    #[serde(rename = "expiresAt")]
    expires_at: u64,
}

impl DshRuntimeClient {
    #[cfg(test)]
    pub fn connect_launch_url(url: &str) -> Result<Self, DshRuntimeError> {
        Self::connect_launch_url_with_limits(url, DshRuntimeLimits::default())
    }

    pub fn connect_session(
        origin: &str,
        cookie: DshRuntimeCookie,
    ) -> Result<Self, DshRuntimeError> {
        Self::connect_session_with_limits(origin, cookie, DshRuntimeLimits::default())
    }

    #[cfg(test)]
    pub(crate) fn connect_launch_url_with_limits(
        url: &str,
        limits: DshRuntimeLimits,
    ) -> Result<Self, DshRuntimeError> {
        let (origin, tokenized) = parse_launch_url(url)?;
        let http = build_http_client(limits.exchange_timeout.max(limits.mutation_timeout))?;
        let cookie = exchange_cookie(&http, &origin, tokenized, limits)?;
        Ok(Self {
            origin,
            cookie,
            http,
            limits,
        })
    }

    pub(crate) fn connect_session_with_limits(
        origin: &str,
        cookie: DshRuntimeCookie,
        limits: DshRuntimeLimits,
    ) -> Result<Self, DshRuntimeError> {
        let origin = parse_session_origin(origin)?;
        if cookie.name != cookie_name_for_authority(&origin.host_header) {
            return Err(DshRuntimeError::invalid_cookie());
        }
        if !cookie_value_syntax_ok(&cookie.value) {
            return Err(DshRuntimeError::invalid_cookie());
        }
        Ok(Self {
            origin,
            cookie,
            http: build_http_client(limits.exchange_timeout.max(limits.mutation_timeout))?,
            limits,
        })
    }

    #[cfg(test)]
    pub fn origin(&self) -> &str {
        self.origin.as_str()
    }

    pub fn list_bundles(&self) -> Result<Vec<DshRuntimeBundle>, DshRuntimeError> {
        let value = self.rpc(DshRpc::ListBundles, Value::Object(Map::new()), None)?;
        let items: Vec<BundleDto> = serde_json::from_value(value).map_err(|_| {
            DshRuntimeError::unknown(DshRuntimeUnknownKind::Protocol, MSG_PROTOCOL, None)
        })?;
        Ok(items
            .into_iter()
            .map(|item| DshRuntimeBundle {
                name: item.name,
                enabled: item.enabled,
                installed: item.installed,
                removable: item.removable,
                version: item.version,
                error_code: item.error.and_then(|error| error.code),
            })
            .collect())
    }

    pub fn list_plugins(&self) -> Result<Vec<DshRuntimePlugin>, DshRuntimeError> {
        let value = self.rpc(DshRpc::ListPlugins, Value::Object(Map::new()), None)?;
        let items: Vec<PluginDto> = serde_json::from_value(value).map_err(|_| {
            DshRuntimeError::unknown(DshRuntimeUnknownKind::Protocol, MSG_PROTOCOL, None)
        })?;
        Ok(items
            .into_iter()
            .map(|item| DshRuntimePlugin {
                module_name: item.module_name,
                enabled: item.enabled,
                fiber_phase: item.fiber_phase,
            })
            .collect())
    }

    pub fn inspect(&self, spec: &str) -> Result<DshSpecInspection, DshRuntimeError> {
        let mut args = Map::new();
        args.insert("spec".into(), Value::String(spec.to_owned()));
        let value = self.rpc(DshRpc::Inspect, Value::Object(args), None)?;
        let dto: InspectionDto = serde_json::from_value(value).map_err(|_| {
            DshRuntimeError::unknown(DshRuntimeUnknownKind::Protocol, MSG_PROTOCOL, None)
        })?;
        match dto.status.as_str() {
            "accepted" => Ok(DshSpecInspection::Accepted {
                kind: dto.kind.unwrap_or_default(),
                name: dto.name,
                version: dto.version,
                bundle: dto.bundle,
            }),
            "refused" => Ok(DshSpecInspection::Refused {
                problem: dto.problem.unwrap_or_default(),
            }),
            _ => Err(DshRuntimeError::unknown(
                DshRuntimeUnknownKind::Protocol,
                MSG_PROTOCOL,
                None,
            )),
        }
    }

    pub fn install_bundle(
        &self,
        spec: &str,
        options: DshInstallOptions,
    ) -> Result<DshInstallResult, DshRuntimeError> {
        let request_id = options
            .request_id
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| Uuid::new_v4().to_string());
        let mut option_fields = Map::new();
        option_fields.insert("requestId".into(), Value::String(request_id.clone()));
        if let Some(enabled) = options.enabled {
            option_fields.insert("enabled".into(), Value::Bool(enabled));
        }
        let mut args = Map::new();
        args.insert("spec".into(), Value::String(spec.to_owned()));
        args.insert("options".into(), Value::Object(option_fields));
        let value = self.rpc(
            DshRpc::InstallBundle,
            Value::Object(args),
            Some(request_id.clone()),
        )?;
        Ok(DshInstallResult {
            request_id: request_id.clone(),
            change: parse_change(value, Some(request_id))?,
        })
    }

    pub fn remove_bundle(&self, name: &str) -> Result<DshChangeResult, DshRuntimeError> {
        let mut args = Map::new();
        args.insert("name".into(), Value::String(name.to_owned()));
        let value = self.rpc(DshRpc::RemoveBundle, Value::Object(args), None)?;
        parse_change(value, None)
    }

    pub fn wait_for_install(
        &self,
        request_id: &str,
    ) -> Result<Option<DshChangeResult>, DshRuntimeError> {
        let mut args = Map::new();
        args.insert("requestId".into(), Value::String(request_id.to_owned()));
        let value = self.rpc(
            DshRpc::WaitForInstall,
            Value::Object(args),
            Some(request_id.to_owned()),
        )?;
        if value.is_null() {
            return Ok(None);
        }
        Ok(Some(parse_change(value, Some(request_id.to_owned()))?))
    }

    fn rpc(
        &self,
        method: DshRpc,
        args: Value,
        request_id: Option<String>,
    ) -> Result<Value, DshRuntimeError> {
        let timeout = if method.is_mutation() {
            self.limits.mutation_timeout
        } else {
            self.limits.rpc_timeout
        };
        rpc_call(self, timeout, method, args, request_id)
    }
}

pub(crate) fn mint_browser_session_cookie(
    origin: &DshRuntimeOrigin,
    secret: &[u8],
    now_ms: Option<u64>,
) -> Result<DshRuntimeCookie, DshRuntimeError> {
    if secret.len() != 32 {
        return Err(DshRuntimeError::invalid_cookie());
    }
    let now_ms = now_ms.unwrap_or_else(unix_now_ms);
    let payload = BrowserCookiePayload {
        version: 1,
        authority: origin.host_header.clone(),
        issued_at: now_ms,
        expires_at: now_ms.saturating_add(COOKIE_TTL.as_millis() as u64),
    };
    let body_json = serde_json::to_vec(&payload).map_err(|_| DshRuntimeError::invalid_cookie())?;
    let body = URL_SAFE_NO_PAD.encode(body_json);
    let mut mac =
        Hmac::<Sha256>::new_from_slice(secret).map_err(|_| DshRuntimeError::invalid_cookie())?;
    mac.update(body.as_bytes());
    let signature = URL_SAFE_NO_PAD.encode(mac.finalize().into_bytes());
    let value = format!("v1.{body}.{signature}");
    if !cookie_value_syntax_ok(&value) {
        return Err(DshRuntimeError::invalid_cookie());
    }
    Ok(DshRuntimeCookie {
        name: cookie_name_for_authority(&origin.host_header),
        value,
    })
}

fn unix_now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0)
}

fn build_http_client(timeout: Duration) -> Result<Client, DshRuntimeError> {
    Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(timeout)
        .build()
        .map_err(|_| DshRuntimeError::unknown(DshRuntimeUnknownKind::Transport, MSG_RUNTIME, None))
}

#[cfg(test)]
fn exchange_cookie(
    http: &Client,
    origin: &DshRuntimeOrigin,
    tokenized: Url,
    limits: DshRuntimeLimits,
) -> Result<DshRuntimeCookie, DshRuntimeError> {
    let response = http
        .request(Method::GET, tokenized)
        .header(HOST, origin.host_header.clone())
        .timeout(limits.exchange_timeout)
        .send()
        .map_err(|error| map_reqwest(error, None))?;
    let status = response.status();
    if status == StatusCode::UNAUTHORIZED {
        return Err(DshRuntimeError::unauthenticated(None));
    }
    if status == StatusCode::FORBIDDEN {
        return Err(DshRuntimeError::forbidden(None));
    }
    if status != StatusCode::SEE_OTHER {
        return Err(DshRuntimeError::invalid_cookie());
    }
    let headers = response.headers().get_all(reqwest::header::SET_COOKIE);
    let mut found = None;
    for value in headers {
        let Ok(header) = value.to_str() else {
            continue;
        };
        if let Ok(cookie) = parse_cookie_header(origin, header) {
            found = Some(cookie);
            break;
        }
    }
    drop(read_bounded(response, limits.max_body_bytes, None));
    found.ok_or_else(DshRuntimeError::invalid_cookie)
}

fn rpc_call(
    client: &DshRuntimeClient,
    timeout: Duration,
    method: DshRpc,
    args: Value,
    request_id: Option<String>,
) -> Result<Value, DshRuntimeError> {
    let DshRuntimeClient {
        http,
        origin,
        cookie,
        limits,
    } = client;
    let max_body_bytes = limits.max_body_bytes;
    let rpc_id = Uuid::new_v4().to_string();
    let endpoint = method.endpoint();
    let url = Url::parse(&format!("{}/api/{endpoint}", origin.display)).map_err(|_| {
        DshRuntimeError::unknown(
            DshRuntimeUnknownKind::Protocol,
            MSG_PROTOCOL,
            request_id.clone(),
        )
    })?;
    let envelope = RpcRequest {
        kind: "client-request",
        rpc_id: rpc_id.clone(),
        method: endpoint.to_owned(),
        payload: RpcPayload { args },
    };
    let body = serde_json::to_vec(&envelope).map_err(|_| {
        DshRuntimeError::unknown(
            DshRuntimeUnknownKind::Protocol,
            MSG_PROTOCOL,
            request_id.clone(),
        )
    })?;
    let response = http
        .request(Method::POST, url)
        .header(HOST, origin.host_header.clone())
        .header(CONTENT_TYPE, "application/json")
        .header(COOKIE, format!("{}={}", cookie.name, cookie.value))
        .timeout(timeout)
        .body(body)
        .send()
        .map_err(|error| map_reqwest(error, request_id.clone()))?;
    let status = response.status();
    if status == StatusCode::UNAUTHORIZED {
        return Err(DshRuntimeError::unauthenticated(request_id));
    }
    if status == StatusCode::FORBIDDEN {
        return Err(DshRuntimeError::forbidden(request_id));
    }
    if !status.is_success() {
        let kind = if method.is_mutation() || status.is_server_error() {
            DshRuntimeUnknownKind::Transport
        } else {
            DshRuntimeUnknownKind::Protocol
        };
        drop(read_bounded(response, max_body_bytes, request_id.clone()));
        return Err(DshRuntimeError::unknown(kind, MSG_TRANSPORT, request_id));
    }
    let content_type = response
        .headers()
        .get(CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("");
    if !content_type
        .split(';')
        .next()
        .unwrap_or("")
        .trim()
        .eq_ignore_ascii_case("application/json")
    {
        drop(read_bounded(response, max_body_bytes, request_id.clone()));
        return Err(DshRuntimeError::unknown(
            DshRuntimeUnknownKind::Protocol,
            MSG_PROTOCOL,
            request_id,
        ));
    }
    let bytes = read_bounded(response, max_body_bytes, request_id.clone())?;
    parse_rpc_value(&bytes, &rpc_id, request_id)
}

fn map_reqwest(error: reqwest::Error, request_id: Option<String>) -> DshRuntimeError {
    if error.is_timeout() {
        DshRuntimeError::unknown(DshRuntimeUnknownKind::Timeout, MSG_TIMEOUT, request_id)
    } else {
        DshRuntimeError::unknown(DshRuntimeUnknownKind::Transport, MSG_TRANSPORT, request_id)
    }
}

fn read_bounded(
    mut response: reqwest::blocking::Response,
    max_body_bytes: usize,
    request_id: Option<String>,
) -> Result<Vec<u8>, DshRuntimeError> {
    if let Some(length) = response.content_length()
        && length > max_body_bytes as u64
    {
        return Err(DshRuntimeError::unknown(
            DshRuntimeUnknownKind::Protocol,
            MSG_BOUNDED,
            request_id,
        ));
    }
    let mut body = Vec::new();
    let mut chunk = [0u8; 8192];
    loop {
        let read = response.read(&mut chunk).map_err(|_| {
            DshRuntimeError::unknown(
                DshRuntimeUnknownKind::Transport,
                MSG_TRANSPORT,
                request_id.clone(),
            )
        })?;
        if read == 0 {
            break;
        }
        if body.len().saturating_add(read) > max_body_bytes {
            return Err(DshRuntimeError::unknown(
                DshRuntimeUnknownKind::Protocol,
                MSG_BOUNDED,
                request_id,
            ));
        }
        body.extend_from_slice(&chunk[..read]);
    }
    Ok(body)
}

fn parse_rpc_value(
    bytes: &[u8],
    expected_rpc_id: &str,
    request_id: Option<String>,
) -> Result<Value, DshRuntimeError> {
    let parsed: RpcResponse = serde_json::from_slice(bytes).map_err(|_| {
        DshRuntimeError::unknown(
            DshRuntimeUnknownKind::Protocol,
            MSG_PROTOCOL,
            request_id.clone(),
        )
    })?;
    if parsed.kind != "server-response" || parsed.rpc_id != expected_rpc_id {
        return Err(DshRuntimeError::unknown(
            DshRuntimeUnknownKind::Protocol,
            MSG_PROTOCOL,
            request_id,
        ));
    }
    if parsed.result.ok {
        Ok(parsed.result.value)
    } else {
        Err(DshRuntimeError::remote(
            parsed.result.error.and_then(|error| error.code),
            request_id,
        ))
    }
}

fn parse_change(
    value: Value,
    request_id: Option<String>,
) -> Result<DshChangeResult, DshRuntimeError> {
    let dto: ChangeDto = serde_json::from_value(value).map_err(|_| {
        DshRuntimeError::unknown(
            DshRuntimeUnknownKind::Protocol,
            MSG_PROTOCOL,
            request_id.clone(),
        )
    })?;
    let application = match dto.application.as_str() {
        "applied" => DshRuntimeApplication::Applied,
        "restart-required" => DshRuntimeApplication::RestartRequired,
        "overridden" => DshRuntimeApplication::Overridden,
        "failed" => DshRuntimeApplication::Failed,
        "cancelled" => DshRuntimeApplication::Cancelled,
        _ => {
            return Err(DshRuntimeError::unknown(
                DshRuntimeUnknownKind::Protocol,
                MSG_PROTOCOL,
                request_id,
            ));
        }
    };
    Ok(DshChangeResult {
        application,
        changed: dto.changed,
        stage: dto.stage,
        target: dto.target,
        error_code: dto.error.and_then(|error| error.code),
        pending_builds: dto.pending_builds,
        bundle: dto.bundle,
    })
}

#[cfg(test)]
fn parse_launch_url(raw: &str) -> Result<(DshRuntimeOrigin, Url), DshRuntimeError> {
    let url = Url::parse(raw).map_err(|_| DshRuntimeError::invalid())?;
    let origin = origin_from_url(raw, &url, true)?;
    Ok((origin, url))
}

fn parse_session_origin(raw: &str) -> Result<DshRuntimeOrigin, DshRuntimeError> {
    let url = Url::parse(raw).map_err(|_| DshRuntimeError::invalid())?;
    origin_from_url(raw, &url, false)
}

fn origin_from_url(
    raw: &str,
    url: &Url,
    require_token: bool,
) -> Result<DshRuntimeOrigin, DshRuntimeError> {
    if url.scheme() != "http" {
        return Err(DshRuntimeError::invalid());
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err(DshRuntimeError::invalid());
    }
    if url.fragment().is_some() {
        return Err(DshRuntimeError::invalid());
    }
    if url.path() != "/" {
        return Err(DshRuntimeError::invalid());
    }
    let Some(authority) = spelled_http_authority(raw) else {
        return Err(DshRuntimeError::invalid());
    };
    let (host, port) = parse_loopback_authority(authority)?;
    if !url_host_matches(url.host_str(), host) {
        return Err(DshRuntimeError::invalid());
    }
    let pairs: Vec<(String, String)> = url
        .query_pairs()
        .map(|(key, value)| (key.into_owned(), value.into_owned()))
        .collect();
    if require_token {
        if pairs.len() != 1 || pairs[0].0 != TOKEN_QUERY || pairs[0].1.is_empty() {
            return Err(DshRuntimeError::invalid());
        }
        if pairs[0].1.contains(['\0', '\r', '\n', ' ']) {
            return Err(DshRuntimeError::invalid());
        }
    } else if !pairs.is_empty() {
        return Err(DshRuntimeError::invalid());
    }
    let host_header = if host.contains(':') {
        format!("[{host}]:{port}")
    } else {
        format!("{host}:{port}")
    };
    let display = if host.contains(':') {
        format!("http://[{host}]:{port}")
    } else {
        format!("http://{host}:{port}")
    };
    Ok(DshRuntimeOrigin {
        display,
        host_header,
    })
}

fn url_host_matches(url_host: Option<&str>, expected: &str) -> bool {
    let Some(host) = url_host else {
        return false;
    };
    if host == expected {
        return true;
    }
    host.strip_prefix('[')
        .and_then(|value| value.strip_suffix(']'))
        == Some(expected)
}

fn spelled_http_authority(raw: &str) -> Option<&str> {
    let rest = raw.strip_prefix("http://").or_else(|| {
        raw.get(..7)
            .filter(|prefix| prefix.eq_ignore_ascii_case("http://"))
            .and_then(|_| raw.get(7..))
    })?;
    let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    Some(&rest[..end])
}

fn parse_loopback_authority(authority: &str) -> Result<(&'static str, u16), DshRuntimeError> {
    let (host, port_str) = if let Some(rest) = authority.strip_prefix("[::1]:") {
        ("::1", rest)
    } else if let Some(rest) = authority.strip_prefix("127.0.0.1:") {
        ("127.0.0.1", rest)
    } else {
        return Err(DshRuntimeError::invalid());
    };
    if port_str.is_empty()
        || !port_str.bytes().all(|byte| byte.is_ascii_digit())
        || (port_str.starts_with('0') && port_str.len() > 1)
    {
        return Err(DshRuntimeError::invalid());
    }
    let port: u16 = port_str.parse().map_err(|_| DshRuntimeError::invalid())?;
    if port == 0 {
        return Err(DshRuntimeError::invalid());
    }
    Ok((host, port))
}

fn cookie_name_for_authority(authority: &str) -> String {
    let digest = Sha256::digest(authority.as_bytes());
    format!("{COOKIE_PREFIX}{}", URL_SAFE_NO_PAD.encode(digest))
}

#[cfg(test)]
fn parse_cookie_header(
    origin: &DshRuntimeOrigin,
    header: &str,
) -> Result<DshRuntimeCookie, DshRuntimeError> {
    let pair = header.split(';').next().unwrap_or("").trim();
    let (name, value) = pair
        .split_once('=')
        .ok_or_else(DshRuntimeError::invalid_cookie)?;
    let name = name.trim();
    let value = value.trim();
    if name != cookie_name_for_authority(&origin.host_header) {
        return Err(DshRuntimeError::invalid_cookie());
    }
    if !cookie_name_syntax_ok(name) || !cookie_value_syntax_ok(value) {
        return Err(DshRuntimeError::invalid_cookie());
    }
    Ok(DshRuntimeCookie {
        name: name.to_owned(),
        value: value.to_owned(),
    })
}

#[cfg(test)]
fn cookie_name_syntax_ok(name: &str) -> bool {
    !name.is_empty()
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
}

fn cookie_value_syntax_ok(value: &str) -> bool {
    !value.is_empty()
        && value.bytes().all(
            |byte| matches!(byte, 0x21 | 0x23..=0x2B | 0x2D..=0x3A | 0x3C..=0x5B | 0x5D..=0x7E),
        )
}

#[cfg(test)]
#[path = "runtime/tests.rs"]
mod tests;
