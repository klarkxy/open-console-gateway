import type { ForwardLog } from "../api/dashboard.ts";
import { planLabel } from "../domain/plans.ts";
import type { ProviderCatalogEntry } from "../api/providers.ts";

/**
 * Display helpers for request (forward) logs under the v2 alias contract:
 * clients see the requested alias as the primary model name; resolved and
 * upstream model IDs plus the wire protocol stay in the technical detail
 * view. Every helper tolerates rows that predate alias-aware logging.
 */

/** The client-facing name: the resolved alias, falling back to the requested model, then `model`. */
export function forwardLogAlias(
  row: Pick<ForwardLog, "resolved_alias" | "requested_model" | "model">,
): string {
  return row.resolved_alias?.trim()
    || row.requested_model?.trim()
    || row.model;
}

/**
 * Total tokens for a request-log row. `prompt_tokens` already includes the
 * cached-read and cache-write portions (the Messages path adds them back
 * explicitly; Chat/Responses rely on the upstream convention where
 * `prompt_tokens` includes cached and cache-write is always 0), so adding
 * them again would double count. The total is input + output only.
 */
export function forwardLogTotalTokens(
  row: Pick<ForwardLog, "prompt_tokens" | "completion_tokens">,
): number {
  return row.prompt_tokens + row.completion_tokens;
}

export function forwardLogLatencyMs(
  row: Pick<ForwardLog, "duration_ms">,
): number | null {
  return typeof row.duration_ms === "number" && Number.isFinite(row.duration_ms)
    ? row.duration_ms
    : null;
}

/**
 * Status shown on the request log. A completed request without a price is
 * ordinary success; the stored `success_unpriced` value stays on the row.
 */
export function forwardLogPresentedStatus(status: string): string {
  return status === "success_unpriced" ? "success" : status;
}

/** Plan label for the row's provider attribution, or null when unattributed. */
export function forwardLogPlanLabel(
  row: Pick<ForwardLog, "provider_id">,
  catalog?: readonly ProviderCatalogEntry[] | null,
): string | null {
  if (!row.provider_id) return null;
  return planLabel({ provider_id: row.provider_id }, catalog);
}

/** Technical detail only: the alias that resolved for this request. */
export function forwardLogResolvedAlias(
  row: Pick<ForwardLog, "resolved_alias">,
): string | null {
  return row.resolved_alias?.trim() || null;
}

/** Technical detail only: the model ID the client originally requested. */
export function forwardLogRequestedModel(
  row: Pick<ForwardLog, "requested_model">,
): string | null {
  return row.requested_model?.trim() || null;
}

/** Technical detail only: the model ID actually sent upstream. */
export function forwardLogUpstreamModel(
  row: Pick<ForwardLog, "upstream_model">,
): string | null {
  return row.upstream_model?.trim() || null;
}

/**
 * Technical detail only: the wire protocol. There is no explicit `protocol`
 * field; we fall back to the diagnostic client→upstream path when present.
 */
export function forwardLogProtocol(
  row: Pick<ForwardLog, "diagnostic">,
): string | null {
  const diagnostic = row.diagnostic;
  if (diagnostic?.client_format) {
    return diagnostic.upstream_format
      ? `${diagnostic.client_format} → ${diagnostic.upstream_format}`
      : diagnostic.client_format;
  }
  return null;
}
