import { computed, ref, shallowRef, watch } from "vue";
import { defineStore } from "pinia";
import { pagesApi } from "../api/pages.ts";
import { dashboardV4 } from "../api/dashboard-v4.ts";
import { isRevisionConflict } from "../api/providers.ts";
import { applyAliasPagePublication, aliasPagePublicationKey, type AliasPage } from "../domain/alias-page.ts";
import { dashboardErrorDetail } from "../utils/errors.ts";
import { useControlPlaneStore, isLocalMutationCancelled } from "./controlPlane.ts";
import { createReadLifecycle, PAGE_READ_MAX_AGE_MS, type ReadOptions } from "./readLifecycle.ts";
import { invalidateManagementPages } from "./managementPages.ts";

export type AliasPageQuery = NonNullable<Parameters<typeof pagesApi.aliases>[0]>;

/** One bounded result, never a catalog snapshot or a persisted business cache. */
export const useAliasPageStore = defineStore("aliasPage", () => {
  const control = useControlPlaneStore();
  const page = shallowRef<AliasPage | null>(null);
  const loading = ref(false);
  const error = ref("");
  const overlays = ref<Record<string, boolean>>({});
  const pending = ref<string[]>([]);
  const publicationErrors = ref<Record<string, string>>({});
  const reads = createReadLifecycle();
  let generation = 0;
  let session = 0;
  let controller: AbortController | null = null;
  let queryKey = "";

  function invalidate(): void {
    generation++;
    controller?.abort();
    controller = null;
    reads.invalidate();
    loading.value = false;
  }

  function load(query: AliasPageQuery, options: ReadOptions = {}): Promise<AliasPage> {
    const key = JSON.stringify(query);
    if (key !== queryKey) {
      invalidate();
      queryKey = key;
    }
    const remaining = page.value?.validUntil ? Date.parse(page.value.validUntil) - Date.now() : 0;
    return reads.run(key, { maxAgeMs: remaining > 0 ? Math.min(options.maxAgeMs ?? 0, PAGE_READ_MAX_AGE_MS) : 0 },
      () => page.value!, async () => {
        const own = ++generation;
        const ownSession = session;
        const ownController = new AbortController();
        controller = ownController;
        loading.value = true;
        try {
          const result = await pagesApi.aliases(query, ownController.signal);
          if (own !== generation || ownSession !== session) return result;
          if (result.revision.processGeneration !== control.processGeneration) return result;
          if (control.revision !== null && result.revision.revision < control.revision) return result;
          if (page.value?.revision.processGeneration === result.revision.processGeneration
            && page.value.revision.revision > result.revision.revision) return result;
          page.value = result;
          error.value = "";
          reads.markSuccessful(key);
          return result;
        } catch (cause) {
          if (own === generation && ownSession === session) error.value = dashboardErrorDetail(cause);
          throw cause;
        } finally {
          if (own === generation && ownSession === session) { loading.value = false; controller = null; }
        }
      });
  }

  function dropOverlay(key: string): void {
    const next = { ...overlays.value };
    delete next[key];
    overlays.value = next;
  }

  async function setPublished(publicModel: string, published: boolean): Promise<void> {
    const key = aliasPagePublicationKey(publicModel);
    if (pending.value.includes(key)) return;
    const ownSession = session;
    invalidate();
    pending.value = [...pending.value, key];
    overlays.value = { ...overlays.value, [key]: published };
    try {
      const receipt = await control.runLocalMutation(`alias-publication:${key}`,
        expectation => dashboardV4.patchAliasPublication({ publicModel, published }, expectation));
      if (ownSession !== session) return;
      if (receipt.revision.processGeneration !== control.processGeneration) return;
      invalidate();
      if (page.value) page.value = applyAliasPagePublication(page.value, receipt.unpublished);
      invalidateManagementPages("aliasPage");
      dropOverlay(key);
      const next = { ...publicationErrors.value };
      delete next[key];
      publicationErrors.value = next;
    } catch (cause) {
      if (ownSession !== session) return;
      dropOverlay(key);
      if (isLocalMutationCancelled(cause)) return;
      publicationErrors.value = { ...publicationErrors.value, [key]: dashboardErrorDetail(cause) };
      if (isRevisionConflict(cause)) invalidate(); // no automatic mutation retry
    } finally {
      if (ownSession === session) pending.value = pending.value.filter(entry => entry !== key);
    }
  }

  function clear(): void {
    session++;
    invalidate();
    page.value = null;
    error.value = "";
    queryKey = "";
    overlays.value = {};
    pending.value = [];
    publicationErrors.value = {};
  }

  watch(() => control.processGeneration, (next, previous) => {
    if (previous !== null && next !== previous) {
      reads.invalidate();
      page.value = null;
      overlays.value = {};
      pending.value = [];
      publicationErrors.value = {};
    }
  }, { flush: "sync" });

  return {
    page, loading, error, publicationErrors, pending,
    groups: computed(() => page.value?.groups.map(group => ({
      ...group, published: overlays.value[group.publicationKey] ?? group.published,
    })) ?? []),
    load, setPublished, invalidate, clear,
  };
});

export function clearAliasPage(): void { useAliasPageStore().clear(); }
