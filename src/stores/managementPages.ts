import { getActivePinia } from "pinia";

export type ManagementPageId = "accountPage" | "providerPage" | "aliasPage" | "dashboardPage";
const PAGE_IDS: readonly ManagementPageId[] = ["accountPage", "providerPage", "aliasPage", "dashboardPage"];

/** Confirmed writes expire other used page reads without fetching or hiding their content. */
export function invalidateManagementPages(source?: ManagementPageId): void {
  const registry = getActivePinia()?._s;
  for (const id of PAGE_IDS) {
    if (id === source) continue;
    const store = registry?.get(id) as { invalidate?: () => void } | undefined;
    store?.invalidate?.();
  }
}
