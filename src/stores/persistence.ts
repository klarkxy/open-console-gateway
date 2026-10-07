/** Cleanup for the retired browser business-cache namespace. UI preferences remain. */
const PREFIX = "ocg.snapshot.v1:";

function storage(): Storage | null {
  try {
    return globalThis.localStorage ?? null;
  } catch {
    return null;
  }
}

/** Discard one rebuildable legacy projection without reading its contents. */
export function dropSnapshot(key: string): void {
  try {
    storage()?.removeItem(PREFIX + key);
  } catch {
    // Storage may be unavailable; cleanup must not block store creation or logout.
  }
}

/** Startup / session cleanup, restricted to the exact retired snapshot prefix. */
export function dropAllSnapshots(): void {
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
    // Ignore storage restrictions; all business stores are memory-only.
  }
}
