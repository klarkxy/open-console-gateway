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

export interface AccountRefreshTargets {
  ids(): readonly string[];
  current(id: string): AccountRefreshTarget | undefined;
}

function normalizeTargets(
  source: AccountRefreshTarget[] | AccountRefreshTargets,
): AccountRefreshTargets {
  if (!Array.isArray(source)) return source;
  return {
    ids: () => source.map((target) => target.id),
    current: (id) => source.find((target) => target.id === id),
  };
}

/** Lazy, bounded refreshes; each account publishes independently of its peers. */
export function createAccountsAutoRefresh(options: {
  allowed: () => boolean;
  targets: () => AccountRefreshTarget[] | AccountRefreshTargets;
  now?: () => number;
  afterRefresh?: () => Promise<void>;
  concurrency?: number;
}) {
  const now = options.now ?? Date.now;
  // Callers that mix control-plane writes keep the serial default; the
  // Accounts quota scheduler explicitly opts into its bounded shared pool.
  const concurrency = options.concurrency ?? 1;
  if (!Number.isInteger(concurrency) || concurrency < 1) {
    throw new RangeError("refresh concurrency must be a positive integer");
  }
  const attempts = new Map<string, { binding: string; at: number }>();
  let generation = 0;
  let running: symbol | undefined;

  async function run(): Promise<void> {
    if (running || !options.allowed()) return;
    const operation = Symbol();
    running = operation;
    const captured = generation;
    const current = () => captured === generation && options.allowed();
    let attempted = false;
    try {
      const ids = [...new Set(normalizeTargets(options.targets()).ids())];
      const retained = new Set(ids);
      for (const id of attempts.keys()) if (!retained.has(id)) attempts.delete(id);
      let cursor = 0;
      async function worker(): Promise<void> {
        while (cursor < ids.length && current()) {
          const id = ids[cursor++]!;
          // Resolve immediately before starting, not when the pass was queued.
          const target = normalizeTargets(options.targets()).current(id);
          if (!target || target.busy) continue;
          const attempt = attempts.get(id);
          const lastAttempt = attempt?.binding === target.binding ? attempt.at : 0;
          const at = now();
          if (target.nextAllowedAt > at) continue;
          if (Math.max(target.observedAt, lastAttempt) + ACCOUNT_AUTO_REFRESH_MS > at) continue;
          const binding = target.binding;
          attempts.set(id, { binding, at });
          attempted = true;
          const isCurrent = () => current()
            && normalizeTargets(options.targets()).current(id)?.binding === binding;
          try {
            await target.refresh(isCurrent);
          } catch {
            // Keep last-good evidence; one failed provider must not stop peers.
          } finally {
            if (captured === generation) attempts.set(id, { binding, at: now() });
          }
        }
      }
      await Promise.all(Array.from({ length: Math.min(concurrency, ids.length) }, () => worker()));
      // Reconcile shared projections once per pass, outside every account's
      // refresh slot. A slow destination read must not hold up the next quota.
      if (attempted && current()) await options.afterRefresh?.().catch(() => undefined);
    } finally {
      if (running === operation) running = undefined;
    }
  }

  return {
    run,
    pause() { generation++; running = undefined; },
    reset() { generation++; running = undefined; attempts.clear(); },
  };
}
