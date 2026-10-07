import { ref, shallowRef, watch } from "vue";
import { defineStore } from "pinia";
import { pagesApi, type DashboardPage } from "../api/pages.ts";
import { dashboardErrorDetail } from "../utils/errors.ts";
import { useControlPlaneStore } from "./controlPlane.ts";
import { createReadLifecycle, PAGE_READ_MAX_AGE_MS, type ReadOptions } from "./readLifecycle.ts";

export type DashboardPageQuery = NonNullable<Parameters<typeof pagesApi.dashboard>[0]>;

/** The only owner of Dashboard facts. Secrets stay in the connection store. */
export const useDashboardPageStore = defineStore("dashboardPage", () => {
  const control = useControlPlaneStore();
  const page = shallowRef<DashboardPage | null>(null);
  const loading = ref(false);
  const error = ref("");
  const reads = createReadLifecycle();
  let generation = 0;
  let session = 0;
  let queryKey = "";
  let controller: AbortController | null = null;

  function invalidate(): void {
    generation++;
    controller?.abort();
    controller = null;
    reads.invalidate();
    loading.value = false;
  }

  function load(query: DashboardPageQuery = {}, options: ReadOptions = {}): Promise<DashboardPage> {
    const key = JSON.stringify(query);
    if (key !== queryKey) { invalidate(); queryKey = key; }
    const remaining = page.value ? Date.parse(page.value.validUntil) - Date.now() : 0;
    const maxAgeMs = remaining > 0 ? Math.min(options.maxAgeMs ?? 0, PAGE_READ_MAX_AGE_MS, remaining) : 0;
    return reads.run(key, { maxAgeMs }, () => page.value!, async () => {
      const own = ++generation;
      const ownSession = session;
      const ownController = new AbortController();
      controller = ownController;
      loading.value = true;
      try {
        const result = await pagesApi.dashboard(query, ownController.signal);
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

  function clear(): void {
    session++;
    invalidate();
    page.value = null;
    error.value = "";
    queryKey = "";
  }

  watch(() => control.processGeneration, (next, previous) => {
    if (previous !== null && next !== previous) { reads.invalidate(); page.value = null; }
  }, { flush: "sync" });

  return { page, loading, error, load, invalidate, clear };
});

export function clearDashboardPage(): void { useDashboardPageStore().clear(); }
