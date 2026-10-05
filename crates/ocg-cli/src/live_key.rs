//! Account-key writes sent to the serve process that owns the database.
//!
//! The caller has already decided that a live listener exists. This module
//! does not open SQLite and does not repeat a failed call against another
//! database. Enable and disable send an absolute `enabled` value.

use crate::api_cmd::{self, ApiRequest};
use anyhow::{Result, anyhow};
use serde_json::{Value, json};

pub async fn add(
    endpoint: &str,
    name: String,
    key: String,
    username: Option<String>,
    password: Option<String>,
) -> Result<()> {
    let mut body = serde_json::Map::new();
    body.insert("name".into(), json!(name));
    body.insert("key".into(), json!(key));
    if let Some(username) = username {
        body.insert("username".into(), json!(username));
    }
    if let Some(password) = password {
        body.insert("password".into(), json!(password));
    }
    let response = send(
        endpoint,
        "POST",
        "/dashboard/api/v4/accounts",
        Some(serde_json::to_vec(&Value::Object(body))?),
        true,
    )
    .await?;
    let (id, name) = mutation_identity(&response)?;
    println!("added key {id} ({name})");
    Ok(())
}

pub async fn remove(endpoint: &str, id: &str) -> Result<()> {
    let path = account_path(id)?;
    let current = send(endpoint, "GET", &path, None, false).await?;
    let name = string_field(&current, "name")?;
    send(
        endpoint,
        "DELETE",
        &path,
        Some(serde_json::to_vec(&json!({}))?),
        true,
    )
    .await?;
    println!("removed key {id} ({name})");
    Ok(())
}

pub async fn set_enabled(endpoint: &str, id: &str, enabled: bool) -> Result<()> {
    let path = account_path(id)?;
    let response = send(
        endpoint,
        "PATCH",
        &path,
        Some(serde_json::to_vec(&json!({ "enabled": enabled }))?),
        true,
    )
    .await?;
    let (_, name) = mutation_identity(&response)?;
    println!(
        "{} key {id} ({name})",
        if enabled { "enabled" } else { "disabled" }
    );
    Ok(())
}

pub async fn ping(
    endpoint: &str,
    id: &str,
    model: &str,
    message: Option<&str>,
    max_tokens: Option<u32>,
) -> Result<String> {
    if max_tokens == Some(0) {
        anyhow::bail!("maxTokens must be positive");
    }
    let path = model_test_path(id)?;
    let mut body = serde_json::Map::new();
    body.insert("modelId".into(), json!(model));
    if let Some(message) = message {
        body.insert("message".into(), json!(message));
    }
    if let Some(max_tokens) = max_tokens {
        body.insert("maxTokens".into(), json!(max_tokens));
    }
    let bytes = send(
        endpoint,
        "POST",
        &path,
        Some(serde_json::to_vec(&Value::Object(body))?),
        false,
    )
    .await
    .map_err(surfaced_control_error)?;
    format_model_test_line(&bytes)
}

fn model_test_path(id: &str) -> Result<String> {
    Ok(format!("{}/model-tests", account_path(id)?))
}

fn format_model_test_line(body: &[u8]) -> Result<String> {
    let value: Value =
        serde_json::from_slice(body).map_err(|_| anyhow!("model test response was not JSON"))?;
    let object = value
        .as_object()
        .ok_or_else(|| anyhow!("model test response was not JSON"))?;
    let account_id = required_str(object, "accountId")?;
    let model_id = required_str(object, "modelId")?;
    let protocol = required_str(object, "protocol")?;
    let success = object
        .get("success")
        .and_then(Value::as_bool)
        .ok_or_else(|| anyhow!("model test response has no success"))?;
    let status = object
        .get("httpStatus")
        .and_then(Value::as_u64)
        .map(|status| status.to_string())
        .unwrap_or_else(|| "none".to_string());
    let duration_ms = object
        .get("durationMs")
        .and_then(Value::as_u64)
        .ok_or_else(|| anyhow!("model test response has no durationMs"))?;
    let error = object
        .get("error")
        .and_then(Value::as_str)
        .map(scrub_display)
        .unwrap_or_default();
    let verdict = if success { "OK" } else { "FAIL" };
    Ok(format!(
        "[{verdict}] {account_id} model={model_id} protocol={protocol} status={status} durationMs={duration_ms} error={error}"
    ))
}

fn required_str<'a>(object: &'a serde_json::Map<String, Value>, field: &str) -> Result<&'a str> {
    object
        .get(field)
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("model test response has no {field}"))
}

fn surfaced_control_error(error: anyhow::Error) -> anyhow::Error {
    let status = error
        .chain()
        .find_map(|item| item.downcast_ref::<crate::api_cmd::ApiFailure>())
        .and_then(|failure| failure.status);
    let scrubbed = scrub_display(&error.to_string());
    match status {
        Some(status) => anyhow!("HTTP {status} {scrubbed}"),
        None => anyhow!(scrubbed),
    }
}

fn scrub_display(text: &str) -> String {
    let mut output = String::with_capacity(text.len());
    let mut token = String::new();
    for character in text.chars() {
        if character.is_whitespace() {
            push_scrubbed(&mut output, &token);
            token.clear();
            output.push(character);
        } else {
            token.push(character);
        }
    }
    push_scrubbed(&mut output, &token);
    output
}

fn push_scrubbed(output: &mut String, token: &str) {
    if token.is_empty() {
        return;
    }
    if token_is_sensitive(token) {
        output.push_str("[redacted]");
    } else {
        output.push_str(token);
    }
}

fn token_is_sensitive(token: &str) -> bool {
    let lower = token.to_ascii_lowercase();
    lower.contains("https://")
        || lower.contains("http://")
        || lower.contains("sk-")
        || lower.contains("bearer")
        || lower.contains("x-ocg-")
        || lower.contains("://")
}

fn account_path(id: &str) -> Result<String> {
    if id.is_empty() || id == "." || id == ".." || id.contains(['/', '?', '#', '%', '\\', ' ']) {
        anyhow::bail!("account id contains an unsupported character");
    }
    Ok(format!("/dashboard/api/v4/accounts/{id}"))
}

async fn send(
    endpoint: &str,
    method: &str,
    path: &str,
    body: Option<Vec<u8>>,
    cas_current: bool,
) -> Result<Vec<u8>> {
    let mut sink = Vec::new();
    api_cmd::execute(
        ApiRequest {
            endpoint: endpoint.to_string(),
            method: method.to_string(),
            path: path.to_string(),
            body,
            cas_current,
            bearer: None,
            session_file: None,
        },
        &mut sink,
    )
    .await
    .map_err(anyhow::Error::new)?;
    Ok(sink)
}

fn mutation_identity(body: &[u8]) -> Result<(String, String)> {
    let value: Value = serde_json::from_slice(body)?;
    let account = value
        .get("account")
        .and_then(Value::as_object)
        .ok_or_else(|| anyhow!("account response has no account"))?;
    let id = account
        .get("id")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("account response has no id"))?;
    let name = account
        .get("name")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("account response has no name"))?;
    Ok((id.to_string(), name.to_string()))
}

fn string_field(body: &[u8], field: &str) -> Result<String> {
    let value: Value = serde_json::from_slice(body)?;
    value
        .get(field)
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| anyhow!("account response has no {field}"))
}
