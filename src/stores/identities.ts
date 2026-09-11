import { computed, ref } from "vue";
import { defineStore } from "pinia";
import { identitiesApi, identityJoinKey, type Identity } from "../api/identities.ts";

/**
 * Secret-free V4 identity projection overlay. The V3 account list remains
 * the mutation and card-id source of truth; this store is display-only.
 */
export const useIdentitiesStore = defineStore("identities", () => {
  const identities = ref<Identity[]>([]);
  const loaded = ref(false);
  const loading = ref(false);
  const error = ref("");

  let loadGeneration = 0;

  const byJoinKey = computed(() => {
    const map = new Map<string, Identity>();
    for (const row of identities.value) map.set(identityJoinKey(row.legacy), row);
    return map;
  });

  const byAccountId = computed(() => {
    const map = new Map<string, Identity>();
    for (const row of identities.value) {
      if (row.legacy.kind === "account") map.set(row.legacy.id, row);
    }
    return map;
  });

  async function loadPresented(): Promise<Identity[]> {
    const generation = ++loadGeneration;
    loading.value = true;
    try {
      const list = await identitiesApi.list();
      if (generation !== loadGeneration) return list;
      identities.value = list;
      loaded.value = true;
      error.value = "";
      return list;
    } catch (e) {
      if (generation === loadGeneration) {
        error.value = e instanceof Error ? e.message : String(e);
      }
      throw e;
    } finally {
      if (generation === loadGeneration) loading.value = false;
    }
  }

  return {
    identities: computed(() => identities.value),
    loaded: computed(() => loaded.value),
    loading: computed(() => loading.value),
    error: computed(() => error.value),
    byJoinKey,
    byAccountId,
    loadPresented,
  };
});
