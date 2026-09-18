//! Pull New API inference tokens into local Custom Keys.
//!
//! Lists `GET /api/token/` then reads each full secret from
//! `GET /api/token/{id}/key`. List rows are masked; the plaintext never
//! leaves this module except as an encrypted local Key.

use serde_json::Value;

use super::reader::{get_json_query, new_api_data, split_new_api_user_credential};

const MAX_IMPORT_KEYS: usize = 50;
const PAGE_SIZE: &str = "50";
const TOKEN_STATUS_ENABLED: i64 = 1;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RemoteToken {
    pub id: String,
    pub name: String,
    pub enabled: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RemoteTokenSecret {
    pub name: String,
    pub key: String,
}

pub(crate) fn parse_token_list(data: &Value) -> Vec<RemoteToken> {
    let rows = token_rows(data);
    let mut tokens = Vec::new();
    for row in rows {
        let Some(id) = token_id(row) else {
            continue;
        };
        let name = row
            .get("name")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string)
            .unwrap_or_else(|| format!("Key {id}"));
        let enabled = match row.get("status") {
            None => true,
            Some(value) => json_i64(value) == Some(TOKEN_STATUS_ENABLED),
        };
        tokens.push(RemoteToken { id, name, enabled });
        if tokens.len() >= MAX_IMPORT_KEYS {
            break;
        }
    }
    tokens
}

pub(crate) fn parse_full_key(data: &Value) -> Option<String> {
    if let Some(key) = data.as_str() {
        let trimmed = key.trim();
        if !trimmed.is_empty() {
            return Some(trimmed.to_string());
        }
    }
    let key = data
        .get("key")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())?;
    Some(key.to_string())
}

fn token_rows(data: &Value) -> Vec<&Value> {
    if let Some(items) = data.as_array() {
        return items.iter().collect();
    }
    for field in ["items", "data", "records"] {
        if let Some(items) = data.get(field).and_then(Value::as_array) {
            return items.iter().collect();
        }
    }
    Vec::new()
}

fn token_id(row: &Value) -> Option<String> {
    match row.get("id") {
        Some(Value::Number(number)) => number
            .as_i64()
            .or_else(|| number.as_u64().map(|value| value as i64))
            .map(|value| value.to_string()),
        Some(Value::String(text)) => {
            let trimmed = text.trim();
            if trimmed.is_empty() {
                None
            } else {
                Some(trimmed.to_string())
            }
        }
        _ => None,
    }
}

fn json_i64(value: &Value) -> Option<i64> {
    match value {
        Value::Number(number) => number
            .as_i64()
            .or_else(|| number.as_u64().and_then(|value| i64::try_from(value).ok())),
        Value::String(text) => text.trim().parse().ok(),
        _ => None,
    }
}

fn list_complete(page_len: usize, accumulated: usize, total: Option<i64>) -> bool {
    if accumulated >= MAX_IMPORT_KEYS {
        return true;
    }
    if page_len == 0 {
        return true;
    }
    if let Some(total) = total {
        return accumulated as i64 >= total;
    }
    page_len < PAGE_SIZE.parse().unwrap_or(50)
}

pub(crate) async fn list_remote_tokens(
    client: &reqwest::Client,
    base: &reqwest::Url,
    user_credential: &str,
) -> Result<Vec<RemoteToken>, String> {
    let (new_api_user, bearer) = split_new_api_user_credential(user_credential);
    let mut tokens = Vec::new();
    let mut page: u32 = 1;
    loop {
        let page_text = page.to_string();
        let fetched = get_json_query(
            client,
            base,
            "api/token/",
            "new_api.token_list",
            Some(bearer),
            new_api_user,
            &[("p", &page_text), ("page_size", PAGE_SIZE)],
        )
        .await?;
        let data = new_api_data(&fetched.value, "new_api.token_list")?;
        let total = data.get("total").and_then(json_i64);
        let mut page_tokens = parse_token_list(data);
        let page_len = page_tokens.len();
        tokens.append(&mut page_tokens);
        tokens.truncate(MAX_IMPORT_KEYS);
        if list_complete(page_len, tokens.len(), total) {
            break;
        }
        page += 1;
        if page > 20 {
            break;
        }
    }
    Ok(tokens)
}

pub(crate) async fn fetch_full_key(
    client: &reqwest::Client,
    base: &reqwest::Url,
    user_credential: &str,
    token_id: &str,
) -> Result<String, String> {
    let (new_api_user, bearer) = split_new_api_user_credential(user_credential);
    let path = format!("api/token/{token_id}/key");
    let fetched = get_json_query(
        client,
        base,
        &path,
        "new_api.token_key",
        Some(bearer),
        new_api_user,
        &[],
    )
    .await?;
    let data = new_api_data(&fetched.value, "new_api.token_key")?;
    parse_full_key(data).ok_or_else(|| "new_api.token_key.parse".to_string())
}

pub(crate) async fn collect_remote_secrets(
    client: &reqwest::Client,
    base: &reqwest::Url,
    user_credential: &str,
) -> Result<(Vec<RemoteTokenSecret>, usize, Vec<(String, String)>), String> {
    let listed = list_remote_tokens(client, base, user_credential).await?;
    let mut secrets = Vec::new();
    let mut skipped_disabled = 0;
    let mut failed = Vec::new();
    for token in listed {
        if !token.enabled {
            skipped_disabled += 1;
            continue;
        }
        match fetch_full_key(client, base, user_credential, &token.id).await {
            Ok(key) => secrets.push(RemoteTokenSecret {
                name: token.name,
                key,
            }),
            Err(_) => failed.push((token.name, "full_key_unavailable".to_string())),
        }
    }
    Ok((secrets, skipped_disabled, failed))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn token_list_reads_items_and_skips_disabled() {
        let data = json!({
            "items": [
                {"id": 12, "name": "Codex", "status": 1},
                {"id": "13", "name": "Grok", "status": 2},
                {"id": 14, "status": 1}
            ],
            "total": 3
        });
        let tokens = parse_token_list(&data);
        assert_eq!(tokens.len(), 3);
        assert_eq!(tokens[0].name, "Codex");
        assert!(tokens[0].enabled);
        assert!(!tokens[1].enabled);
        assert_eq!(tokens[2].name, "Key 14");
    }

    #[test]
    fn token_list_accepts_a_bare_array() {
        let data = json!([{"id": 1, "name": "a"}]);
        assert_eq!(parse_token_list(&data)[0].id, "1");
    }

    #[test]
    fn full_key_reads_object_or_string() {
        assert_eq!(
            parse_full_key(&json!({"key": " sk-live "})).as_deref(),
            Some("sk-live")
        );
        assert_eq!(
            parse_full_key(&json!("sk-plain")).as_deref(),
            Some("sk-plain")
        );
        assert_eq!(parse_full_key(&json!({"key": ""})), None);
    }
}
