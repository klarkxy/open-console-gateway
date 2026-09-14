//! Typed client for one user-operated local CLIProxyAPI (CPA) instance.
//!
//! This module deliberately exposes only the reviewed Management operations;
//! it is not a generic Management API proxy and never reads raw auth files.

use crate::db::CpaCatalogModel;
use crate::http_client::{self, RouteLabel};
use crate::models::AppConfig;
use reqwest::{Method, StatusCode, header::HeaderMap};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::HashSet;
use std::time::Duration;

pub const DEFAULT_CPA_BASE_URL: &str = "http://127.0.0.1:8317";
pub const CPA_BASE_URL_ENV: &str = "OCG_CPA_BASE_URL";
pub const CPA_COMPOSE_BASE_URL: &str = "http://cpa:8317";
pub const MIN_CPA_MANAGEMENT_VERSION: &str = "7.1.0";
const MAX_BODY_BYTES: usize = 2 * 1024 * 1024;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(20);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CpaSecrets {
    pub inference_key: String,
    pub management_key: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct CpaVersion {
    pub version: String,
    pub commit: Option<String>,
    pub build_date: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CpaConnectionReport {
    pub reachable: bool,
    pub management_ready: bool,
    pub inference_ready: bool,
    pub version: Option<CpaVersion>,
    pub model_count: usize,
    pub management_error: Option<String>,
    pub inference_error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CpaAccountView {
    pub name: String,
    pub auth_index: Option<String>,
    pub provider: String,
    pub label: Option<String>,
    pub status: Option<String>,
    pub status_message: Option<String>,
    pub disabled: bool,
    pub unavailable: bool,
    pub runtime_only: bool,
    pub mutable: bool,
    pub email: Option<String>,
    pub quota: Option<Value>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CpaOAuthProvider {
    Codex,
    Anthropic,
    Antigravity,
    Kimi,
    Xai,
}

impl CpaOAuthProvider {
    pub const ALL: [Self; 5] = [
        Self::Codex,
        Self::Anthropic,
        Self::Antigravity,
        Self::Kimi,
        Self::Xai,
    ];

    fn start_path(self) -> &'static str {
        match self {
            Self::Codex => "codex-auth-url?is_webui=true",
            Self::Anthropic => "anthropic-auth-url?is_webui=true",
            Self::Antigravity => "antigravity-auth-url?is_webui=true",
            Self::Kimi => "kimi-auth-url",
            Self::Xai => "xai-auth-url",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CpaOAuthStart {
    pub provider: CpaOAuthProvider,
    pub state: String,
    pub url: String,
    pub flow: String,
    pub user_code: Option<String>,
    pub expires_in: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CpaOAuthStatus {
    pub status: String,
    pub error: Option<String>,
}

#[derive(Debug)]
pub enum CpaError {
    Invalid(String),
    Unreachable(String),
    Http { status: u16, message: String },
    Response(String),
    Incompatible(String),
}

impl std::fmt::Display for CpaError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Invalid(message) => formatter.write_str(message),
            Self::Unreachable(message) => write!(formatter, "CPA is unreachable: {message}"),
            Self::Http { status, message } => {
                write!(formatter, "CPA returned HTTP {status}: {message}")
            }
            Self::Response(message) => write!(formatter, "CPA response is invalid: {message}"),
            Self::Incompatible(message) => {
                write!(formatter, "CPA Management API is incompatible: {message}")
            }
        }
    }
}

impl std::error::Error for CpaError {}

pub fn normalize_base_url(input: &str, allow_compose_service: bool) -> Result<String, CpaError> {
    let input = input.trim();
    let url = reqwest::Url::parse(input)
        .map_err(|error| CpaError::Invalid(format!("invalid CPA URL: {error}")))?;
    if !matches!(url.scheme(), "http" | "https") {
        return Err(CpaError::Invalid("CPA URL must use http or https".into()));
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err(CpaError::Invalid(
            "CPA URL must not contain credentials".into(),
        ));
    }
    if url.query().is_some() || url.fragment().is_some() {
        return Err(CpaError::Invalid(
            "CPA URL must not contain a query or fragment".into(),
        ));
    }
    if !matches!(url.path(), "" | "/") {
        return Err(CpaError::Invalid(
            "CPA URL must be an origin without a path".into(),
        ));
    }
    let host = url.host_str().unwrap_or_default();
    let loopback = matches!(host, "localhost" | "127.0.0.1" | "::1");
    let compose = allow_compose_service && host.eq_ignore_ascii_case("cpa");
    if !loopback && !compose {
        return Err(CpaError::Invalid(
            "CPA must run on loopback or the fixed Docker service `cpa`".into(),
        ));
    }
    Ok(input.trim_end_matches('/').to_string())
}

pub fn env_base_url() -> Result<Option<String>, CpaError> {
    match std::env::var(CPA_BASE_URL_ENV) {
        Ok(value) if !value.trim().is_empty() => normalize_base_url(&value, true).map(Some),
        Ok(_) | Err(std::env::VarError::NotPresent) => Ok(None),
        Err(error) => Err(CpaError::Invalid(format!(
            "failed to read {CPA_BASE_URL_ENV}: {error}"
        ))),
    }
}

pub struct CpaClient {
    client: reqwest::Client,
    base_url: String,
    management_key: String,
    inference_key: String,
}

impl CpaClient {
    pub fn new(
        app_config: &AppConfig,
        base_url: &str,
        management_key: String,
        inference_key: String,
        allow_compose_service: bool,
    ) -> Result<Self, CpaError> {
        let base_url = normalize_base_url(base_url, allow_compose_service)?;
        if management_key.trim().is_empty() || inference_key.trim().is_empty() {
            return Err(CpaError::Invalid(
                "CPA Inference Key and Management Key are required".into(),
            ));
        }
        let client = http_client::build_no_redirect_for_route(app_config, RouteLabel::Direct)
            .map_err(|error| CpaError::Invalid(error.to_string()))?;
        Ok(Self {
            client,
            base_url,
            management_key,
            inference_key,
        })
    }

    fn url(&self, path: &str) -> String {
        format!("{}/{}", self.base_url, path.trim_start_matches('/'))
    }

    async fn send_json(
        &self,
        method: Method,
        path: &str,
        auth: Option<&str>,
        body: Option<Value>,
    ) -> Result<(HeaderMap, Value, StatusCode), CpaError> {
        let url = reqwest::Url::parse(&self.url(path))
            .map_err(|error| CpaError::Invalid(format!("invalid CPA request URL: {error}")))?;
        self.send_json_url(method, url, auth, body).await
    }

    async fn send_json_url(
        &self,
        method: Method,
        url: reqwest::Url,
        auth: Option<&str>,
        body: Option<Value>,
    ) -> Result<(HeaderMap, Value, StatusCode), CpaError> {
        let mut request = self.client.request(method, url).timeout(REQUEST_TIMEOUT);
        if let Some(key) = auth {
            request = request.bearer_auth(key);
        }
        if let Some(body) = body {
            request = request.json(&body);
        }
        let response = request
            .send()
            .await
            .map_err(|error| CpaError::Unreachable(error.to_string()))?;
        let status = response.status();
        let headers = response.headers().clone();
        if response
            .content_length()
            .is_some_and(|size| size > MAX_BODY_BYTES as u64)
        {
            return Err(CpaError::Response("response exceeds 2 MiB".into()));
        }
        let bytes = response
            .bytes()
            .await
            .map_err(|error| CpaError::Response(error.to_string()))?;
        if bytes.len() > MAX_BODY_BYTES {
            return Err(CpaError::Response("response exceeds 2 MiB".into()));
        }
        let value = if bytes.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&bytes)
                .map_err(|error| CpaError::Response(format!("invalid JSON body: {error}")))?
        };
        if !status.is_success() {
            let message = value
                .get("error")
                .and_then(Value::as_str)
                .or_else(|| value.get("message").and_then(Value::as_str))
                .unwrap_or("request failed")
                .to_string();
            return Err(CpaError::Http {
                status: status.as_u16(),
                message,
            });
        }
        Ok((headers, value, status))
    }

    pub async fn health(&self) -> Result<(), CpaError> {
        let (_, value, _) = self.send_json(Method::GET, "healthz", None, None).await?;
        if value.get("status").and_then(Value::as_str) != Some("ok") {
            return Err(CpaError::Response("health response is not `ok`".into()));
        }
        Ok(())
    }

    pub async fn models(&self) -> Result<Vec<CpaCatalogModel>, CpaError> {
        let (_, value, _) = self
            .send_json(Method::GET, "v1/models", Some(&self.inference_key), None)
            .await?;
        parse_models(&value)
    }

    pub async fn api_keys(&self) -> Result<Vec<String>, CpaError> {
        let (_, value, _) = self
            .send_json(
                Method::GET,
                "v0/management/api-keys",
                Some(&self.management_key),
                None,
            )
            .await?;
        parse_api_keys(&value)
    }

    pub async fn replace_api_keys(&self, keys: &[String]) -> Result<(), CpaError> {
        if keys.iter().any(|key| key.trim().is_empty()) {
            return Err(CpaError::Invalid("CPA API keys must not be empty".into()));
        }
        self.send_json(
            Method::PUT,
            "v0/management/api-keys",
            Some(&self.management_key),
            Some(json!(keys)),
        )
        .await?;
        Ok(())
    }

    pub async fn accounts(&self) -> Result<(CpaVersion, Vec<CpaAccountView>), CpaError> {
        let (headers, value, _) = self
            .send_json(
                Method::GET,
                "v0/management/auth-files",
                Some(&self.management_key),
                None,
            )
            .await?;
        let version = version_from_headers(&headers)?;
        ensure_supported_version(&version.version)?;
        let rows = value
            .get("files")
            .and_then(Value::as_array)
            .ok_or_else(|| CpaError::Response("`files` must be an array".into()))?;
        Ok((version, rows.iter().map(account_view).collect()))
    }

    pub async fn test(&self) -> Result<CpaConnectionReport, CpaError> {
        if let Err(error) = self.health().await {
            return Ok(CpaConnectionReport {
                reachable: false,
                management_ready: false,
                inference_ready: false,
                version: None,
                model_count: 0,
                management_error: Some(error.to_string()),
                inference_error: Some(error.to_string()),
            });
        }
        let (version, management_error) = match self.accounts().await {
            Ok((version, _)) => (Some(version), None),
            Err(error) => (None, Some(error.to_string())),
        };
        let (model_count, inference_error) = match self.models().await {
            Ok(models) => (models.len(), None),
            Err(error) => (0, Some(error.to_string())),
        };
        Ok(CpaConnectionReport {
            reachable: true,
            management_ready: management_error.is_none(),
            inference_ready: inference_error.is_none(),
            version,
            model_count,
            management_error,
            inference_error,
        })
    }

    pub async fn start_oauth(&self, provider: CpaOAuthProvider) -> Result<CpaOAuthStart, CpaError> {
        let (_, value, _) = self
            .send_json(
                Method::GET,
                &format!("v0/management/{}", provider.start_path()),
                Some(&self.management_key),
                None,
            )
            .await?;
        let state = required_string(&value, "state")?;
        let url = required_string(&value, "url")?;
        let parsed = reqwest::Url::parse(&url)
            .map_err(|error| CpaError::Response(format!("invalid OAuth URL: {error}")))?;
        if parsed.scheme() != "https" {
            return Err(CpaError::Response("CPA OAuth URL must use HTTPS".into()));
        }
        Ok(CpaOAuthStart {
            provider,
            state,
            url,
            flow: value
                .get("flow")
                .and_then(Value::as_str)
                .unwrap_or("browser")
                .to_string(),
            user_code: optional_string(&value, "user_code"),
            expires_in: value.get("expires_in").and_then(Value::as_u64),
        })
    }

    pub async fn oauth_status(&self, state: &str) -> Result<CpaOAuthStatus, CpaError> {
        validate_flow_state(state)?;
        let (_, value, _) = self
            .send_json(
                Method::GET,
                &format!("v0/management/get-auth-status?state={state}"),
                Some(&self.management_key),
                None,
            )
            .await?;
        Ok(CpaOAuthStatus {
            status: required_string(&value, "status")?,
            error: optional_string(&value, "error"),
        })
    }

    pub async fn cancel_oauth(&self, state: &str) -> Result<bool, CpaError> {
        validate_flow_state(state)?;
        let (_, value, _) = self
            .send_json(
                Method::DELETE,
                &format!("v0/management/oauth-session?state={state}"),
                Some(&self.management_key),
                None,
            )
            .await?;
        Ok(value
            .get("cancelled")
            .and_then(Value::as_bool)
            .unwrap_or(false))
    }

    pub async fn set_account_disabled(
        &self,
        name: &str,
        auth_index: &str,
        disabled: bool,
    ) -> Result<(), CpaError> {
        let (name, auth_index) = self.require_mutable_account(name, auth_index).await?;
        let body = json!({ "name": name, "auth_index": auth_index, "disabled": disabled });
        self.send_json(
            Method::PATCH,
            "v0/management/auth-files/status",
            Some(&self.management_key),
            Some(body),
        )
        .await?;
        Ok(())
    }

    pub async fn delete_account(
        &self,
        name: &str,
        auth_index: &str,
    ) -> Result<StatusCode, CpaError> {
        let (name, _) = self.require_mutable_account(name, auth_index).await?;
        let mut url = reqwest::Url::parse(&self.url("v0/management/auth-files"))
            .map_err(|error| CpaError::Invalid(format!("invalid CPA request URL: {error}")))?;
        url.query_pairs_mut().append_pair("name", &name);
        let (_, _, status) = self
            .send_json_url(Method::DELETE, url, Some(&self.management_key), None)
            .await?;
        Ok(status)
    }

    pub async fn reset_quota(&self, name: &str, auth_index: &str) -> Result<(), CpaError> {
        let (_, auth_index) = self.require_mutable_account(name, auth_index).await?;
        self.send_json(
            Method::POST,
            "v0/management/reset-quota",
            Some(&self.management_key),
            Some(json!({ "auth_index": auth_index })),
        )
        .await?;
        Ok(())
    }

    /// Only normalized server-read CLI credentials can reach this write. Never
    /// forward an upstream error body: it may echo the uploaded OAuth fields.
    pub(crate) async fn upload_cli_credential(
        &self,
        credential: &crate::cpa_cli_import::ImportedCredential,
    ) -> Result<(), CpaError> {
        let mut url = reqwest::Url::parse(&self.url("v0/management/auth-files"))
            .map_err(|_| CpaError::Invalid("Invalid CPA import endpoint".into()))?;
        url.query_pairs_mut().append_pair("name", &credential.name);
        self.send_json_url(
            Method::POST,
            url,
            Some(&self.management_key),
            Some(credential.payload.clone()),
        )
        .await
        .map(|_| ())
        .map_err(|error| match error {
            CpaError::Http { status, .. } => CpaError::Http {
                status,
                message: "CPA did not confirm the credential import".into(),
            },
            _ => CpaError::Unreachable("CPA did not confirm the credential import".into()),
        })
    }

    async fn require_mutable_account(
        &self,
        name: &str,
        auth_index: &str,
    ) -> Result<(String, String), CpaError> {
        let name = validate_account_name(name)?;
        let auth_index = validate_auth_index(auth_index)?;
        let (_, accounts) = self.accounts().await?;
        let exact = accounts.iter().find(|account| {
            account.name == name && account.auth_index.as_deref() == Some(auth_index.as_str())
        });
        let Some(exact) = exact else {
            return Err(CpaError::Invalid(
                "CPA account identity changed; reload the account list".into(),
            ));
        };
        if !exact.mutable {
            return Err(CpaError::Invalid(
                "runtime plugin accounts are read-only".into(),
            ));
        }
        Ok((name, auth_index))
    }
}

fn parse_api_keys(value: &Value) -> Result<Vec<String>, CpaError> {
    let rows = match value {
        Value::Array(rows) => rows,
        Value::Object(object) => object
            .get("items")
            .or_else(|| object.get("api-keys"))
            .or_else(|| object.get("apiKeys"))
            .or_else(|| object.get("keys"))
            .and_then(Value::as_array)
            .ok_or_else(|| CpaError::Response("CPA API keys must be an array".into()))?,
        _ => {
            return Err(CpaError::Response("CPA API keys must be an array".into()));
        }
    };
    let mut keys = Vec::new();
    let mut seen = HashSet::new();
    for row in rows {
        let key = match row {
            Value::String(value) => value.trim().to_string(),
            Value::Object(object) => object
                .get("key")
                .or_else(|| object.get("api-key"))
                .and_then(Value::as_str)
                .unwrap_or("")
                .trim()
                .to_string(),
            _ => String::new(),
        };
        if !key.is_empty() && seen.insert(key.clone()) {
            keys.push(key);
        }
    }
    Ok(keys)
}

fn parse_models(value: &Value) -> Result<Vec<CpaCatalogModel>, CpaError> {
    let data = value
        .get("data")
        .and_then(Value::as_array)
        .ok_or_else(|| CpaError::Response("`data` must be an array".into()))?;
    let mut seen = HashSet::new();
    let mut models = Vec::new();
    for row in data {
        let Some(id) = row.get("id").and_then(Value::as_str).map(str::trim) else {
            continue;
        };
        if id.is_empty() || !seen.insert(id.to_string()) {
            continue;
        }
        let owned_by = row
            .get("owned_by")
            .or_else(|| row.get("ownedBy"))
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string)
            .or_else(|| {
                row.get("type")
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|value| !value.is_empty() && !value.eq_ignore_ascii_case("model"))
                    .map(str::to_string)
            });
        models.push(CpaCatalogModel {
            id: id.to_string(),
            owned_by,
            enabled: true,
        });
    }
    // A newly installed CPA has no OAuth accounts and legitimately lists no models.
    // A nonempty payload without any valid IDs is still malformed.
    if !data.is_empty() && models.is_empty() {
        return Err(CpaError::Response("CPA model catalog is empty".into()));
    }
    Ok(models)
}

fn version_from_headers(headers: &HeaderMap) -> Result<CpaVersion, CpaError> {
    let version = headers
        .get("x-cpa-version")
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| CpaError::Incompatible("missing X-CPA-VERSION".into()))?
        .to_string();
    Ok(CpaVersion {
        version,
        commit: header_string(headers, "x-cpa-commit"),
        build_date: header_string(headers, "x-cpa-build-date"),
    })
}

fn ensure_supported_version(version: &str) -> Result<(), CpaError> {
    let clean = version.trim_start_matches('v');
    let parts: Vec<_> = clean.split('.').collect();
    let major = parts.first().and_then(|value| value.parse::<u64>().ok());
    let minor = parts.get(1).and_then(|value| value.parse::<u64>().ok());
    let supported = match (major, minor) {
        (Some(major), Some(minor)) => major > 7 || (major == 7 && minor >= 1),
        _ => false,
    };
    if !supported {
        return Err(CpaError::Incompatible(format!(
            "version {version} is older than the minimum supported version {MIN_CPA_MANAGEMENT_VERSION}"
        )));
    }
    Ok(())
}

fn header_string(headers: &HeaderMap, name: &str) -> Option<String> {
    headers
        .get(name)
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
}

fn account_view(value: &Value) -> CpaAccountView {
    let provider = optional_string(value, "provider")
        .or_else(|| optional_string(value, "type"))
        .unwrap_or_else(|| "unknown".into());
    let builtin = matches!(
        provider.as_str(),
        "codex" | "anthropic" | "claude" | "antigravity" | "kimi" | "xai"
    );
    let runtime_only = value
        .get("runtime_only")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    CpaAccountView {
        name: optional_string(value, "name")
            .or_else(|| optional_string(value, "id"))
            .unwrap_or_else(|| "unknown".into()),
        auth_index: optional_string(value, "auth_index"),
        provider,
        label: optional_string(value, "label"),
        status: optional_string(value, "status"),
        status_message: optional_string(value, "status_message"),
        disabled: value
            .get("disabled")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        unavailable: value
            .get("unavailable")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        runtime_only,
        mutable: builtin && !runtime_only,
        email: optional_string(value, "email"),
        quota: value
            .get("quota")
            .cloned()
            .or_else(|| value.get("model_quotas").cloned()),
    }
}

fn required_string(value: &Value, key: &str) -> Result<String, CpaError> {
    optional_string(value, key).ok_or_else(|| CpaError::Response(format!("missing `{key}`")))
}

fn optional_string(value: &Value, key: &str) -> Option<String> {
    value
        .get(key)
        .and_then(|value| match value {
            Value::String(value) => Some(value.trim().to_string()),
            Value::Number(value) => Some(value.to_string()),
            _ => None,
        })
        .filter(|value| !value.is_empty())
}

fn validate_flow_state(value: &str) -> Result<&str, CpaError> {
    let value = value.trim();
    if value.is_empty()
        || value.len() > 512
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
    {
        return Err(CpaError::Invalid("invalid CPA OAuth state".into()));
    }
    Ok(value)
}

fn validate_account_name(value: &str) -> Result<String, CpaError> {
    let value = value.trim();
    if value.is_empty() || value.len() > 512 || value.contains(['\r', '\n']) {
        return Err(CpaError::Invalid("invalid CPA account name".into()));
    }
    Ok(value.to_string())
}

fn validate_auth_index(value: &str) -> Result<String, CpaError> {
    let value = value.trim();
    if value.is_empty() || value.len() > 256 || value.contains(['\r', '\n']) {
        return Err(CpaError::Invalid("invalid CPA auth index".into()));
    }
    Ok(value.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::CpaCatalogModel;
    use axum::extract::Query;
    use axum::routing::get;
    use axum::{Json, Router};
    use serde_json::json;
    use std::collections::HashMap;

    #[test]
    fn url_boundary_accepts_only_loopback_or_fixed_compose_service() {
        assert_eq!(
            normalize_base_url("http://127.0.0.1:8317/", false).unwrap(),
            DEFAULT_CPA_BASE_URL
        );
        assert!(normalize_base_url(CPA_COMPOSE_BASE_URL, true).is_ok());
        assert!(normalize_base_url(CPA_COMPOSE_BASE_URL, false).is_err());
        for rejected in [
            "http://192.168.1.10:8317",
            "https://cpa.example.com",
            "http://user:pass@127.0.0.1:8317",
            "http://127.0.0.1:8317/v1",
            "http://127.0.0.1:8317?x=1",
        ] {
            assert!(normalize_base_url(rejected, true).is_err(), "{rejected}");
        }
    }

    #[test]
    fn version_gate_accepts_forward_versions_after_7_1() {
        for accepted in ["7.1.0", "v7.2.145", "7.99.1", "8.0.0", "12.4.0"] {
            assert!(ensure_supported_version(accepted).is_ok(), "{accepted}");
        }
        for rejected in ["6.99.0", "7.0.99", "unknown"] {
            assert!(ensure_supported_version(rejected).is_err(), "{rejected}");
        }
    }

    #[test]
    fn api_keys_are_deduplicated_from_object_or_array() {
        assert_eq!(
            parse_api_keys(&json!({"items": ["one", "one", "two"]})).unwrap(),
            ["one", "two"]
        );
        assert_eq!(parse_api_keys(&json!(["alpha"])).unwrap(), ["alpha"]);
        assert_eq!(
            parse_api_keys(&json!({"keys": [{"key": "k1"}]})).unwrap(),
            ["k1"]
        );
    }

    #[test]
    fn model_catalog_is_deduplicated_and_allows_an_empty_list() {
        let models = parse_models(&json!({
            "data": [
                {"id":"gpt-5", "owned_by":"openai"},
                {"id":"gpt-5", "owned_by":"openai"},
                {"id":"claude", "type":"anthropic"}
            ]
        }))
        .unwrap();
        assert_eq!(
            models,
            [
                CpaCatalogModel {
                    id: "gpt-5".into(),
                    owned_by: Some("openai".into()),
                    enabled: true,
                },
                CpaCatalogModel {
                    id: "claude".into(),
                    owned_by: Some("anthropic".into()),
                    enabled: true,
                },
            ]
        );
        assert!(parse_models(&json!({"data": []})).unwrap().is_empty());
        assert!(parse_models(&json!({"data": [{"id": " "}]})).is_err());
        assert!(parse_models(&json!({"data": null})).is_err());
    }

    #[test]
    fn provider_set_is_fixed() {
        assert_eq!(CpaOAuthProvider::ALL.len(), 5);
        assert_eq!(
            CpaOAuthProvider::Codex.start_path(),
            "codex-auth-url?is_webui=true"
        );
        assert_eq!(CpaOAuthProvider::Kimi.start_path(), "kimi-auth-url");
    }

    #[tokio::test]
    async fn claude_rows_are_mutable_and_writes_use_exact_identity() {
        async fn list() -> impl axum::response::IntoResponse {
            (
                [
                    ("x-cpa-version", "7.2.145"),
                    ("x-cpa-commit", "test"),
                    ("x-cpa-build-date", "today"),
                ],
                Json(json!({
                    "files": [{
                        "name": "claude account.json",
                        "auth_index": "claude-1",
                        "provider": "claude",
                        "disabled": false,
                        "runtime_only": false
                    }]
                })),
            )
        }

        async fn delete(Query(query): Query<HashMap<String, String>>) -> Json<Value> {
            assert_eq!(
                query.get("name").map(String::as_str),
                Some("claude account.json")
            );
            Json(json!({ "status": "ok" }))
        }

        async fn status(Json(body): Json<Value>) -> Json<Value> {
            assert_eq!(body["name"], "claude account.json");
            assert_eq!(body["auth_index"], "claude-1");
            assert_eq!(body["disabled"], true);
            Json(json!({ "status": "ok" }))
        }

        async fn reset(Json(body): Json<Value>) -> Json<Value> {
            assert_eq!(body["auth_index"], "claude-1");
            Json(json!({ "status": "ok" }))
        }

        let app = Router::new()
            .route("/v0/management/auth-files", get(list).delete(delete))
            .route(
                "/v0/management/auth-files/status",
                axum::routing::patch(status),
            )
            .route("/v0/management/reset-quota", axum::routing::post(reset));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let client = CpaClient::new(
            &AppConfig::default(),
            &format!("http://{address}"),
            "management".into(),
            "inference".into(),
            false,
        )
        .unwrap();
        let (_, accounts) = client.accounts().await.unwrap();
        assert_eq!(accounts.len(), 1);
        assert!(accounts[0].mutable);
        client
            .set_account_disabled("claude account.json", "claude-1", true)
            .await
            .unwrap();
        client
            .reset_quota("claude account.json", "claude-1")
            .await
            .unwrap();
        assert_eq!(
            client
                .delete_account("claude account.json", "claude-1")
                .await
                .unwrap(),
            StatusCode::OK
        );
    }

    #[tokio::test]
    async fn runtime_plugin_accounts_are_read_only_for_every_write() {
        async fn list() -> impl axum::response::IntoResponse {
            (
                [("x-cpa-version", "7.2.145")],
                Json(json!({
                    "files": [{
                        "name": "plugin-account",
                        "auth_index": "plugin-1",
                        "provider": "codex",
                        "runtime_only": true
                    }]
                })),
            )
        }

        let app = Router::new().route("/v0/management/auth-files", get(list));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let client = CpaClient::new(
            &AppConfig::default(),
            &format!("http://{address}"),
            "management".into(),
            "inference".into(),
            false,
        )
        .unwrap();

        assert!(matches!(
            client
                .set_account_disabled("plugin-account", "plugin-1", true)
                .await,
            Err(CpaError::Invalid(message)) if message.contains("read-only")
        ));
        assert!(matches!(
            client.reset_quota("plugin-account", "plugin-1").await,
            Err(CpaError::Invalid(message)) if message.contains("read-only")
        ));
        assert!(matches!(
            client.delete_account("plugin-account", "plugin-1").await,
            Err(CpaError::Invalid(message)) if message.contains("read-only")
        ));
    }

    #[tokio::test]
    async fn replace_api_keys_puts_a_json_string_array() {
        use axum::extract::State;
        use std::sync::{Arc, Mutex};

        #[derive(Clone)]
        struct Capture(Arc<Mutex<Option<Value>>>);

        async fn list() -> Json<Value> {
            Json(json!(["keep"]))
        }
        async fn put_keys(State(capture): State<Capture>, Json(body): Json<Value>) -> Json<Value> {
            *capture.0.lock().unwrap() = Some(body);
            Json(json!([]))
        }

        let capture = Capture(Arc::new(Mutex::new(None)));
        let app = Router::new()
            .route("/v0/management/api-keys", get(list).put(put_keys))
            .with_state(capture.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let client = CpaClient::new(
            &AppConfig::default(),
            &format!("http://{address}"),
            "management".into(),
            "inference".into(),
            false,
        )
        .unwrap();
        client
            .replace_api_keys(&["keep".into(), "new".into()])
            .await
            .unwrap();
        let body = capture.0.lock().unwrap().clone().unwrap();
        assert_eq!(body, json!(["keep", "new"]));
        assert!(body.as_array().is_some());
        assert!(body.get("api-keys").is_none());
    }
}
