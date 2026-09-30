/**
 * localStorage persistence for secret-free read-model snapshots.
 *
 * Only projections whose API contract is secret-free may be persisted: the
 * generated V3/V4 DTOs guarantee write-only credential fields always come
 * back as empty strings. Never persist the connection store (it holds the
 * live primary gateway Key), session/auth state, settings (proxy secrets),
 * logs, or transient OAuth/draft/overlay state.
 *
 * A hydrated snapshot is stale by definition. That is safe here because:
 * - views revalidate on mount through their freshness gates and render from
 *   the stores while revalidation runs (stale-while-revalidate);
 * - every successful load commits over the hydrated value wholesale;
 * - CAS revisions and process generations tolerate staleness — a mutation
 *   attempted against a stale expectation is rejected with a conflict and
 *   the existing recovery path reloads.
 *
 * Keys carry a schema version; a mismatch discards the entry. Writes are
 * debounced per key so a burst of commits stringifies once, and pending
 * writes flush on pagehide. All storage access is try/catch: persistence is
 * a hint, never a failure path (quota errors and locked-down browsers just
 * skip).
 */

const PREFIX = "ocg.snapshot.v1:";
const WRITE_DEBOUNCE_MS = 500;

interface Envelope {
  v: number;
  data: unknown;
}

interface PendingWrite {
  timer: ReturnType<typeof setTimeout>;
  data: unknown;
}

const pendingWrites = new Map<string, PendingWrite>();

function storage(): Storage | null {
  try {
    return globalThis.localStorage ?? null;
  } catch {
    return null;
  }
}

/** Read a persisted snapshot, or null when absent/corrupt/wrong version. */
export function readSnapshot<T>(key: string, validate?: (data: unknown) => T | null): T | null {
  const store = storage();
  if (!store) return null;
  try {
    const raw = store.getItem(PREFIX + key);
    if (!raw) return null;
    const envelope = JSON.parse(raw) as Partial<Envelope> | null;
    if (envelope?.v !== 1 || envelope.data === undefined || envelope.data === null) return null;
    return validate ? validate(envelope.data) : (envelope.data as T);
  } catch {
    return null;
  }
}

function writeNow(key: string, data: unknown): void {
  const store = storage();
  if (!store) return;
  try {
    store.setItem(PREFIX + key, JSON.stringify({ v: 1, data } satisfies Envelope));
  } catch {
    // Quota or serialization failure: the in-memory state is unaffected.
  }
}

/** Schedule a debounced write; rapid commits coalesce into one stringify. */
export function writeSnapshot(key: string, data: unknown): void {
  const pending = pendingWrites.get(key);
  if (pending) clearTimeout(pending.timer);
  const timer = setTimeout(() => {
    pendingWrites.delete(key);
    writeNow(key, data);
  }, WRITE_DEBOUNCE_MS);
  pendingWrites.set(key, { timer, data });
}

/** Drop one snapshot and cancel its pending write. */
export function dropSnapshot(key: string): void {
  const pending = pendingWrites.get(key);
  if (pending) {
    clearTimeout(pending.timer);
    pendingWrites.delete(key);
  }
  try {
    storage()?.removeItem(PREFIX + key);
  } catch {
    // Ignore: nothing persisted beats a thrown teardown.
  }
}

/** Logout / session drop wipes every persisted snapshot. */
export function dropAllSnapshots(): void {
  for (const pending of pendingWrites.values()) clearTimeout(pending.timer);
  pendingWrites.clear();
  const store = storage();
  if (!store) return;
  try {
    const doomed: string[] = [];
    for (let index = 0; index < store.length; index += 1) {
      const key = store.key(index);
      if (key?.startsWith(PREFIX)) doomed.push(key);
    }
    for (const key of doomed) store.removeItem(key);
  } catch {
    // Ignore.
  }
}

/** pagehide: debounced writes would otherwise be lost with the page. */
export function flushSnapshots(): void {
  const pending = [...pendingWrites.entries()];
  pendingWrites.clear();
  for (const [key, write] of pending) {
    clearTimeout(write.timer);
    writeNow(key, write.data);
  }
}
