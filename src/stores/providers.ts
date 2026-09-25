import { computed, ref, shallowRef } from "vue";
import { defineStore } from "pinia";
import { connectionsApi, type Connection } from "../api/connections.ts";
import { isRevisionConflict } from "../api/dashboard.ts";
import { providerApi, type ProviderDefinitionView } from "../api/providers.ts";
import type {
  ContractScopeKind,
  EffectiveModelContract,
  ModelProtocolOverrideUpdate,
  ProviderCatalogEntry,
  ProviderContractsResponse,
} from "../api/providers.ts";
import type { MutationExpectation } from "../api/generated/dashboard-v3.ts";
import { applyModelContractToResponse, type ProviderScopeRef } from "../domain/provider-contracts.ts";

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

  /** Drop cached catalog/contracts/connections on 401 / logout. */
  function clear(): void {
    sessionGeneration += 1;
    catalogGeneration += 1;
    contractsGeneration += 1;
    connectionsGeneration += 1;
    definitionsGenerations.clear();
    catalog.value = null;
    contracts.value = null;
    connections.value = null;
    definitions.value = new Map();
    loading.value = false;
    error.value = "";
  }

  return {
    catalog: computed(() => catalog.value),
    contracts: computed(() => contracts.value),
    connections: computed(() => connections.value),
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
    loadDefinition,
    invalidateDefinition,
    loadContracts,
    refreshContractCatalog,
    removeContractCatalogModels,
    putModelProtocolOverrides,
    applyModelContract,
    clear,
  };
});
