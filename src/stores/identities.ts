import { computed, ref } from "vue";
import { defineStore } from "pinia";
import {
  identitiesApi,
  identityJoinKey,
  type Identity,
  type IdentityListSnapshot,
} from "../api/identities.ts";
import type { MutationExpectation } from "../api/generated/dashboard-v3.ts";

/**
 * Secret-free V4 identity projection overlay. The V3 account list remains
 * the mutation and card-id source of truth; this store is display-only.
 * Identities and the GET snapshot pair are applied together so an editor
 * can submit the view that is on screen.
 */
export const useIdentitiesStore = defineStore("identities", () => {
  const identities = ref<Identity[]>([]);
  const snapshotExpectation = ref<MutationExpectation | null>(null);
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
      for (const credential of row.credentials) {
        if (credential.legacy.kind === "account") map.set(credential.legacy.id, row);
      }
    }
    return map;
  });

  function applySnapshot(snapshot: IdentityListSnapshot): void {
    identities.value = snapshot.identities;
    snapshotExpectation.value = snapshot.expectation;
    loaded.value = true;
    error.value = "";
  }

  async function loadPresented(): Promise<Identity[]> {
    const generation = ++loadGeneration;
    loading.value = true;
    try {
      const snapshot = await identitiesApi.listSnapshot();
      if (generation !== loadGeneration) return snapshot.identities;
      applySnapshot(snapshot);
      return snapshot.identities;
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
    snapshotExpectation: computed(() => snapshotExpectation.value),
    loaded: computed(() => loaded.value),
    loading: computed(() => loading.value),
    error: computed(() => error.value),
    byJoinKey,
    byAccountId,
    loadPresented,
  };
});
