//! Origin and path checks for the HTTP CLI.
//!
//! The client talks only to an operator-supplied origin. It rejects
//! credentials, fragments, authority tricks, and encoded path traversal
//! before a socket is opened.

use anyhow::{Result, bail};
use reqwest::Url;

#[cfg(test)]
pub const DEFAULT_ORIGIN: &str = "http://127.0.0.1:9042";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Endpoint {
    pub origin: String,
    pub loopback: bool,
}

pub fn parse_endpoint(raw: &str) -> Result<Endpoint> {
    if raw.is_empty()
        || raw.chars().any(|character| {
            character.is_whitespace() || matches!(character, '\\' | '@' | '#' | '%')
        })
    {
        bail!("endpoint must be an origin without credentials, escapes, or a fragment");
    }
    let url = Url::parse(raw).map_err(|_| anyhow::anyhow!("invalid endpoint URL"))?;
    if url.fragment().is_some() || url.query().is_some() {
        bail!("endpoint must not include a query or fragment");
    }
    if !url.username().is_empty() || url.password().is_some() {
        bail!("endpoint must not include credentials");
    }
    if !matches!(url.scheme(), "http" | "https") {
        bail!("endpoint scheme must be http or https");
    }
    let host = url
        .host_str()
        .filter(|host| !host.is_empty() && host.is_ascii())
        .ok_or_else(|| anyhow::anyhow!("endpoint host is missing"))?;
    // `url` may return an IPv6 host with one pair of brackets already.
    let host = host
        .strip_prefix('[')
        .and_then(|host| host.strip_suffix(']'))
        .unwrap_or(host);
    if host.contains('@') || host.contains('\\') {
        bail!("endpoint host is not allowed");
    }
    let path = url.path();
    if path != "/" && !path.is_empty() {
        bail!("endpoint must be an origin; put the route in the path argument");
    }
    let port = url
        .port_or_known_default()
        .ok_or_else(|| anyhow::anyhow!("endpoint port is missing"))?;
    let loopback = host.eq_ignore_ascii_case("localhost")
        || host
            .parse::<std::net::IpAddr>()
            .is_ok_and(|ip| ip.is_loopback());
    let origin = if host.contains(':') {
        format!("{}://[{host}]:{port}", url.scheme())
    } else {
        format!("{}://{host}:{port}", url.scheme())
    };
    Ok(Endpoint { origin, loopback })
}

pub fn parse_route(raw: &str) -> Result<String> {
    if raw.is_empty() || !raw.starts_with('/') {
        bail!("path must start with /");
    }
    if raw
        .chars()
        .any(|character| character.is_control() || character == '#' || character == ' ')
    {
        bail!("path contains an unsupported character");
    }
    let (path, query) = raw
        .split_once('?')
        .map(|(path, query)| (path, Some(query)))
        .unwrap_or((raw, None));
    if path
        .chars()
        .any(|character| matches!(character, '\\' | '@'))
    {
        bail!("path contains an unsupported character");
    }
    reject_encoded_traversal(path)?;
    if let Some(query) = query
        && query.to_ascii_lowercase().contains("%00")
    {
        bail!("query contains a rejected percent encoding");
    }
    let mut decoded = path.to_string();
    for _ in 0..4 {
        let next = percent_decode(&decoded);
        if next == decoded {
            break;
        }
        reject_encoded_traversal(&next)?;
        decoded = next;
    }
    for segment in path.split('/').skip(1) {
        if segment.is_empty() || segment == "." || segment == ".." {
            bail!("encoded path traversal is not allowed");
        }
    }
    if !route_allowed(path) {
        if path == "/dashboard/api/v3" || path.starts_with("/dashboard/api/v3/") {
            bail!("V3 routes are retired; use /dashboard/api/v4");
        }
        bail!("path is not a live V4, auth, or inference route");
    }
    Ok(raw.to_string())
}

pub fn route_allowed(path: &str) -> bool {
    path == "/dashboard/api/v4"
        || path.starts_with("/dashboard/api/v4/")
        || matches!(
            path,
            "/dashboard/api/auth/status"
                | "/dashboard/api/auth/register"
                | "/dashboard/api/auth/login"
                | "/dashboard/api/auth/logout"
        )
        || matches!(
            path,
            "/v1/chat/completions" | "/v1/responses" | "/v1/messages" | "/v1/models"
        )
        || has_action_suffix(path, "/v1/models/")
        || has_action_suffix(path, "/v1beta/models/")
}

fn has_action_suffix(path: &str, prefix: &str) -> bool {
    path.strip_prefix(prefix)
        .is_some_and(|rest| !rest.is_empty() && !rest.ends_with('/'))
}

pub fn is_inference(path: &str) -> bool {
    let path = path.split('?').next().unwrap_or(path);
    matches!(
        path,
        "/v1/chat/completions" | "/v1/responses" | "/v1/messages" | "/v1/models"
    ) || has_action_suffix(path, "/v1/models/")
        || has_action_suffix(path, "/v1beta/models/")
}

fn reject_encoded_traversal(value: &str) -> Result<()> {
    let lower = value.to_ascii_lowercase();
    if lower.contains("%2e")
        || lower.contains("%2f")
        || lower.contains("%5c")
        || lower.contains("%00")
    {
        bail!("encoded path traversal is not allowed");
    }
    Ok(())
}

fn percent_decode(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut output = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%'
            && index + 2 < bytes.len()
            && let (Some(high), Some(low)) =
                (hex_nibble(bytes[index + 1]), hex_nibble(bytes[index + 2]))
        {
            output.push((high << 4) | low);
            index += 3;
            continue;
        }
        output.push(bytes[index]);
        index += 1;
    }
    String::from_utf8_lossy(&output).into_owned()
}

fn hex_nibble(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

#[cfg(test)]
#[path = "endpoint/tests.rs"]
mod tests;
