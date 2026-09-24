export interface SearchableNavigationItem { key: string; label: string }

/** Search labels in the active locale and stable view IDs without touching server state. */
export function filterNavigationItems<T extends SearchableNavigationItem>(items: readonly T[], query: string): T[] {
  const terms = query.normalize("NFKC").toLocaleLowerCase().trim().split(/\s+/).filter(Boolean);
  return items.filter((item) => {
    const haystack = `${item.key} ${item.label}`.normalize("NFKC").toLocaleLowerCase();
    return terms.every((term) => haystack.includes(term));
  });
}

export function stepSelection(current: number, delta: number, count: number): number {
  if (count <= 0) return -1;
  return ((current + delta) % count + count) % count;
}

export function isNavigationShortcut(event: Pick<KeyboardEvent, "key" | "metaKey" | "ctrlKey" | "altKey" | "isComposing" | "repeat">): boolean {
  return !event.isComposing && !event.repeat && !event.altKey
    && (event.metaKey || event.ctrlKey) && event.key.toLowerCase() === "k";
}

export const SIDEBAR_STORAGE_KEY = "ocg-manager.sidebar-collapsed";
export function readSidebarCollapsed(storage: Pick<Storage, "getItem"> | null): boolean {
  try { return storage?.getItem(SIDEBAR_STORAGE_KEY) === "true"; } catch { return false; }
}
export function writeSidebarCollapsed(storage: Pick<Storage, "setItem"> | null, value: boolean): void {
  try { storage?.setItem(SIDEBAR_STORAGE_KEY, String(value)); } catch {
    // Layout preferences never prevent using the console when storage is blocked.
  }
}
