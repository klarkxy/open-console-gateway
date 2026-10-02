import type { ForwardLog as V3ForwardLog } from "./generated/dashboard-v3.ts";
import { presentForwardLog, type ForwardLog } from "./dashboard-presenters.ts";
import { requestV4 } from "./dashboard-v3.ts";
import type {
  OperationLogPage,
  OperationLogQuery,
  RequestLogPage,
  RequestLogQuery,
} from "./log-ledger-types.ts";

/**
 * Read-only V4 log client. Wire bodies stay camelCase.
 * Attempt rows are the existing V3 ForwardLog DTO, presented for the current detail view.
 */

function setText(params: URLSearchParams, key: string, value: string | number | null | undefined): void {
  if (value === null || value === undefined) return;
  const text = String(value).trim();
  if (!text) return;
  params.set(key, text);
}

export function operationLogSearch(query: OperationLogQuery = {}): string {
  const params = new URLSearchParams();
  setText(params, "action", query.action);
  setText(params, "source", query.source);
  setText(params, "outcome", query.outcome);
  setText(params, "subjectType", query.subjectType);
  setText(params, "subjectId", query.subjectId);
  setText(params, "startTime", query.startTime);
  setText(params, "endTime", query.endTime);
  params.set("limit", String(query.limit ?? 20));
  params.set("offset", String(query.offset ?? 0));
  return params.toString();
}

export function requestLogSearch(query: RequestLogQuery = {}): string {
  const params = new URLSearchParams();
  setText(params, "status", query.status);
  setText(params, "providerId", query.providerId);
  setText(params, "accountId", query.accountId);
  setText(params, "routeAccountId", query.routeAccountId);
  setText(params, "credentialAccountId", query.credentialAccountId);
  setText(params, "keyId", query.keyId);
  setText(params, "model", query.model);
  setText(params, "startTime", query.startTime);
  setText(params, "endTime", query.endTime);
  // Nonblank request identities keep their exact bytes, including edge spaces.
  if (query.requestId?.trim()) params.set("requestId", query.requestId);
  params.set("limit", String(query.limit ?? 20));
  params.set("offset", String(query.offset ?? 0));
  return params.toString();
}

export function requestAttemptsPath(requestKey: string): string {
  return `/logs/requests/${encodeURIComponent(requestKey)}/attempts`;
}

export function getOperationLogs(query: OperationLogQuery = {}, signal?: AbortSignal): Promise<OperationLogPage> {
  return requestV4<OperationLogPage>(`/logs/operations?${operationLogSearch(query)}`, { signal });
}

export function getRequestLogs(query: RequestLogQuery = {}, signal?: AbortSignal): Promise<RequestLogPage> {
  return requestV4<RequestLogPage>(`/logs/requests?${requestLogSearch(query)}`, { signal });
}

export async function getRequestLogAttempts(
  requestKey: string,
  signal?: AbortSignal,
): Promise<{ items: ForwardLog[] }> {
  const page = await requestV4<{ items?: V3ForwardLog[] }>(requestAttemptsPath(requestKey), { signal });
  return { items: (page.items ?? []).map(presentForwardLog) };
}
