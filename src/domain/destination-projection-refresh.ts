import type { Destination, DestinationCredential } from "../api/destinations.ts";
import type { MessageKey } from "../i18n/index.ts";
import { buildDestinationGroups, type DestinationGroup } from "./destination-groups.ts";

export type DestinationProjectionRefreshCode =
  | "refresh_failed"
  | "created_refresh_failed"
  | "deleted_refresh_failed";

export const DESTINATION_PROJECTION_REFRESH_KEYS = {
  refresh_failed: "目的地投影刷新失败：{error}",
  created_refresh_failed: "已创建，但列表刷新失败：{error}",
  deleted_refresh_failed: "已删除，但列表刷新失败：{error}",
} as const satisfies Record<DestinationProjectionRefreshCode, MessageKey>;

export type DestinationLoadCode = "load_failed";

export const DESTINATION_LOAD_KEYS = {
  load_failed: "加载目的地投影失败：{error}",
} as const satisfies Record<DestinationLoadCode, MessageKey>;

/** First ordinary dest load failure. Revalidation keeps a successful snapshot. */
export function destinationFirstLoadFailed(
  loaded: boolean,
  error: string,
  refusalCount: number,
): boolean {
  return !loaded && error !== "" && refusalCount === 0;
}

export type DestinationProjectionRefreshResult =
  | { ok: true }
  | { ok: false; code: DestinationProjectionRefreshCode; error: unknown };

/**
 * Reload the destination / credential snapshot after an account mutation.
 * Failures are returned, never swallowed, so the view can keep the mutation
 * result and show a distinct refresh error.
 */
export async function refreshDestinationProjection(
  load: () => Promise<void>,
): Promise<DestinationProjectionRefreshResult> {
  try {
    await load();
    return { ok: true };
  } catch (error) {
    return { ok: false, code: "refresh_failed", error };
  }
}

export function groupsContainLegacyAccount(
  groups: readonly DestinationGroup[],
  accountId: string,
): boolean {
  return groups.some((group) => (
    group.credentials.some((credential) => credential.legacy_account_id === accountId)
  ));
}

/**
 * After create/delete, cards follow the dest snapshot — not the V3 account
 * just committed. A failed refresh leaves the previous credential set.
 */
export function groupsFromDestinationSnapshot(
  destinations: readonly Destination[],
  credentials: readonly DestinationCredential[],
): DestinationGroup[] {
  return buildDestinationGroups(destinations, credentials);
}
