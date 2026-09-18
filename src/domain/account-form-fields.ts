import type { ProviderCatalogEntry, ProviderCatalogFormField } from "../api/providers.ts";
import { isLegacyGoFallbackPlan } from "./account-capabilities.ts";
import type { PlanDefinition } from "./plans.ts";

const LEGACY_GO_FIELDS: readonly ProviderCatalogFormField[] = [
  { id: "name", kind: "text", required: true, immutable_after_create: false },
  { id: "key", kind: "secret", required: true, immutable_after_create: false },
  { id: "purchase_date", kind: "date", required: false, immutable_after_create: false },
  { id: "notes", kind: "text", required: false, immutable_after_create: false },
];

/**
 * Resolve creation fields from the catalog. A successful catalog row is
 * authoritative; only the offline Go surface supplies compatibility fields.
 */
export function resolveAccountFormFields(
  plan: PlanDefinition | null,
  catalogEntry: ProviderCatalogEntry | undefined,
): ProviderCatalogFormField[] {
  if (!plan) return [];
  const fields = [...(catalogEntry?.form_fields ?? plan.form_fields)];
  if (plan.dynamic) {
    // User-defined Providers model no billing cadence: lifecycle date fields
    // never apply to their accounts, even if a stale catalog row declares
    // them.
    return fields.filter((field) => field.id !== "purchase_date");
  }
  if (catalogEntry) return fields;
  if (isLegacyGoFallbackPlan(plan, null)) {
    return LEGACY_GO_FIELDS.map((field) => ({ ...field }));
  }
  return [];
}
