//! Generic settings, unpublished public models, and the durable free-channel cooldown.

use super::*;

impl Database {
    // Settings
    pub fn set_setting(&self, key: &str, value: &str) -> Result<()> {
        self.conn.execute(
            "INSERT OR REPLACE INTO settings (key, value) VALUES (?1, ?2)",
            [key, value],
        )?;
        Ok(())
    }

    pub fn get_setting(&self, key: &str) -> Result<Option<String>> {
        self.conn
            .query_row("SELECT value FROM settings WHERE key = ?1", [key], |row| {
                row.get(0)
            })
            .optional()
            .map_err(|e| e.into())
    }

    /// Public names hidden from authenticated `GET /v1/models`, sorted.
    pub fn list_unpublished_public_models(&self) -> Result<Vec<String>> {
        let mut stmt = self.conn.prepare(
            "SELECT public_model FROM unpublished_public_models ORDER BY public_model COLLATE NOCASE",
        )?;
        let rows = stmt.query_map([], |row| row.get(0))?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(|error| error.into())
    }

    pub fn upsert_unpublished_public_model(&self, public_model: &str) -> Result<()> {
        self.conn.execute(
            "INSERT OR REPLACE INTO unpublished_public_models (public_model, updated_at)
             VALUES (?1, ?2)",
            params![public_model, Utc::now().to_rfc3339()],
        )?;
        Ok(())
    }

    pub fn remove_unpublished_public_model(&self, public_model: &str) -> Result<()> {
        self.conn.execute(
            "DELETE FROM unpublished_public_models WHERE public_model = ?1",
            [public_model],
        )?;
        Ok(())
    }

    /// Return the active egress-IP-wide Zen free cooldown, if any.
    pub fn free_channel_cooldown_until(&self) -> Result<Option<DateTime<Utc>>> {
        self.free_channel_cooldown_until_at(Utc::now())
    }

    /// Evaluate the durable Free cooldown against an explicit wall time.
    pub(crate) fn free_channel_cooldown_until_at(
        &self,
        now: DateTime<Utc>,
    ) -> Result<Option<DateTime<Utc>>> {
        let Some(value) = self.get_setting(FREE_CHANNEL_COOLDOWN_SETTING)? else {
            return Ok(None);
        };
        let until = DateTime::parse_from_rfc3339(&value)
            .map(|value| value.with_timezone(&Utc))
            .map_err(|error| {
                anyhow::anyhow!(
                    "invalid {FREE_CHANNEL_COOLDOWN_SETTING} setting {value:?}: {error}"
                )
            })?;
        Ok((until > now).then_some(until))
    }
}
