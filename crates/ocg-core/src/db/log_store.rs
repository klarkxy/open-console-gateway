//! Gateway and forward log persistence.

use super::*;

impl Database {
    pub fn log_gateway(&self, level: &str, category: &str, message: &str) -> Result<()> {
        self.log_gateway_diagnostic(level, category, message, None, None, None, None, None, None)
    }

    #[allow(clippy::too_many_arguments)]
    pub fn log_gateway_diagnostic(
        &self,
        level: &str,
        category: &str,
        message: &str,
        request_id: Option<&str>,
        attempt: Option<i64>,
        error_source: Option<&str>,
        error_stage: Option<&str>,
        duration_ms: Option<i64>,
        diagnostic_json: Option<&str>,
    ) -> Result<()> {
        let created_at = Utc::now().to_rfc3339();
        let level = crate::runtime_log::Level::parse(level)
            .ok_or_else(|| anyhow::anyhow!("invalid runtime log level"))?;
        if level < self.log_level {
            return Ok(());
        }
        let level = level.as_str();
        ocg_infra::sqlite_logs::insert_gateway_log(
            &self.conn,
            &GatewayLogInsertRow {
                level,
                category,
                message,
                created_at: &created_at,
                request_id,
                attempt,
                error_source,
                error_stage,
                duration_ms,
                diagnostic_json,
            },
        )?;
        Ok(())
    }

    /// Insert many forward_logs rows in one transaction (bulk seeding).
    /// Test-only helper: production writes go through `log_forward`.
    pub fn log_forward_batch(&self, logs: &[ForwardLog]) -> Result<()> {
        let tx = self.conn.unchecked_transaction()?;
        for log in logs {
            tx.execute(
                "INSERT INTO forward_logs
                 (timestamp, model, account_id, account_name, client_key_id, client_key_name,
                  route_account_id, provider_id, credential_account_id,
                  status, http_status, prompt_tokens, completion_tokens, cached_tokens,
                  cache_creation_tokens, cost, cost_state, raw_cost_usd, quota_debit,
                  effective_paid_cost_usd)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11,
                         0, 0, 0, 0, 0, 'legacy_estimate', ?12, ?13, ?14)",
                params![
                    log.timestamp.to_rfc3339(),
                    log.model,
                    log.account_id,
                    log.account_name,
                    log.client_key_id,
                    log.client_key_name,
                    log.route_account_id,
                    log.provider_id,
                    log.credential_account_id,
                    log.status,
                    log.http_status,
                    log.raw_cost_usd,
                    log.quota_debit,
                    log.effective_paid_cost_usd,
                ],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    /// Insert a forward_logs row. Returns the auto-assigned row id.
    pub fn log_forward(&self, log: &ForwardLog) -> Result<i64> {
        let diagnostic_json = log
            .diagnostic
            .as_ref()
            .map(serde_json::to_string)
            .transpose()?;
        let attribution = ForwardLogNativeAttribution::inferred_from_forward_log(log);
        let timestamp = log.timestamp.to_rfc3339();
        Ok(ocg_infra::sqlite_logs::insert_forward_log(
            &self.conn,
            &ForwardLogInsertRow {
                timestamp: &timestamp,
                model: &log.model,
                account_id: &log.account_id,
                account_name: &log.account_name,
                client_key_id: log.client_key_id.as_deref(),
                client_key_name: log.client_key_name.as_deref(),
                route_account_id: log.route_account_id.as_deref(),
                provider_id: log.provider_id.as_deref(),

                credential_account_id: log.credential_account_id.as_deref(),
                status: &log.status,
                http_status: log.http_status,
                route: &log.route,
                prompt_tokens: log.prompt_tokens,
                completion_tokens: log.completion_tokens,
                cached_tokens: log.cached_tokens,
                cache_creation_tokens: log.cache_creation_tokens,
                cost: log.cost.unwrap_or(0.0),
                raw_cost_usd: log.raw_cost_usd,
                quota_debit: log.quota_debit,
                effective_paid_cost_usd: log.effective_paid_cost_usd,
                pricing_revision_id: log.pricing_revision_id.as_deref(),
                quota_multiplier: log.quota_multiplier,
                local_adjustment_multiplier: log.local_adjustment_multiplier,
                service_tier: log.service_tier.as_deref(),
                cost_state: &log.cost_state,
                error_message: log.error_message.as_deref(),
                request_id: log.request_id.as_deref(),
                attempt: log.attempt,
                error_source: log.error_source.as_deref(),
                error_stage: log.error_stage.as_deref(),
                duration_ms: log.duration_ms,
                diagnostic_json: diagnostic_json.as_deref(),
                requested_model: attribution.requested_model.as_deref(),
                resolved_alias: attribution.resolved_alias.as_deref(),
                upstream_model: attribution.upstream_model.as_deref(),
                native_cost_value: attribution.native_cost_value,
                native_cost_unit: attribution.native_cost_unit.as_deref(),
                native_cost_currency: attribution.native_cost_currency.as_deref(),
            },
        )?)
    }

    /// Finalize a forward_logs row once the upstream response ends. `http_status` and
    /// `error_message` may be `None` to leave them at their initial value. `id` is the
    /// primary key returned from the original `log_forward` insert.
    pub fn update_forward_log(
        &self,
        id: i64,
        status: &str,
        http_status: Option<i32>,
        mut metrics: ForwardMetrics,
        error_message: Option<&str>,
        diagnostic: Option<&ForwardLogDiagnosticUpdate<'_>>,
    ) -> Result<()> {
        let binding = self
            .conn
            .query_row(
                "SELECT provider_id FROM forward_logs WHERE id = ?1",
                [id],
                |row| row.get::<_, Option<String>>(0),
            )
            .optional()?;
        if let Some(provider_id) = binding.as_ref() {
            metrics.scope_to_provider(provider_id.as_deref(), status.starts_with("success"));
        }
        let cost_state = match (metrics.cost_state, status) {
            ("not_applicable", "outcome_unknown") => "outcome_unknown",
            ("not_applicable", "success_no_usage") => "usage_missing",
            ("not_applicable", "success_unpriced") => "unpriced",
            (state, _) => state,
        };
        let stored_status = if status.starts_with("success") {
            match cost_state {
                "priced" | "free" => "success",
                "usage_missing" => "success_no_usage",
                _ => "success_unpriced",
            }
        } else {
            status
        };
        let stored_cost = if cost_state == "priced" {
            metrics.cost
        } else {
            0.0
        };
        // Stream inserts dual-write native USD from the preliminary row. Finalize
        // native_cost_* from the same cost/raw_cost_usd/cost_state tuple written
        // here so Go/Zen cannot keep a 0/NULL native snapshot after success.
        let (native_cost_value, native_cost_unit, native_cost_currency) =
            ForwardLogNativeAttribution::usd_fields_from_cost(
                metrics.raw_cost_usd,
                (cost_state == "priced").then_some(metrics.cost),
                cost_state,
            );
        ocg_infra::sqlite_logs::update_forward_log(
            &self.conn,
            &ForwardLogUpdateRow {
                id,
                status: stored_status,
                http_status,
                prompt_tokens: metrics.prompt_tokens,
                completion_tokens: metrics.completion_tokens,
                cached_tokens: metrics.cached_tokens,
                cache_creation_tokens: metrics.cache_creation_tokens,
                cost: stored_cost,
                raw_cost_usd: metrics.raw_cost_usd,
                quota_debit: metrics.quota_debit,
                effective_paid_cost_usd: metrics.effective_paid_cost_usd,
                pricing_revision_id: metrics.pricing_revision_id.as_deref(),
                quota_multiplier: metrics.quota_multiplier,
                local_adjustment_multiplier: metrics.local_adjustment_multiplier,
                service_tier: metrics.service_tier.as_deref(),
                cost_state,
                error_message,
                error_source: diagnostic.map(|diagnostic| diagnostic.error_source),
                error_stage: diagnostic.map(|diagnostic| diagnostic.error_stage),
                duration_ms: diagnostic.map(|diagnostic| diagnostic.duration_ms),
                diagnostic_json: diagnostic.map(|diagnostic| diagnostic.diagnostic_json),
                native_cost_value,
                native_cost_unit: native_cost_unit.as_deref(),
                native_cost_currency: native_cost_currency.as_deref(),
            },
        )?;
        Ok(())
    }

    pub fn list_gateway_logs(&self, limit: i64) -> Result<Vec<GatewayLog>> {
        self.query_gateway_logs(limit, None)
    }

    pub fn query_gateway_logs(
        &self,
        limit: i64,
        request_id: Option<&str>,
    ) -> Result<Vec<GatewayLog>> {
        self.query_gateway_logs_filtered(limit, request_id, None, None)
    }

    pub fn query_gateway_logs_filtered(
        &self,
        limit: i64,
        request_id: Option<&str>,
        level: Option<&str>,
        category: Option<&str>,
    ) -> Result<Vec<GatewayLog>> {
        let sql = "SELECT id, level, category, message, created_at, request_id, attempt,
                    error_source, error_stage, duration_ms, diagnostic_json
             FROM gateway_logs WHERE (?1 IS NULL OR request_id = ?1)
                AND (?2 IS NULL OR level = ?2) AND (?3 IS NULL OR category = ?3)
             ORDER BY id DESC LIMIT ?4";
        let mut stmt = self.conn.prepare(sql)?;
        let map = |row: &rusqlite::Row<'_>| {
            Ok(GatewayLog {
                id: row.get(0)?,
                level: row.get(1)?,
                category: row.get(2)?,
                message: row.get(3)?,
                created_at: parse_datetime(row.get::<_, String>(4)?),
                request_id: row.get(5)?,
                attempt: row.get(6)?,
                error_source: row.get(7)?,
                error_stage: row.get(8)?,
                duration_ms: row.get(9)?,
                diagnostic: row
                    .get::<_, Option<String>>(10)?
                    .and_then(|json| serde_json::from_str(&json).ok()),
            })
        };
        let rows = stmt.query_map(
            params![request_id, level, category, limit.clamp(1, 1000)],
            map,
        )?;
        rows.collect::<Result<Vec<_>, _>>().map_err(|e| e.into())
    }

    pub fn latest_gateway_error(&self) -> Result<Option<String>> {
        self.conn
            .query_row(
                "SELECT message FROM gateway_logs
                 WHERE lower(level) = 'error' AND category = 'gateway'
                 ORDER BY id DESC LIMIT 1",
                [],
                |row| row.get(0),
            )
            .optional()
            .map_err(|e| e.into())
    }

    pub fn latest_error_summary(&self) -> Result<Option<String>> {
        self.conn
            .query_row(
                "SELECT message FROM gateway_logs
                 WHERE lower(level) IN ('error', 'warn')
                 ORDER BY id DESC LIMIT 1",
                [],
                |row| row.get(0),
            )
            .optional()
            .map_err(|e| e.into())
    }

    pub fn list_forward_logs(&self, limit: i64) -> Result<Vec<ForwardLog>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, timestamp, model, account_id, account_name, status, http_status, route,
                    prompt_tokens, completion_tokens, cached_tokens, cache_creation_tokens, cost,
                    pricing_revision_id, quota_multiplier, local_adjustment_multiplier,
                    service_tier, cost_state, error_message, request_id, attempt,
                    error_source, error_stage, duration_ms, diagnostic_json,
                    client_key_id, client_key_name, route_account_id, provider_id,
                    credential_account_id, raw_cost_usd, quota_debit,
                    effective_paid_cost_usd
             FROM forward_logs ORDER BY id DESC LIMIT ?1",
        )?;
        let rows = stmt.query_map([limit], forward_log_from_row)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(|e| e.into())
    }

    pub fn query_forward_logs(
        &self,
        options: ForwardLogQueryOptions<'_>,
    ) -> Result<ForwardLogPage> {
        let limit = options.limit.clamp(1, 200);
        let offset = options.offset.max(0);
        let (filter, filter_params) = forward_log_filter(&options);
        let order_clause = forward_log_order(options.sort_by, options.sort_order);
        let summary_sql = format!(
            "SELECT COUNT(*),
                    COALESCE(SUM(prompt_tokens), 0),
                    COALESCE(SUM(completion_tokens), 0),
                    COALESCE(SUM(cached_tokens), 0),
                    COALESCE(SUM(cost), 0.0)
             FROM forward_logs{filter}"
        );
        let summary = self.conn.query_row(
            &summary_sql,
            params_from_iter(filter_params.iter()),
            |row| {
                Ok(ForwardLogSummary {
                    total_requests: row.get(0)?,
                    prompt_tokens: row.get(1)?,
                    completion_tokens: row.get(2)?,
                    cached_tokens: row.get(3)?,
                    cost: row.get(4)?,
                })
            },
        )?;

        let items_sql = format!(
            "SELECT id, timestamp, model, account_id, account_name, status, http_status, route,
                    prompt_tokens, completion_tokens, cached_tokens, cache_creation_tokens, cost,
                    pricing_revision_id, quota_multiplier, local_adjustment_multiplier,
                    service_tier, cost_state, error_message, request_id, attempt,
                    error_source, error_stage, duration_ms, diagnostic_json,
                    client_key_id, client_key_name, route_account_id, provider_id,
                    credential_account_id, raw_cost_usd, quota_debit,
                    effective_paid_cost_usd
             FROM forward_logs{filter}
             {order_clause}
             LIMIT ? OFFSET ?"
        );
        let mut item_params = filter_params;
        item_params.push(Value::Integer(limit));
        item_params.push(Value::Integer(offset));
        let mut stmt = self.conn.prepare(&items_sql)?;
        let items = stmt
            .query_map(params_from_iter(item_params.iter()), forward_log_from_row)?
            .collect::<Result<Vec<_>, _>>()?;

        Ok(ForwardLogPage { items, summary })
    }

    pub fn list_forward_log_models(&self) -> Result<Vec<String>> {
        let mut stmt = self
            .conn
            .prepare("SELECT DISTINCT model FROM forward_logs ORDER BY model ASC")?;
        let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
        rows.collect::<Result<Vec<_>, _>>().map_err(|e| e.into())
    }

    /// Distinct client keys that appear in forward logs, mirroring
    /// [`Database::list_forward_log_models`]. Includes disabled, soft-deleted,
    /// and dangling ids so historical logs stay filterable. Each id resolves
    /// its most recent non-null name snapshot, so renamed keys appear under
    /// their current name.
    pub fn list_forward_log_keys(&self) -> Result<Vec<ForwardLogClientKey>> {
        let mut stmt = self.conn.prepare(
            "SELECT client_key_id, COALESCE((
                    SELECT f2.client_key_name FROM forward_logs f2
                    WHERE f2.client_key_id = f.client_key_id
                      AND f2.client_key_name IS NOT NULL
                    ORDER BY f2.rowid DESC LIMIT 1
                ), '')
             FROM forward_logs f
             WHERE client_key_id IS NOT NULL
             GROUP BY client_key_id
             ORDER BY 2 ASC, client_key_id ASC",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok(ForwardLogClientKey {
                id: row.get(0)?,
                name: row.get::<_, String>(1)?,
            })
        })?;
        let mut keys = rows.collect::<Result<Vec<_>, _>>()?;
        // Empty snapshot names (logs written before the key existed in
        // config) still deserve a stable display label; within that
        // fallback the primary key's fixed display name takes precedence
        // over its raw id.
        for key in &mut keys {
            if key.name.is_empty() {
                key.name = if key.id == PRIMARY_KEY_ID {
                    PRIMARY_KEY_NAME.to_string()
                } else {
                    key.id.clone()
                };
            }
        }
        Ok(keys)
    }

    // ----- forward log client-key backfill -----

    /// Backfills `client_key_id`/`client_key_name` on historical rows in
    /// bounded rowid chunks. Each call performs at most one short transaction
    /// (range update + watermark persist) so callers can release the
    /// connection between chunks and keep the gateway responsive.
    /// Returns `true` while more chunks remain.
    pub fn backfill_forward_logs_client_key_step(
        &self,
        key_id: &str,
        key_name: &str,
        chunk_rows: i64,
    ) -> Result<bool> {
        let chunk_rows = chunk_rows.max(1);
        let Some(watermark) = self.backfill_watermark()? else {
            // Completion marker present. A downgrade window (an older binary
            // writing NULL rows after this marker was recorded) must not
            // leave those rows permanently unattributed: probe the index
            // once and restart the scan when any NULL row appears.
            if !self.forward_logs_have_unattributed_rows()? {
                return Ok(false);
            }
            self.set_setting(BACKFILL_SETTING_KEY, "0")?;
            return Ok(true);
        };
        let max_rowid: i64 = self.conn.query_row(
            "SELECT COALESCE(MAX(rowid), 0) FROM forward_logs",
            [],
            |row| row.get(0),
        )?;
        let start = watermark + 1;
        if start <= max_rowid {
            let end = (start + chunk_rows - 1).min(max_rowid);
            let tx = self.conn.unchecked_transaction()?;
            tx.execute(
                "UPDATE forward_logs
                 SET client_key_id = ?1, client_key_name = ?2
                 WHERE client_key_id IS NULL AND rowid BETWEEN ?3 AND ?4",
                params![key_id, key_name, start, end],
            )?;
            tx.execute(
                "INSERT OR REPLACE INTO settings (key, value) VALUES (?1, ?2)",
                params![BACKFILL_SETTING_KEY, end.to_string()],
            )?;
            tx.commit()?;
            if end < max_rowid {
                return Ok(true);
            }
        }
        // The whole table is covered. New writes always carry a key id, so
        // the NULL set can only shrink; record completion once nothing is
        // left, otherwise late NULL rows (an older binary still writing)
        // force a restart from the beginning.
        if !self.forward_logs_have_unattributed_rows()? {
            self.set_setting(BACKFILL_SETTING_KEY, BACKFILL_DONE)?;
            return Ok(false);
        }
        self.set_setting(BACKFILL_SETTING_KEY, "0")?;
        Ok(true)
    }

    /// Whether any forward log row still lacks a client key id; served by
    /// `idx_forward_logs_client_key` in one index probe.
    fn forward_logs_have_unattributed_rows(&self) -> Result<bool> {
        let found: i64 = self.conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM forward_logs WHERE client_key_id IS NULL LIMIT 1)",
            [],
            |row| row.get(0),
        )?;
        Ok(found == 1)
    }

    /// `None` when the backfill already completed; otherwise the max rowid
    /// whose range has been attributed.
    fn backfill_watermark(&self) -> Result<Option<i64>> {
        Ok(match self.get_setting(BACKFILL_SETTING_KEY)? {
            None => Some(0),
            Some(value) if value == BACKFILL_DONE => None,
            Some(value) => Some(value.parse::<i64>().unwrap_or(0)),
        })
    }

    /// Test/inspection helper: the raw persisted backfill marker.
    pub fn forward_log_backfill_marker(&self) -> Result<Option<String>> {
        self.get_setting(BACKFILL_SETTING_KEY)
    }
}

fn forward_log_filter(options: &ForwardLogQueryOptions<'_>) -> (String, Vec<Value>) {
    let mut filter = String::new();
    let mut params = Vec::new();
    // (clause, bound text values); clause order must match parameter order.
    // The key filter goes last because the unattributed sentinel expands to
    // a literal `IS NULL` clause with no parameter.
    let mut clauses: Vec<(String, Vec<&str>)> = Vec::new();
    // Missing prices are the common success case. The logs page presents
    // `success_unpriced` as success, so that filter has to include both.
    if let Some(status) = options.status {
        if status == "success" {
            clauses.push((
                "status IN ('success', 'success_unpriced')".to_string(),
                Vec::new(),
            ));
        } else {
            clauses.push(("status = ?".to_string(), vec![status]));
        }
    }
    clauses.extend(
        [
            ("account_id = ?", options.account_id),
            ("provider_id = ?", options.provider_id),
            ("route_account_id = ?", options.route_account_id),
            ("credential_account_id = ?", options.credential_account_id),
        ]
        .into_iter()
        .filter_map(|(clause, value)| value.map(|value| (clause.to_string(), vec![value]))),
    );
    // Exact-match any stored identity so alias/upstream/legacy rows stay
    // filterable. Bind the same value once per column; OR stays inside this
    // predicate so other filters still AND and a row never duplicates.
    if let Some(model) = options.model.filter(|value| !value.is_empty()) {
        clauses.push((
            "(model = ? OR requested_model = ? OR resolved_alias = ? OR upstream_model = ?)"
                .to_string(),
            vec![model, model, model, model],
        ));
    }
    for (clause, value) in [
        ("request_id = ?", options.request_id),
        ("julianday(timestamp) >= julianday(?)", options.start_time),
        ("julianday(timestamp) <= julianday(?)", options.end_time),
    ] {
        if let Some(value) = value {
            clauses.push((clause.to_string(), vec![value]));
        }
    }
    match options.key_id {
        Some(UNATTRIBUTED_KEY_FILTER) => {
            clauses.push(("client_key_id IS NULL".to_string(), Vec::new()));
        }
        Some(id) => clauses.push(("client_key_id = ?".to_string(), vec![id])),
        None => {}
    }
    for (clause, values) in clauses {
        append_filter_clause(&mut filter, &clause);
        for value in values {
            params.push(Value::Text(value.to_owned()));
        }
    }
    (filter, params)
}

fn append_filter_clause(filter: &mut String, clause: &str) {
    filter.push_str(if filter.is_empty() {
        " WHERE "
    } else {
        " AND "
    });
    filter.push_str(clause);
}

fn forward_log_order(sort_by: Option<&str>, sort_order: Option<&str>) -> String {
    let column = match sort_by {
        Some("timestamp") => "timestamp",
        Some("attempt") => "attempt",
        Some("prompt_tokens") => "prompt_tokens",
        Some("completion_tokens") => "completion_tokens",
        Some("cached_tokens") => "cached_tokens",
        Some("cost") => "cost",
        Some("model") => "model",
        Some("status") => "status",
        _ => "id",
    };
    let direction = if sort_order == Some("asc") {
        "ASC"
    } else {
        "DESC"
    };
    format!("ORDER BY {column} {direction}, id DESC")
}
