//! Quota windows, credit balances, and usage calibration.

use super::*;

impl Database {
    pub fn upsert_quota_window(&self, window: &QuotaWindow) -> Result<()> {
        anyhow::ensure!(
            self.get_account(&window.account_id)?.is_some(),
            "account not found"
        );
        self.conn.execute(
            "INSERT INTO quota_windows (
                account_id, window_kind, used, limit_value, started_at, resets_at,
                calibration_offset, unit, source, observed_at, updated_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)
             ON CONFLICT(account_id, window_kind) DO UPDATE SET
                used = excluded.used,
                limit_value = excluded.limit_value,
                started_at = excluded.started_at,
                resets_at = excluded.resets_at,
                calibration_offset = excluded.calibration_offset,
                unit = excluded.unit,
                source = excluded.source,
                observed_at = excluded.observed_at,
                updated_at = excluded.updated_at",
            params![
                window.account_id,
                window.window_kind,
                window.used,
                window.limit_value,
                window.started_at.map(|value| value.to_rfc3339()),
                window.resets_at.map(|value| value.to_rfc3339()),
                window.calibration_offset,
                window.unit,
                window.source,
                window.observed_at.map(|value| value.to_rfc3339()),
                window.updated_at.to_rfc3339(),
            ],
        )?;
        Ok(())
    }

    /// Atomically replace the authoritative snapshot for one sealed Provider.
    pub fn replace_quota_windows_by_source(
        &self,
        account_id: &str,
        source: &str,
        windows: &[QuotaWindow],
    ) -> Result<()> {
        anyhow::ensure!(self.get_account(account_id)?.is_some(), "account not found");
        anyhow::ensure!(
            windows
                .iter()
                .all(|window| window.account_id == account_id && window.source == source),
            "provider quota snapshot contains a mismatched account or source"
        );
        let tx = self.conn.unchecked_transaction()?;
        tx.execute(
            "DELETE FROM quota_windows WHERE account_id = ?1 AND source = ?2",
            params![account_id, source],
        )?;
        for window in windows {
            tx.execute(
                "INSERT INTO quota_windows (
                    account_id, window_kind, used, limit_value, started_at, resets_at,
                    calibration_offset, unit, source, observed_at, updated_at
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)
                 ON CONFLICT(account_id, window_kind) DO UPDATE SET
                    used = excluded.used,
                    limit_value = excluded.limit_value,
                    started_at = excluded.started_at,
                    resets_at = excluded.resets_at,
                    calibration_offset = excluded.calibration_offset,
                    unit = excluded.unit,
                    source = excluded.source,
                    observed_at = excluded.observed_at,
                    updated_at = excluded.updated_at",
                params![
                    window.account_id,
                    window.window_kind,
                    window.used,
                    window.limit_value,
                    window.started_at.map(|value| value.to_rfc3339()),
                    window.resets_at.map(|value| value.to_rfc3339()),
                    window.calibration_offset,
                    window.unit,
                    window.source,
                    window.observed_at.map(|value| value.to_rfc3339()),
                    window.updated_at.to_rfc3339(),
                ],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    pub fn list_quota_windows(&self, account_id: &str) -> Result<Vec<QuotaWindow>> {
        let mut stmt = self.conn.prepare(
            "SELECT account_id, window_kind, used, limit_value, started_at,
                    resets_at, calibration_offset, unit, source, observed_at, updated_at
             FROM quota_windows WHERE account_id = ?1 ORDER BY window_kind ASC",
        )?;
        let rows = stmt.query_map([account_id], |row| {
            Ok(QuotaWindow {
                account_id: row.get(0)?,
                window_kind: row.get(1)?,
                used: row.get(2)?,
                limit_value: row.get(3)?,
                started_at: row.get::<_, Option<String>>(4)?.map(parse_datetime),
                resets_at: row.get::<_, Option<String>>(5)?.map(parse_datetime),
                calibration_offset: row.get(6)?,
                unit: row.get(7)?,
                source: row.get(8)?,
                observed_at: row.get::<_, Option<String>>(9)?.map(parse_datetime),
                updated_at: parse_datetime(row.get::<_, String>(10)?),
            })
        })?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(Into::into)
    }

    pub fn upsert_credit_balance(&self, balance: &CreditBalance) -> Result<()> {
        anyhow::ensure!(
            self.get_account(&balance.account_id)?.is_some(),
            "account not found"
        );
        self.conn.execute(
            "INSERT INTO credit_balances (
                account_id, balance_kind, amount, unit, source, observed_at, updated_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
             ON CONFLICT(account_id, balance_kind) DO UPDATE SET
                amount = excluded.amount,
                unit = excluded.unit,
                source = excluded.source,
                observed_at = excluded.observed_at,
                updated_at = excluded.updated_at",
            params![
                balance.account_id,
                balance.balance_kind,
                balance.amount,
                balance.unit,
                balance.source,
                balance.observed_at.map(|value| value.to_rfc3339()),
                balance.updated_at.to_rfc3339(),
            ],
        )?;
        Ok(())
    }

    /// Atomically replace official balance rows for one source on one account.
    pub fn replace_credit_balances_by_source(
        &self,
        account_id: &str,
        source: &str,
        balances: &[CreditBalance],
    ) -> Result<()> {
        anyhow::ensure!(self.get_account(account_id)?.is_some(), "account not found");
        anyhow::ensure!(
            balances
                .iter()
                .all(|row| row.account_id == account_id && row.source == source),
            "credit snapshot contains a mismatched account or source"
        );
        let tx = self.conn.unchecked_transaction()?;
        tx.execute(
            "DELETE FROM credit_balances WHERE account_id = ?1 AND source = ?2",
            params![account_id, source],
        )?;
        for balance in balances {
            tx.execute(
                "INSERT INTO credit_balances (
                    account_id, balance_kind, amount, unit, source, observed_at, updated_at
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
                 ON CONFLICT(account_id, balance_kind) DO UPDATE SET
                    amount = excluded.amount,
                    unit = excluded.unit,
                    source = excluded.source,
                    observed_at = excluded.observed_at,
                    updated_at = excluded.updated_at",
                params![
                    balance.account_id,
                    balance.balance_kind,
                    balance.amount,
                    balance.unit,
                    balance.source,
                    balance.observed_at.map(|value| value.to_rfc3339()),
                    balance.updated_at.to_rfc3339(),
                ],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    pub fn list_credit_balances(&self, account_id: &str) -> Result<Vec<CreditBalance>> {
        let mut stmt = self.conn.prepare(
            "SELECT account_id, balance_kind, amount, unit, source, observed_at, updated_at
             FROM credit_balances WHERE account_id = ?1 ORDER BY balance_kind ASC",
        )?;
        let rows = stmt.query_map([account_id], |row| {
            Ok(CreditBalance {
                account_id: row.get(0)?,
                balance_kind: row.get(1)?,
                amount: row.get(2)?,
                unit: row.get(3)?,
                source: row.get(4)?,
                observed_at: row.get::<_, Option<String>>(5)?.map(parse_datetime),
                updated_at: parse_datetime(row.get::<_, String>(6)?),
            })
        })?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(Into::into)
    }

    pub fn account_usage_sync_state(
        &self,
        account_id: &str,
    ) -> Result<Option<AccountUsageSyncState>> {
        let row = self
            .conn
            .query_row(
                "SELECT last_success_at, last_attempt_at, next_eligible_at,
                        failure_streak, last_expedited_at
                 FROM provider_usage_sync_state WHERE account_id = ?1",
                [account_id],
                |row| {
                    Ok(AccountUsageSyncState {
                        account_id: account_id.to_string(),
                        last_success_at: row.get::<_, Option<String>>(0)?.map(parse_datetime),
                        last_attempt_at: row.get::<_, Option<String>>(1)?.map(parse_datetime),
                        next_eligible_at: row.get::<_, Option<String>>(2)?.map(parse_datetime),
                        failure_streak: row.get::<_, i64>(3)?,
                        last_expedited_at: row.get::<_, Option<String>>(4)?.map(parse_datetime),
                    })
                },
            )
            .optional()?;
        Ok(row)
    }

    /// Pull `next_eligible_at` earlier when `proposal` is sooner.
    ///
    /// When `respect_failure_backoff` is true and the account is in a failure
    /// streak, the existing next-eligible floor is left untouched so threshold,
    /// cadence, and reset logic cannot defeat the backoff ladder. Callers that
    /// intentionally override (real inference 429) pass false.
    pub fn pull_account_usage_sync_next_eligible(
        &self,
        account_id: &str,
        proposal: DateTime<Utc>,
        respect_failure_backoff: bool,
    ) -> Result<()> {
        let current = self.account_usage_sync_state(account_id)?;
        let Some(current) = current else {
            // Account gone — nothing to schedule.
            return Ok(());
        };
        if respect_failure_backoff && current.failure_streak > 0 {
            return Ok(());
        }
        let next = match current.next_eligible_at {
            Some(existing) => existing.min(proposal),
            None => proposal,
        };
        self.conn.execute(
            "UPDATE provider_usage_sync_state
             SET next_eligible_at = ?1
             WHERE account_id = ?2",
            params![next.to_rfc3339(), account_id],
        )?;
        Ok(())
    }

    pub fn record_account_usage_sync_success(
        &self,
        account_id: &str,
        now: DateTime<Utc>,
        next_eligible_at: DateTime<Utc>,
        mark_expedited: bool,
    ) -> Result<()> {
        record_account_usage_sync_success_on(
            &self.conn,
            account_id,
            AccountUsageSyncSuccessMetadata {
                now,
                next_eligible_at,
                mark_expedited,
            },
        )?;
        Ok(())
    }

    pub fn record_account_usage_sync_failure(
        &self,
        account_id: &str,
        now: DateTime<Utc>,
        failure_streak: i64,
        next_eligible_at: DateTime<Utc>,
    ) -> Result<()> {
        // Never clear last_success_at on failure.
        self.conn.execute(
            "INSERT INTO provider_usage_sync_state(account_id, last_attempt_at, next_eligible_at, failure_streak)
             VALUES (?4, ?1, ?2, ?3) ON CONFLICT(account_id) DO UPDATE
             SET last_attempt_at = excluded.last_attempt_at,
                 next_eligible_at = excluded.next_eligible_at,
                 failure_streak = excluded.failure_streak",
            params![
                now.to_rfc3339(),
                next_eligible_at.to_rfc3339(),
                failure_streak,
                account_id
            ],
        )?;
        Ok(())
    }

    /// Touch only the manual-throttle timestamp without changing success,
    /// streak, or next-eligible fields (e.g. post-network CAS conflicts).
    pub fn touch_account_usage_sync_attempt(
        &self,
        account_id: &str,
        now: DateTime<Utc>,
    ) -> Result<()> {
        self.conn.execute(
            "INSERT INTO provider_usage_sync_state(account_id, last_attempt_at)
             VALUES (?2, ?1) ON CONFLICT(account_id) DO UPDATE
             SET last_attempt_at = excluded.last_attempt_at",
            params![now.to_rfc3339(), account_id],
        )?;
        Ok(())
    }

    /// True when the account has at least one successful, possibly
    /// Go-quota-consuming forward log at or after `since` (active cadence).
    /// Uses `julianday` so lexicographic RFC3339 edge cases cannot mis-order,
    /// and `EXISTS` so the scan can stop early. Zen free successes are excluded.
    pub fn account_has_local_activity_since(
        &self,
        account_id: &str,
        since: DateTime<Utc>,
    ) -> Result<bool> {
        let exists: i64 = self.conn.query_row(
            "SELECT EXISTS(
                SELECT 1 FROM forward_logs
                WHERE account_id = ?1
                  AND status IN ('success', 'success_no_usage', 'success_unpriced')
                  AND cost_state IN ('priced', 'legacy_estimate', 'unpriced', 'usage_missing')
                  AND julianday(timestamp) >= julianday(?2)
                LIMIT 1
             )",
            params![account_id, since.to_rfc3339()],
            |row| row.get(0),
        )?;
        Ok(exists != 0)
    }

    /// v51 secret store. Empty or missing credential rows fall back to the
    /// `accounts` row until that table is dropped.
    pub(crate) fn credential_key_cipher_for_legacy_account(
        &self,
        legacy_account_id: &str,
    ) -> Result<Option<String>> {
        if !table_has_column(&self.conn, "credentials", "key_cipher")? {
            return Ok(None);
        }
        let cipher = self
            .conn
            .query_row(
                "SELECT key_cipher FROM credentials WHERE legacy_account_id = ?1",
                [legacy_account_id],
                |row| row.get::<_, String>(0),
            )
            .optional()?;
        Ok(cipher.filter(|value| !value.is_empty()))
    }

    pub fn get_account(&self, id: &str) -> Result<Option<Account>> {
        account_store::get_account_on(&self.conn, id)
    }

    pub fn list_accounts(&self) -> Result<Vec<Account>> {
        account_store::list_accounts_on(&self.conn)
    }

    /// Advance a managed-account onboarding step with an optimistic current-step
    /// guard. The caller is responsible for validating that `to` is the only
    /// legal successor of `from`.
    pub fn advance_managed_setup(
        &self,
        id: &str,
        from: AccountSetupStep,
        to: AccountSetupStep,
    ) -> Result<bool> {
        let confirmed_purchase_date = (from == AccountSetupStep::Payment
            && to == AccountSetupStep::KeyVerification)
            .then(local_today);
        let tx = self.conn.unchecked_transaction()?;
        let changed = tx.execute(
            "UPDATE credentials
             SET setup_step = ?1, enabled = 0,
                 purchase_date = CASE WHEN ?5 IS NULL THEN purchase_date ELSE ?5 END,
                 usage_month_window_cost_offset = CASE WHEN ?5 IS NULL
                     THEN usage_month_window_cost_offset ELSE 0 END,
                 updated_at = ?2
             WHERE legacy_account_id = ?3 AND account_type = 'managed' AND setup_step = ?4",
            params![
                to.as_str(),
                Utc::now().to_rfc3339(),
                id,
                from.as_str(),
                confirmed_purchase_date,
            ],
        )?;
        if changed == 1 {
            account_store::sync_inference_credential_projection_on(&tx, id)?;
        }
        tx.commit()?;
        Ok(changed == 1)
    }

    /// Persist a candidate key while keeping the account isolated from routing.
    pub fn save_managed_key_for_verification(&self, id: &str, key_cipher: &str) -> Result<bool> {
        let tx = self.conn.unchecked_transaction()?;
        let changed = tx.execute(
            "UPDATE credentials
             SET key_cipher = ?1, enabled = 0, auth_error = NULL, last_error = NULL,
                 quota_recovery_json = NULL, updated_at = ?2
             WHERE legacy_account_id = ?3 AND account_type = 'managed' AND setup_step = 'key_verification'",
            params![key_cipher, Utc::now().to_rfc3339(), id],
        )?;
        if changed == 1 {
            account_store::sync_inference_credential_projection_on(&tx, id)?;
        }
        tx.commit()?;
        Ok(changed == 1)
    }

    /// Commit a V3 managed-key verification as one all-or-nothing SQLite
    /// transaction. The captured row fingerprint is checked by the first write,
    /// before the candidate ciphertext can replace a concurrent update.
    ///
    /// A changed ciphertext uses the same identity bump as credential rotation:
    /// `credential_version` and `auth_state_version` advance together and
    /// `rotated_at` is set, in this statement. The bump compares the captured
    /// ciphertext with the candidate, and the WHERE clause requires that
    /// captured ciphertext to still be the stored one. Completing the ciphertext
    /// already stored does not bump either version again.
    pub fn commit_managed_key_verification(
        &self,
        id: &str,
        expected: &ManagedKeyVerificationCas,
        candidate_key_cipher: &str,
        write: &ManagedKeyVerificationWrite,
    ) -> Result<ManagedKeyVerificationCommit> {
        if matches!(write, ManagedKeyVerificationWrite::Verified { .. }) {
            ensure_enabled_provider_is_routable(&expected.provider_id, true)?;
        }

        let now = Utc::now();
        let now_rfc = now.to_rfc3339();
        let tx = self.conn.unchecked_transaction()?;
        let changed = tx.execute(
            "UPDATE credentials
             SET key_cipher = ?1, enabled = 0, auth_error = NULL, last_error = NULL,
                 quota_recovery_json = NULL, updated_at = ?2,
                 credential_version = COALESCE(credential_version, 1)
                     + CASE WHEN ?4 = ?1 THEN 0 ELSE 1 END,
                 auth_state_version = COALESCE(auth_state_version, 1)
                     + CASE WHEN ?4 = ?1 THEN 0 ELSE 1 END,
                 rotated_at = CASE WHEN ?4 = ?1 THEN rotated_at ELSE ?2 END
             WHERE legacy_account_id = ?3 AND key_cipher = ?4 AND updated_at = ?5
               AND provider_id = ?6
               AND account_type = ?7 AND setup_step = ?8",
            params![
                candidate_key_cipher,
                now_rfc,
                id,
                expected.key_cipher,
                expected.updated_at.to_rfc3339(),
                expected.provider_id,
                expected.account_type.as_str(),
                expected.setup_step.as_str(),
            ],
        )?;
        if changed != 1 {
            return Ok(ManagedKeyVerificationCommit::Conflict);
        }

        match write {
            ManagedKeyVerificationWrite::Verified {
                rate_limit,
                account_name,
            } => {
                if let Some(rate_limit) = rate_limit {
                    let column = match rate_limit.window {
                        Some(UsageWindowKind::FiveHours) => "cooldown_5h_until",
                        Some(UsageWindowKind::Week) => "cooldown_week_until",
                        Some(UsageWindowKind::Month) => "cooldown_month_until",
                        Some(UsageWindowKind::Free) => "cooldown_free_until",
                        None => "cooldown_generic_until",
                    };
                    let completed = tx.execute(
                        &format!(
                            "UPDATE credentials
                             SET {column} = ?2, last_error = ?3,
                                 setup_step = 'ready', enabled = 1, auth_error = NULL,
                                 verification_status = CASE
                                     WHEN verification_status = 'not_required'
                                     THEN 'not_required' ELSE 'verified' END,
                                 connection_verified_at = CASE
                                     WHEN verification_status = 'not_required'
                                     THEN NULL ELSE ?4 END,
                                 verification_error = NULL, updated_at = ?4
                             WHERE legacy_account_id = ?1 AND key_cipher = ?5"
                        ),
                        params![
                            id,
                            rate_limit.until.to_rfc3339(),
                            rate_limit.error,
                            now_rfc,
                            candidate_key_cipher,
                        ],
                    )?;
                    anyhow::ensure!(completed == 1, "managed verification row disappeared");
                    let cooldown_until = compute_cooldown_until(&tx, id, &now_rfc)?;
                    tx.execute(
                        "UPDATE credentials SET cooldown_until = ?2 WHERE legacy_account_id = ?1",
                        params![id, cooldown_until],
                    )?;
                    if rate_limit.window == Some(UsageWindowKind::Free) {
                        upsert_free_channel_cooldown(&tx, &rate_limit.until.to_rfc3339())?;
                    }
                } else {
                    let completed = tx.execute(
                        "UPDATE credentials
                         SET setup_step = 'ready', enabled = 1, auth_error = NULL,
                             cooldown_until = NULL, cooldown_generic_until = NULL,
                             cooldown_5h_until = NULL, cooldown_week_until = NULL,
                             cooldown_month_until = NULL, cooldown_free_until = NULL,
                             last_error = NULL,
                             verification_status = CASE
                                 WHEN verification_status = 'not_required'
                                 THEN 'not_required' ELSE 'verified' END,
                             connection_verified_at = CASE
                                 WHEN verification_status = 'not_required'
                                 THEN NULL ELSE ?2 END,
                             verification_error = NULL, updated_at = ?2
                         WHERE legacy_account_id = ?1 AND key_cipher = ?3",
                        params![id, now_rfc, candidate_key_cipher],
                    )?;
                    anyhow::ensure!(completed == 1, "managed verification row disappeared");
                }
                let message = format!("verified managed account {account_name}");
                ocg_infra::sqlite_logs::insert_gateway_log(
                    &tx,
                    &GatewayLogInsertRow {
                        level: "info",
                        category: "account",
                        message: &message,
                        created_at: &now_rfc,
                        request_id: None,
                        attempt: None,
                        error_source: None,
                        error_stage: None,
                        duration_ms: None,
                        diagnostic_json: None,
                    },
                )?;
            }
            ManagedKeyVerificationWrite::AuthFailed { auth_error } => {
                let updated = tx.execute(
                    "UPDATE credentials SET auth_error = ?2, updated_at = ?3
                     WHERE legacy_account_id = ?1 AND key_cipher = ?4",
                    params![id, auth_error, now_rfc, candidate_key_cipher],
                )?;
                anyhow::ensure!(updated == 1, "managed verification row disappeared");
            }
            ManagedKeyVerificationWrite::Pending => {}
        }

        account_store::sync_inference_credential_projection_on(&tx, id)?;
        tx.commit()?;
        Ok(ManagedKeyVerificationCommit::Applied)
    }

    /// Make a verified managed account routable only if the tested encrypted key
    /// is still the one stored in the row.
    pub fn complete_managed_setup_if_key_matches(
        &self,
        id: &str,
        expected_key_cipher: &str,
    ) -> Result<bool> {
        let Some(account) = self.get_account(id)? else {
            return Ok(false);
        };
        ensure_enabled_provider_is_routable(&account.provider_id, true)?;
        let tx = self.conn.unchecked_transaction()?;
        let changed = tx.execute(
            "UPDATE credentials
             SET setup_step = 'ready', enabled = 1, auth_error = NULL, updated_at = ?1
             WHERE legacy_account_id = ?2 AND account_type = 'managed'
               AND setup_step = 'key_verification' AND key_cipher = ?3",
            params![Utc::now().to_rfc3339(), id, expected_key_cipher],
        )?;
        if changed == 1 {
            account_store::sync_inference_credential_projection_on(&tx, id)?;
        }
        tx.commit()?;
        Ok(changed == 1)
    }

    /// Reset only an unfinished managed onboarding. Ready accounts keep their key
    /// when their browser profile is reset.
    pub fn reset_pending_managed_setup(&self, id: &str) -> Result<bool> {
        let tx = self.conn.unchecked_transaction()?;
        let changed = tx.execute(
            "UPDATE credentials
             SET setup_step = 'google_account', key_cipher = '', enabled = 0,
                 auth_error = NULL, last_error = NULL, cooldown_until = NULL,
                 cooldown_generic_until = NULL, cooldown_5h_until = NULL,
                 cooldown_week_until = NULL, cooldown_month_until = NULL, cooldown_free_until = NULL,
                 updated_at = ?1
             WHERE legacy_account_id = ?2 AND account_type = 'managed' AND setup_step <> 'ready'",
            params![Utc::now().to_rfc3339(), id],
        )?;
        if changed == 1 {
            account_store::sync_inference_credential_projection_on(&tx, id)?;
        }
        tx.commit()?;
        Ok(changed == 1)
    }

    pub fn reorder_accounts(
        &self,
        account_ids: &[String],
    ) -> std::result::Result<(), ReorderAccountsError> {
        let tx = self.conn.unchecked_transaction()?;
        let mut requested_ids = HashSet::with_capacity(account_ids.len());
        if account_ids
            .iter()
            .any(|id| !requested_ids.insert(id.as_str()))
        {
            return Err(ReorderAccountsError::DuplicateAccountId);
        }

        let current_ids = {
            let mut stmt = tx.prepare(
                "SELECT legacy_account_id FROM credentials
                 WHERE COALESCE(credential_purpose, 'inference') = 'inference'",
            )?;
            let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
            rows.collect::<rusqlite::Result<Vec<_>>>()?
        };
        if current_ids.len() != account_ids.len()
            || current_ids
                .iter()
                .any(|id| !requested_ids.contains(id.as_str()))
        {
            return Err(ReorderAccountsError::AccountSetMismatch);
        }

        for (sort_order, id) in account_ids.iter().enumerate() {
            tx.execute(
                "UPDATE credentials SET routing_rank = ?1 WHERE legacy_account_id = ?2",
                params![sort_order as i64, id],
            )?;
        }
        routing_cards::reconcile_on(&tx).map_err(ReorderAccountsError::Layout)?;
        tx.commit()?;
        Ok(())
    }

    // Cooldown
    /// Set or clear a per-account rate-limit cooldown.
    /// Pass `None` for both `until` and `err` to clear.
    pub fn set_account_cooldown(
        &self,
        id: &str,
        until: Option<DateTime<Utc>>,
        err: Option<&str>,
    ) -> Result<()> {
        let now = Utc::now().to_rfc3339();
        let tx = self.conn.unchecked_transaction()?;
        if until.is_none() && err.is_none() {
            tx.execute(
                "UPDATE credentials
                 SET cooldown_until = NULL,
                     cooldown_generic_until = NULL,
                     cooldown_5h_until = NULL,
                     cooldown_week_until = NULL,
                     cooldown_month_until = NULL,
                     cooldown_free_until = NULL,
                     last_error = NULL,
                     updated_at = ?2
                 WHERE legacy_account_id = ?1",
                params![id, now],
            )?;
        } else {
            tx.execute(
                "UPDATE credentials
                 SET cooldown_generic_until = ?2, last_error = ?3, updated_at = ?4
                 WHERE legacy_account_id = ?1",
                params![id, until.map(|t| t.to_rfc3339()), err, now],
            )?;
            let new_cooldown = compute_cooldown_until(&tx, id, &now)?;
            tx.execute(
                "UPDATE credentials SET cooldown_until = ?2 WHERE legacy_account_id = ?1",
                params![id, new_cooldown],
            )?;
        }
        identity::fanout_shared_pool_cooldown(&tx, id, !(until.is_none() && err.is_none()))?;
        tx.commit()?;
        Ok(())
    }

    pub fn clear_account_cooldown(&self, id: &str) -> Result<()> {
        self.set_account_cooldown(id, None, None)
    }

    /// Persist or clear an account-specific upstream 401. This state is kept
    /// separate from cooldowns because authentication failures do not carry a
    /// reset deadline and must not be reported as rate limits.
    pub fn set_account_auth_error(&self, id: &str, error: Option<&str>) -> Result<()> {
        let tx = self.conn.unchecked_transaction()?;
        tx.execute(
            "UPDATE credentials SET auth_error = ?2, updated_at = ?3 WHERE legacy_account_id = ?1",
            params![id, error, Utc::now().to_rfc3339()],
        )?;
        account_store::sync_inference_credential_projection_on(&tx, id)?;
        tx.commit()?;
        Ok(())
    }

    /// Update auth state only when the stored credential is still the one that
    /// produced this upstream response. A late response from a replaced key
    /// must not break or recover the new credential.
    pub fn set_account_auth_error_if_key_matches(
        &self,
        id: &str,
        expected_key_cipher: &str,
        error: Option<&str>,
    ) -> Result<bool> {
        let tx = self.conn.unchecked_transaction()?;
        let updated = tx.execute(
            "UPDATE credentials
             SET auth_error = ?3, updated_at = ?4
             WHERE legacy_account_id = ?1 AND key_cipher = ?2",
            params![id, expected_key_cipher, error, Utc::now().to_rfc3339()],
        )?;
        if updated > 0 {
            account_store::sync_inference_credential_projection_on(&tx, id)?;
        }
        tx.commit()?;
        Ok(updated > 0)
    }

    /// Record a real upstream 429 and reset only the identified manual usage window.
    pub fn set_account_rate_limit(
        &self,
        id: &str,
        until: DateTime<Utc>,
        err: &str,
        window: Option<UsageWindowKind>,
    ) -> Result<()> {
        self.set_account_rate_limit_inner(id, None, until, err, window)?;
        Ok(())
    }

    /// Record a 429 only when the credential that produced it is still current.
    /// This prevents a delayed response from an old key from cooling down a
    /// replacement credential.
    pub fn set_account_rate_limit_if_key_matches(
        &self,
        id: &str,
        expected_key_cipher: &str,
        until: DateTime<Utc>,
        err: &str,
        window: Option<UsageWindowKind>,
    ) -> Result<bool> {
        self.set_account_rate_limit_inner(id, Some(expected_key_cipher), until, err, window)
    }

    fn set_account_rate_limit_inner(
        &self,
        id: &str,
        expected_key_cipher: Option<&str>,
        until: DateTime<Utc>,
        err: &str,
        window: Option<UsageWindowKind>,
    ) -> Result<bool> {
        let now = Utc::now();
        let now_rfc = now.to_rfc3339();
        let tx = self.conn.unchecked_transaction()?;

        // Unknown upstream rate limits need their own slot so a later known window
        // cannot overwrite a still-active generic cooldown.
        let column = match window {
            Some(UsageWindowKind::FiveHours) => "cooldown_5h_until",
            Some(UsageWindowKind::Week) => "cooldown_week_until",
            Some(UsageWindowKind::Month) => "cooldown_month_until",
            Some(UsageWindowKind::Free) => "cooldown_free_until",
            None => "cooldown_generic_until",
        };
        let updated = tx.execute(
            &format!(
                "UPDATE credentials SET {column} = ?2, last_error = ?3, updated_at = ?4
                 WHERE legacy_account_id = ?1 AND (?5 IS NULL OR key_cipher = ?5)"
            ),
            params![id, until.to_rfc3339(), err, now_rfc, expected_key_cipher],
        )?;
        if updated == 0 && window != Some(UsageWindowKind::Free) {
            return Ok(false);
        }

        if updated > 0 {
            // Legacy callers use cooldown_until as the time when this account is usable.
            let new_cooldown = compute_cooldown_until(&tx, id, &now_rfc)?;
            tx.execute(
                "UPDATE credentials SET cooldown_until = ?2 WHERE legacy_account_id = ?1",
                params![id, new_cooldown],
            )?;
        }

        if window == Some(UsageWindowKind::Free) {
            // A Free 429 proves the egress-IP quota is exhausted even if the
            // originating key was concurrently replaced or its account deleted.
            // Keep the furthest observed deadline and commit it atomically with
            // the account-local compatibility copy when that row still exists.
            upsert_free_channel_cooldown(&tx, &until.to_rfc3339())?;
        }

        if updated > 0 {
            identity::fanout_shared_pool_cooldown(&tx, id, true)?;
        }

        // ponytail: 不再在 429 时设置 baseline。固定窗口的"重置"由 forward_logs 自然驱动；
        // 冷却到期后账号恢复可用，用量窗口照常计算。429 仅用于阻断选择器重试。
        // 旧 baseline 列保留不读不写，避免迁移风险。
        tx.commit()?;
        Ok(updated > 0)
    }

    /// Among all enabled accounts, return the first time any account becomes usable.
    /// `None` means no account is in cooldown.
    pub fn soonest_cooldown_reset(&self) -> Result<Option<DateTime<Utc>>> {
        let now = Utc::now().to_rfc3339();
        let res: Option<String> = self
            .conn
            .query_row(
                "SELECT MIN(cooldown_until)
                 FROM credentials
                 WHERE enabled = 1
                   AND setup_step = 'ready'
                   AND key_cipher <> ''
                   AND auth_error IS NULL
                   AND cooldown_until IS NOT NULL
                   AND cooldown_until > ?1",
                params![now],
                |row| row.get(0),
            )
            .optional()?
            .flatten();
        Ok(res.and_then(|s| {
            DateTime::parse_from_rfc3339(&s)
                .ok()
                .map(|dt| dt.with_timezone(&Utc))
        }))
    }

    // Usage
    /// 手动校准一个固定窗口的"当前已用百分比"与"距上游重置还剩多久"。
    /// `percent` = 当前已用百分比（0-100），`resets_in_minutes` = 距上游重置还剩多少分钟
    /// （None 表示从 now 起算满窗口时长；月窗口忽略此参数——窗口由 purchase_date/expires_on 决定）。
    /// `limit` = 当前窗口的限额（从 PricingSnapshot 读取，避免硬编码）。
    pub fn calibrate_account_usage(
        &self,
        account_id: &str,
        window: UsageWindowKind,
        percent: f64,
        resets_in_minutes: Option<i64>,
        limit: f64,
    ) -> Result<bool> {
        calibrate_account_usage_on(
            &self.conn,
            account_id,
            window,
            percent,
            resets_in_minutes,
            limit,
            Utc::now(),
        )
    }

    /// Atomically calibrate rolling, weekly, and monthly Go usage windows.
    /// Any input, SQL, or missing-account error rolls the whole transaction back.
    pub fn calibrate_account_usage_snapshot(
        &self,
        account_id: &str,
        snapshot: &AccountUsageCalibrationSnapshot,
        limits: &PricingLimits,
    ) -> Result<UsageWindow> {
        let tx = self.conn.unchecked_transaction()?;
        let now = Utc::now();
        if !calibrate_account_usage_on(
            &tx,
            account_id,
            UsageWindowKind::FiveHours,
            snapshot.rolling_percent,
            Some(snapshot.rolling_resets_in_minutes),
            limits.window_5h,
            now,
        )? {
            anyhow::bail!("account {account_id} not found");
        }
        if !calibrate_account_usage_on(
            &tx,
            account_id,
            UsageWindowKind::Week,
            snapshot.weekly_percent,
            Some(snapshot.weekly_resets_in_minutes),
            limits.window_week,
            now,
        )? {
            anyhow::bail!("account {account_id} not found");
        }
        if !calibrate_account_usage_on(
            &tx,
            account_id,
            UsageWindowKind::Month,
            snapshot.monthly_percent,
            None,
            limits.window_month,
            now,
        )? {
            anyhow::bail!("account {account_id} not found");
        }
        tx.commit()?;
        self.account_usage_with_limits(account_id, limits)
    }

    /// Atomically CAS the credential/setup state, calibrate all three official
    /// usage windows, persist sync-success metadata, and compute the returned
    /// usage. `None` means the account disappeared or changed while the
    /// network request was in flight. Any SQL/read failure rolls everything
    /// back, so a failed refresh never exposes a partially updated baseline.
    pub fn commit_official_usage_sync_success(
        &self,
        account_id: &str,
        expected_key_cipher: &str,
        snapshot: &AccountUsageCalibrationSnapshot,
        limits: &PricingLimits,
        metadata: AccountUsageSyncSuccessMetadata,
    ) -> Result<Option<UsageWindow>> {
        let tx = self.conn.unchecked_transaction()?;
        let matches: i64 = tx.query_row(
            "SELECT EXISTS(
                SELECT 1 FROM credentials
                WHERE legacy_account_id = ?1
                  AND key_cipher = ?2
                  AND key_cipher <> ''
                  AND setup_step = 'ready'
             )",
            params![account_id, expected_key_cipher],
            |row| row.get(0),
        )?;
        if matches == 0 {
            return Ok(None);
        }

        if !calibrate_account_usage_on(
            &tx,
            account_id,
            UsageWindowKind::FiveHours,
            snapshot.rolling_percent,
            Some(snapshot.rolling_resets_in_minutes),
            limits.window_5h,
            metadata.now,
        )? {
            anyhow::bail!("account {account_id} disappeared during official usage sync");
        }
        if !calibrate_account_usage_on(
            &tx,
            account_id,
            UsageWindowKind::Week,
            snapshot.weekly_percent,
            Some(snapshot.weekly_resets_in_minutes),
            limits.window_week,
            metadata.now,
        )? {
            anyhow::bail!("account {account_id} disappeared during official usage sync");
        }
        if !calibrate_account_usage_on(
            &tx,
            account_id,
            UsageWindowKind::Month,
            snapshot.monthly_percent,
            None,
            limits.window_month,
            metadata.now,
        )? {
            anyhow::bail!("account {account_id} disappeared during official usage sync");
        }

        record_account_usage_sync_success_on(&tx, account_id, metadata)?;
        let usage = account_usage_with_limits_on(&tx, account_id, limits, metadata.now)?;
        tx.commit()?;
        Ok(Some(usage))
    }

    /// OpenCode Go windows. Uses the latest Go pricing snapshot, or
    /// [`SEED_LIMITS`] when none is stored. Other plans pass their own limits
    /// to [`Self::account_usage_with_limits`].
    pub fn opencode_go_account_usage(&self, account_id: &str) -> Result<UsageWindow> {
        let limits = self
            .latest_pricing_snapshot()?
            .map(|snapshot| snapshot.limits)
            .unwrap_or(SEED_LIMITS);
        self.account_usage_with_limits(account_id, &limits)
    }

    pub fn account_usage_with_limits(
        &self,
        account_id: &str,
        limits: &PricingLimits,
    ) -> Result<UsageWindow> {
        account_usage_with_limits_on(&self.conn, account_id, limits, Utc::now())
    }

    /// Project the canonical legacy Go accounting windows into the provider
    /// API shape. The v22 `quota_windows` rows are migration/interoperability
    /// storage, not a second Go accounting authority: local forward logs and
    /// calibration offsets continue to advance between official syncs.
    pub fn live_opencode_go_quota_windows(
        &self,
        account_id: &str,
        limits: &PricingLimits,
    ) -> Result<Vec<QuotaWindow>> {
        let observed_at = self
            .account_usage_sync_state(account_id)?
            .and_then(|sync| sync.last_success_at);
        self.live_fixed_quota_windows(account_id, limits, "opencode-go-live", observed_at)
    }

    /// Project locally priced request logs plus manual calibration into the
    /// provider-neutral quota window shape. This is the single read authority
    /// for plans such as GOAT that have no machine-readable upstream usage API.
    pub fn live_local_quota_windows(
        &self,
        account_id: &str,
        limits: &PricingLimits,
        source: &str,
    ) -> Result<Vec<QuotaWindow>> {
        self.live_fixed_quota_windows(account_id, limits, source, None)
    }

    /// One monthly USD-credit window from locally priced request logs plus
    /// calibration. Used credit is not clamped to the soft limit. 5h/week
    /// windows are not published.
    pub fn live_ollama_month_quota_window(
        &self,
        account_id: &str,
        month_limit: f64,
    ) -> Result<Vec<QuotaWindow>> {
        let now = Utc::now();
        let (offset, purchase_date): (f64, String) = self.conn.query_row(
            "SELECT usage_month_window_cost_offset, purchase_date FROM credentials WHERE legacy_account_id = ?1",
            [account_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        let (used, resets_at) =
            compute_ollama_month_window(&self.conn, account_id, &purchase_date, offset)?;
        Ok(vec![QuotaWindow {
            account_id: account_id.to_string(),
            window_kind: QUOTA_WINDOW_MONTH.to_string(),
            used,
            limit_value: Some(month_limit),
            started_at: month_window_start_utc(&purchase_date).ok(),
            resets_at,
            calibration_offset: offset,
            unit: "usd_credits".to_string(),
            source: "ollama-cloud-local".to_string(),
            observed_at: None,
            updated_at: now,
        }])
    }

    /// Unclamped Ollama month used credit and reset instant.
    pub fn ollama_month_usage(&self, account_id: &str) -> Result<(f64, Option<DateTime<Utc>>)> {
        let Some((offset, purchase_date)) = self
            .conn
            .query_row(
                "SELECT usage_month_window_cost_offset, purchase_date FROM credentials WHERE legacy_account_id = ?1",
                [account_id],
                |row| Ok((row.get::<_, f64>(0)?, row.get::<_, String>(1)?)),
            )
            .optional()?
        else {
            return Ok((0.0, None));
        };
        compute_ollama_month_window(&self.conn, account_id, &purchase_date, offset)
    }

    pub fn calibrate_ollama_month_usage(
        &self,
        account_id: &str,
        percent: f64,
        limit: f64,
        now: DateTime<Utc>,
    ) -> Result<bool> {
        let purchase_date: String = match self
            .conn
            .query_row(
                "SELECT purchase_date FROM credentials WHERE legacy_account_id = ?1",
                [account_id],
                |row| row.get(0),
            )
            .optional()?
        {
            Some(value) => value,
            None => return Ok(false),
        };
        let actual_cost = sum_ollama_month_cost_on(&self.conn, account_id, &purchase_date)?;
        let offset = limit * percent / 100.0 - actual_cost;
        let changed = self.conn.execute(
            "UPDATE credentials
             SET usage_month_window_cost_offset = ?2,
                 updated_at = ?3
             WHERE legacy_account_id = ?1",
            params![account_id, offset, now.to_rfc3339()],
        )?;
        Ok(changed > 0)
    }

    fn live_fixed_quota_windows(
        &self,
        account_id: &str,
        limits: &PricingLimits,
        source: &str,
        observed_at: Option<DateTime<Utc>>,
    ) -> Result<Vec<QuotaWindow>> {
        let now = Utc::now();
        let usage = account_usage_with_limits_on(&self.conn, account_id, limits, now)?;
        let metadata = self
            .conn
            .query_row(
                "SELECT usage_5h_window_started_at, usage_5h_window_cost_offset,
                        usage_week_window_started_at, usage_week_window_cost_offset,
                        usage_month_window_cost_offset, purchase_date
                 FROM credentials WHERE legacy_account_id = ?1",
                [account_id],
                |row| {
                    Ok((
                        row.get::<_, Option<String>>(0)?,
                        row.get::<_, f64>(1)?,
                        row.get::<_, Option<String>>(2)?,
                        row.get::<_, f64>(3)?,
                        row.get::<_, f64>(4)?,
                        row.get::<_, String>(5)?,
                    ))
                },
            )
            .optional()?
            .ok_or_else(|| anyhow::anyhow!("account {account_id} not found"))?;
        let month_started_at = month_window_start_utc(&metadata.5).ok();

        Ok(vec![
            QuotaWindow {
                account_id: account_id.to_string(),
                window_kind: QUOTA_WINDOW_FIVE_HOURS.to_string(),
                used: usage.window_5h,
                limit_value: Some(limits.window_5h),
                started_at: metadata.0.map(parse_datetime),
                resets_at: usage.resets_in_5h,
                calibration_offset: metadata.1,
                unit: "usd".to_string(),
                source: source.to_string(),
                observed_at,
                updated_at: now,
            },
            QuotaWindow {
                account_id: account_id.to_string(),
                window_kind: QUOTA_WINDOW_WEEK.to_string(),
                used: usage.window_week,
                limit_value: Some(limits.window_week),
                started_at: metadata.2.map(parse_datetime),
                resets_at: usage.resets_in_week,
                calibration_offset: metadata.3,
                unit: "usd".to_string(),
                source: source.to_string(),
                observed_at,
                updated_at: now,
            },
            QuotaWindow {
                account_id: account_id.to_string(),
                window_kind: QUOTA_WINDOW_MONTH.to_string(),
                used: usage.window_month,
                limit_value: Some(limits.window_month),
                started_at: month_started_at,
                resets_at: usage.resets_in_month,
                calibration_offset: metadata.4,
                unit: "usd".to_string(),
                source: source.to_string(),
                observed_at,
                updated_at: now,
            },
        ])
    }

    pub fn total_usage(&self) -> Result<(f64, f64, f64)> {
        let now = Utc::now();
        let today_start = now
            .date_naive()
            .and_hms_opt(0, 0, 0)
            .unwrap()
            .and_utc()
            .to_rfc3339();
        let week_ago = (now - Duration::days(7)).to_rfc3339();
        let month_ago = (now - Duration::days(30)).to_rfc3339();

        let today: f64 = self.conn.query_row(
            "SELECT COALESCE(SUM(cost), 0) FROM forward_logs WHERE cost_state IN ('priced', 'legacy_estimate') AND timestamp > ?1",
            [&today_start],
            |row| row.get(0),
        )?;
        let week: f64 = self.conn.query_row(
            "SELECT COALESCE(SUM(cost), 0) FROM forward_logs WHERE cost_state IN ('priced', 'legacy_estimate') AND timestamp > ?1",
            [&week_ago],
            |row| row.get(0),
        )?;
        let month: f64 = self.conn.query_row(
            "SELECT COALESCE(SUM(cost), 0) FROM forward_logs WHERE cost_state IN ('priced', 'legacy_estimate') AND timestamp > ?1",
            [&month_ago],
            |row| row.get(0),
        )?;

        Ok((today, week, month))
    }

    /// Aggregate `forward_logs` into per-day, per-model token buckets covering
    /// the last `days` UTC calendar days. The window is half-open: from
    /// midnight UTC of the earliest day, up to but not including midnight UTC
    /// of the next day. Rows with zero tokens on a given day are omitted — the
    /// frontend synthesizes empty days so the x-axis never collapses. Token
    /// totals are independent of pricing state: free, priced, legacy estimate,
    /// and not_applicable rows all contribute as long as they carry non-zero
    /// prompt or completion tokens.
    pub fn daily_tokens_by_model(&self, days: i64) -> Result<Vec<DailyModelTokens>> {
        let (start, end) = utc_token_day_bounds(Utc::now(), days)?;
        let start = start.to_rfc3339();
        let end = end.to_rfc3339();
        let mut stmt = self.conn.prepare(
            "SELECT substr(timestamp, 1, 10) AS day, model, COALESCE(SUM(prompt_tokens + completion_tokens), 0)
             FROM forward_logs
             WHERE timestamp >= ?1 AND timestamp < ?2 AND prompt_tokens + completion_tokens > 0
             GROUP BY day, model
             ORDER BY day ASC, model ASC",
        )?;
        let rows = stmt.query_map(params![start, end], |row| {
            Ok(DailyModelTokens {
                date: row.get::<_, String>(0)?,
                model: row.get::<_, String>(1)?,
                tokens: row.get::<_, i64>(2)?,
            })
        })?;
        rows.collect::<Result<Vec<_>, _>>().map_err(|e| e.into())
    }
}

/// Half-open UTC calendar window `[start, end)` covering `days` natural days
/// ending today. `days` must be at least 1.
pub(crate) fn utc_token_day_bounds(
    now: DateTime<Utc>,
    days: i64,
) -> Result<(DateTime<Utc>, DateTime<Utc>)> {
    if days < 1 {
        anyhow::bail!("days must be at least 1");
    }
    let today = now.date_naive();
    let start_day = today
        .checked_sub_signed(Duration::days(days - 1))
        .ok_or_else(|| anyhow::anyhow!("days is outside the supported range"))?;
    let end_day = today
        .checked_add_signed(Duration::days(1))
        .ok_or_else(|| anyhow::anyhow!("days is outside the supported range"))?;
    let start = start_day
        .and_hms_opt(0, 0, 0)
        .ok_or_else(|| anyhow::anyhow!("invalid UTC day start"))?
        .and_utc();
    let end = end_day
        .and_hms_opt(0, 0, 0)
        .ok_or_else(|| anyhow::anyhow!("invalid UTC day end"))?
        .and_utc();
    Ok((start, end))
}

fn record_account_usage_sync_success_on(
    conn: &Connection,
    account_id: &str,
    metadata: AccountUsageSyncSuccessMetadata,
) -> Result<()> {
    let changed = if metadata.mark_expedited {
        conn.execute(
            "UPDATE provider_usage_sync_state
             SET last_success_at = ?1,
                 last_attempt_at = ?1,
                 next_eligible_at = ?2,
                 failure_streak = 0,
                 last_expedited_at = ?1
             WHERE account_id = ?3",
            params![
                metadata.now.to_rfc3339(),
                metadata.next_eligible_at.to_rfc3339(),
                account_id
            ],
        )?
    } else {
        conn.execute(
            "UPDATE provider_usage_sync_state
             SET last_success_at = ?1,
                 last_attempt_at = ?1,
                 next_eligible_at = ?2,
                 failure_streak = 0
             WHERE account_id = ?3",
            params![
                metadata.now.to_rfc3339(),
                metadata.next_eligible_at.to_rfc3339(),
                account_id
            ],
        )?
    };
    if changed != 1 {
        anyhow::bail!("account {account_id} disappeared while recording usage sync success");
    }
    Ok(())
}

pub(super) fn account_usage_with_limits_on(
    conn: &Connection,
    account_id: &str,
    limits: &PricingLimits,
    now: DateTime<Utc>,
) -> Result<UsageWindow> {
    let row_sql = if table_exists(conn, "accounts")? {
        "SELECT usage_5h_window_started_at, usage_5h_window_cost_offset,
                usage_week_window_started_at, usage_week_window_cost_offset,
                usage_month_window_cost_offset,
                recharge_date
         FROM accounts WHERE id = ?1"
    } else if table_exists(conn, "credentials")?
        && table_has_column(conn, "credentials", "purchase_date")?
    {
        "SELECT usage_5h_window_started_at, usage_5h_window_cost_offset,
                usage_week_window_started_at, usage_week_window_cost_offset,
                usage_month_window_cost_offset,
                purchase_date
         FROM credentials WHERE legacy_account_id = ?1"
    } else {
        return Ok(UsageWindow {
            account_id: account_id.to_string(),
            window_5h: 0.0,
            window_week: 0.0,
            window_month: 0.0,
            resets_in_5h: None,
            resets_in_week: None,
            resets_in_month: None,
        });
    };
    let row = conn.query_row(row_sql, [account_id], |row| {
        Ok((
            row.get::<_, Option<String>>(0)?,
            row.get::<_, Option<f64>>(1)?.unwrap_or(0.0),
            row.get::<_, Option<String>>(2)?,
            row.get::<_, Option<f64>>(3)?.unwrap_or(0.0),
            row.get::<_, Option<f64>>(4)?.unwrap_or(0.0),
            row.get::<_, Option<String>>(5)?.unwrap_or_default(),
        ))
    });
    let (started_5h_str, offset_5h, started_week_str, offset_week, offset_month, purchase_date) =
        match row.optional()? {
            Some(value) => value,
            None => {
                return Ok(UsageWindow {
                    account_id: account_id.to_string(),
                    window_5h: 0.0,
                    window_week: 0.0,
                    window_month: 0.0,
                    resets_in_5h: None,
                    resets_in_week: None,
                    resets_in_month: None,
                });
            }
        };

    let (cost_5h, reset_5h) = compute_fixed_window(
        conn,
        account_id,
        started_5h_str.as_deref(),
        offset_5h,
        limits.window_5h,
        now,
        FixedWindowSpec {
            length: Duration::hours(5),
            started_col: "usage_5h_window_started_at",
            offset_col: "usage_5h_window_cost_offset",
        },
    )?;
    let (cost_week, reset_week) = compute_fixed_window(
        conn,
        account_id,
        started_week_str.as_deref(),
        offset_week,
        limits.window_week,
        now,
        FixedWindowSpec {
            length: Duration::days(7),
            started_col: "usage_week_window_started_at",
            offset_col: "usage_week_window_cost_offset",
        },
    )?;
    let (cost_month, reset_month) = compute_month_window(
        conn,
        account_id,
        &purchase_date,
        offset_month,
        limits.window_month,
    )?;

    Ok(UsageWindow {
        account_id: account_id.to_string(),
        window_5h: cost_5h,
        window_week: cost_week,
        window_month: cost_month,
        resets_in_5h: reset_5h,
        resets_in_week: reset_week,
        resets_in_month: reset_month,
    })
}

/// 计算固定窗口的当前用量与清零时刻。`started_at_str` 为 `None` 表示账号从未使用过该窗口；
/// 窗口已过期时从 `forward_logs` lazy 重建新起点。
struct FixedWindowSpec {
    length: Duration,
    started_col: &'static str,
    offset_col: &'static str,
}

fn compute_fixed_window(
    conn: &Connection,
    account_id: &str,
    started_at_str: Option<&str>,
    offset: f64,
    limit: f64,
    now: DateTime<Utc>,
    spec: FixedWindowSpec,
) -> Result<(f64, Option<DateTime<Utc>>)> {
    let mut started_at = match started_at_str {
        None => {
            // ponytail: lazy 初始化——查 forward_logs 第一条计费请求作为窗口起点。
            // 计费行 = cost_state IN ('priced', 'legacy_estimate')，
            // 与下方 SUM(cost) 的过滤保持一致，确保迁移后的 legacy error 也能触发窗口。
            let first: Option<String> = conn
                .query_row(
                    "SELECT MIN(timestamp) FROM forward_logs
                     WHERE account_id = ?1
                       AND cost_state IN ('priced', 'legacy_estimate')",
                    [account_id],
                    |row| row.get(0),
                )
                .optional()?
                .flatten();
            match first {
                None => return Ok((0.0, None)), // 真的没用过
                Some(s) => {
                    let source = account_store::account_row_source(conn)?;
                    conn.execute(
                        &format!(
                            "UPDATE {} SET {} = ?2, {} = 0
                             WHERE {} = ?1",
                            source.table, spec.started_col, spec.offset_col, source.id_col
                        ),
                        params![account_id, &s],
                    )?;
                    parse_rfc3339(&s)?
                }
            }
        }
        Some(s) => parse_rfc3339(s)?,
    };
    // 第一次进入循环时使用调用方传入的 offset（来自手动校准）；任何一次前进后，
    // offset 都被清零（`offset_col = 0` 已写入 DB），用 effective_offset 跟踪。
    let mut effective_offset = offset;

    loop {
        let ends_at = started_at + spec.length;
        if now < ends_at {
            // 窗口仍有效：用量 = effective_offset + SUM(cost WHERE ts >= started_at)
            let cost: f64 = conn.query_row(
                "SELECT COALESCE(SUM(cost), 0) FROM forward_logs
                 WHERE account_id = ?1
                   AND cost_state IN ('priced', 'legacy_estimate')
                   AND timestamp >= ?2",
                params![account_id, started_at.to_rfc3339()],
                |row| row.get(0),
            )?;
            return Ok(((effective_offset + cost).min(limit), Some(ends_at)));
        }

        // 窗口已过期：找 forward_logs 中第一条 timestamp >= ends_at 的计费请求作为新起点。
        // 关键修复：旧实现只前进一次就 return，遇到多条稀疏日志（间隔 > 5h）时每次刷新
        // 只前进一个窗口，造成前端可见的"用量从 60 → 30 → 13 → 5.8 → 0"递减幻觉；
        // 当 next=None 清空后下次刷新又 lazy-init 回最旧日志，循环重启。
        // 用 loop 在一次调用内连过所有过期窗口，直到落在有效窗口或彻底无新请求。
        let next: Option<String> = conn
            .query_row(
                "SELECT MIN(timestamp) FROM forward_logs
                 WHERE account_id = ?1
                   AND cost_state IN ('priced', 'legacy_estimate')
                   AND timestamp >= ?2",
                params![account_id, ends_at.to_rfc3339()],
                |row| row.get(0),
            )
            .optional()?
            .flatten();
        match next {
            None => {
                // 过期后无新请求：清空窗口，等待下次请求触发新窗口。
                let source = account_store::account_row_source(conn)?;
                conn.execute(
                    &format!(
                        "UPDATE {} SET {} = NULL, {} = 0
                         WHERE {} = ?1",
                        source.table, spec.started_col, spec.offset_col, source.id_col
                    ),
                    [account_id],
                )?;
                return Ok((0.0, None));
            }
            Some(s) => {
                started_at = parse_rfc3339(&s)?;
                effective_offset = 0.0;
                let source = account_store::account_row_source(conn)?;
                conn.execute(
                    &format!(
                        "UPDATE {} SET {} = ?2, {} = 0
                         WHERE {} = ?1",
                        source.table, spec.started_col, spec.offset_col, source.id_col
                    ),
                    params![account_id, &s],
                )?;
                // 继续循环：新起点对应的窗口可能也已过期，需要再判一次。
            }
        }
    }
}

/// Ollama month window: `[purchase_date 00:00 local, next-month-same-day 00:00 local)`.
/// Used credit is `offset + cost` and is not clamped to the soft limit.
fn compute_ollama_month_window(
    conn: &Connection,
    account_id: &str,
    purchase_date: &str,
    offset: f64,
) -> Result<(f64, Option<DateTime<Utc>>)> {
    if purchase_date.trim().is_empty() {
        return Ok((0.0, None));
    }
    let (start, end) = ollama_month_bounds(purchase_date)?;
    let cost = sum_priced_cost_between(conn, account_id, start, end)?;
    Ok((offset + cost, Some(end)))
}

fn ollama_month_bounds(purchase_date: &str) -> Result<(DateTime<Utc>, DateTime<Utc>)> {
    let start = month_window_start_utc(purchase_date)?;
    let expires = purchase_expires_on(purchase_date)?;
    let end_naive = NaiveDate::parse_from_str(&expires, "%Y-%m-%d")?
        .and_hms_opt(0, 0, 0)
        .unwrap();
    let end = Local
        .from_local_datetime(&end_naive)
        .single()
        .ok_or_else(|| anyhow::anyhow!("ambiguous local datetime for expires_on"))?
        .with_timezone(&Utc);
    Ok((start, end))
}

fn sum_ollama_month_cost_on(
    conn: &Connection,
    account_id: &str,
    purchase_date: &str,
) -> Result<f64> {
    if purchase_date.trim().is_empty() {
        return Ok(0.0);
    }
    let (start, end) = ollama_month_bounds(purchase_date)?;
    sum_priced_cost_between(conn, account_id, start, end)
}

fn sum_priced_cost_between(
    conn: &Connection,
    account_id: &str,
    start: DateTime<Utc>,
    end: DateTime<Utc>,
) -> Result<f64> {
    Ok(conn.query_row(
        "SELECT COALESCE(SUM(cost), 0) FROM forward_logs
         WHERE account_id = ?1
           AND cost_state IN ('priced', 'legacy_estimate')
           AND timestamp >= ?2
           AND timestamp < ?3",
        params![account_id, start.to_rfc3339(), end.to_rfc3339()],
        |row| row.get(0),
    )?)
}

/// 月窗口：从 `purchase_date 00:00 本地时区` 累计到 `purchase_expires_on(purchase_date) 00:00 本地时区`，不重置。
/// `offset` = 手动校准时写入的 `usage_month_window_cost_offset`，与 `compute_fixed_window` 对齐：
/// 返回值 = `(offset + cost).min(limit)`，让月窗口支持手动校准。
fn compute_month_window(
    conn: &Connection,
    account_id: &str,
    purchase_date: &str,
    offset: f64,
    limit: f64,
) -> Result<(f64, Option<DateTime<Utc>>)> {
    if purchase_date.trim().is_empty() {
        return Ok((0.0, None));
    }
    let start = month_window_start_utc(purchase_date)?;
    let expires = purchase_expires_on(purchase_date)?;
    let end_naive = NaiveDate::parse_from_str(&expires, "%Y-%m-%d")?
        .and_hms_opt(0, 0, 0)
        .unwrap();
    let end: DateTime<Utc> = Local
        .from_local_datetime(&end_naive)
        .single()
        .ok_or_else(|| anyhow::anyhow!("ambiguous local datetime for expires_on"))?
        .with_timezone(&Utc);
    let cost: f64 = conn.query_row(
        "SELECT COALESCE(SUM(cost), 0) FROM forward_logs
         WHERE account_id = ?1
           AND cost_state IN ('priced', 'legacy_estimate')
           AND timestamp >= ?2",
        params![account_id, start.to_rfc3339()],
        |row| row.get(0),
    )?;
    // ponytail: 月窗口已过期也照常返回终点，前端按"已到期"显示。
    Ok(((offset + cost).min(limit), Some(end)))
}

fn calibrate_account_usage_on(
    conn: &Connection,
    account_id: &str,
    window: UsageWindowKind,
    percent: f64,
    resets_in_minutes: Option<i64>,
    limit: f64,
    now: DateTime<Utc>,
) -> Result<bool> {
    // (started_at, offset_col, started_col_or_empty)
    // started_col 为空字符串表示月窗口——不写 started_at 列（起点固定为 purchase_date）。
    let (started_at, started_col, offset_col): (Option<DateTime<Utc>>, &str, &str) = match window {
        UsageWindowKind::FiveHours => {
            let window_len = Duration::hours(5);
            let started_at = calibrated_window_start(now, window_len, resets_in_minutes, "5-hour")?;
            (
                Some(started_at),
                "usage_5h_window_started_at",
                "usage_5h_window_cost_offset",
            )
        }
        UsageWindowKind::Week => {
            let window_len = Duration::days(7);
            let started_at = calibrated_window_start(now, window_len, resets_in_minutes, "weekly")?;
            (
                Some(started_at),
                "usage_week_window_started_at",
                "usage_week_window_cost_offset",
            )
        }
        UsageWindowKind::Month => {
            // 月窗口的起点/终点由 purchase_date 决定，不写 started_at 列。
            // resets_in_minutes 被忽略——窗口已由账号购买日期固定。
            (None, "", "usage_month_window_cost_offset")
        }
        UsageWindowKind::Free => {
            anyhow::bail!("free promo quota cannot be calibrated as a Go usage window")
        }
    };

    // 计算 actual_cost：窗口内已有 forward_logs 的 cost 总和。
    // 5h/周窗口的起点是刚算出的 started_at；月窗口的起点是 purchase_date 00:00 本地时区。
    let actual_cost: f64 = match started_at {
        Some(started) => conn.query_row(
            "SELECT COALESCE(SUM(cost), 0) FROM forward_logs
             WHERE account_id = ?1
               AND cost_state IN ('priced', 'legacy_estimate')
               AND timestamp >= ?2",
            params![account_id, started.to_rfc3339()],
            |row| row.get(0),
        )?,
        None => {
            let purchase_date: String = conn
                .query_row(
                    "SELECT purchase_date FROM credentials WHERE legacy_account_id = ?1",
                    [account_id],
                    |row| row.get(0),
                )
                .optional()?
                .ok_or_else(|| anyhow::anyhow!("account not found"))?;
            let started = month_window_start_utc(&purchase_date)?;
            conn.query_row(
                "SELECT COALESCE(SUM(cost), 0) FROM forward_logs
                 WHERE account_id = ?1
                   AND cost_state IN ('priced', 'legacy_estimate')
                   AND timestamp >= ?2",
                params![account_id, started.to_rfc3339()],
                |row| row.get(0),
            )?
        }
    };

    let target_cost = limit * percent / 100.0;
    // Bug 1.5 修复：去掉 max(0, ...) 钳制，允许负 offset。
    // 之前 max(0, target - actual) 配合 schema CHECK (offset >= 0) 让向左拉
    // 滑块时被锁死在实际 cost 对应的百分比。现在 offset 可以为负，
    // compute_fixed_window 返回 offset + actual = target_cost，与用户输入一致。
    let offset = target_cost - actual_cost;

    let changed = if started_col.is_empty() {
        // 月窗口：只更新 cost_offset（started_at 由 purchase_date 派生，不存储）
        conn.execute(
            "UPDATE credentials
             SET usage_month_window_cost_offset = ?2,
                 updated_at = ?3
             WHERE legacy_account_id = ?1",
            params![account_id, offset, now.to_rfc3339()],
        )?
    } else {
        let started = started_at.unwrap();
        conn.execute(
            &format!(
                "UPDATE credentials
                 SET {started_col} = ?2,
                     {offset_col} = ?3,
                     updated_at = ?4
                 WHERE legacy_account_id = ?1"
            ),
            params![account_id, started.to_rfc3339(), offset, now.to_rfc3339()],
        )?
    };
    Ok(changed > 0)
}

fn calibrated_window_start(
    now: DateTime<Utc>,
    window_len: Duration,
    resets_in_minutes: Option<i64>,
    window_name: &str,
) -> Result<DateTime<Utc>> {
    let max_minutes = window_len.num_minutes();
    let remaining_minutes = resets_in_minutes.unwrap_or(max_minutes);
    if !(0..=max_minutes).contains(&remaining_minutes) {
        return Err(anyhow::anyhow!(
            "{window_name} resets_in_minutes must be between 0 and {max_minutes}"
        ));
    }
    let remaining = Duration::try_minutes(remaining_minutes)
        .ok_or_else(|| anyhow::anyhow!("resets_in_minutes is out of range"))?;
    let ends_at = now
        .checked_add_signed(remaining)
        .ok_or_else(|| anyhow::anyhow!("usage window end is out of range"))?;
    ends_at
        .checked_sub_signed(window_len)
        .ok_or_else(|| anyhow::anyhow!("usage window start is out of range"))
}

fn parse_rfc3339(s: &str) -> Result<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(s)
        .map(|dt| dt.with_timezone(&Utc))
        .map_err(|e| anyhow::anyhow!("invalid RFC3339 timestamp: {e}"))
}

/// 把 `purchase_date`（YYYY-MM-DD）解释为本时区 00:00，转 UTC。
/// purchase_date 是 local_today() 写入的本地日期；转 UTC 时必须经过 Local 时区，
/// 否则本地早上的请求会被 UTC 午夜 cutoff 漏算。
pub(super) fn month_window_start_utc(purchase_date: &str) -> Result<DateTime<Utc>> {
    let normalized = normalize_purchase_date(purchase_date)?;
    let start_naive = NaiveDate::parse_from_str(&normalized, "%Y-%m-%d")?
        .and_hms_opt(0, 0, 0)
        .unwrap();
    Ok(Local
        .from_local_datetime(&start_naive)
        .single()
        .ok_or_else(|| anyhow::anyhow!("ambiguous local datetime for purchase_date"))?
        .with_timezone(&Utc))
}

pub(super) fn upsert_free_channel_cooldown(
    tx: &rusqlite::Transaction<'_>,
    until: &str,
) -> Result<()> {
    tx.execute(
        "INSERT INTO settings (key, value) VALUES (?1, ?2)
         ON CONFLICT(key) DO UPDATE SET value = CASE
             WHEN excluded.value > settings.value THEN excluded.value
             ELSE settings.value
         END",
        params![FREE_CHANNEL_COOLDOWN_SETTING, until],
    )?;
    Ok(())
}

pub(super) fn compute_cooldown_until(
    tx: &rusqlite::Transaction,
    id: &str,
    now_rfc: &str,
) -> Result<Option<String>> {
    let source = account_store::account_row_source(tx)?;
    let max: Option<String> = tx.query_row(
        &format!(
            "SELECT MAX(until) FROM (
            SELECT cooldown_generic_until AS until FROM {table} WHERE {id_col} = ?1
            UNION ALL
            SELECT cooldown_5h_until FROM {table} WHERE {id_col} = ?1
            UNION ALL
            SELECT cooldown_week_until FROM {table} WHERE {id_col} = ?1
            UNION ALL
            SELECT cooldown_month_until FROM {table} WHERE {id_col} = ?1
            UNION ALL
            SELECT cooldown_free_until FROM {table} WHERE {id_col} = ?1
        ) WHERE until IS NOT NULL AND until > ?2",
            table = source.table,
            id_col = source.id_col
        ),
        params![id, now_rfc],
        |row| row.get(0),
    )?;
    Ok(max)
}

pub(super) fn effective_usage(
    local_window_cost: f64,
    baseline: Option<(f64, f64)>,
    total_success_cost: f64,
    limit: f64,
) -> f64 {
    baseline.map_or(local_window_cost, |(percent, anchor)| {
        (limit * percent / 100.0 + (total_success_cost - anchor).max(0.0)).min(limit)
    })
}
