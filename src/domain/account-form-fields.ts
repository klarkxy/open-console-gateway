import type { ProviderCatalogEntry, ProviderCatalogFormField } from "../api/providers.ts";
import type { PlanDefinition } from "./plans.ts";

const LEGACY_GO_FIELDS: readonly ProviderCatalogFormField[] = [
  { id: "name", kind: "text", required: true, immutable_after_create: false },
  { id: "key", kind: "secret", required: true, immutable_after_create: false },
  { id: "purchase_date", kind: "date", required: false, immutable_after_create: false },
  { id: "notes", kind: "text", required: false, immutable_after_create: false },
];

/**
 * Resolve creation fields from the catalog without allowing a missing, empty,
 * or partially deployed catalog to remove the legacy OpenCode Go Key path.
 * Non-legacy plans remain catalog-owned and therefore fail closed.
 */
export function resolveAccountFormFields(
  plan: PlanDefinition | null,
  catalogEntry: ProviderCatalogEntry | undefined,
): ProviderCatalogFormField[] {
  if (!plan) return [];
  if (plan.id === "dynamic-http") {
    // User-defined Providers model no billing cadence: lifecycle date fields
    // never apply to their accounts, even if a stale catalog row declares
    // them.
    const fields = [...(catalogEntry?.form_fields ?? [])]
      .filter((field) => field.id !== "purchase_date");
    if (fields.length > 0) return fields;
    const fallback: ProviderCatalogFormField[] = [
      { id: "name", kind: "text", required: true, immutable_after_create: false },
      { id: "notes", kind: "text", required: false, immutable_after_create: false },
    ];
    if (plan.credential_kind !== "none") {
      fallback.splice(1, 0, { id: "key", kind: "secret", required: true, immutable_after_create: false });
    }
    return fallback;
  }
  if (plan.id !== "opencode-go") return [...(catalogEntry?.form_fields ?? [])];

  const fields = [...(catalogEntry?.form_fields ?? [])];
  const ids = new Set(fields.map(({ id }) => id));
  for (const fallback of LEGACY_GO_FIELDS) {
    if (!ids.has(fallback.id)) fields.push({ ...fallback });
  }
  return fields;
}
