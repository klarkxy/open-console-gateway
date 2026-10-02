import type { MessageKey } from "../i18n/messages/en-US.ts";
import type { PlatformAccount } from "../api/platform-accounts.ts";
import { watch } from "vue";
import type { AccountRefreshState } from "./account-refresh-scheduler.ts";

export { createAccountRefreshScheduler as createAccountRefreshQueue } from "./account-refresh-scheduler.ts";
export type { AccountRefreshState, AccountRefreshJobOptions } from "./account-refresh-scheduler.ts";
export const ACCOUNT_REFRESH_STATE_KEYS: Record<AccountRefreshState, MessageKey> = {
  queued: "排队中",
  running: "刷新中",
};

/** Observation revisions advance on refresh; only a different target invalidates waiting work. */
export function platformRefreshBinding(parent: PlatformAccount | undefined): string | null {
  return parent ? JSON.stringify([parent.id, parent.kind, parent.baseUrl]) : null;
}

/** Wait on reactive write locks, without polling or discarding the queued action. */
export function waitForAccountRefreshIdle(blocked: () => boolean, current: () => boolean): Promise<boolean> {
  if (!current() || !blocked()) return Promise.resolve(current());
  return new Promise(resolve => {
    const stop = watch(() => [blocked(), current()] as const, ([busy, valid]) => {
      if (!busy || !valid) {
        stop();
        resolve(valid);
      }
    }, { flush: "sync" });
  });
}
