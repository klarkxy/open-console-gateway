import type { BillingStatus } from "../api/billing.ts";

export const ACCOUNT_AUTO_REFRESH_MS = 5 * 60_000;

export function timestampMs(value: string | null | undefined): number {
  const parsed = value ? Date.parse(value) : NaN;
  return Number.isFinite(parsed) ? parsed : 0;
}

/** Local estimates' updatedAt is not evidence of a fresh upstream read. */
export function billingObservedAt(status: BillingStatus | null): number {
  return Math.max(0,
    timestampMs(status?.usage?.syncState?.lastSuccessAt),
    ...(status?.usage?.quotaWindows ?? []).map(row => timestampMs(row.observedAt)),
    ...(status?.usage?.creditBalances ?? []).map(row => timestampMs(row.observedAt)),
    ...(status?.cash?.balances ?? []).map(row => timestampMs(row.observedAt)),
  );
}

export interface AccountRefreshTarget {
  id: string;
  binding: string;
  observedAt: number;
  nextAllowedAt: number;
  busy: boolean;
  refresh: (isCurrent: () => boolean) => Promise<void>;
}

/** One serial, lazy pass over all eligible accounts, independent of filters. */
export function createAccountsAutoRefresh(options: {
  allowed: () => boolean;
  targets: () => AccountRefreshTarget[];
  now?: () => number;
  afterRefresh?: () => Promise<void>;
}) {
  const now = options.now ?? Date.now;
  const attempts = new Map<string, { binding: string; at: number }>();
  let generation = 0;
  let running = false;

  async function run(): Promise<void> {
    if (running || !options.allowed()) return;
    running = true;
    const captured = generation;
    const current = () => captured === generation && options.allowed();
    try {
      const ids = options.targets().map(target => target.id);
      const retained = new Set(ids);
      for (const id of attempts.keys()) if (!retained.has(id)) attempts.delete(id);
      for (const id of ids) {
        if (!current()) break;
        // Previous I/O may have deleted, rebound, disabled, or refreshed this row.
        const target = options.targets().find(row => row.id === id);
        if (!target || target.busy) continue;
        const attempt = attempts.get(id);
        const lastAttempt = attempt?.binding === target.binding ? attempt.at : 0;
        const at = now();
        if (target.nextAllowedAt > at) continue;
        if (Math.max(target.observedAt, lastAttempt) + ACCOUNT_AUTO_REFRESH_MS > at) continue;
        attempts.set(id, { binding: target.binding, at });
        const isCurrent = () => current()
          && options.targets().some(row => row.id === id && row.binding === target.binding);
        try {
          await target.refresh(isCurrent);
        } catch {
          // Keep last-good evidence; one failed provider must not stop the pass.
        } finally {
          if (captured === generation) {
            attempts.set(id, { binding: target.binding, at: now() });
            await options.afterRefresh?.().catch(() => undefined);
          }
        }
      }
    } finally {
      running = false;
    }
  }

  return {
    run,
    pause() { generation++; },
    reset() { generation++; attempts.clear(); },
  };
}
