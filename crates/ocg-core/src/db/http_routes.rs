//! Schema helper and encode/decode for `destinations.protocol_routes_json`.
//!
//! `ensure_storage_on` is additive and idempotent so v63 can call it. Null or
//! `[]` keep legacy route interpretation. Load refuses malformed, unknown, or
//! duplicate protocol lists instead of dropping members.

use super::{table_exists, table_has_column};
use anyhow::{Context, Result, anyhow};
use ocg_domain::destination::{
    AdapterKind, Destination, HttpProtocolRoute, validate_destination_protocol_routes,
    validate_http_protocol_route_list,
};
use rusqlite::Connection;

pub const PROTOCOL_ROUTES_COLUMN: &str = "protocol_routes_json";

/// Add nullable `destinations.protocol_routes_json` when the table exists.
pub(crate) fn ensure_storage_on(conn: &Connection) -> Result<()> {
    if !table_exists(conn, "destinations")? {
        return Ok(());
    }
    if table_has_column(conn, "destinations", PROTOCOL_ROUTES_COLUMN)? {
        return Ok(());
    }
    conn.execute_batch("ALTER TABLE destinations ADD COLUMN protocol_routes_json TEXT;")?;
    Ok(())
}

pub(crate) fn encode_protocol_routes_json(routes: &[HttpProtocolRoute]) -> Result<Option<String>> {
    if routes.is_empty() {
        return Ok(None);
    }
    Ok(Some(serde_json::to_string(routes)?))
}

pub(crate) fn decode_protocol_routes_json(raw: Option<&str>) -> Result<Vec<HttpProtocolRoute>> {
    let Some(raw) = raw.map(str::trim).filter(|value| !value.is_empty()) else {
        return Ok(Vec::new());
    };
    if raw.eq_ignore_ascii_case("null") || raw == "[]" {
        return Ok(Vec::new());
    }
    let routes: Vec<HttpProtocolRoute> =
        serde_json::from_str(raw).with_context(|| "invalid destinations.protocol_routes_json")?;
    validate_http_protocol_route_list(&routes).map_err(|error| anyhow!("{error}"))?;
    Ok(routes)
}

pub(crate) fn validate_loaded_destination(destination: &Destination) -> Result<()> {
    validate_destination_protocol_routes(destination).map_err(|error| anyhow!("{error}"))?;
    if destination.adapter == AdapterKind::Http {
        anyhow::ensure!(
            destination
                .protocol_routes
                .iter()
                .all(
                    |route| (route.auth_scheme == ocg_domain::destination::AuthScheme::None)
                        == (destination.auth_scheme == ocg_domain::destination::AuthScheme::None)
                ),
            "keyless and keyed routes cannot share one connection"
        );
        for route in &destination.protocol_routes {
            crate::custom::validate_custom_endpoint_url(&route.endpoint_url).map_err(|error| {
                anyhow!(
                    "destination `{}` has an invalid protocol route endpoint: {error}",
                    destination.id
                )
            })?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests;
