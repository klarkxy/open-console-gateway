//! Copy one bounded page of New API inference tokens into local Custom Keys.
//! A continuation is explicit; disabled/existing rows still advance the page.
use serde_json::Value;
use super::reader::{get_json_query, new_api_data, post_json, split_new_api_user_credential};

const PAGE_SIZE: usize = 50;
pub(crate) const MAX_PAGE: u32 = 100_000;
const TOKEN_STATUS_ENABLED: i64 = 1;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RemoteToken {
    pub id: String,
    pub name: String,
    pub enabled: bool,
}

// Secrets deliberately have no Debug/Serialize implementation.
pub(crate) struct RemoteTokenSecret {
    pub name: String,
    pub key: String,
}

pub(crate) struct RemoteKeyBatch {
    pub secrets: Vec<RemoteTokenSecret>,
    pub skipped_disabled: usize,
    pub failed: Vec<(String, String)>,
    pub next_page: Option<u32>,
}

pub(crate) fn parse_token_list(data: &Value) -> Vec<RemoteToken> {
    token_rows(data).into_iter().take(PAGE_SIZE).filter_map(|row| {
        let id = token_id(row)?;
        let name = row.get("name").and_then(Value::as_str).map(str::trim)
            .filter(|value| !value.is_empty()).map(str::to_string)
            .unwrap_or_else(|| format!("Key {id}"));
        let enabled = match row.get("status") {
            None => true,
            Some(value) => json_i64(value) == Some(TOKEN_STATUS_ENABLED),
        };
        Some(RemoteToken { id, name, enabled })
    }).collect()
}

pub(crate) fn parse_full_key(data: &Value) -> Option<String> {
    let key = data.as_str().or_else(|| data.get("key").and_then(Value::as_str))?;
    let key = key.trim();
    (!key.is_empty()).then(|| key.to_string())
}

fn token_rows(data: &Value) -> Vec<&Value> {
    if let Some(items) = data.as_array() { return items.iter().collect(); }
    for field in ["items", "data", "records"] {
        if let Some(items) = data.get(field).and_then(Value::as_array) {
            return items.iter().collect();
        }
    }
    Vec::new()
}

fn token_id(row: &Value) -> Option<String> {
    match row.get("id")? {
        Value::Number(number) => number.as_u64().filter(|id| *id > 0).map(|id| id.to_string()),
        Value::String(text) => {
            let text = text.trim();
            (!text.is_empty() && text.bytes().all(|byte| byte.is_ascii_digit()))
                .then(|| text.to_string())
        }
        _ => None,
    }
}

fn json_i64(value: &Value) -> Option<i64> {
    match value {
        Value::Number(number) => number.as_i64(),
        Value::String(text) => text.trim().parse().ok(),
        _ => None,
    }
}

fn next_page(page: u32, raw_len: usize, total: Option<i64>) -> Result<Option<u32>, String> {
    if !(1..=MAX_PAGE).contains(&page) || raw_len > PAGE_SIZE || total.is_some_and(|n| n < 0) {
        return Err("new_api.token_list.parse".into());
    }
    let offset = i64::from(page - 1) * PAGE_SIZE as i64;
    if raw_len == 0 && total.is_some_and(|n| n > offset) {
        return Err("new_api.token_list.incomplete_page".into());
    }
    let more = raw_len != 0 && total.map_or(raw_len == PAGE_SIZE, |n| offset + (raw_len as i64) < n);
    if more && page == MAX_PAGE { return Err("new_api.token_list.page_limit".into()); }
    Ok(more.then_some(page + 1))
}

pub(crate) async fn list_remote_tokens(
    client: &reqwest::Client,
    base: &reqwest::Url,
    user_credential: &str,
    page: u32,
) -> Result<(Vec<RemoteToken>, Option<u32>), String> {
    if !(1..=MAX_PAGE).contains(&page) { return Err("new_api.token_list.page_limit".into()); }
    let (new_api_user, bearer) = split_new_api_user_credential(user_credential);
    let page_text = page.to_string();
    let page_size = PAGE_SIZE.to_string();
    let fetched = get_json_query(client, base, "api/token/", "new_api.token_list", Some(bearer), new_api_user,
        &[("p", &page_text), ("page_size", &page_size)]).await?;
    let data = new_api_data(&fetched.value, "new_api.token_list")?;
    // A malformed envelope is not proof of an empty remote inventory.
    if !data.is_array() && !["items", "data", "records"].iter().any(|field| data.get(field).is_some_and(Value::is_array)) {
        return Err("new_api.token_list.parse".into());
    }
    let total = data.get("total").and_then(json_i64);
    let continuation = next_page(page, token_rows(data).len(), total)?;
    Ok((parse_token_list(data), continuation))
}

pub(crate) async fn fetch_full_key(
    client: &reqwest::Client,
    base: &reqwest::Url,
    user_credential: &str,
    token_id: &str,
) -> Result<String, String> {
    let (new_api_user, bearer) = split_new_api_user_credential(user_credential);
    let path = format!("api/token/{token_id}/key");
    let fetched = post_json(client, base, &path, "new_api.token_key", Some(bearer), new_api_user).await?;
    let data = new_api_data(&fetched.value, "new_api.token_key")?;
    parse_full_key(data).ok_or_else(|| "new_api.token_key.parse".to_string())
}

pub(crate) async fn collect_remote_secrets(
    client: &reqwest::Client,
    base: &reqwest::Url,
    user_credential: &str,
    page: u32,
) -> Result<RemoteKeyBatch, String> {
    let (listed, next_page) = list_remote_tokens(client, base, user_credential, page).await?;
    let mut batch = RemoteKeyBatch { secrets: Vec::new(), skipped_disabled: 0, failed: Vec::new(), next_page };
    for token in listed {
        if !token.enabled { batch.skipped_disabled += 1; continue; }
        match fetch_full_key(client, base, user_credential, &token.id).await {
            Ok(key) => batch.secrets.push(RemoteTokenSecret {
                name: crate::redaction::redact_known_secret(&token.name, &key), key,
            }),
            Err(_) => batch.failed.push((token.name, "full_key_unavailable".into())),
        }
    }
    Ok(batch)
}

#[cfg(test)]
mod tests;
