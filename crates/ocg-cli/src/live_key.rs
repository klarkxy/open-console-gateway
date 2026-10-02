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
