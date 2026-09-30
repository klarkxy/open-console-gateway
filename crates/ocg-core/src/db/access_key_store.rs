//! Access-key storage. The sub-key view excludes the primary row.

use super::*;

impl Database {
    // ----- access keys (schema v27; sub-key view excludes the primary row) -----

    /// All non-primary keys including soft-delete tombstones, in creation order.
    pub fn list_sub_gateway_keys(&self) -> Result<Vec<SubGatewayKey>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, name, key, enabled, deleted_at, created_at
             FROM access_keys
             WHERE is_primary = 0
             ORDER BY created_at ASC, rowid ASC",
        )?;
        let rows = stmt.query_map([], sub_gateway_key_from_row)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    /// Non-deleted sub keys (enabled and disabled alike); disabled rows keep
    /// their plaintext so re-enabling can revalidate it.
    pub fn list_active_sub_gateway_keys(&self) -> Result<Vec<SubGatewayKey>> {
        let mut keys = self.list_sub_gateway_keys()?;
        keys.retain(|key| key.is_active());
        Ok(keys)
    }

    /// Count of non-deleted sub keys; tombstones never count against the
    /// active ceiling. The live primary row is not counted.
    pub fn count_active_sub_gateway_keys(&self) -> Result<usize> {
        let count: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM access_keys
             WHERE is_primary = 0 AND deleted_at IS NULL",
            [],
            |row| row.get(0),
        )?;
        Ok(count.max(0) as usize)
    }

    pub fn get_sub_gateway_key(&self, id: &str) -> Result<Option<SubGatewayKey>> {
        if id == PRIMARY_KEY_ID {
            return Ok(None);
        }
        let key = self
            .conn
            .query_row(
                "SELECT id, name, key, enabled, deleted_at, created_at
                 FROM access_keys WHERE id = ?1 AND is_primary = 0",
                params![id],
                sub_gateway_key_from_row,
            )
            .optional()?;
        Ok(key)
    }

    /// Inserts a new sub key. The partial unique index backstops value
    /// uniqueness among all non-deleted access keys, including the primary;
    /// a collision surfaces as a constraint error the caller maps to a clear
    /// rejection.
    pub fn insert_sub_gateway_key(&self, key: &SubGatewayKey) -> Result<()> {
        anyhow::ensure!(
            key.id != PRIMARY_KEY_ID,
            "sub access keys cannot use the fixed primary id"
        );
        self.conn.execute(
            "INSERT INTO access_keys (id, name, key, is_primary, enabled, deleted_at, created_at)
             VALUES (?1, ?2, ?3, 0, ?4, ?5, ?6)",
            params![
                key.id,
                key.name,
                key.key,
                key.enabled as i32,
                key.deleted_at.map(|t| t.to_rfc3339()),
                key.created_at.to_rfc3339(),
            ],
        )?;
        Ok(())
    }

    /// Renames a non-deleted sub key. Returns `false` when the id matches no
    /// active row.
    pub fn rename_sub_gateway_key(&self, id: &str, name: &str) -> Result<bool> {
        if id == PRIMARY_KEY_ID {
            return Ok(false);
        }
        let updated = self.conn.execute(
            "UPDATE access_keys SET name = ?2
             WHERE id = ?1 AND is_primary = 0 AND deleted_at IS NULL",
            params![id, name],
        )?;
        Ok(updated == 1)
    }

    /// Flips the enabled flag of a non-deleted sub key. Returns `false` when
    /// the id matches no active row. The primary row cannot be disabled.
    pub fn set_sub_gateway_key_enabled(&self, id: &str, enabled: bool) -> Result<bool> {
        if id == PRIMARY_KEY_ID {
            return Ok(false);
        }
        let updated = self.conn.execute(
            "UPDATE access_keys SET enabled = ?2
             WHERE id = ?1 AND is_primary = 0 AND deleted_at IS NULL",
            params![id, enabled as i32],
        )?;
        Ok(updated == 1)
    }

    /// Assigns a fresh value to a non-deleted sub key. Returns `false` when
    /// the id matches no active row.
    pub fn update_sub_gateway_key_value(&self, id: &str, new_value: &str) -> Result<bool> {
        if id == PRIMARY_KEY_ID {
            return Ok(false);
        }
        let updated = self.conn.execute(
            "UPDATE access_keys SET key = ?2
             WHERE id = ?1 AND is_primary = 0 AND deleted_at IS NULL",
            params![id, new_value],
        )?;
        Ok(updated == 1)
    }

    /// Soft-deletes a sub key: clears the plaintext, disables it, and keeps
    /// id/name/deleted_at for log attribution. Returns `false` when the id
    /// matches no active row. The primary row cannot be deleted.
    pub fn soft_delete_sub_gateway_key(&self, id: &str, now: DateTime<Utc>) -> Result<bool> {
        if id == PRIMARY_KEY_ID {
            return Ok(false);
        }
        let updated = self.conn.execute(
            "UPDATE access_keys
             SET key = '', enabled = 0, deleted_at = ?2
             WHERE id = ?1 AND is_primary = 0 AND deleted_at IS NULL",
            params![id, now.to_rfc3339()],
        )?;
        Ok(updated == 1)
    }

    /// Plaintext values of all non-deleted sub keys (enabled and disabled);
    /// used to keep generated values unique across tiers.
    pub fn active_sub_gateway_key_values(&self) -> Result<Vec<String>> {
        let mut stmt = self.conn.prepare(
            "SELECT key FROM access_keys
             WHERE is_primary = 0 AND deleted_at IS NULL AND key <> ''",
        )?;
        let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    /// Whether any non-deleted sub key (enabled or disabled) already holds
    /// this value; the cross-tier uniqueness gate for candidate primary key
    /// values.
    pub fn sub_gateway_key_value_exists(&self, value: &str) -> Result<bool> {
        let found: i64 = self.conn.query_row(
            "SELECT EXISTS(
                SELECT 1 FROM access_keys
                WHERE is_primary = 0 AND deleted_at IS NULL AND key = ?1 LIMIT 1
            )",
            params![value],
            |row| row.get(0),
        )?;
        Ok(found == 1)
    }
}
