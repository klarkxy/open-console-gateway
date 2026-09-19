import { computed, ref } from "vue";
import { defineStore } from "pinia";
import { connectionsApi, type Connection } from "../api/connections.ts";
import { isRevisionConflict } from "../api/dashboard.ts";
import { providerApi } from "../api/providers.ts";
import type {
  ContractScopeKind,
  EffectiveModelContract,
  ModelProtocolOverrideUpdate,
  ProviderCatalogEntry,
  ProviderContractsResponse,
} from "../api/providers.ts";
import { applyModelContractToResponse, type ProviderScopeRef } from "../domain/provider-contracts.ts";

/**
 * Provider catalog and contract fetches used by Providers and Aliases.
 * Probe progress and pricing refresh stay page-local.
 */
export const useProvidersStore = defineStore("providers", () => {
  const catalog = ref<ProviderCatalogEntry[] | null>(null);
  const contracts = ref<ProviderContractsResponse | null>(null);
  const connections = ref<Connection[] | null>(null);
  const loading = ref(false);
  const error = ref("");

  // Overlapping loads resolve out of order; only the latest request commits
  // state. Mutation responses bump the contracts generation so a slow pending
  // load cannot clobber fresher post-mutation state. Stale calls still
  // return/throw to their own caller unchanged.
  let catalogGeneration = 0;
  let contractsGeneration = 0;
  let connectionsGeneration = 0;
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
  ): Promise<ProviderContractsResponse> {
    const token = beginContractsMutation();
    try {
      const result = await providerApi.updateModelProtocolOverrides(scopeKind, scopeId, overrides);
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
    catalog.value = null;
    contracts.value = null;
    connections.value = null;
    loading.value = false;
    error.value = "";
  }

  return {
    catalog: computed(() => catalog.value),
    contracts: computed(() => contracts.value),
    connections: computed(() => connections.value),
    loading: computed(() => loading.value),
    error: computed(() => error.value),
    loadCatalog,
    loadConnections,
    loadContracts,
    refreshContractCatalog,
    removeContractCatalogModels,
    putModelProtocolOverrides,
    applyModelContract,
    clear,
  };
});
