import { computed, ref, shallowRef } from "vue";
import { defineStore } from "pinia";
import { DashboardRequestError } from "../api/dashboard-v3.ts";
import { isRevisionConflict } from "../api/dashboard.ts";
import {
  credentialsApi,
  destinationsApi,
  routingApi,
  routingCardsApi,
  type Destination,
  type DestinationCatalogUpdateInput,
  type DestinationCredential,
  type DestinationPatchInput,
  type RoutingCardListSnapshot,
  type RoutingCardView,
  type RoutingExplanationView,
} from "../api/destinations.ts";
import type { MutationExpectation } from "../api/generated/dashboard-v3.ts";
import type {
  MappingErrorCodeDto,
  ProtocolDto,
  RefusedRowKindDto,
  RoutingClientProtocol,
} from "../api/generated/dashboard-v4.ts";

export interface DestinationProjectionRefusal {
  kind: RefusedRowKindDto | string;
  id: string;
  providerId: string | null;
  error: MappingErrorCodeDto | string;
  detail: string;
}

function isDestinationProjectionRefused(error: unknown): error is DashboardRequestError {
  return error instanceof DashboardRequestError
    && error.status === 409
    && error.code === "destinationProjectionRefused";
}

function presentRefusal(value: unknown): DestinationProjectionRefusal | null {
  if (!value || typeof value !== "object") return null;
  const record = value as {
    detail?: unknown;
    error?: unknown;
    row?: { kind?: unknown; id?: unknown; providerId?: unknown };
  };
  const id = typeof record.row?.id === "string" ? record.row.id : "";
  if (!id) return null;
  return {
    kind: typeof record.row?.kind === "string" ? record.row.kind : "",
    id,
    providerId: typeof record.row?.providerId === "string" ? record.row.providerId : null,
    error: typeof record.error === "string" ? record.error : "",
    detail: typeof record.detail === "string" ? record.detail : "",
  };
}

function refusalsFromError(error: DashboardRequestError): DestinationProjectionRefusal[] {
  return error.details
    .map(presentRefusal)
    .filter((row): row is DestinationProjectionRefusal => row !== null);
}

/**
 * Single owner of the V4 destination / credential projection. Views never
 * group from platform links; a pending load can never clobber a newer
 * snapshot, and a 409 refusal keeps the last successful lists on screen.
 */
export const useDestinationsStore = defineStore("destinations", () => {
  // Every write path replaces these arrays wholesale (applySnapshot, map /
  // filter commits), so shallow refs are sufficient and skip deep
  // traversal of the largest lists in the projection.
  const destinations = shallowRef<Destination[]>([]);
  const credentials = shallowRef<DestinationCredential[]>([]);
  const cards = shallowRef<RoutingCardView[]>([]);
  const expectation = ref<MutationExpectation | null>(null);
  const loaded = ref(false);
  const loading = ref(false);
  const error = ref("");
  const refusals = ref<DestinationProjectionRefusal[]>([]);

  // On-demand routing explanations, keyed by `explainKey(model, protocol)`.
  const explanations = ref<Record<string, RoutingExplanationView>>({});
  const explainLoading = ref<Record<string, boolean>>({});
  const explainErrors = ref<Record<string, string>>({});

  let loadGeneration = 0;
  // Bumped by `clear` so an explanation resolving after logout never commits.
  let sessionGeneration = 0;
  const explainRequests = new Map<string, number>();

  interface DestinationMutationToken {
    session: number;
    processGeneration: number | null;
  }

  /** Invalidate any load that started before this write. */
  function beginDestinationMutation(): DestinationMutationToken {
    loadGeneration += 1;
    loading.value = false;
    return {
      session: sessionGeneration,
      processGeneration: expectation.value?.processGeneration ?? null,
    };
  }

  function mutationSessionIsCurrent(token: DestinationMutationToken): boolean {
    return token.session === sessionGeneration;
  }

  /**
   * Same-process receipts are revision-monotonic. Process identity is opaque:
   * a snapshot from a different process than this write started under (a
   * restart) invalidates the in-flight receipt. Do not order generations.
   */
  function mutationExpectationIsCurrent(
    token: DestinationMutationToken,
    next: MutationExpectation,
  ): boolean {
    const current = expectation.value;
    if (!current) return true;
    if (token.processGeneration !== current.processGeneration) return false;
    if (next.processGeneration !== current.processGeneration) return false;
    return next.expectedRevision >= current.expectedRevision;
  }

  /** A write receipt wins over loads that started while that write was pending. */
  function beginMutationCommit(
    token: DestinationMutationToken,
    next: MutationExpectation,
  ): boolean {
    if (!mutationSessionIsCurrent(token) || !mutationExpectationIsCurrent(token, next)) {
      return false;
    }
    loadGeneration += 1;
    loading.value = false;
    return true;
  }

  const destinationsById = computed(() => {
    const map = new Map<string, Destination>();
    for (const destination of destinations.value) map.set(destination.id, destination);
    return map;
  });

  const credentialsByLegacyAccountId = computed(() => {
    const map = new Map<string, DestinationCredential>();
    for (const credential of credentials.value) map.set(credential.legacy_account_id, credential);
    return map;
  });

  function destinationForAccount(accountId: string): Destination | null {
    const credential = credentialsByLegacyAccountId.value.get(accountId);
    return credential ? destinationsById.value.get(credential.destination_id) ?? null : null;
  }

  function applySnapshot(
    nextDestinations: Destination[],
    nextCredentials: DestinationCredential[],
    nextCards: RoutingCardView[],
    nextExpectation: MutationExpectation,
  ): void {
    destinations.value = nextDestinations;
    credentials.value = nextCredentials;
    cards.value = nextCards;
    expectation.value = nextExpectation;
    refusals.value = [];
    loaded.value = true;
    error.value = "";
  }

  /** Commit a fresh snapshot and invalidate in-flight loads, like an in-place mutation. */
  function commitSnapshot(snapshot: RoutingCardListSnapshot): void {
    loadGeneration += 1;
    loading.value = false;
    applySnapshot(snapshot.destinations, snapshot.credentials, snapshot.cards, snapshot.expectation);
  }

  async function load(): Promise<void> {
    const generation = ++loadGeneration;
    loading.value = true;
    try {
      const snapshot = await routingCardsApi.listSnapshot();
      if (generation !== loadGeneration) return;
      applySnapshot(snapshot.destinations, snapshot.credentials, snapshot.cards, snapshot.expectation);
    } catch (e) {
      if (generation === loadGeneration) {
        if (isDestinationProjectionRefused(e)) {
          error.value = e instanceof Error ? e.message : String(e);
          refusals.value = refusalsFromError(e);
        } else if (!loaded.value) {
          error.value = e instanceof Error ? e.message : String(e);
        }
      }
      throw e;
    } finally {
      if (generation === loadGeneration) loading.value = false;
    }
  }

  /** Generation-guarded reload after a mutation. Keeps the last snapshot on failure. */
  async function refreshAfterMutation(): Promise<void> {
    await load();
  }

  async function refreshCatalog(id: string) {
    const token = beginDestinationMutation();
    try {
      const result = await destinationsApi.refreshCatalog(id, expectation.value ?? undefined);
      if (beginMutationCommit(token, result.expectation)) {
        destinations.value = destinations.value.map((destination) => (
          destination.id === id ? result.destination : destination
        ));
        expectation.value = result.expectation;
      }
      return result;
    } catch (cause) {
      if (isRevisionConflict(cause) && mutationSessionIsCurrent(token)) {
        await refreshAfterMutation();
      }
      throw cause;
    }
  }

  /**
   * PUT catalog enablement / protocol / preferred / removals. Commits the
   * destination and credential receipt in place. Probe is a separate route.
   */
  async function updateCatalog(
    id: string,
    input: DestinationCatalogUpdateInput,
    capturedExpectation?: MutationExpectation,
  ): Promise<Destination> {
    const token = beginDestinationMutation();
    try {
      const result = await destinationsApi.updateCatalog(
        id,
        input,
        capturedExpectation ?? expectation.value ?? undefined,
      );
      if (beginMutationCommit(token, result.expectation)) {
        destinations.value = destinations.value.map((destination) => (
          destination.id === id ? result.destination : destination
        ));
        credentials.value = result.credentials;
        expectation.value = result.expectation;
      }
      return result.destination;
    } catch (cause) {
      if (isRevisionConflict(cause) && mutationSessionIsCurrent(token)) {
        await refreshAfterMutation();
      }
      throw cause;
    }
  }

  /**
   * POST a bounded model/protocol probe. Updates the CAS pair from the
   * receipt and never rewrites catalog enablement. HTTP 200 with ok=false is
   * a completed observation, not a thrown transport failure.
   */
  async function testModel(
    id: string,
    publicModel: string,
    protocol: ProtocolDto,
    capturedExpectation?: MutationExpectation,
  ) {
    const token = beginDestinationMutation();
    try {
      const result = await destinationsApi.testModel(
        id,
        publicModel,
        protocol,
        capturedExpectation ?? expectation.value ?? undefined,
      );
      if (beginMutationCommit(token, result.expectation)) {
        expectation.value = result.expectation;
      }
      return result;
    } catch (cause) {
      if (isRevisionConflict(cause) && mutationSessionIsCurrent(token)) {
        await refreshAfterMutation();
      }
      throw cause;
    }
  }

  /**
   * PATCH one destination and commit the returned row in place. A CAS
   * conflict reloads the projection before rethrowing so the editor can show
   * the conflict against fresh state; the mutation is never replayed.
   */
  async function patchDestination(
    id: string,
    input: DestinationPatchInput,
    capturedExpectation?: MutationExpectation,
  ): Promise<Destination> {
    const token = beginDestinationMutation();
    try {
      const result = await destinationsApi.patch(
        id,
        input,
        capturedExpectation ?? expectation.value ?? undefined,
      );
      if (beginMutationCommit(token, result.expectation)) {
        destinations.value = destinations.value.map((destination) => (
          destination.id === id ? result.destination : destination
        ));
        credentials.value = result.credentials;
        expectation.value = result.expectation;
      }
      return result.destination;
    } catch (cause) {
      if (isRevisionConflict(cause) && mutationSessionIsCurrent(token)) {
        await refreshAfterMutation();
      }
      throw cause;
    }
  }

  /**
   * POST quota-retry for one Key and merge the returned credential in place.
   * Does not rewrite siblings, pools, enablement, or cards. A CAS conflict
   * reloads the projection before rethrowing and is never replayed.
   */
  async function retryQuotaRecovery(
    id: string,
    capturedExpectation?: MutationExpectation,
  ): Promise<DestinationCredential> {
    const token = beginDestinationMutation();
    try {
      const result = await credentialsApi.retryQuota(
        id,
        capturedExpectation ?? expectation.value ?? undefined,
      );
      if (beginMutationCommit(token, result.expectation)) {
        credentials.value = credentials.value.map((credential) => (
          credential.id === id ? result.credential : credential
        ));
        expectation.value = result.expectation;
      }
      return result.credential;
    } catch (cause) {
      if (isRevisionConflict(cause) && mutationSessionIsCurrent(token)) {
        await refreshAfterMutation();
      }
      throw cause;
    }
  }

  /** Delete an empty destination and drop it (and any stale rows) locally. */
  async function deleteDestination(id: string): Promise<void> {
    const token = beginDestinationMutation();
    try {
      const nextExpectation = await destinationsApi.delete(id, expectation.value ?? undefined);
      if (beginMutationCommit(token, nextExpectation)) {
        destinations.value = destinations.value.filter((destination) => destination.id !== id);
        credentials.value = credentials.value.filter((credential) => credential.destination_id !== id);
        cards.value = cards.value.filter((card) => card.destination_id !== id);
        expectation.value = nextExpectation;
      }
    } catch (cause) {
      if (isRevisionConflict(cause) && mutationSessionIsCurrent(token)) {
        await refreshAfterMutation();
      }
      throw cause;
    }
  }

  /**
   * Submit a full routing card layout (grouping + flattened rank) under CAS.
   * The committed snapshot is written back in place so the visible order and
   * ranks stay consistent. A 409 conflict reloads the projection and is never
   * replayed; any other failure leaves the last committed layout on screen.
   */
  async function replaceRoutingCardLayout(
    input: { id: string; destinationId: string; credentialIds: string[] }[],
    capturedExpectation?: MutationExpectation,
  ): Promise<void> {
    const token = beginDestinationMutation();
    try {
      const snapshot = await routingCardsApi.replace(
        { cards: input },
        capturedExpectation ?? expectation.value ?? undefined,
      );
      if (beginMutationCommit(token, snapshot.expectation)) {
        commitSnapshot(snapshot);
      }
    } catch (cause) {
      if (isRevisionConflict(cause) && mutationSessionIsCurrent(token)) {
        await refreshAfterMutation();
      }
      throw cause;
    }
  }

  /** Per-model explain cache key: one pending request per (model, protocol). */
  function explainKey(model: string, clientProtocol: RoutingClientProtocol): string {
    return `${clientProtocol} ${model.trim().toLocaleLowerCase()}`;
  }

  /**
   * On-demand `GET /routing/explain`. Repeating a key re-fetches; only the
   * latest request per key commits, and nothing commits after `clear`.
   */
  async function explainRouting(
    model: string,
    clientProtocol: RoutingClientProtocol,
  ): Promise<RoutingExplanationView> {
    const key = explainKey(model, clientProtocol);
    const requestId = (explainRequests.get(key) ?? 0) + 1;
    explainRequests.set(key, requestId);
    const session = sessionGeneration;
    const owns = () => session === sessionGeneration && explainRequests.get(key) === requestId;
    explainLoading.value = { ...explainLoading.value, [key]: true };
    try {
      const result = await routingApi.explain(model, clientProtocol);
      if (!owns()) return result;
      explanations.value = { ...explanations.value, [key]: result };
      const nextErrors = { ...explainErrors.value };
      delete nextErrors[key];
      explainErrors.value = nextErrors;
      return result;
    } catch (e) {
      if (owns()) {
        explainErrors.value = {
          ...explainErrors.value,
          [key]: e instanceof Error ? e.message : String(e),
        };
      }
      throw e;
    } finally {
      if (owns()) {
        const nextLoading = { ...explainLoading.value };
        delete nextLoading[key];
        explainLoading.value = nextLoading;
      }
    }
  }

  /** Drop the cached projection on 401 / logout so the next session reloads fresh. */
  function clear(): void {
    loadGeneration++;
    sessionGeneration++;
    explainRequests.clear();
    destinations.value = [];
    credentials.value = [];
    cards.value = [];
    expectation.value = null;
    loaded.value = false;
    loading.value = false;
    error.value = "";
    refusals.value = [];
    explanations.value = {};
    explainLoading.value = {};
    explainErrors.value = {};
  }

  return {
    destinations: computed(() => destinations.value),
    credentials: computed(() => credentials.value),
    cards: computed(() => cards.value),
    expectation: computed(() => expectation.value),
    loaded: computed(() => loaded.value),
    loading: computed(() => loading.value),
    error: computed(() => error.value),
    refusals: computed(() => refusals.value),
    explanations: computed(() => explanations.value),
    explainLoading: computed(() => explainLoading.value),
    explainErrors: computed(() => explainErrors.value),
    byId: destinationsById,
    credentialsByLegacyAccountId,
    destinationForAccount,
    load,
    refreshAfterMutation,
    commitSnapshot,
    patchDestination,
    refreshCatalog,
    updateCatalog,
    testModel,
    retryQuotaRecovery,
    deleteDestination,
    replaceRoutingCardLayout,
    explainKey,
    explainRouting,
    clear,
  };
});
