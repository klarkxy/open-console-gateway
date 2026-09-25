/**
 * Time-based revalidation gate for keep-alive views. The first call always
 * runs; later calls within the TTL are suppressed so returning to a view does
 * not restorm the local server with identical reads. Views must reset the
 * gate on session drop so a fresh login never inherits the previous window.
 */
export interface RevalidateGate {
  shouldRun(now?: number): boolean;
  record(now?: number): void;
  reset(): void;
}

export function createRevalidateGate(ttlMs: number): RevalidateGate {
  let last = 0;
  return {
    shouldRun(now = Date.now()): boolean {
      return last === 0 || now - last >= ttlMs;
    },
    record(now = Date.now()): void {
      last = now;
    },
    reset(): void {
      last = 0;
    },
  };
}
