import type { AccountTestModelChoice } from "../api/generated/dashboard-v4.ts";

export type AccountTestModel = AccountTestModelChoice;

export function filterAccountTestModels(
  models: readonly AccountTestModel[],
  query: string,
): AccountTestModel[] {
  const needle = query.trim().toLowerCase();
  if (!needle) return [...models];
  return models.filter((model) => (
    model.modelId.toLowerCase().includes(needle)
    || model.alias.toLowerCase().includes(needle)
  ));
}
