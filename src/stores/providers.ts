import { computed, ref, shallowRef, watch } from "vue";
import { defineStore } from "pinia";
import { connectionsApi, type Connection } from "../api/connections.ts";
import { isRevisionConflict } from "../api/dashboard.ts";
import { dashboardV4 } from "../api/dashboard-v4.ts";
import { providerApi, type ProviderDefinitionView } from "../api/providers.ts";
import type {
  ContractCatalogModelsRemoval,
  ContractScopeKind,
  EffectiveModelContract,
  ModelProtocolOverrideUpdate,
  ProviderCatalogEntry,
  ProviderContractsResponse,
} from "../api/providers.ts";
import type { MutationExpectation } from "../api/generated/dashboard-v3.ts";
import type { CpaCatalogEntry } from "../api/generated/dashboard-v4.ts";
import { applyModelContractToResponse, type ProviderScopeRef } from "../domain/provider-contracts.ts";
import { publicModelPublicationKey } from "../domain/provider-aliases.ts";
import { dashboardErrorDetail } from "../utils/errors.ts";
import { isLocalMutationCancelled, useControlPlaneStore } from "./controlPlane.ts";
import { dropSnapshot } from "./persistence.ts";
import { createReadLifecycle, PAGE_READ_MAX_AGE_MS, readProcessIsCurrent, readSnapshotIsCurrent, type ReadOptions } from "./readLifecycle.ts";

const SNAPSHOT_KEY = "providers";

/**
 * In-place projection of a confirmed V4 catalog removal onto cached
 * contracts. Mirrors the backend's own receipt-time compensation
 * (`restrict_provider_catalog_after_reload_failure`): catalog membership is
 * the receipt's post-removal list, and contract models survive only while
 * their `model_id` stays in that list. The receipt also carries the advanced
 * settings CAS revision.
 */
export function projectCatalogModelsRemoval(
  response: ProviderContractsResponse,
  scope: ProviderScopeRef,
  receipt: ContractCatalogModelsRemoval,
): ProviderContractsResponse {
  const retained = new Set(receipt.catalog_models);
  return {
    ...response,
    revision: receipt.revision,
    process_generation: receipt.process_generation,
    providers: response.providers.map((group) => (
      scope.scope_kind === "provider" && group.scope_kind === "provider" && group.scope_id === scope.scope_id
        ? {
          ...group,
          catalog: { ...group.catalog, models: [...receipt.catalog_models] },
          models: group.models.filter((model) => retained.has(model.model_id)),
        }
        : group
    )),
  };
}

/**
 * Provider catalog and contract fetches used by Providers and Aliases.
 * Probe progress stays page-local.
 */
export const useProvidersStore = defineStore("providers", () => {
  // Rust owns persisted business state; these shallow projections are session-local.
  dropSnapshot(SNAPSHOT_KEY);
  const catalog = shallowRef<ProviderCatalogEntry[] | null>(null);
  const contracts = shallowRef<ProviderContractsResponse | null>(null);
  const connections = shallowRef<Connection[] | null>(null);
  const connectionsExpectation = shallowRef<MutationExpectation | null>(null);
  const cpaModels = shallowRef<CpaCatalogEntry[] | null>(null);
  const definitions = shallowRef<Map<string, ProviderDefinitionView>>(new Map());
  const loading = ref(false);
  const error = ref("");

  // Identical reads share a flight; only the current generation commits
  // state. Mutation responses bump the contracts generation so a slow pending
  // load cannot clobber fresher post-mutation state. Stale calls still
  // return/throw to their own caller unchanged.
  let catalogGeneration = 0;
  let contractsGeneration = 0;
  let connectionsGeneration = 0;
  let cpaGeneration = 0;
  // Definition loads are per provider: concurrent loads for different
  // providers must not invalidate each other.
  const definitionsGenerations = new Map<string, number>();
  let sessionGeneration = 0;
  const reads = createReadLifecycle();
  const controlPlane = useControlPlaneStore();
  let backendEpoch = 0;
  watch(() => controlPlane.processGeneration, (_next, previous) => {
    if (previous === null) return;
    backendEpoch++;
    reads.invalidate();
  }, { flush: "sync" });

  /** Invalidate all projections affected by an external settings/identity write. */
  function invalidateReads(): void {
    reads.invalidate();
    catalogGeneration++;
    contractsGeneration++;
    connectionsGeneration++;
    cpaGeneration++;
    aliasPublicationGeneration++;
    for (const [id, generation] of definitionsGenerations) definitionsGenerations.set(id, generation + 1);
    loading.value = false;
  }

  interface ContractsMutationToken {
    session: number;
    invalidatedLoad: number;
  }

  function beginContractsMutation(): ContractsMutationToken {
    reads.invalidate("contracts");
    return {
      session: sessionGeneration,
      invalidatedLoad: ++contractsGeneration,
    };
  }

  function mutationSessionIsCurrent(token: ContractsMutationToken): boolean {
    return token.session === sessionGeneration;
  }

  function commitContractsMutation(
    token: ContractsMutationToken,
    result: ProviderContractsResponse,
  ): void {
    if (!mutationSessionIsCurrent(token)) return;
    if (result.process_generation !== controlPlane.processGeneration) return;
    // Settings revisions restart from a fresh random epoch with the backend.
    // Reject regression only when both snapshots came from that same process.
    if (
      contracts.value
      && result.process_generation === contracts.value.process_generation
      && result.revision < contracts.value.revision
    ) return;
    // A load may have started after this mutation. Its snapshot can predate
    // the committed mutation, so invalidate it before installing the receipt.
    contractsGeneration += 1;
    reads.invalidate("contracts");
    contracts.value = result;
    loading.value = false;
    error.value = "";
  }

  function failContractsMutation(token: ContractsMutationToken): void {
    if (!mutationSessionIsCurrent(token)) return;
    // Release only the load invalidated by this mutation. A newer load still
    // owns the loading flag and will clear it in its own finally block.
    if (contractsGeneration === token.invalidatedLoad) loading.value = false;
  }

  function shouldRecoverContractsConflict(token: ContractsMutationToken): boolean {
    return mutationSessionIsCurrent(token) && contractsGeneration === token.invalidatedLoad;
  }

  function loadCatalog(options?: ReadOptions): Promise<ProviderCatalogEntry[]> {
    return reads.run("catalog", options, () => catalog.value!, readCatalog);
  }

  async function readCatalog(): Promise<ProviderCatalogEntry[]> {
    const generation = ++catalogGeneration;
    const origin = controlPlane.processGeneration;
    const snapshot = await providerApi.getProviderCatalogSnapshot();
    const result = snapshot.catalog;
    if (generation !== catalogGeneration
      || !readSnapshotIsCurrent(snapshot.expectation.processGeneration, snapshot.expectation.expectedRevision, controlPlane, origin)) return result;
    catalog.value = result;
    reads.markSuccessful("catalog");
    // Brand marks for preset-derived rows resolve through the persisted
    // preset id on the definition; warm those definitions in the background.
    for (const entry of result) {
      if (entry.origin !== "preset" || definitions.value.has(entry.provider_id)) continue;
      void loadDefinition(entry.provider_id).catch(() => {});
    }
    return result;
  }

  function loadConnections(options?: ReadOptions): Promise<Connection[]> {
    return reads.run("connections", options, () => connections.value!, readConnections);
  }

  async function readConnections(): Promise<Connection[]> {
    const generation = ++connectionsGeneration;
    const origin = controlPlane.processGeneration;
    const snapshot = await connectionsApi.listSnapshot();
    const result = snapshot.connections;
    if (generation !== connectionsGeneration
      || !readSnapshotIsCurrent(snapshot.expectation.processGeneration, snapshot.expectation.expectedRevision, controlPlane, origin)) return result;
    connections.value = result;
    connectionsExpectation.value = snapshot.expectation;
    reads.markSuccessful("connections");
    return result;
  }

  function loadCpaModels(options?: ReadOptions): Promise<void> {
    return reads.run("cpa", options, () => undefined, readCpaModels);
  }

  async function readCpaModels(): Promise<void> {
    const generation = ++cpaGeneration;
    const origin = controlPlane.processGeneration;
    const result = await dashboardV4.getCpaCatalog();
    if (generation === cpaGeneration
      && readSnapshotIsCurrent(result.revision?.processGeneration, result.revision?.revision, controlPlane, origin)) {
      cpaModels.value = result.models;
      reads.markSuccessful("cpa");
    }
  }

  function loadContracts(options?: ReadOptions): Promise<ProviderContractsResponse> {
    return reads.run("contracts", options, () => contracts.value!, readContracts);
  }

  async function readContracts(): Promise<ProviderContractsResponse> {
    const generation = ++contractsGeneration;
    const backend = backendEpoch;
    const origin = controlPlane.processGeneration;
    loading.value = true;
    try {
      const result = await providerApi.getProviderContracts();
      if (generation !== contractsGeneration
        || !readSnapshotIsCurrent(result.process_generation, result.revision, controlPlane, origin)) return result;
      contracts.value = result;
      reads.markSuccessful("contracts");
      error.value = "";
      return result;
    } catch (e) {
      if (generation === contractsGeneration && backend === backendEpoch) {
        error.value = e instanceof Error ? e.message : String(e);
      }
      throw e;
    } finally {
      if (generation === contractsGeneration) loading.value = false;
    }
  }

  async function refreshContractCatalog(
    scopeKind: ContractScopeKind,
    scopeId: string,
  ): Promise<ProviderContractsResponse> {
    const token = beginContractsMutation();
    try {
      const result = await providerApi.refreshContractCatalog(scopeKind, scopeId);
      commitContractsMutation(token, result);
      return result;
    } catch (cause) {
      failContractsMutation(token);
      throw cause;
    }
  }

  function loadDefinition(
    providerId: string,
    forceOrOptions: boolean | ReadOptions = { maxAgeMs: PAGE_READ_MAX_AGE_MS },
  ): Promise<ProviderDefinitionView> {
    const options = typeof forceOrOptions === "boolean"
      ? { maxAgeMs: forceOrOptions ? 0 : Infinity }
      : forceOrOptions;
    return reads.run(`definition:${providerId}`, options, () => definitions.value.get(providerId)!, () => readDefinition(providerId));
  }

  async function readDefinition(providerId: string): Promise<ProviderDefinitionView> {
    const generation = (definitionsGenerations.get(providerId) ?? 0) + 1;
    definitionsGenerations.set(providerId, generation);
    const session = sessionGeneration;
    const result = await providerApi.getProviderDefinition(providerId);
    if (definitionsGenerations.get(providerId) !== generation || session !== sessionGeneration
      || result.process_generation !== controlPlane.processGeneration) return result;
    const next = new Map(definitions.value);
    next.set(providerId, result);
    definitions.value = next;
    reads.markSuccessful(`definition:${providerId}`);
    return result;
  }

  function invalidateDefinition(providerId: string): void {
    reads.invalidate(`definition:${providerId}`);
    definitionsGenerations.set(providerId, (definitionsGenerations.get(providerId) ?? 0) + 1);
    if (!definitions.value.has(providerId)) return;
    const next = new Map(definitions.value);
    next.delete(providerId);
    definitions.value = next;
  }

  async function editContractCatalogModel(
    scopeId: string, input: Parameters<typeof providerApi.editContractCatalogModel>[1], expectation: MutationExpectation,
  ): Promise<ProviderContractsResponse> {
    const token = beginContractsMutation();
    try {
      const result = await providerApi.editContractCatalogModel(scopeId, input, expectation);
      commitContractsMutation(token, result);
      return result;
    } catch (cause) { failContractsMutation(token); throw cause; }
  }

  async function addContractCatalogModels(
    scopeId: string, modelIds: string[], expectation: MutationExpectation,
  ): Promise<ProviderContractsResponse> {
    const token = beginContractsMutation();
    try {
      const result = await providerApi.addContractCatalogModels(scopeId, modelIds, expectation);
      commitContractsMutation(token, result);
      return result;
    } catch (cause) {
      failContractsMutation(token);
      throw cause;
    }
  }

  async function removeContractCatalogModels(
    scopeKind: ContractScopeKind,
    scopeId: string,
    modelIds: string[],
  ): Promise<ContractCatalogModelsRemoval> {
    const token = beginContractsMutation();
    try {
      const result = await providerApi.removeContractCatalogModels(scopeKind, scopeId, modelIds);
      commitCatalogModelsRemoval(token, { scope_kind: scopeKind, scope_id: scopeId }, result);
      return result;
    } catch (cause) {
      failContractsMutation(token);
      if (isRevisionConflict(cause) && shouldRecoverContractsConflict(token)) {
        await loadContracts();
      }
      throw cause;
    }
  }

  // A confirmed removal commits from its receipt, never from a re-fetch:
  // older reads are invalidated first so a slow pending load cannot restore
  // the deleted rows, and a same-process revision regression is rejected.
  function commitCatalogModelsRemoval(
    token: ContractsMutationToken,
    scope: ProviderScopeRef,
    receipt: ContractCatalogModelsRemoval,
  ): void {
    if (!mutationSessionIsCurrent(token)) return;
    if (receipt.process_generation !== controlPlane.processGeneration) return;
    if (
      contracts.value
      && receipt.process_generation === contracts.value.process_generation
      && receipt.revision < contracts.value.revision
    ) return;
    contractsGeneration += 1;
    reads.invalidate("contracts");
    if (contracts.value) {
      contracts.value = projectCatalogModelsRemoval(contracts.value, scope, receipt);
    }
    loading.value = false;
    error.value = "";
  }

  async function putModelProtocolOverrides(
    scopeKind: ContractScopeKind,
    scopeId: string,
    overrides: ModelProtocolOverrideUpdate[],
    authorizeCredentialIds?: string[],
    capturedExpectation?: MutationExpectation,
  ): Promise<ProviderContractsResponse> {
    const token = beginContractsMutation();
    try {
      const result = await providerApi.updateModelProtocolOverrides(
        scopeKind,
        scopeId,
        overrides,
        authorizeCredentialIds,
        capturedExpectation,
      );
      commitContractsMutation(token, result);
      return result;
    } catch (cause) {
      failContractsMutation(token);
      if (isRevisionConflict(cause) && shouldRecoverContractsConflict(token)) {
        await loadContracts();
      }
      throw cause;
    }
  }

  // A successful probe returns the effective contract of one model; merge it
  // in place and invalidate pending loads like any other mutation commit.
  function applyModelContract(scope: ProviderScopeRef, contract: EffectiveModelContract): void {
    reads.invalidate("contracts");
    if (!contracts.value) return;
    contractsGeneration += 1;
    contracts.value = applyModelContractToResponse(contracts.value, scope, contract);
    loading.value = false;
  }

  // --- Alias publication ---------------------------------------------------
  // The authoritative hidden-name list lives here; views render it plus
  // per-name optimistic overlays. Writes go through the control-plane local
  // lane so rapid toggles on different rows serialize on fresh CAS tokens
  // instead of self-conflicting, and each receipt commits only when the
  // session is still current.
  const aliasUnpublished = shallowRef<string[] | null>(null);
  const aliasPublicationOverlays = ref<Readonly<Record<string, boolean>>>({});
  const aliasPublicationPending = ref<readonly string[]>([]);
  const aliasPublicationLoadError = ref("");
  const aliasPublicationSaveError = ref("");
  let aliasPublicationGeneration = 0;

  /** Effective hidden-name list: authoritative server state plus overlays. */
  const effectiveAliasUnpublished = computed((): string[] => {
    let next = aliasUnpublished.value ?? [];
    for (const [key, published] of Object.entries(aliasPublicationOverlays.value)) {
      const hidden = next.some((name) => publicModelPublicationKey(name) === key);
      if (published && hidden) {
        next = next.filter((name) => publicModelPublicationKey(name) !== key);
      } else if (!published && !hidden) {
        next = [...next, key];
      }
    }
    return next;
  });

  function loadAliasPublication(options?: ReadOptions): Promise<void> {
    return reads.run("aliasPublication", options, () => undefined, readAliasPublication);
  }

  async function readAliasPublication(): Promise<void> {
    const generation = ++aliasPublicationGeneration;
    const session = sessionGeneration;
    const backend = backendEpoch;
    const origin = controlPlane.processGeneration;
    try {
      const result = await dashboardV4.getAliasPublication();
      if (generation !== aliasPublicationGeneration || session !== sessionGeneration
        || !readSnapshotIsCurrent(result.revision?.processGeneration, result.revision?.revision, controlPlane, origin)) return;
      aliasUnpublished.value = result.unpublished;
      reads.markSuccessful("aliasPublication");
      aliasPublicationLoadError.value = "";
    } catch (cause) {
      // A failed read keeps the last committed list (and its ready state).
      if (generation !== aliasPublicationGeneration || session !== sessionGeneration || backend !== backendEpoch) return;
      aliasPublicationLoadError.value = dashboardErrorDetail(cause);
    }
  }

  function dropAliasPublicationOverlay(key: string): void {
    if (!(key in aliasPublicationOverlays.value)) return;
    const next = { ...aliasPublicationOverlays.value };
    delete next[key];
    aliasPublicationOverlays.value = next;
  }

  async function setAliasPublished(publicModel: string, published: boolean): Promise<void> {
    const key = publicModelPublicationKey(publicModel);
    if (aliasPublicationPending.value.includes(key)) return;
    reads.invalidate("aliasPublication");
    aliasPublicationGeneration++;
    const session = sessionGeneration;
    const origin = controlPlane.processGeneration;
    aliasPublicationPending.value = [...aliasPublicationPending.value, key];
    aliasPublicationOverlays.value = { ...aliasPublicationOverlays.value, [key]: published };
    try {
      const result = await useControlPlaneStore().runLocalMutation(
        `alias-publication:${key}`,
        (expectation) => dashboardV4.patchAliasPublication({ publicModel, published }, expectation),
      );
      if (session !== sessionGeneration) return;
      if (!readProcessIsCurrent(result.revision?.processGeneration, controlPlane.processGeneration, origin)) {
        dropAliasPublicationOverlay(key);
        return;
      }
      // Invalidate any load that started before this ordered receipt.
      aliasPublicationGeneration += 1;
      reads.invalidate("aliasPublication");
      aliasUnpublished.value = result.unpublished;
      dropAliasPublicationOverlay(key);
      aliasPublicationSaveError.value = "";
    } catch (cause) {
      if (session !== sessionGeneration) return;
      // Failure reconciles only this row's overlay; another row's accepted
      // or optimistic presentation is never restored away.
      dropAliasPublicationOverlay(key);
      if (isLocalMutationCancelled(cause)) return;
      if (isRevisionConflict(cause)) {
        // Reconcile from the server; a failed reconciliation keeps the
        // reverted (authoritative) presentation.
        void loadAliasPublication();
      }
      aliasPublicationSaveError.value = dashboardErrorDetail(cause);
    } finally {
      if (session === sessionGeneration) {
        aliasPublicationPending.value = aliasPublicationPending.value.filter((entry) => entry !== key);
      }
    }
  }

  /** Drop cached catalog/contracts/connections on 401 / logout. */
  function clear(): void {
    reads.invalidate();
    sessionGeneration += 1;
    catalogGeneration += 1;
    contractsGeneration += 1;
    connectionsGeneration += 1;
    cpaGeneration += 1;
    aliasPublicationGeneration += 1;
    definitionsGenerations.clear();
    catalog.value = null;
    contracts.value = null;
    connections.value = null;
    connectionsExpectation.value = null;
    cpaModels.value = null;
    definitions.value = new Map();
    aliasUnpublished.value = null;
    aliasPublicationOverlays.value = {};
    aliasPublicationPending.value = [];
    aliasPublicationLoadError.value = "";
    aliasPublicationSaveError.value = "";
    loading.value = false;
    error.value = "";
    dropSnapshot(SNAPSHOT_KEY);
  }

  /** Complete authoritative resources only; summary rows cannot seed these caches. */
  function commitReadProjection(projection: {
    catalog?: ProviderCatalogEntry[];
    contracts?: ProviderContractsResponse;
    connections?: Connection[];
    expectation?: MutationExpectation;
    definitions?: ProviderDefinitionView[];
  }): void {
    if (projection.catalog) {
      catalogGeneration++;
      reads.invalidate("catalog");
      catalog.value = projection.catalog;
      reads.markSuccessful("catalog");
    }
    if (projection.contracts) {
      contractsGeneration++;
      reads.invalidate("contracts");
      contracts.value = projection.contracts;
      loading.value = false;
      error.value = "";
      reads.markSuccessful("contracts");
    }
    if (projection.connections) {
      connectionsGeneration++;
      reads.invalidate("connections");
      connections.value = projection.connections;
      connectionsExpectation.value = projection.expectation ?? null;
      reads.markSuccessful("connections");
    }
    for (const definition of projection.definitions ?? []) {
      invalidateDefinition(definition.id);
      const next = new Map(definitions.value);
      next.set(definition.id, definition);
      definitions.value = next;
      reads.markSuccessful(`definition:${definition.id}`);
    }
  }

  return {
    catalog: computed(() => catalog.value),
    contracts: computed(() => contracts.value),
    connections: computed(() => connections.value),
    connectionsExpectation: computed(() => connectionsExpectation.value),
    cpaModels: computed(() => cpaModels.value),
    definitions: computed(() => definitions.value),
    presetIds: computed(() => {
      const map = new Map<string, string | null>();
      for (const [providerId, definition] of definitions.value) {
        map.set(providerId, definition.preset_id ?? null);
      }
      return map;
    }),
    loading: computed(() => loading.value),
    error: computed(() => error.value),
    loadCatalog,
    loadConnections,
    loadCpaModels,
    loadDefinition,
    commitReadProjection,
    invalidateReads,
    invalidateDefinition,
    loadContracts,
    refreshContractCatalog,
    editContractCatalogModel,
    addContractCatalogModels,
    removeContractCatalogModels,
    putModelProtocolOverrides,
    applyModelContract,
    aliasUnpublished: effectiveAliasUnpublished,
    aliasPublicationReady: computed(() => aliasUnpublished.value !== null),
    aliasPublicationPending: computed(() => aliasPublicationPending.value),
    aliasPublicationLoadError: computed(() => aliasPublicationLoadError.value),
    aliasPublicationSaveError: computed(() => aliasPublicationSaveError.value),
    loadAliasPublication,
    setAliasPublished,
    clear,
  };
});
