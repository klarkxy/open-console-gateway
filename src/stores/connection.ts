import { computed, ref } from "vue";
import { defineStore } from "pinia";
import { dashboardApi, isRevisionConflict } from "../api/dashboard.ts";
import type { ConnectionInfo, ConnectionSubKey } from "../api/dashboard.ts";
import { useControlPlaneStore } from "./controlPlane.ts";

/**
 * Post-ack plaintext read-back outcome. Never rejects:
 * - `loaded`: the canonical connection committed.
 * - `unavailable`: the write is committed but the read-back failed; the
 *   caller shows a refresh warning instead of retrying the write.
 * - `cancelled`: the session ended or a newer request superseded this one.
 */
export type KeyRevalidation = "loaded" | "unavailable" | "cancelled";

/**
 * Result of an acknowledged Key write. The write committed server-side at
 * its receipt, so callers must never retry it and must never report it as a
 * generic failure. `value` carries plaintext only when the operation already
 * read it; `revalidation` settles the background read-back without rejecting.
 */
export interface CommittedKeyWrite<T = void> {
  committed: true;
  value: T | undefined;
  revalidation: Promise<KeyRevalidation>;
}

/**
 * Connection center: the plaintext primary/sub Key values plus the URL fields
 * shown on the Dashboard and Access Keys pages.
 *
 * Secrets are memory-only: nothing here is persisted, and `clearSecrets()`
 * wipes the payload. The session store calls `clearSecrets()` on logout and
 * whenever a 401 invalidates the session.
 *
 * Every Key write completes at its mutation receipt: the receipt invalidates
 * older reads and applies the acknowledged local projection (revoked secrets
 * are blanked at that moment), then one guarded background GET read-backs
 * the canonical plaintext. A write started before session teardown never
 * launches a post-teardown plaintext GET.
 */
export const useConnectionStore = defineStore("connection", () => {
  const controlPlane = useControlPlaneStore();

  const info = ref<ConnectionInfo | null>(null);
  const loading = ref(false);
  const error = ref("");
  /** Read-back failure after an acknowledged write; never a write failure. */
  const refreshError = ref("");

  // `load` and the post-mutation reads share one generation so a slow pending
  // load can never clobber fresher post-mutation state; `clearSecrets` bumps
  // it so a load resolving after logout cannot re-populate plaintext.
  // Stale calls still return/throw to their own caller unchanged.
  let loadGeneration = 0;
  // Distinct from loadGeneration: a harness refresh captured before an awaited
  // mutation must not start a *new* plaintext fetch after session teardown.
  let sessionEpoch = 0;

  async function load(): Promise<ConnectionInfo> {
    const generation = ++loadGeneration;
    const session = sessionEpoch;
    loading.value = true;
    try {
      const connection = await dashboardApi.getConnection();
      if (generation !== loadGeneration || session !== sessionEpoch) return connection;
      info.value = connection;
      error.value = "";
      refreshError.value = "";
      return connection;
    } catch (e) {
      if (generation === loadGeneration && session === sessionEpoch) {
        error.value = e instanceof Error ? e.message : String(e);
      }
      throw e;
    } finally {
      if (generation === loadGeneration && session === sessionEpoch) loading.value = false;
    }
  }

  /** Session invalidation token; bumped only by `clearSecrets`. */
  function currentSession(): number {
    return sessionEpoch;
  }

  /** Refresh after a key mutation; the mutation ack has no plaintext. */
  async function reloadAfterMutation(expectedSession?: number): Promise<ConnectionInfo> {
    if (expectedSession !== undefined && expectedSession !== sessionEpoch) {
      if (info.value) return info.value;
      throw new Error("connection session ended");
    }
    const generation = ++loadGeneration;
    const session = expectedSession ?? sessionEpoch;
    try {
      const connection = await dashboardApi.getConnection();
      if (generation !== loadGeneration || session !== sessionEpoch) return connection;
      info.value = connection;
      error.value = "";
      refreshError.value = "";
      return connection;
    } catch (e) {
      if (generation === loadGeneration && session === sessionEpoch) {
        error.value = e instanceof Error ? e.message : String(e);
      }
      throw e;
    } finally {
      // Latest request owns the flag, including releasing a superseded
      // in-flight `load` whose own finally no longer clears it.
      if (generation === loadGeneration && session === sessionEpoch) loading.value = false;
    }
  }

  /**
   * Run a Key write. A revision conflict stays the outcome; its recovery
   * reload is best-effort and session-guarded, so a failed reload never
   * replaces the original conflict and never dispatches after teardown.
   */
  async function runKeyMutation<T>(run: () => Promise<T>, session: number): Promise<T> {
    try {
      return await run();
    } catch (cause) {
      if (isRevisionConflict(cause) && session === sessionEpoch) {
        try {
          await reloadAfterMutation(session);
        } catch {
          // The original conflict stays the outcome when recovery fails.
        }
      }
      throw cause;
    }
  }

  /**
   * Receipt-side bookkeeping shared by every Key write: the acknowledged
   * revision supersedes every earlier read, and the receipt — not the
   * read-back — releases the load gate.
   */
  function beginReceipt(): void {
    loadGeneration += 1;
    loading.value = false;
  }

  /**
   * Canonical plaintext read-back after an acknowledged write. Runs in the
   * background: it never rejects, never turns the committed write into a
   * failure, and reports a lost read through `refreshError` only.
   */
  async function revalidateAfterCommit(session: number): Promise<KeyRevalidation> {
    if (session !== sessionEpoch) return "cancelled";
    const generation = ++loadGeneration;
    try {
      const connection = await dashboardApi.getConnection();
      if (generation !== loadGeneration || session !== sessionEpoch) return "cancelled";
      info.value = connection;
      error.value = "";
      refreshError.value = "";
      return "loaded";
    } catch (cause) {
      if (generation !== loadGeneration || session !== sessionEpoch) return "cancelled";
      refreshError.value = cause instanceof Error ? cause.message : String(cause);
      return "unavailable";
    } finally {
      if (generation === loadGeneration && session === sessionEpoch) loading.value = false;
    }
  }

  function openReceipt<T>(value: T | undefined, read: (receipt: CommittedKeyWrite<T>) => Promise<KeyRevalidation>): CommittedKeyWrite<T> {
    const receipt: CommittedKeyWrite<T> = {
      committed: true,
      value,
      revalidation: Promise.resolve("cancelled"),
    };
    receipt.revalidation = read(receipt);
    return receipt;
  }

  /** Ids known before the create write. A session end here must not POST. */
  async function previousSubKeyIds(session: number): Promise<Set<string>> {
    if (info.value) return new Set(info.value.sub_keys.map((key) => key.id));
    const generation = ++loadGeneration;
    loading.value = true;
    let before: ConnectionInfo;
    try {
      before = await dashboardApi.getConnection();
    } catch (cause) {
      if (generation === loadGeneration && session === sessionEpoch) {
        error.value = cause instanceof Error ? cause.message : String(cause);
        loading.value = false;
      }
      throw cause;
    }
    if (session !== sessionEpoch) {
      if (generation === loadGeneration) loading.value = false;
      throw new Error("connection session ended");
    }
    if (generation === loadGeneration) {
      info.value = before;
      error.value = "";
      refreshError.value = "";
      loading.value = false;
    }
    return new Set(before.sub_keys.map((key) => key.id));
  }

  async function revalidateCreated(
    session: number,
    previousIds: ReadonlySet<string>,
    receipt: CommittedKeyWrite<ConnectionSubKey>,
  ): Promise<KeyRevalidation> {
    const status = await revalidateAfterCommit(session);
    if (status !== "loaded" || session !== sessionEpoch) return status;
    const created = (info.value?.sub_keys ?? []).filter((key) => !previousIds.has(key.id));
    if (created.length !== 1) return "unavailable";
    receipt.value = created[0];
    return "loaded";
  }

  async function revalidateSubKey(
    session: number,
    id: string,
    receipt: CommittedKeyWrite<ConnectionSubKey>,
  ): Promise<KeyRevalidation> {
    const status = await revalidateAfterCommit(session);
    if (status !== "loaded" || session !== sessionEpoch) return status;
    const match = info.value?.sub_keys.find((entry) => entry.id === id);
    if (!match?.value) return "unavailable";
    receipt.value = match;
    return "loaded";
  }

  async function revalidatePrimary(
    session: number,
    receipt: CommittedKeyWrite<string>,
  ): Promise<KeyRevalidation> {
    const status = await revalidateAfterCommit(session);
    if (status !== "loaded" || session !== sessionEpoch) return status;
    const secret = info.value?.primary_key ?? "";
    if (!secret) return "unavailable";
    receipt.value = secret;
    return "loaded";
  }

  /** Apply the acknowledged local projection onto the cached connection. */
  function projectSubKeys(project: (subKeys: ConnectionSubKey[]) => ConnectionSubKey[]): void {
    const current = info.value;
    if (!current) return;
    info.value = { ...current, sub_keys: project(current.sub_keys) };
    error.value = "";
  }

  async function createKey(name: string): Promise<CommittedKeyWrite<ConnectionSubKey>> {
    const session = currentSession();
    const previousIds = await previousSubKeyIds(session);
    if (session !== sessionEpoch) throw new Error("connection session ended");
    await runKeyMutation(() => controlPlane.runMutation((exp) => dashboardApi.createKey(name, exp)), session);
    if (session !== sessionEpoch) throw new Error("connection session ended");
    beginReceipt();
    // More than one unseen id is committed but not uniquely identifiable.
    // That stays a read-back outcome and never retries the create.
    return openReceipt<ConnectionSubKey>(undefined, (receipt) => revalidateCreated(session, previousIds, receipt));
  }

  async function updateKey(id: string, update: { name?: string; enabled?: boolean }): Promise<CommittedKeyWrite> {
    const session = currentSession();
    await runKeyMutation(() => controlPlane.runMutation((exp) => dashboardApi.updateKey(id, update, exp)), session);
    if (session !== sessionEpoch) throw new Error("connection session ended");
    beginReceipt();
    if (info.value) {
      projectSubKeys((subKeys) => subKeys.map((entry) => (entry.id === id ? { ...entry, ...update } : entry)));
    }
    return openReceipt(undefined, () => revalidateAfterCommit(session));
  }

  async function deleteKey(id: string): Promise<CommittedKeyWrite> {
    const session = currentSession();
    await runKeyMutation(() => controlPlane.runMutation((exp) => dashboardApi.deleteKey(id, exp)), session);
    if (session !== sessionEpoch) throw new Error("connection session ended");
    beginReceipt();
    if (info.value) {
      // The acknowledged deletion also drops the cached plaintext value.
      projectSubKeys((subKeys) => subKeys.filter((entry) => entry.id !== id));
    }
    return openReceipt(undefined, () => revalidateAfterCommit(session));
  }

  async function regenerateKey(id: string): Promise<CommittedKeyWrite<ConnectionSubKey>> {
    const session = currentSession();
    await runKeyMutation(() => controlPlane.runMutation((exp) => dashboardApi.regenerateKey(id, exp)), session);
    if (session !== sessionEpoch) throw new Error("connection session ended");
    beginReceipt();
    if (info.value) {
      // The receipt already revoked the old value: blank it at once instead
      // of waiting for the read-back, so no stale secret can be copied.
      projectSubKeys((subKeys) => subKeys.map((entry) => (entry.id === id ? { ...entry, value: "" } : entry)));
    }
    return openReceipt<ConnectionSubKey>(undefined, (receipt) => revalidateSubKey(session, id, receipt));
  }

  /**
   * Rotate the primary Key. Every path completes at the POST receipt: the
   * cached secret is blanked immediately, and the new plaintext arrives only
   * through one detached read-back for this primary Key.
   */
  async function regeneratePrimaryKey(): Promise<CommittedKeyWrite<string>> {
    const session = currentSession();
    await runKeyMutation(() => controlPlane.runMutation((exp) => dashboardApi.regeneratePrimaryKey(exp)), session);
    if (session !== sessionEpoch) throw new Error("connection session ended");
    beginReceipt();
    if (info.value) {
      info.value = { ...info.value, primary_key: "" };
      error.value = "";
    }
    return openReceipt<string>(undefined, (receipt) => revalidatePrimary(session, receipt));
  }

  /** Drop all plaintext Key material held in memory (401 / logout). */
  function clearSecrets(): void {
    sessionEpoch += 1;
    loadGeneration += 1;
    info.value = null;
    error.value = "";
    refreshError.value = "";
    loading.value = false;
  }

  return {
    info: computed(() => info.value),
    loading: computed(() => loading.value),
    error: computed(() => error.value),
    refreshError: computed(() => refreshError.value),
    load,
    currentSession,
    reloadAfterMutation,
    createKey,
    updateKey,
    deleteKey,
    regenerateKey,
    regeneratePrimaryKey,
    clearSecrets,
  };
});
