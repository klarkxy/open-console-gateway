import { ref, shallowRef, watch } from "vue";
import { defineStore } from "pinia";
import { pagesApi } from "../api/pages.ts";
import type { ProviderCatalogPresentation, ControlRevision } from "../api/generated/dashboard-v3.ts";
import type { ProviderContractsResponse } from "../api/providers.ts";
import type { Destination, DestinationModelMetadataSnapshot } from "../api/destinations.ts";
import { dashboardErrorDetail } from "../utils/errors.ts";
import { useControlPlaneStore } from "./controlPlane.ts";
import { createReadLifecycle, PAGE_READ_MAX_AGE_MS, type ReadOptions } from "./readLifecycle.ts";

export type ProvidersPage = Awaited<ReturnType<typeof pagesApi.providers>>;
export type ProviderPageDetail = Awaited<ReturnType<typeof pagesApi.providerDetail>>;
export type ProviderModelsPage = Awaited<ReturnType<typeof pagesApi.providerModels>>;
export type ProviderEditDetail = Awaited<ReturnType<typeof pagesApi.providerEditDetail>>;
export type ProviderRailQuery = NonNullable<Parameters<typeof pagesApi.providers>[0]>;
export type ProviderModelsQuery = NonNullable<Parameters<typeof pagesApi.providerModels>[1]>;

/** Bounded rail/header/model resources; complete edit data has a separate identity. */
export const useProviderPageStore = defineStore("providerPage", () => {
  const control = useControlPlaneStore();
  const rail = shallowRef<ProvidersPage | null>(null);
  const detail = shallowRef<ProviderPageDetail | null>(null);
  const models = shallowRef<ProviderModelsPage | null>(null);
  const editDetail = shallowRef<ProviderEditDetail | null>(null);
  const loading = ref<Record<string, boolean>>({});
  const errors = ref<Record<string, string>>({});
  const reads = createReadLifecycle();
  const identities = new Map<string, string>();
  const generations = new Map<string, number>();
  const controllers = new Map<string, AbortController>();
  let session = 0;

  function invalidate(slot?: string): void {
    for (const own of slot ? [slot] : ["rail", "detail", "models", "edit"]) {
      generations.set(own, (generations.get(own) ?? 0) + 1);
      controllers.get(own)?.abort();
      controllers.delete(own);
      reads.invalidate(own);
      loading.value = { ...loading.value, [own]: false };
    }
  }

  function read<T>(slot: string, identity: string, cached: () => T | null,
    commit: (value: T) => void, request: (signal: AbortSignal) => Promise<T>, options: ReadOptions = {}): Promise<T> {
    if (identities.get(slot) !== identity) {
      invalidate(slot);
      identities.set(slot, identity);
    }
    const value = cached();
    const validUntil = value && typeof value === "object" && "validUntil" in value
      ? Date.parse(String(value.validUntil)) - Date.now() : PAGE_READ_MAX_AGE_MS;
    const maxAgeMs = validUntil > 0 ? Math.max(0, Math.min(options.maxAgeMs ?? 0, PAGE_READ_MAX_AGE_MS)) : 0;
    return reads.run(slot, { maxAgeMs }, () => cached()!, async () => {
      const own = (generations.get(slot) ?? 0) + 1;
      generations.set(slot, own);
      const ownSession = session;
      const controller = new AbortController();
      controllers.set(slot, controller);
      loading.value = { ...loading.value, [slot]: true };
      try {
        const result = await request(controller.signal);
        if (generations.get(slot) !== own || session !== ownSession) return result;
        const revision = (result as { revision?: { revision: number; processGeneration: number } }).revision;
        if (revision && revision.processGeneration !== control.processGeneration) return result;
        if (revision && control.revision !== null && revision.revision < control.revision) return result;
        const previous = cached() as { revision?: { revision: number; processGeneration: number } } | null;
        if (revision && previous?.revision?.processGeneration === revision.processGeneration
          && previous.revision.revision > revision.revision) return result;
        commit(result);
        errors.value = { ...errors.value, [slot]: "" };
        reads.markSuccessful(slot);
        return result;
      } catch (cause) {
        if (generations.get(slot) === own && session === ownSession) errors.value = { ...errors.value, [slot]: dashboardErrorDetail(cause) };
        throw cause;
      } finally {
        if (generations.get(slot) === own && session === ownSession) {
          loading.value = { ...loading.value, [slot]: false };
          controllers.delete(slot);
        }
      }
    });
  }

  function loadRail(query: ProviderRailQuery, options?: ReadOptions) {
    return read("rail", JSON.stringify(query), () => rail.value, value => { rail.value = value; },
      signal => pagesApi.providers(query, signal), options);
  }
  function loadDetail(key: string, options?: ReadOptions) {
    const item = detail.value?.item;
    const sameSelected = item && (item.railKey === key || (key.startsWith("p:") && item.providerId === key.slice(2))
      || (key.startsWith("c:") && item.connectionId === key.slice(2)));
    const identity = sameSelected ? item.railKey : key;
    return read("detail", identity, () => detail.value, value => {
      const selectionChanged = detail.value?.item.railKey !== value.item.railKey;
      if (detail.value?.readVersion !== value.readVersion || selectionChanged) {
        invalidate("models");
        // A post-save header can finish while the user opens the next editor.
        // Keep that same-selection edit read alive; its revision guard below
        // still rejects older data. Switching selections cancels it as before.
        if (selectionChanged || !controllers.has("edit")) invalidate("edit");
      }
      detail.value = value;
      identities.set("detail", value.item.railKey);
    },
      signal => pagesApi.providerDetail(key, signal), options);
  }
  function loadModels(key: string, query: ProviderModelsQuery, options?: ReadOptions) {
    return read("models", JSON.stringify([key, query]), () => models.value, value => { models.value = value; },
      signal => pagesApi.providerModels(key, query, signal), options);
  }
  function loadEditDetail(key: string, options?: ReadOptions) {
    return read("edit", key, () => editDetail.value, value => { editDetail.value = value; },
      signal => pagesApi.providerEditDetail(key, signal), options);
  }
  function hasIdentity(slot: string, identity: string): boolean { return identities.get(slot) === identity; }

  function commitRemoval(scopeId: string, removed: readonly string[], catalogModels?: readonly string[]): void {
    const selectedDetail = detail.value;
    const current = models.value;
    if (!selectedDetail?.scope || selectedDetail.scope.scopeId !== scopeId || !current) return;
    invalidate();
    const ids = new Set(removed);
    const total = catalogModels?.length ?? Math.max(0, current.total - ids.size);
    const next: ProviderModelsPage = { ...current,
      revision: { ...current.revision, revision: control.revision!, processGeneration: control.processGeneration! },
      total, filteredTotal: Math.max(0, current.filteredTotal - ids.size),
      models: current.models.filter(row => !ids.has(row.contract.modelId)),
    };
    models.value = next;
    detail.value = { ...selectedDetail, revision: next.revision,
      item: { ...selectedDetail.item, catalogCount: total },
      scope: { ...selectedDetail.scope, catalog: { ...selectedDetail.scope.catalog, modelCount: total } },
    };
  }

  function commitPresentation(presentation: ProviderCatalogPresentation, revision: ControlRevision): void {
    const selected = detail.value;
    const current = models.value;
    if (!selected?.scope || !current || identities.get("detail") !== selected.item.railKey
      || revision.processGeneration !== control.processGeneration || revision.revision !== control.revision
      || selected.revision.processGeneration !== revision.processGeneration
      || current.revision.processGeneration !== revision.processGeneration
      || selected.revision.revision > revision.revision || current.revision.revision > revision.revision) return;
    const identity = identities.get("models");
    if (!identity) return;
    const [key, query] = JSON.parse(identity) as [string, ProviderModelsQuery];
    if (key !== selected.item.railKey && key !== `p:${selected.item.providerId}` && key !== `c:${selected.item.connectionId}`) return;
    const previous = new Map(current.models.map(row => [row.publicModel, row]));
    const needle = query.search?.trim().toLowerCase() ?? "";
    const rows = presentation.models.filter(row => (!query.enabledOnly || row.effectiveOn)
      && (!needle || row.publicModel.toLowerCase().includes(needle) || row.upstreamModel.toLowerCase().includes(needle)))
      .sort((a, b) => a.publicModel.localeCompare(b.publicModel, undefined, { numeric: true })
        || a.upstreamModel.localeCompare(b.upstreamModel));
    let offset = current.offset;
    const target = query.model ? rows.findIndex(row => row.publicModel === query.model
      || row.upstreamModel === query.model || row.contract.modelId === query.model) : -1;
    if (target >= 0) offset = Math.floor(target / current.limit) * current.limit;
    invalidate();
    const next = { ...current, revision, total: presentation.total, filteredTotal: rows.length,
      allDisabled: presentation.allDisabled, offset, hasMore: offset + current.limit < rows.length,
      models: rows.slice(offset, offset + current.limit).map(row => {
        const previousRow = previous.get(row.publicModel);
        const cached = previousRow?.upstreamModel === row.upstreamModel ? previousRow : undefined;
        return { ...row, metadata: cached?.metadata ?? null, metadataSource: cached?.metadataSource ?? null };
      }) };
    models.value = next;
    detail.value = { ...selected, revision, item: { ...selected.item, catalogCount: presentation.total },
      scope: { ...selected.scope, revision: revision.revision, allDisabled: presentation.allDisabled,
        catalog: { ...selected.scope.catalog, modelCount: presentation.total } } };
    if (rail.value?.revision.processGeneration === revision.processGeneration && rail.value.revision.revision <= revision.revision) {
      rail.value = { ...rail.value, revision, items: rail.value.items.map(item => item.railKey === selected.item.railKey
        ? { ...item, catalogCount: presentation.total } : item) };
    }
  }

  function commitContracts(result: ProviderContractsResponse): void {
    const selected = detail.value?.scope;
    if (!selected) return;
    const scope = selected.scopeKind === "provider" ? result.providers.find(row => row.scope_id === selected.scopeId)
      : result.custom_endpoints.find(row => row.scope_id === selected.scopeId);
    const pricingRevision = result.pricing_revision ?? models.value?.revision.pricingRevision;
    if (!scope?.presentation || pricingRevision == null) return;
    commitPresentation(scope.presentation, { revision: result.revision, processGeneration: result.process_generation,
      pricingRevision });
  }

  function commitDestination(destination: Destination): void {
    if (detail.value?.destination?.id !== destination.id || !destination.presentation || !destination.presentation_revision) return;
    commitPresentation(destination.presentation, destination.presentation_revision);
  }

  function commitMetadata(snapshot: DestinationModelMetadataSnapshot): void {
    if (detail.value?.destination?.id !== snapshot.destination_id || !models.value
      || snapshot.expectation.processGeneration !== control.processGeneration) return;
    const entries = new Map(snapshot.models.map(entry => [entry.public_model, entry]));
    invalidate("models");
    models.value = { ...models.value, models: models.value.models.map(row => {
      const entry = entries.get(row.publicModel);
      return entry ? { ...row, metadata: entry.metadata, metadataSource: entry.source } : row;
    }) };
  }
  function clear(): void {
    session++;
    invalidate();
    identities.clear();
    rail.value = detail.value = models.value = editDetail.value = null;
    loading.value = {};
    errors.value = {};
  }
  watch(() => control.processGeneration, (next, previous) => {
    if (previous !== null && next !== previous) {
      // The response that announces this process is still a valid read.
      // Its body is accepted below; late bodies naming the old process are not.
      reads.invalidate();
      rail.value = detail.value = models.value = editDetail.value = null;
      errors.value = {};
    }
  }, { flush: "sync" });
  return { rail, detail, models, editDetail, loading, errors, loadRail, loadDetail, loadModels, loadEditDetail, hasIdentity, commitRemoval, commitContracts, commitDestination, commitMetadata, invalidate, clear };
});

export function clearProviderPage(): void { useProviderPageStore().clear(); }
