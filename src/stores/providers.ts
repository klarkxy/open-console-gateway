import { computed, ref, shallowRef } from "vue";
import { defineStore } from "pinia";
import { connectionsApi, type Connection } from "../api/connections.ts";
import { isRevisionConflict } from "../api/dashboard.ts";
import { dashboardV4 } from "../api/dashboard-v4.ts";
import { providerApi, type ProviderDefinitionView } from "../api/providers.ts";
import type {
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

/**
 * Provider catalog and contract fetches used by Providers and Aliases.
 * Probe progress and pricing refresh stay page-local.
 */
export const useProvidersStore = defineStore("providers", () => {
  // Snapshots are always committed wholesale (immutable style), so shallow
  // refs skip the deep reactive wrap of these large payloads.
  const catalog = shallowRef<ProviderCatalogEntry[] | null>(null);
  const contracts = shallowRef<ProviderContractsResponse | null>(null);
  const connections = shallowRef<Connection[] | null>(null);
  const cpaModels = shallowRef<CpaCatalogEntry[] | null>(null);
  const definitions = shallowRef<Map<string, ProviderDefinitionView>>(new Map());
  const loading = ref(false);
  const error = ref("");

  // Overlapping loads resolve out of order; only the latest request commits
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

  interface ContractsMutationToken {
    session: number;
    invalidatedLoad: number;
  }

  function beginContractsMutation(): ContractsMutationToken {
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

  async function loadCatalog(): Promise<ProviderCatalogEntry[]> {
    const generation = ++catalogGeneration;
    const result = await providerApi.getProviderCatalog();
    if (generation !== catalogGeneration) return result;
    catalog.value = result;
    // Brand marks for preset-derived rows resolve through the persisted
    // preset id on the definition; warm those definitions in the background.
    for (const entry of result) {
      if (entry.origin !== "preset" || definitions.value.has(entry.provider_id)) continue;
      void loadDefinition(entry.provider_id).catch(() => {});
    }
    return result;
  }

  async function loadConnections(): Promise<Connection[]> {
    const generation = ++connectionsGeneration;
    const result = await connectionsApi.list();
    if (generation !== connectionsGeneration) return result;
    connections.value = result;
    return result;
  }

  async function loadCpaModels(): Promise<void> {
    const generation = ++cpaGeneration;
    const result = await dashboardV4.getCpaCatalog();
    if (generation === cpaGeneration) cpaModels.value = result.models;
  }

  async function loadContracts(): Promise<ProviderContractsResponse> {
    const generation = ++contractsGeneration;
    loading.value = true;
    try {
      const result = await providerApi.getProviderContracts();
      if (generation !== contractsGeneration) return result;
      contracts.value = result;
      error.value = "";
      return result;
    } catch (e) {
      if (generation === contractsGeneration) {
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

  async function loadDefinition(
    providerId: string,
    force = false,
  ): Promise<ProviderDefinitionView> {
    const cached = definitions.value.get(providerId);
    if (cached && !force) return cached;
    const generation = (definitionsGenerations.get(providerId) ?? 0) + 1;
    definitionsGenerations.set(providerId, generation);
    const session = sessionGeneration;
    const result = await providerApi.getProviderDefinition(providerId);
    if (definitionsGenerations.get(providerId) !== generation || session !== sessionGeneration) return result;
    const next = new Map(definitions.value);
    next.set(providerId, result);
    definitions.value = next;
    return result;
  }

  function invalidateDefinition(providerId: string): void {
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
  ): Promise<ProviderContractsResponse> {
    const token = beginContractsMutation();
    try {
      const result = await providerApi.removeContractCatalogModels(scopeKind, scopeId, modelIds);
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

  async function loadAliasPublication(): Promise<void> {
    const generation = ++aliasPublicationGeneration;
    const session = sessionGeneration;
    try {
      const result = await dashboardV4.getAliasPublication();
      if (generation !== aliasPublicationGeneration || session !== sessionGeneration) return;
      aliasUnpublished.value = result.unpublished;
      aliasPublicationLoadError.value = "";
    } catch (cause) {
      // A failed read keeps the last committed list (and its ready state).
      if (generation !== aliasPublicationGeneration || session !== sessionGeneration) return;
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
    const session = sessionGeneration;
    aliasPublicationPending.value = [...aliasPublicationPending.value, key];
    aliasPublicationOverlays.value = { ...aliasPublicationOverlays.value, [key]: published };
    try {
      const result = await useControlPlaneStore().runLocalMutation(
        `alias-publication:${key}`,
        (expectation) => dashboardV4.patchAliasPublication({ publicModel, published }, expectation),
      );
      if (session !== sessionGeneration) return;
      // Invalidate any load that started before this ordered receipt.
      aliasPublicationGeneration += 1;
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
    cpaModels.value = null;
    definitions.value = new Map();
    aliasUnpublished.value = null;
    aliasPublicationOverlays.value = {};
    aliasPublicationPending.value = [];
    aliasPublicationLoadError.value = "";
    aliasPublicationSaveError.value = "";
    loading.value = false;
    error.value = "";
  }

  return {
    catalog: computed(() => catalog.value),
    contracts: computed(() => contracts.value),
    connections: computed(() => connections.value),
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
