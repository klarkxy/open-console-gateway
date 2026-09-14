import { computed, ref } from "vue";
import { defineStore } from "pinia";
import { dashboardApi, isRevisionConflict } from "../api/dashboard.ts";
import type { AppConfig } from "../api/dashboard.ts";

/**
 * Application settings.
 *
 * The Settings resource never carries Key plaintext (that lives in the
 * connection store). Update-check / install progress is transient and stays
 * page-local in the Settings view.
 */
export const useSettingsStore = defineStore("settings", () => {
  const settings = ref<AppConfig | null>(null);
  const loading = ref(false);
  const error = ref("");

  // Overlapping loads resolve out of order; only the latest request commits
  // state. Stale calls still return/throw to their own caller unchanged.
  let loadGeneration = 0;

  async function load(): Promise<AppConfig> {
    const generation = ++loadGeneration;
    loading.value = true;
    try {
      const result = await dashboardApi.getSettings();
      if (generation !== loadGeneration) return result;
      settings.value = result;
      error.value = "";
      return result;
    } catch (e) {
      if (generation === loadGeneration) {
        error.value = e instanceof Error ? e.message : String(e);
      }
      throw e;
    } finally {
      if (generation === loadGeneration) loading.value = false;
    }
  }

  async function loadPresented(): Promise<AppConfig> {
    return load();
  }

  async function putPresented(update: AppConfig): Promise<AppConfig> {
    try {
      await dashboardApi.updateSettings(update);
      await load();
    } catch (cause) {
      if (isRevisionConflict(cause)) await load();
      throw cause;
    }
    if (!settings.value) throw new Error("settings reload returned no resource");
    return settings.value;
  }

  return {
    settings: computed(() => settings.value),
    loading: computed(() => loading.value),
    error: computed(() => error.value),
    loadPresented,
    putPresented,
  };
});
