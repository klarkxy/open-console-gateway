import assert from "node:assert/strict";
import test from "node:test";
import type { ProviderCatalogEntry } from "../api/providers.ts";
import { resolveAccountFormFields } from "./account-form-fields.ts";
import { OPENCODE_GO_PLAN, providerSurfaceFromCatalog } from "./plans.ts";

function entry(provider_id: string, form_fields: ProviderCatalogEntry["form_fields"]): ProviderCatalogEntry {
  return {
    ...OPENCODE_GO_PLAN,
    id: undefined,
    label: undefined,
    kind: undefined,
    legacy: undefined,
    dynamic: undefined,
    provider_id,
    display_name: provider_id,
    form_fields,
  } as unknown as ProviderCatalogEntry;
}

test("only the offline Go surface supplies compatibility fields", () => {
  const fallback = resolveAccountFormFields(OPENCODE_GO_PLAN, undefined);
  assert.ok(fallback.some(({ id, required }) => id === "name" && required));
  assert.ok(fallback.some(({ id, required }) => id === "key" && required));

  const successfulEmpty = entry("opencode", []);
  const surface = providerSurfaceFromCatalog(successfulEmpty);
  assert.deepEqual(resolveAccountFormFields(surface, successfulEmpty), []);
});

test("non-legacy surfaces never invent fields when their catalog row is absent", () => {
  const surface = providerSurfaceFromCatalog(entry("future-plan", []));
  assert.deepEqual(resolveAccountFormFields(surface, undefined), []);
});

test("dynamic Provider fields come from the catalog and drop lifecycle dates", () => {
  const dynamicEntry = entry("11111111-1111-4111-8111-111111111111", [
    { id: "name", kind: "text", required: true, immutable_after_create: false },
    { id: "key", kind: "secret", required: true, immutable_after_create: false },
    { id: "purchase_date", kind: "date", required: false, immutable_after_create: false },
    { id: "notes", kind: "text", required: false, immutable_after_create: false },
  ]);
  dynamicEntry.model_source = "dynamic_provider";
  const surface = providerSurfaceFromCatalog(dynamicEntry);
  assert.deepEqual(
    resolveAccountFormFields(surface, dynamicEntry).map((field) => field.id),
    ["name", "key", "notes"],
  );
});
