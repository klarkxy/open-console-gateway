import type { MessageKey } from "../i18n/index.ts";

export type ProviderSort = "name_asc" | "name_desc";

export const PROVIDER_SORT_KEYS: Record<ProviderSort, MessageKey> = {
  name_asc: "名称 A–Z",
  name_desc: "名称 Z–A",
};

const nameCollator = new Intl.Collator("en", { sensitivity: "base", numeric: true });

/** Presentation order only; never mutates stored routing priority. */
export function sortProvidersByName<T>(
  entries: readonly T[],
  nameOf: (entry: T) => string,
  order: ProviderSort = "name_asc",
): T[] {
  return [...entries].sort((a, b) => (
    nameCollator.compare(nameOf(a), nameOf(b)) * (order === "name_desc" ? -1 : 1)
  ));
}
