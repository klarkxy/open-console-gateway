/** Freshness is a read optimization only; it never validates CAS or authorization. */
export interface ReadOptions {
  maxAgeMs?: number;
}

export const PAGE_READ_MAX_AGE_MS = 15_000;

/** A response may discover the current process; an abandoned origin must not commit. */
export function readProcessIsCurrent(
  incoming: number | null | undefined,
  current: number | null,
  origin: number | null,
): boolean {
  return incoming == null ? current === origin : incoming === current;
}

/** Full reads cannot regress behind another confirmed operation in the same process. */
export function readSnapshotIsCurrent(
  incomingProcess: number | null | undefined,
  incomingRevision: number | null | undefined,
  current: { processGeneration: number | null; revision: number | null },
  origin: number | null,
): boolean {
  return readProcessIsCurrent(incomingProcess, current.processGeneration, origin)
    && (incomingRevision == null || current.revision === null || incomingRevision >= current.revision);
}

/** Store-local successful reads and pending flights. Nothing is persisted. */
export function createReadLifecycle(now: () => number = Date.now) {
  const successfulAt = new Map<string, number>();
  const flights = new Map<string, Promise<unknown>>();

  function invalidate(key?: string): void {
    if (key === undefined) {
      successfulAt.clear();
      flights.clear();
    } else {
      successfulAt.delete(key);
      flights.delete(key);
    }
  }

  function markSuccessful(key: string): void {
    successfulAt.set(key, now());
  }

  function run<T>(key: string, options: ReadOptions | undefined, cached: () => T, read: () => Promise<T>): Promise<T> {
    const pending = flights.get(key);
    if (pending) return pending as Promise<T>;
    const at = successfulAt.get(key);
    const age = at === undefined ? Infinity : now() - at;
    if (at !== undefined && age >= 0 && age < (options?.maxAgeMs ?? 0)) {
      return Promise.resolve(cached());
    }
    // A failed revalidation must not retain an earlier success as fresh.
    successfulAt.delete(key);
    const promise = read().finally(() => {
      if (flights.get(key) === promise) flights.delete(key);
    });
    flights.set(key, promise);
    return promise;
  }

  return { run, markSuccessful, invalidate };
}
