//! Bounded New API / Sub2API metadata reader.
//!
//! Verified against official sources:
//! - New API `71c1fd7caad738db4d13aabbf28eeadb293d0cfe`
//!   (`router/api-router.go`, `router/relay-router.go`, `controller/user.go`,
//!   `controller/group.go`, `controller/subscription.go`, `controller/token.go`,
//!   `controller/pricing.go`, `controller/model.go`, `controller/misc.go`,
//!   `middleware/auth.go`, `model/pricing.go`, `model/token.go`,
//!   `model/subscription.go`)
//! - Sub2API `772a0382f079676983c06f24b0d41e09139a8462`
//!   (`backend/internal/server/router.go`, `.../routes/user.go`,
//!   `.../routes/gateway.go`, `.../routes/model_plaza.go`,
//!   `backend/internal/handler/subscription_handler.go`,
//!   `.../api_key_handler.go`, `.../model_plaza_handler.go`,
//!   `.../available_channel_handler.go` (`userSupportedModelPricing`,
//!   `toUserPricing`), `.../gateway_key_billing.go`,
//!   `.../gateway_handler.go` (`Usage`), `.../user_handler.go` (`GetProfile`),
//!   `backend/internal/pkg/response/response.go`,
//!   `backend/internal/handler/dto/types.go`)
//!
//! Inference stays Custom HTTP. This leaf only reads account-owned metadata.

use super::{
    PlatformGroup, PlatformKind, PlatformModel, PlatformPrice, PlatformQuota, PlatformQuotaKind,
    PlatformReadRequest, PlatformSnapshot,
};
use crate::custom_http::join_inference_endpoint;
use futures_util::StreamExt;
use reqwest::StatusCode;
use reqwest::header::HeaderValue;
use serde_json::Value;
use std::collections::BTreeSet;
use std::time::Duration;

const REQUEST_TIMEOUT: Duration = Duration::from_secs(15);
const MAX_BODY_BYTES: usize = 256 * 1024;
const SNAPSHOT_TTL_SECS: i64 = 24 * 60 * 60;

const ERR_BASE_URL_INVALID: &str = "base_url.invalid";
const ERR_AUTH_MISSING: &str = "auth.missing";
const CODE_REDIRECT: &str = "redirect_rejected";
const CODE_NETWORK: &str = "network";
const CODE_TIMEOUT: &str = "timeout";
const CODE_UNAUTHORIZED: &str = "unauthorized";
const CODE_USER_ID_REQUIRED: &str = "user_id_required";
const CODE_USER_ID_MISMATCH: &str = "user_id_mismatch";
const CODE_FORBIDDEN: &str = "forbidden";
const CODE_HTTP_STATUS: &str = "http_status";
const CODE_PARSE: &str = "parse";
const CODE_OVERSIZE: &str = "oversize";
const CODE_ENDPOINT: &str = "endpoint_override";
const CODE_SECRET: &str = "secret_reflected";

const SRC_V1_MODELS: &str = "v1_models";
const SRC_TOKEN_LIMITS: &str = "token_limits";
const SRC_NEW_API_PRICING: &str = "new_api.pricing";
const SRC_SUB2_OFFICIAL: &str = "sub2api.official_pricing";
const SRC_SUB2_BILLED: &str = "sub2api.billed_pricing";

const UNAVAIL_EXPRESSION: &str = "expression";
const UNAVAIL_TIERED: &str = "tiered";
const UNAVAIL_TIME: &str = "time_varying";
const UNAVAIL_AUTO: &str = "auto_group";
const UNAVAIL_PER_REQUEST: &str = "per_request";
const UNAVAIL_MISSING_RATE: &str = "missing_rate";
const UNAVAIL_MISSING_UNIT: &str = "missing_quota_per_unit";
const UNAVAIL_MISSING_MULT: &str = "missing_multiplier";

#[cfg(test)]
#[path = "tests.rs"]
mod tests;

/// Read one platform snapshot. Failures are recorded as fixed codes on the
/// snapshot; this never returns upstream bodies or credentials.
pub async fn read(client: &reqwest::Client, request: &PlatformReadRequest<'_>) -> PlatformSnapshot {
    let mut snapshot = PlatformSnapshot {
        observed_at: request.now,
        stale: false,
        errors: Vec::new(),
        quotas: Vec::new(),
        models: Vec::new(),
        prices: Vec::new(),
        groups: Vec::new(),
        billing_preference: None,
        wallet_overflow: None,
    };

    let Ok(base) = super::validate_platform_base_url(request.base_url) else {
        snapshot.errors.push(ERR_BASE_URL_INVALID.to_string());
        snapshot.stale = true;
        return snapshot;
    };
    let root = base.strip_suffix("/v1").unwrap_or(&base);
    let Ok(base_url) = reqwest::Url::parse(root) else {
        snapshot.errors.push(ERR_BASE_URL_INVALID.to_string());
        snapshot.stale = true;
        return snapshot;
    };

    let user = trim_secret(request.user_credential);
    let key = trim_secret(request.key);
    if user.is_none() && key.is_none() {
        snapshot.errors.push(ERR_AUTH_MISSING.to_string());
        snapshot.stale = true;
        return snapshot;
    }

    match request.kind {
        PlatformKind::NewApi => {
            read_new_api(client, &base_url, request, user, key, &mut snapshot).await
        }
        PlatformKind::Sub2api => {
            read_sub2(client, &base_url, request, user, key, &mut snapshot).await
        }
    }

    scrub_secrets(&mut snapshot, user, key);
    snapshot.stale = snapshot.quotas.is_empty()
        && snapshot.models.is_empty()
        && snapshot.prices.is_empty()
        && snapshot.groups.is_empty()
        && !snapshot.errors.is_empty();
    snapshot
}

fn trim_secret(value: Option<&str>) -> Option<&str> {
    value.map(str::trim).filter(|value| !value.is_empty())
}

fn component_error(component: &str, code: &str) -> String {
    format!("{component}.{code}")
}

fn same_origin(left: &reqwest::Url, right: &reqwest::Url) -> bool {
    left.scheme() == right.scheme()
        && left.host() == right.host()
        && left.port_or_known_default() == right.port_or_known_default()
}

pub(crate) struct Fetched {
    pub value: Value,
}

async fn get_json(
    client: &reqwest::Client,
    base: &reqwest::Url,
    path: &str,
    component: &str,
    auth: Option<&str>,
    new_api_user: Option<&str>,
) -> Result<Fetched, String> {
    get_json_query(client, base, path, component, auth, new_api_user, &[]).await
}

pub(crate) async fn get_json_query(
    client: &reqwest::Client,
    base: &reqwest::Url,
    path: &str,
    component: &str,
    auth: Option<&str>,
    new_api_user: Option<&str>,
    query: &[(&str, &str)],
) -> Result<Fetched, String> {
    request_json(
        client,
        reqwest::Method::GET,
        base,
        path,
        component,
        auth,
        new_api_user,
        query,
    )
    .await
}

pub(crate) async fn post_json(
    client: &reqwest::Client,
    base: &reqwest::Url,
    path: &str,
    component: &str,
    auth: Option<&str>,
    new_api_user: Option<&str>,
) -> Result<Fetched, String> {
    request_json(
        client,
        reqwest::Method::POST,
        base,
        path,
        component,
        auth,
        new_api_user,
        &[],
    )
    .await
}

async fn request_json(
    client: &reqwest::Client,
    method: reqwest::Method,
    base: &reqwest::Url,
    path: &str,
    component: &str,
    auth: Option<&str>,
    new_api_user: Option<&str>,
    query: &[(&str, &str)],
) -> Result<Fetched, String> {
    let mut url = join_inference_endpoint(base.as_str(), path)
        .map_err(|_| component_error(component, CODE_ENDPOINT))?;
    if !query.is_empty() {
        let mut pairs = url.query_pairs_mut();
        for (key, value) in query {
            pairs.append_pair(key, value);
        }
    }
    if !same_origin(base, &url) {
        return Err(component_error(component, CODE_ENDPOINT));
    }

    let mut builder = client
        .request(method, url.clone())
        .header(reqwest::header::ACCEPT, "application/json")
        .timeout(REQUEST_TIMEOUT);
    if let Some(token) = auth {
        let value = HeaderValue::from_str(&format!("Bearer {token}"))
            .map_err(|_| component_error(component, CODE_PARSE))?;
        builder = builder.header(reqwest::header::AUTHORIZATION, value);
    }
    if let Some(user_id) = new_api_user {
        let value =
            HeaderValue::from_str(user_id).map_err(|_| component_error(component, CODE_PARSE))?;
        builder = builder.header("New-Api-User", value);
    }

    let response = builder.send().await.map_err(|error| {
        if error.is_timeout() {
            component_error(component, CODE_TIMEOUT)
        } else {
            component_error(component, CODE_NETWORK)
        }
    })?;

    if response.status().is_redirection() || !same_origin(&url, response.url()) {
        return Err(component_error(component, CODE_REDIRECT));
    }
    let status = response.status();

    let mut body = Vec::new();
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|error| {
            if error.is_timeout() {
                component_error(component, CODE_TIMEOUT)
            } else {
                component_error(component, CODE_NETWORK)
            }
        })?;
        if body.len().saturating_add(chunk.len()) > MAX_BODY_BYTES {
            return Err(component_error(component, CODE_OVERSIZE));
        }
        body.extend_from_slice(&chunk);
    }

    match status {
        StatusCode::OK | StatusCode::CREATED => {}
        StatusCode::UNAUTHORIZED => {
            return Err(classify_unauthorized(component, &body));
        }
        StatusCode::FORBIDDEN => return Err(component_error(component, CODE_FORBIDDEN)),
        _ => return Err(component_error(component, CODE_HTTP_STATUS)),
    }

    let value: Value =
        serde_json::from_slice(&body).map_err(|_| component_error(component, CODE_PARSE))?;
    Ok(Fetched { value })
}

fn strip_bearer_prefix(value: &str) -> &str {
    value
        .strip_prefix("Bearer ")
        .or_else(|| value.strip_prefix("bearer "))
        .unwrap_or(value)
        .trim()
}

/// New API v0.4.6+ PAT calls need `New-Api-User: <numeric id>` alongside Bearer.
/// Store that as `userId:token`. Newer builds ignore the extra header.
pub(crate) fn split_new_api_user_credential(raw: &str) -> (Option<&str>, &str) {
    let raw = strip_bearer_prefix(raw.trim());
    if let Some((id, token)) = raw.split_once(':') {
        let id = id.trim();
        let token = strip_bearer_prefix(token.trim());
        if !id.is_empty() && id.bytes().all(|b| b.is_ascii_digit()) && !token.is_empty() {
            return (Some(id), token);
        }
    }
    (None, raw)
}

fn classify_unauthorized(component: &str, body: &[u8]) -> String {
    let parsed = serde_json::from_slice::<Value>(body).ok();
    let message = parsed
        .as_ref()
        .and_then(|value| json_str(value.get("message")))
        .unwrap_or("");
    let lower = message.to_ascii_lowercase();
    let code = if message.contains("New-Api-User") || lower.contains("new-api-user") {
        if lower.contains("match") || message.contains("不匹配") || message.contains("不符") {
            CODE_USER_ID_MISMATCH
        } else {
            CODE_USER_ID_REQUIRED
        }
    } else {
        CODE_UNAUTHORIZED
    };
    component_error(component, code)
}

fn payload(value: &Value) -> &Value {
    value.get("data").unwrap_or(value)
}

pub(crate) fn new_api_data<'a>(value: &'a Value, component: &str) -> Result<&'a Value, String> {
    match value.get("success") {
        Some(Value::Bool(true)) => value
            .get("data")
            .ok_or_else(|| component_error(component, CODE_PARSE)),
        _ => Err(component_error(component, CODE_PARSE)),
    }
}

fn new_api_token_usage_data<'a>(value: &'a Value, component: &str) -> Result<&'a Value, String> {
    match value.get("code") {
        Some(Value::Bool(true)) => value
            .get("data")
            .filter(|value| value.is_object())
            .ok_or_else(|| component_error(component, CODE_PARSE)),
        _ => Err(component_error(component, CODE_PARSE)),
    }
}

fn sub2_data<'a>(value: &'a Value, component: &str) -> Result<&'a Value, String> {
    match json_i64(value.get("code")) {
        Some(0) => value
            .get("data")
            .ok_or_else(|| component_error(component, CODE_PARSE)),
        _ => Err(component_error(component, CODE_PARSE)),
    }
}

fn models_rows<'a>(value: &'a Value, component: &str) -> Result<&'a Vec<Value>, String> {
    let payload = payload(value);
    payload
        .as_array()
        .or_else(|| payload.get("data").and_then(Value::as_array))
        .or_else(|| value.get("data").and_then(Value::as_array))
        .ok_or_else(|| component_error(component, CODE_PARSE))
}

fn json_f64(value: Option<&Value>) -> Option<f64> {
    let value = value?;
    let parsed: Option<f64> = match value {
        Value::Null => None,
        Value::Number(number) => number.as_f64(),
        Value::String(text) => {
            let text = text.trim();
            if text.is_empty() {
                None
            } else {
                text.parse().ok()
            }
        }
        _ => None,
    };
    parsed.filter(|number| number.is_finite())
}

fn json_i64(value: Option<&Value>) -> Option<i64> {
    let value = value?;
    match value {
        Value::Null => None,
        Value::Number(number) => number.as_i64(),
        Value::String(text) => text.trim().parse().ok(),
        _ => None,
    }
}

fn json_bool(value: Option<&Value>) -> Option<bool> {
    match value? {
        Value::Bool(flag) => Some(*flag),
        _ => None,
    }
}

fn json_str(value: Option<&Value>) -> Option<&str> {
    value
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
}

fn snapshot_expiry(now: i64) -> i64 {
    now.saturating_add(SNAPSHOT_TTL_SECS)
}

fn leaks_secret(text: &str, secrets: &[&str]) -> bool {
    secrets
        .iter()
        .any(|secret| !secret.is_empty() && (text == *secret || text.contains(secret)))
}

fn snapshot_record_leaks<T: serde::Serialize>(record: &T, secrets: &[&str]) -> bool {
    fn inspect(value: &Value, secrets: &[&str]) -> bool {
        match value {
            Value::String(text) => leaks_secret(text, secrets),
            Value::Array(values) => values.iter().any(|v| inspect(v, secrets)),
            Value::Object(values) => values.values().any(|v| inspect(v, secrets)),
            _ => false,
        }
    }
    serde_json::to_value(record).map_or(true, |value| inspect(&value, secrets))
}

fn scrub_secrets(snapshot: &mut PlatformSnapshot, user: Option<&str>, key: Option<&str>) {
    let secrets: Vec<&str> = [user, key].into_iter().flatten().collect();
    if secrets.is_empty() {
        return;
    }
    let mut leaked = false;
    snapshot.models.retain(|row| {
        let safe = !snapshot_record_leaks(row, &secrets);
        leaked |= !safe;
        safe
    });
    snapshot.groups.retain(|row| {
        let safe = !snapshot_record_leaks(row, &secrets);
        leaked |= !safe;
        safe
    });
    snapshot.quotas.retain(|row| {
        let safe = !snapshot_record_leaks(row, &secrets);
        leaked |= !safe;
        safe
    });
    snapshot.prices.retain(|row| {
        let safe = !snapshot_record_leaks(row, &secrets);
        leaked |= !safe;
        safe
    });
    if snapshot_record_leaks(&snapshot.billing_preference, &secrets) {
        leaked = true;
        snapshot.billing_preference = None;
    }
    if leaked {
        push_error(snapshot, component_error("snapshot", CODE_SECRET));
    }
}
fn push_error(snapshot: &mut PlatformSnapshot, error: String) {
    if !snapshot.errors.iter().any(|existing| existing == &error) {
        snapshot.errors.push(error);
    }
}

async fn read_new_api(
    client: &reqwest::Client,
    base: &reqwest::Url,
    request: &PlatformReadRequest<'_>,
    user: Option<&str>,
    key: Option<&str>,
    snapshot: &mut PlatformSnapshot,
) {
    let (new_api_user, bearer) = match user {
        Some(raw) => {
            let (id, token) = split_new_api_user_credential(raw);
            (id, Some(token))
        }
        None => (None, None),
    };

    let mut quota_per_unit: Option<f64> = None;
    match get_json(client, base, "api/status", "new_api.status", None, None).await {
        Ok(fetched) => match new_api_data(&fetched.value, "new_api.status") {
            Ok(data) => {
                quota_per_unit = json_f64(data.get("quota_per_unit")).filter(|value| *value > 0.0);
            }
            Err(error) => push_error(snapshot, error),
        },
        Err(error) => push_error(snapshot, error),
    }

    let mut user_group: Option<String> = None;
    if let Some(user) = bearer {
        match get_json(
            client,
            base,
            "api/user/self",
            "new_api.user_self",
            Some(user),
            new_api_user,
        )
        .await
        {
            Ok(fetched) => match new_api_data(&fetched.value, "new_api.user_self") {
                Ok(data) => parse_new_api_self(data, snapshot, &mut user_group, quota_per_unit),
                Err(error) => push_error(snapshot, error),
            },
            Err(error) => push_error(snapshot, error),
        }

        match get_json(
            client,
            base,
            "api/user/self/groups",
            "new_api.user_groups",
            Some(user),
            new_api_user,
        )
        .await
        {
            Ok(fetched) => match new_api_data(&fetched.value, "new_api.user_groups") {
                Ok(data) => {
                    if let Err(error) = parse_new_api_groups(data, snapshot) {
                        push_error(snapshot, error);
                    }
                }
                Err(error) => push_error(snapshot, error),
            },
            Err(error) => push_error(snapshot, error),
        }

        match get_json(
            client,
            base,
            "api/subscription/self",
            "new_api.subscription_self",
            Some(user),
            new_api_user,
        )
        .await
        {
            Ok(fetched) => match new_api_data(&fetched.value, "new_api.subscription_self") {
                Ok(data) => parse_new_api_subscriptions(data, snapshot, quota_per_unit),
                Err(error) => push_error(snapshot, error),
            },
            Err(error) => push_error(snapshot, error),
        }

        // Optional catalog: older sites omit or change this envelope, so a
        // fetch or envelope error is not recorded.
        if let Ok(fetched) = get_json(
            client,
            base,
            "api/token/auto-groups",
            "new_api.token_auto_groups",
            Some(user),
            new_api_user,
        )
        .await
            && let Ok(data) = new_api_data(&fetched.value, "new_api.token_auto_groups")
        {
            parse_new_api_auto_groups(data, snapshot);
        }
    }

    let mut allowed_models: BTreeSet<String> = BTreeSet::new();
    if let Some(key) = key {
        match get_json(client, base, "v1/models", "new_api.models", Some(key), None).await {
            Ok(fetched) => {
                if let Err(error) = collect_models(
                    &fetched.value,
                    request.group.id.as_deref(),
                    request.group.platform.as_deref(),
                    SRC_V1_MODELS,
                    "new_api.models",
                    &mut allowed_models,
                    snapshot,
                ) {
                    push_error(snapshot, error);
                }
            }
            Err(error) => push_error(snapshot, error),
        }

        match get_json(
            client,
            base,
            "api/usage/token/",
            "new_api.token_usage",
            Some(key),
            None,
        )
        .await
        {
            Ok(fetched) => match new_api_token_usage_data(&fetched.value, "new_api.token_usage") {
                Ok(data) => {
                    parse_new_api_token_usage(data, &mut allowed_models, snapshot, quota_per_unit)
                }
                Err(error) => push_error(snapshot, error),
            },
            Err(error) => push_error(snapshot, error),
        }
    }

    match get_json(
        client,
        base,
        "api/pricing",
        "new_api.pricing",
        bearer,
        new_api_user,
    )
    .await
    {
        Ok(fetched) => match new_api_data(&fetched.value, "new_api.pricing") {
            Ok(_) => parse_new_api_pricing(
                &fetched.value,
                request,
                user_group.as_deref(),
                quota_per_unit,
                &allowed_models,
                snapshot,
            ),
            Err(error) => push_error(snapshot, error),
        },
        Err(error) => push_error(snapshot, error),
    }
}

/// New API stores wallet/token amounts as integer quota points. The site
/// `quota_per_unit` (typically 500000) is the observed points-per-dollar rate
/// from `GET /api/status`. Convert when that rate is known so remaining is a
/// dollar balance; otherwise keep the native `quota` unit.
fn scale_new_api_quota(value: Option<f64>, quota_per_unit: Option<f64>) -> Option<f64> {
    match (value, quota_per_unit) {
        (Some(raw), Some(unit)) if unit > 0.0 && raw.is_finite() => Some(raw / unit),
        (Some(raw), _) if raw.is_finite() => Some(raw),
        _ => None,
    }
}

fn new_api_quota_unit(quota_per_unit: Option<f64>) -> &'static str {
    if quota_per_unit.filter(|unit| *unit > 0.0).is_some() {
        "usd"
    } else {
        "quota"
    }
}

fn parse_new_api_self(
    data: &Value,
    snapshot: &mut PlatformSnapshot,
    user_group: &mut Option<String>,
    quota_per_unit: Option<f64>,
) {
    if !data.is_object() {
        push_error(snapshot, component_error("new_api.user_self", CODE_PARSE));
        return;
    }
    if let Some(group) = json_str(data.get("group")) {
        *user_group = Some(group.to_string());
        if !snapshot
            .groups
            .iter()
            .any(|item| item.id.as_deref() == Some(group))
        {
            snapshot.groups.push(PlatformGroup {
                subscription_type: None,
                id: Some(group.to_string()),
                platform: None,
                auto_groups: Vec::new(),
                verified: true,
            });
        }
    }

    let remaining = scale_new_api_quota(json_f64(data.get("quota")), quota_per_unit);
    let used = scale_new_api_quota(json_f64(data.get("used_quota")), quota_per_unit);
    if remaining.is_some() || used.is_some() {
        snapshot.quotas.push(PlatformQuota {
            kind: PlatformQuotaKind::Wallet,
            scope_id: "wallet".to_string(),
            unit: new_api_quota_unit(quota_per_unit).to_string(),
            used,
            remaining,
            // Wallet balance + lifetime consumption is not an observed limit.
            limit: None,
            unlimited: false,
            period: None,
            resets_at: None,
            expires_at: None,
            source: "new_api.user_self".to_string(),
        });
    }
}

fn parse_new_api_groups(data: &Value, snapshot: &mut PlatformSnapshot) -> Result<(), String> {
    let Some(map) = data.as_object() else {
        return Err(component_error("new_api.user_groups", CODE_PARSE));
    };
    for (name, _meta) in map {
        let auto = name == "auto";
        if !snapshot
            .groups
            .iter()
            .any(|item| item.id.as_deref() == Some(name))
        {
            snapshot.groups.push(PlatformGroup {
                subscription_type: None,
                id: Some(name.clone()),
                platform: None,
                auto_groups: Vec::new(),
                verified: !auto,
            });
        }
    }
    Ok(())
}

fn parse_new_api_auto_groups(data: &Value, snapshot: &mut PlatformSnapshot) {
    let groups = data
        .get("groups")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let auto: Vec<String> = groups
        .iter()
        .filter_map(|value| json_str(Some(value)).map(str::to_string))
        .collect();
    if auto.is_empty() {
        return;
    }
    if let Some(existing) = snapshot
        .groups
        .iter_mut()
        .find(|group| group.id.as_deref() == Some("auto"))
    {
        existing.auto_groups = auto;
        existing.verified = true;
        return;
    }
    snapshot.groups.push(PlatformGroup {
        subscription_type: None,
        id: Some("auto".to_string()),
        platform: None,
        auto_groups: auto,
        verified: true,
    });
}

fn parse_new_api_subscriptions(
    data: &Value,
    snapshot: &mut PlatformSnapshot,
    quota_per_unit: Option<f64>,
) {
    if !data.is_object() {
        push_error(
            snapshot,
            component_error("new_api.subscription_self", CODE_PARSE),
        );
        return;
    }
    snapshot.billing_preference = json_str(data.get("billing_preference")).map(str::to_string);

    let mut overflow: Option<bool> = None;
    if let Some(items) = data.get("subscriptions").and_then(Value::as_array) {
        for item in items {
            let sub = item.get("subscription").unwrap_or(item);
            let scope = json_i64(sub.get("id"))
                .map(|id| id.to_string())
                .or_else(|| json_str(sub.get("id")).map(str::to_string))
                .unwrap_or_else(|| "subscription".to_string());
            let total = scale_new_api_quota(json_f64(sub.get("amount_total")), quota_per_unit);
            let used = scale_new_api_quota(json_f64(sub.get("amount_used")), quota_per_unit);
            let remaining = match (total, used) {
                (Some(total), Some(used)) => Some(total - used),
                _ => None,
            };
            let unlimited = total == Some(0.0);
            snapshot.quotas.push(PlatformQuota {
                kind: PlatformQuotaKind::Subscription,
                scope_id: scope,
                unit: new_api_quota_unit(quota_per_unit).to_string(),
                used,
                remaining: if unlimited { None } else { remaining },
                limit: if unlimited { None } else { total },
                unlimited,
                period: json_str(sub.get("quota_reset_period")).map(str::to_string),
                resets_at: json_i64(sub.get("next_reset_time")).filter(|ts| *ts > 0),
                expires_at: json_i64(sub.get("end_time")).filter(|ts| *ts > 0),
                source: "new_api.subscription_self".to_string(),
            });
            if let Some(flag) = json_bool(sub.get("allow_wallet_overflow")) {
                overflow = Some(overflow.unwrap_or(true) && flag);
            }
        }
    }
    if overflow.is_some() {
        snapshot.wallet_overflow = overflow;
    }
}

fn parse_new_api_token_usage(
    data: &Value,
    allowed_models: &mut BTreeSet<String>,
    snapshot: &mut PlatformSnapshot,
    quota_per_unit: Option<f64>,
) {
    let unlimited = json_bool(data.get("unlimited_quota")).unwrap_or(false);
    let used = scale_new_api_quota(json_f64(data.get("total_used")), quota_per_unit);
    let remaining = scale_new_api_quota(json_f64(data.get("total_available")), quota_per_unit);
    let granted = scale_new_api_quota(json_f64(data.get("total_granted")), quota_per_unit);
    snapshot.quotas.push(PlatformQuota {
        kind: PlatformQuotaKind::KeyLimit,
        scope_id: json_str(data.get("name")).unwrap_or("key").to_string(),
        unit: new_api_quota_unit(quota_per_unit).to_string(),
        used,
        remaining: if unlimited { None } else { remaining },
        limit: if unlimited { None } else { granted },
        unlimited,
        period: None,
        resets_at: None,
        expires_at: json_i64(data.get("expires_at")).filter(|ts| *ts > 0),
        source: "new_api.token_usage".to_string(),
    });
    if let Some(group) = json_str(data.get("group"))
        && !snapshot
            .groups
            .iter()
            .any(|item| item.id.as_deref() == Some(group))
    {
        snapshot.groups.push(PlatformGroup {
            subscription_type: None,
            id: Some(group.to_string()),
            platform: None,
            auto_groups: Vec::new(),
            verified: true,
        });
    }

    if json_bool(data.get("model_limits_enabled")) == Some(true) {
        if let Some(limits) = data.get("model_limits").and_then(Value::as_object) {
            for (model, allowed) in limits {
                if allowed.as_bool() == Some(true) {
                    allowed_models.insert(model.clone());
                }
            }
        }
        for model in allowed_models.iter() {
            if snapshot.models.iter().any(|item| item.id == *model) {
                continue;
            }
            snapshot.models.push(PlatformModel {
                id: model.clone(),
                platform: None,
                group_id: None,
                source: SRC_TOKEN_LIMITS.to_string(),
            });
        }
    }
}

fn collect_models(
    value: &Value,
    group_id: Option<&str>,
    platform: Option<&str>,
    source: &str,
    component: &str,
    allowed: &mut BTreeSet<String>,
    snapshot: &mut PlatformSnapshot,
) -> Result<(), String> {
    let rows = models_rows(value, component)?;
    for row in rows {
        let id = json_str(Some(row))
            .or_else(|| json_str(row.get("id")))
            .or_else(|| json_str(row.get("name")))
            .map(str::to_string);
        let Some(id) = id else {
            continue;
        };
        if !allowed.insert(id.clone()) {
            continue;
        }
        snapshot.models.push(PlatformModel {
            id,
            platform: json_str(row.get("platform"))
                .or_else(|| json_str(row.get("owned_by")))
                .or(platform)
                .map(str::to_string),
            group_id: group_id.map(str::to_string),
            source: source.to_string(),
        });
    }
    Ok(())
}

fn parse_new_api_pricing(
    value: &Value,
    request: &PlatformReadRequest<'_>,
    user_group: Option<&str>,
    quota_per_unit: Option<f64>,
    allowed_models: &BTreeSet<String>,
    snapshot: &mut PlatformSnapshot,
) {
    if let Some(auto) = value.get("auto_groups").and_then(Value::as_array) {
        let groups: Vec<String> = auto
            .iter()
            .filter_map(|item| json_str(Some(item)).map(str::to_string))
            .collect();
        if !groups.is_empty() {
            if let Some(existing) = snapshot
                .groups
                .iter_mut()
                .find(|group| group.id.as_deref() == Some("auto"))
            {
                if existing.auto_groups.is_empty() {
                    existing.auto_groups = groups;
                }
            } else {
                snapshot.groups.push(PlatformGroup {
                    subscription_type: None,
                    id: Some("auto".to_string()),
                    platform: None,
                    auto_groups: groups,
                    verified: true,
                });
            }
        }
    }

    let selected = request
        .group
        .id
        .as_deref()
        .or(user_group)
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let auto_selected = selected == Some("auto");
    let group_ratios = value.get("group_ratio").and_then(Value::as_object);
    let multiplier = if auto_selected {
        None
    } else {
        selected.and_then(|name| json_f64(group_ratios.and_then(|map| map.get(name))))
    };

    let valid_until = snapshot_expiry(request.now);
    let Some(rows) = payload(value).as_array() else {
        push_error(snapshot, component_error("new_api.pricing", CODE_PARSE));
        return;
    };
    if allowed_models.is_empty() {
        return;
    }
    for row in rows {
        let model = match json_str(row.get("model_name")).or_else(|| json_str(row.get("model"))) {
            Some(name) if allowed_models.contains(name) => name.to_string(),
            _ => continue,
        };
        let mut price = PlatformPrice {
            model,
            group_id: selected.map(str::to_string),
            currency: "USD".to_string(),
            input: None,
            output: None,
            cache_read: None,
            cache_write: None,
            source: SRC_NEW_API_PRICING.to_string(),
            official_reference: false,
            unavailable_reason: None,
            valid_until,
        };
        let billing_mode = json_str(row.get("billing_mode")).unwrap_or("");
        let billing_expr = json_str(row.get("billing_expr")).unwrap_or("");
        if billing_mode.contains("expr")
            || billing_mode.contains("tier")
            || !billing_expr.is_empty()
        {
            price.unavailable_reason = Some(
                if billing_mode.contains("tier") {
                    UNAVAIL_TIERED
                } else {
                    UNAVAIL_EXPRESSION
                }
                .to_string(),
            );
            snapshot.prices.push(price);
            continue;
        }
        let Some(quota_type) = json_i64(row.get("quota_type")) else {
            price.unavailable_reason = Some(UNAVAIL_MISSING_RATE.into());
            snapshot.prices.push(price);
            continue;
        };
        if quota_type != 0 {
            price.unavailable_reason = Some(UNAVAIL_PER_REQUEST.to_string());
            snapshot.prices.push(price);
            continue;
        }
        if auto_selected {
            price.unavailable_reason = Some(UNAVAIL_AUTO.to_string());
            snapshot.prices.push(price);
            continue;
        }
        let Some(unit) = quota_per_unit else {
            price.unavailable_reason = Some(UNAVAIL_MISSING_UNIT.to_string());
            snapshot.prices.push(price);
            continue;
        };
        let Some(group_ratio) = multiplier else {
            price.unavailable_reason = Some(UNAVAIL_MISSING_MULT.to_string());
            snapshot.prices.push(price);
            continue;
        };
        let Some(model_ratio) = json_f64(row.get("model_ratio")) else {
            price.unavailable_reason = Some(UNAVAIL_MISSING_RATE.to_string());
            snapshot.prices.push(price);
            continue;
        };
        // Currency per token = model_ratio * group_ratio / quota_per_unit.
        // group_ratio is applied exactly once.
        let input = (model_ratio * group_ratio) / unit;
        let Some(completion) = json_f64(row.get("completion_ratio")) else {
            price.unavailable_reason = Some(UNAVAIL_MISSING_RATE.to_string());
            snapshot.prices.push(price);
            continue;
        };
        price.input = Some(input);
        price.output = Some(input * completion);
        if let Some(cache_ratio) = json_f64(row.get("cache_ratio")) {
            price.cache_read = Some(input * cache_ratio);
        }
        if let Some(create_ratio) = json_f64(row.get("create_cache_ratio")) {
            price.cache_write = Some(input * create_ratio);
        }
        // Anonymous pricing cannot resolve the user's group-to-group override.
        if user_group.is_none() {
            price.unavailable_reason = Some("user_identity_required".into());
        } else if !row
            .get("enable_groups")
            .and_then(Value::as_array)
            .is_some_and(|groups| {
                groups
                    .iter()
                    .any(|g| g.as_str() == Some("all") || g.as_str() == selected)
            })
        {
            price.unavailable_reason = Some("group_model_unavailable".into());
        }
        snapshot.prices.push(price);
    }
}

async fn read_sub2(
    client: &reqwest::Client,
    base: &reqwest::Url,
    request: &PlatformReadRequest<'_>,
    user: Option<&str>,
    key: Option<&str>,
    snapshot: &mut PlatformSnapshot,
) {
    let mut allowed_models: BTreeSet<String> = BTreeSet::new();
    let mut billing_multiplier: Option<f64> = None;
    let mut peak_enabled = false;

    if let Some(user) = user {
        match get_json(
            client,
            base,
            "api/v1/user/profile",
            "sub2api.profile",
            Some(user),
            None,
        )
        .await
        {
            Ok(fetched) => match sub2_data(&fetched.value, "sub2api.profile") {
                Ok(data) => parse_sub2_profile(data, snapshot),
                Err(error) => push_error(snapshot, error),
            },
            Err(error) => push_error(snapshot, error),
        }

        match get_json(
            client,
            base,
            "api/v1/subscriptions/summary",
            "sub2api.subscriptions",
            Some(user),
            None,
        )
        .await
        {
            Ok(fetched) => match sub2_data(&fetched.value, "sub2api.subscriptions") {
                Ok(data) => parse_sub2_subscriptions(data, snapshot),
                Err(error) => push_error(snapshot, error),
            },
            Err(error) => push_error(snapshot, error),
        }

        match get_json(
            client,
            base,
            "api/v1/groups/available",
            "sub2api.groups",
            Some(user),
            None,
        )
        .await
        {
            Ok(fetched) => match sub2_data(&fetched.value, "sub2api.groups") {
                Ok(data) => parse_sub2_groups(data, snapshot),
                Err(error) => push_error(snapshot, error),
            },
            Err(error) => push_error(snapshot, error),
        }
    }

    if let Some(key) = key {
        match get_json(client, base, "v1/models", "sub2api.models", Some(key), None).await {
            Ok(fetched) => {
                if let Err(error) = collect_models(
                    &fetched.value,
                    request.group.id.as_deref(),
                    request.group.platform.as_deref(),
                    SRC_V1_MODELS,
                    "sub2api.models",
                    &mut allowed_models,
                    snapshot,
                ) {
                    push_error(snapshot, error);
                }
            }
            Err(error) => push_error(snapshot, error),
        }

        match get_json(client, base, "v1/usage", "sub2api.usage", Some(key), None).await {
            Ok(fetched) => {
                if let Err(error) = parse_sub2_usage(&fetched.value, snapshot) {
                    push_error(snapshot, error);
                }
            }
            Err(error) => push_error(snapshot, error),
        }

        match get_json(
            client,
            base,
            "v1/sub2api/billing",
            "sub2api.billing",
            Some(key),
            None,
        )
        .await
        {
            Ok(fetched) => match parse_sub2_billing(&fetched.value) {
                Ok((multiplier, peak)) => {
                    billing_multiplier = multiplier;
                    peak_enabled = peak;
                }
                Err(error) => push_error(snapshot, error),
            },
            Err(error) => push_error(snapshot, error),
        }
    }

    let plaza_auth = user;
    match get_json(
        client,
        base,
        "api/v1/model-plaza",
        "sub2api.plaza",
        plaza_auth,
        None,
    )
    .await
    {
        Ok(fetched) => match sub2_data(&fetched.value, "sub2api.plaza") {
            Ok(data) => parse_sub2_plaza_prices(
                data,
                request,
                &allowed_models,
                billing_multiplier,
                peak_enabled,
                snapshot,
            ),
            Err(error) => push_error(snapshot, error),
        },
        Err(error) => push_error(snapshot, error),
    }
}

fn parse_sub2_profile(data: &Value, snapshot: &mut PlatformSnapshot) {
    if !data.is_object() {
        push_error(snapshot, component_error("sub2api.profile", CODE_PARSE));
        return;
    }
    let remaining = json_f64(data.get("balance"));
    if remaining.is_none() && data.get("balance").is_none() {
        push_error(snapshot, component_error("sub2api.profile", CODE_PARSE));
        return;
    }
    snapshot.quotas.push(PlatformQuota {
        kind: PlatformQuotaKind::Wallet,
        scope_id: "wallet".to_string(),
        unit: "usd".to_string(),
        used: None,
        remaining,
        limit: None,
        unlimited: false,
        period: None,
        resets_at: None,
        expires_at: None,
        source: "sub2api.user.profile".to_string(),
    });
}

fn parse_sub2_usage(value: &Value, snapshot: &mut PlatformSnapshot) -> Result<(), String> {
    let mode =
        json_str(value.get("mode")).ok_or_else(|| component_error("sub2api.usage", CODE_PARSE))?;
    match mode {
        "quota_limited" => {
            let absent = Value::Null;
            let quota = value
                .get("quota")
                .filter(|value| value.is_object())
                .unwrap_or(&absent);
            snapshot.quotas.push(PlatformQuota {
                kind: PlatformQuotaKind::KeyLimit,
                scope_id: "key".to_string(),
                unit: json_str(quota.get("unit"))
                    .or_else(|| json_str(value.get("unit")))
                    .unwrap_or("usd")
                    .to_ascii_lowercase(),
                used: json_f64(quota.get("used"))
                    .or_else(|| json_f64(value.pointer("/usage/total/actual_cost"))),
                remaining: json_f64(quota.get("remaining"))
                    .or_else(|| json_f64(value.get("remaining"))),
                limit: json_f64(quota.get("limit")),
                unlimited: false,
                period: None,
                resets_at: None,
                expires_at: json_str(value.get("expires_at")).and_then(parse_rfc3339_secs),
                source: "sub2api.v1.usage".to_string(),
            });
            if let Some(windows) = value.get("rate_limits").and_then(Value::as_array) {
                for window in windows {
                    let period = json_str(window.get("window")).map(str::to_string);
                    snapshot.quotas.push(PlatformQuota {
                        kind: PlatformQuotaKind::KeyLimit,
                        scope_id: format!("key:{}", period.as_deref().unwrap_or("window")),
                        unit: "usd".into(),
                        used: json_f64(window.get("used")),
                        remaining: json_f64(window.get("remaining")),
                        limit: json_f64(window.get("limit")),
                        unlimited: false,
                        period,
                        resets_at: json_str(window.get("reset_at")).and_then(parse_rfc3339_secs),
                        expires_at: None,
                        source: "sub2api.v1.usage".into(),
                    });
                }
            }
            Ok(())
        }
        "unrestricted" => {
            snapshot.quotas.push(PlatformQuota {
                kind: PlatformQuotaKind::KeyLimit,
                scope_id: "key".into(),
                unit: "usd".into(),
                used: json_f64(value.pointer("/usage/total/actual_cost")),
                remaining: None,
                limit: None,
                unlimited: true,
                period: None,
                resets_at: None,
                expires_at: None,
                source: "sub2api.v1.usage".into(),
            });
            if let Some(subscription) = value.get("subscription").filter(|v| v.is_object()) {
                for period in ["daily", "weekly", "monthly"] {
                    let used = json_f64(subscription.get(format!("{period}_usage_usd")));
                    let limit = json_f64(subscription.get(format!("{period}_limit_usd")));
                    if used.is_none() && limit.is_none() {
                        continue;
                    }
                    snapshot.quotas.push(PlatformQuota {
                        kind: PlatformQuotaKind::Subscription,
                        scope_id: format!("key_subscription:{period}"),
                        unit: "usd".into(),
                        used,
                        limit,
                        remaining: limit.zip(used).map(|(l, u)| l - u),
                        unlimited: false,
                        period: Some(period.into()),
                        resets_at: if period == "weekly" {
                            json_str(subscription.get("weekly_window_start"))
                                .and_then(parse_rfc3339_secs)
                                .map(|t| t + 7 * 86400)
                        } else {
                            None
                        },
                        expires_at: json_str(subscription.get("expires_at"))
                            .and_then(parse_rfc3339_secs),
                        source: "sub2api.v1.usage".into(),
                    });
                }
                return Ok(());
            }
            if snapshot
                .quotas
                .iter()
                .any(|quota| matches!(quota.kind, PlatformQuotaKind::Wallet))
            {
                return Ok(());
            }
            // Subscription mode may expose a limiting-window `remaining` too.
            // Only the explicit `balance` field proves a wallet observation.
            let remaining = json_f64(value.get("balance"));
            if remaining.is_none() {
                return Ok(());
            }
            snapshot.quotas.push(PlatformQuota {
                kind: PlatformQuotaKind::Wallet,
                scope_id: "wallet".to_string(),
                unit: json_str(value.get("unit"))
                    .unwrap_or("usd")
                    .to_ascii_lowercase(),
                used: None,
                remaining,
                limit: None,
                unlimited: false,
                period: None,
                resets_at: None,
                expires_at: None,
                source: "sub2api.v1.usage".to_string(),
            });
            Ok(())
        }
        _ => Err(component_error("sub2api.usage", CODE_PARSE)),
    }
}

fn parse_sub2_billing(value: &Value) -> Result<(Option<f64>, bool), String> {
    let object = json_str(value.get("object"));
    let multiplier = json_f64(value.get("effective_rate_multiplier"))
        .or_else(|| json_f64(value.get("resolved_rate_multiplier")));
    if object != Some("sub2api.key_billing")
        || json_i64(value.get("schema_version")) != Some(1)
        || json_str(value.get("billing_scope")) != Some("token")
    {
        return Err(component_error("sub2api.billing", CODE_PARSE));
    }
    Ok((
        multiplier,
        json_bool(value.get("peak_rate_enabled")).unwrap_or(false),
    ))
}

fn parse_sub2_subscriptions(data: &Value, snapshot: &mut PlatformSnapshot) {
    if !data.is_object() {
        push_error(
            snapshot,
            component_error("sub2api.subscriptions", CODE_PARSE),
        );
        return;
    }
    let items = data
        .get("subscriptions")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    for item in items {
        let scope = json_i64(item.get("id"))
            .map(|id| id.to_string())
            .or_else(|| json_str(item.get("id")).map(str::to_string))
            .unwrap_or_else(|| "subscription".to_string());
        let expires = json_str(item.get("expires_at")).and_then(parse_rfc3339_secs);
        for (period, used_key, limit_key) in [
            ("daily", "daily_used_usd", "daily_limit_usd"),
            ("weekly", "weekly_used_usd", "weekly_limit_usd"),
            ("monthly", "monthly_used_usd", "monthly_limit_usd"),
        ] {
            let used = json_f64(item.get(used_key));
            let limit = json_f64(item.get(limit_key));
            if used.is_none() && limit.is_none() {
                continue;
            }
            let remaining = match (limit, used) {
                (Some(limit), Some(used)) => Some(limit - used),
                _ => None,
            };
            snapshot.quotas.push(PlatformQuota {
                kind: PlatformQuotaKind::Subscription,
                scope_id: format!("{scope}:{period}"),
                unit: "usd".to_string(),
                used,
                remaining,
                limit,
                unlimited: false,
                period: Some(period.to_string()),
                resets_at: None,
                expires_at: expires,
                source: "sub2api.subscriptions.summary".to_string(),
            });
        }
        if let Some(name) = json_str(item.get("group_name")) {
            let id = json_i64(item.get("group_id")).map(|id| id.to_string());
            if !snapshot
                .groups
                .iter()
                .any(|group| group.id == id || group.id.as_deref() == Some(name))
            {
                snapshot.groups.push(PlatformGroup {
                    subscription_type: None,
                    id: id.or_else(|| Some(name.to_string())),
                    platform: None,
                    auto_groups: Vec::new(),
                    verified: true,
                });
            }
        }
    }
}

fn parse_sub2_groups(data: &Value, snapshot: &mut PlatformSnapshot) {
    let rows = data
        .as_array()
        .or_else(|| data.get("items").and_then(Value::as_array));
    let Some(rows) = rows else {
        push_error(snapshot, component_error("sub2api.groups", CODE_PARSE));
        return;
    };
    for row in rows {
        let id = json_i64(row.get("id"))
            .map(|id| id.to_string())
            .or_else(|| json_str(row.get("id")).map(str::to_string))
            .or_else(|| json_str(row.get("name")).map(str::to_string));
        let platform = json_str(row.get("platform")).map(str::to_string);
        if let Some(existing) = snapshot.groups.iter_mut().find(|group| group.id == id) {
            if existing.platform.is_none() {
                existing.platform = platform;
            }
            existing.verified = true;
            existing.subscription_type = json_str(row.get("subscription_type")).map(str::to_string);
            continue;
        }
        snapshot.groups.push(PlatformGroup {
            subscription_type: json_str(row.get("subscription_type")).map(str::to_string),
            id,
            platform,
            auto_groups: Vec::new(),
            verified: true,
        });
    }
}

fn parse_sub2_plaza_prices(
    data: &Value,
    request: &PlatformReadRequest<'_>,
    allowed_models: &BTreeSet<String>,
    billing_multiplier: Option<f64>,
    billing_peak_enabled: bool,
    snapshot: &mut PlatformSnapshot,
) {
    if allowed_models.is_empty() {
        return;
    }
    let Some(groups) = data.get("groups").and_then(Value::as_array) else {
        push_error(snapshot, component_error("sub2api.plaza", CODE_PARSE));
        return;
    };
    let valid_until = snapshot_expiry(request.now);
    let selected = request.group.id.as_deref();

    for group in groups {
        let group_id = json_i64(group.get("id"))
            .map(|id| id.to_string())
            .or_else(|| json_str(group.get("id")).map(str::to_string));
        if let Some(selected) = selected
            && group_id.as_deref() != Some(selected)
            && json_str(group.get("name")) != Some(selected)
        {
            continue;
        }
        let peak_enabled =
            billing_peak_enabled || json_bool(group.get("peak_rate_enabled")).unwrap_or(false);
        let time_varying = peak_enabled
            || group
                .get("time_pricing")
                .is_some_and(|value| !value.is_null());
        let group_multiplier = json_f64(group.get("user_rate_multiplier"))
            .or_else(|| json_f64(group.get("rate_multiplier")));
        // ChannelModelPricing / userSupportedModelPricing are USD per token
        // copies (toUserPricing does not apply group rate). Apply the resolved
        // multiplier exactly once on the billed row only.
        let billed_multiplier = billing_multiplier.or(group_multiplier);

        let models = group
            .get("models")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        for model in models {
            let id = match json_str(model.get("name")).or_else(|| json_str(model.get("id"))) {
                Some(name) if allowed_models.contains(name) => name.to_string(),
                _ => continue,
            };
            if model
                .get("time_pricing")
                .is_some_and(|value| !value.is_null())
            {
                snapshot.prices.push(unavailable_price(
                    &id,
                    group_id.clone(),
                    SRC_SUB2_BILLED,
                    false,
                    UNAVAIL_TIME,
                    valid_until,
                ));
            } else {
                snapshot.prices.push(sub2_billed_price(
                    &model,
                    &id,
                    group_id.clone(),
                    billed_multiplier,
                    time_varying,
                    valid_until,
                ));
            }
            snapshot.prices.push(sub2_official_price(
                &model,
                &id,
                group_id.clone(),
                valid_until,
            ));
        }
    }
}

fn unavailable_price(
    model: &str,
    group_id: Option<String>,
    source: &str,
    official: bool,
    reason: &str,
    valid_until: i64,
) -> PlatformPrice {
    PlatformPrice {
        model: model.to_string(),
        group_id,
        currency: "USD".to_string(),
        input: None,
        output: None,
        cache_read: None,
        cache_write: None,
        source: source.to_string(),
        official_reference: official,
        unavailable_reason: Some(reason.to_string()),
        valid_until,
    }
}

fn copy_official_rates(from: &Value, into: &mut PlatformPrice) {
    into.input = json_f64(from.get("input_price"));
    into.output = json_f64(from.get("output_price"));
    into.cache_read = json_f64(from.get("cache_read_price"));
    into.cache_write = json_f64(from.get("cache_write_price"));
}

fn sub2_official_price(
    model: &Value,
    id: &str,
    group_id: Option<String>,
    valid_until: i64,
) -> PlatformPrice {
    let mut price = PlatformPrice {
        model: id.to_string(),
        group_id,
        currency: "USD".to_string(),
        input: None,
        output: None,
        cache_read: None,
        cache_write: None,
        source: SRC_SUB2_OFFICIAL.to_string(),
        official_reference: true,
        unavailable_reason: None,
        valid_until,
    };
    let Some(official) = model
        .get("official_pricing")
        .filter(|value| !value.is_null())
    else {
        price.unavailable_reason = Some(UNAVAIL_MISSING_RATE.to_string());
        return price;
    };
    if official
        .get("intervals")
        .and_then(Value::as_array)
        .is_some_and(|rows| !rows.is_empty())
    {
        price.unavailable_reason = Some(UNAVAIL_TIERED.to_string());
        return price;
    }
    copy_official_rates(official, &mut price);
    if price.input.is_none()
        && price.output.is_none()
        && price.cache_read.is_none()
        && price.cache_write.is_none()
    {
        price.unavailable_reason = Some(UNAVAIL_MISSING_RATE.to_string());
    }
    price
}

fn sub2_billed_price(
    model: &Value,
    id: &str,
    group_id: Option<String>,
    multiplier: Option<f64>,
    time_varying: bool,
    valid_until: i64,
) -> PlatformPrice {
    let mut price = PlatformPrice {
        model: id.to_string(),
        group_id,
        currency: "USD".to_string(),
        input: None,
        output: None,
        cache_read: None,
        cache_write: None,
        source: SRC_SUB2_BILLED.to_string(),
        official_reference: false,
        unavailable_reason: None,
        valid_until,
    };
    if time_varying {
        price.unavailable_reason = Some(UNAVAIL_TIME.to_string());
        return price;
    }
    let Some(pricing) = model.get("pricing").filter(|value| !value.is_null()) else {
        price.unavailable_reason = Some(UNAVAIL_MISSING_RATE.to_string());
        return price;
    };
    let billing_mode = json_str(pricing.get("billing_mode")).unwrap_or("unknown");
    if json_f64(pricing.get("max_reasoning_effort_multiplier")).is_some_and(|m| m != 1.0) {
        price.unavailable_reason = Some("reasoning_multiplier".into());
        return price;
    }
    if billing_mode != "token" {
        price.unavailable_reason = Some(
            if billing_mode.contains("tier") {
                UNAVAIL_TIERED
            } else {
                UNAVAIL_PER_REQUEST
            }
            .to_string(),
        );
        return price;
    }
    if pricing
        .get("per_request_price")
        .is_some_and(|value| !value.is_null())
    {
        price.unavailable_reason = Some(UNAVAIL_PER_REQUEST.to_string());
        return price;
    }
    if pricing
        .get("intervals")
        .and_then(Value::as_array)
        .is_some_and(|rows| !rows.is_empty())
    {
        price.unavailable_reason = Some(UNAVAIL_TIERED.to_string());
        return price;
    }
    let Some(multiplier) = multiplier else {
        price.unavailable_reason = Some(UNAVAIL_MISSING_MULT.to_string());
        return price;
    };
    price.input = json_f64(pricing.get("input_price")).map(|rate| rate * multiplier);
    price.output = json_f64(pricing.get("output_price")).map(|rate| rate * multiplier);
    price.cache_read = json_f64(pricing.get("cache_read_price")).map(|rate| rate * multiplier);
    price.cache_write = json_f64(pricing.get("cache_write_price")).map(|rate| rate * multiplier);
    if price.input.is_none() || price.output.is_none() {
        price.unavailable_reason = Some(UNAVAIL_MISSING_RATE.to_string());
    }
    price
}

fn parse_rfc3339_secs(value: &str) -> Option<i64> {
    chrono::DateTime::parse_from_rfc3339(value)
        .ok()
        .map(|dt| dt.timestamp())
}
