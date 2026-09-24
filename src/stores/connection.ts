import { computed, ref } from "vue";
import { defineStore } from "pinia";
import { dashboardApi, isRevisionConflict } from "../api/dashboard.ts";
import type { ConnectionInfo, ConnectionSubKey } from "../api/dashboard.ts";
import { t } from "../i18n/index.ts";
import { useControlPlaneStore } from "./controlPlane.ts";

/**
 * Connection center: the plaintext primary/sub Key values plus the URL fields
 * shown on the Dashboard and Access Keys pages.
 *
 * Secrets are memory-only: nothing here is persisted, and `clearSecrets()`
 * wipes the payload. The session store calls `clearSecrets()` on logout and
 * whenever a 401 invalidates the session.
 */
export const useConnectionStore = defineStore("connection", () => {
  const controlPlane = useControlPlaneStore();

  const info = ref<ConnectionInfo | null>(null);
  const loading = ref(false);
  const error = ref("");

  // `load` and `reloadAfterMutation` share one generation so a slow pending
  // load can never clobber fresher post-mutation state; `clearSecrets` bumps
  // it so a load resolving after logout cannot re-populate plaintext.
  // Stale calls still return/throw to their own caller unchanged.
  let loadGeneration = 0;

  async function load(): Promise<ConnectionInfo> {
    const generation = ++loadGeneration;
    loading.value = true;
    try {
      const connection = await dashboardApi.getConnection();
      if (generation !== loadGeneration) return connection;
      info.value = connection;
      error.value = "";
      return connection;
    } catch (e) {
      if (generation === loadGeneration) {
        error.value = e instanceof Error ? e.message : String(e);
      }
      throw e;
    } finally {
      if (generation === loadGeneration) loading.value = false;
    }
  }

  /** Refresh after a key mutation; the mutation ack has no plaintext. */
  async function reloadAfterMutation(): Promise<ConnectionInfo> {
    const generation = ++loadGeneration;
    try {
      const connection = await dashboardApi.getConnection();
      if (generation !== loadGeneration) return connection;
      info.value = connection;
      error.value = "";
      return connection;
    } catch (e) {
      if (generation === loadGeneration) {
        error.value = e instanceof Error ? e.message : String(e);
      }
      throw e;
    } finally {
      // Latest request owns the flag, including releasing a superseded
      // in-flight `load` whose own finally no longer clears it.
      if (generation === loadGeneration) loading.value = false;
    }
  }

  async function runKeyMutation<T>(run: () => Promise<T>): Promise<T> {
    try {
      return await run();
    } catch (cause) {
      if (isRevisionConflict(cause)) await reloadAfterMutation();
      throw cause;
    }
  }

  async function createKey(name: string): Promise<ConnectionSubKey> {
    const before = info.value ?? await load();
    const previousIds = new Set(before.sub_keys.map((key) => key.id));
    await runKeyMutation(() => controlPlane.runMutation((exp) => dashboardApi.createKey(name, exp)));
    const connection = await reloadAfterMutation();
    const created = connection.sub_keys.filter((key) => !previousIds.has(key.id));
    if (created.length !== 1) {
      throw new Error(t("创建 Key 后未能识别新条目，刷新后重试"));
    }
    return created[0]!;
  }

  async function updateKey(id: string, update: { name?: string; enabled?: boolean }): Promise<void> {
    await runKeyMutation(() => controlPlane.runMutation((exp) => dashboardApi.updateKey(id, update, exp)));
    await reloadAfterMutation();
  }

  async function deleteKey(id: string): Promise<void> {
    await runKeyMutation(() => controlPlane.runMutation((exp) => dashboardApi.deleteKey(id, exp)));
    await reloadAfterMutation();
  }

  async function regenerateKey(id: string): Promise<ConnectionSubKey> {
    await runKeyMutation(() => controlPlane.runMutation((exp) => dashboardApi.regenerateKey(id, exp)));
    const connection = await reloadAfterMutation();
    const regenerated = connection.sub_keys.find((key) => key.id === id);
    if (!regenerated) throw new Error(t("重新生成后找不到对应 Key，刷新后重试"));
    return regenerated;
  }

  async function regeneratePrimaryKey(): Promise<string> {
    await runKeyMutation(() => controlPlane.runMutation((exp) => dashboardApi.regeneratePrimaryKey(exp)));
    const connection = await reloadAfterMutation();
    return connection.primary_key;
  }

  /** Drop all plaintext Key material held in memory (401 / logout). */
  function clearSecrets(): void {
    loadGeneration += 1;
    info.value = null;
    error.value = "";
    loading.value = false;
  }

  return {
    info: computed(() => info.value),
    loading: computed(() => loading.value),
    error: computed(() => error.value),
    load,
    createKey,
    updateKey,
    deleteKey,
    regenerateKey,
    regeneratePrimaryKey,
    clearSecrets,
  };
});
