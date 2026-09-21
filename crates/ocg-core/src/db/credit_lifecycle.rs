//! Preserve a personal credit meter across canonical configuration rewrites.

use anyhow::Result;
use rusqlite::{Connection, params};

pub(crate) struct CreditMeterSnapshot {
    pub credential_id: String,
    pub destination_id: String,
    pub endpoint: Option<String>,
    pub json: String,
}

pub(crate) fn snapshot_on(conn: &Connection) -> Result<Vec<CreditMeterSnapshot>> {
    if !super::table_has_column(conn, "credentials", "credit_meter_json")? {
        return Ok(Vec::new());
    }
    let mut statement = conn.prepare(
        "SELECT c.id, c.destination_id, d.base_url, c.credit_meter_json
         FROM credentials c JOIN destinations d ON d.id = c.destination_id
         WHERE c.credit_meter_json IS NOT NULL",
    )?;
    let rows = statement.query_map([], |row| {
        Ok(CreditMeterSnapshot {
            credential_id: row.get(0)?,
            destination_id: row.get(1)?,
            endpoint: row.get(2)?,
            json: row.get(3)?,
        })
    })?;
    rows.collect::<rusqlite::Result<Vec<_>>>()
        .map_err(Into::into)
}

pub(crate) fn restore_on(conn: &Connection, snapshots: &[CreditMeterSnapshot]) -> Result<()> {
    if snapshots.is_empty() || !super::table_has_column(conn, "credentials", "credit_meter_json")? {
        return Ok(());
    }
    for snapshot in snapshots {
        conn.execute(
            "UPDATE credentials SET credit_meter_json = ?2
             WHERE id = ?1 AND destination_id = ?3
               AND EXISTS (SELECT 1 FROM destinations d
                           WHERE d.id = ?3 AND d.base_url IS ?4)",
            params![
                snapshot.credential_id,
                snapshot.json,
                snapshot.destination_id,
                snapshot.endpoint
            ],
        )?;
    }
    Ok(())
}
