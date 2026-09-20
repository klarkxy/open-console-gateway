import { computed, ref } from "vue";
import { defineStore } from "pinia";
import { DashboardRequestError } from "../api/dashboard-v3.ts";
import { isRevisionConflict } from "../api/dashboard.ts";
import {
  credentialsApi,
  destinationsApi,
  routingApi,
  type Destination,
  type DestinationCredential,
  type DestinationPatchInput,
  type RoutingExplanationView,
} from "../api/destinations.ts";
import type { MutationExpectation } from "../api/generated/dashboard-v3.ts";
import type {
  MappingErrorCodeDto,
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

const SNAPSHOT_CONSISTENCY_ATTEMPTS = 3;

function sameControlExpectation(
  left: MutationExpectation,
  right: MutationExpectation,
): boolean {
  return left.expectedRevision === right.expectedRevision
    && left.processGeneration === right.processGeneration;
}

/**
 * Single owner of the V4 destination / credential projection. Views never
 * group from platform links; a pending load can never clobber a newer
 * snapshot, and a 409 refusal keeps the last successful lists on screen.
 */
export const useDestinationsStore = defineStore("destinations", () => {
  const destinations = ref<Destination[]>([]);
  const credentials = ref<DestinationCredential[]>([]);
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
  }

  /** Invalidate any load that started before this write. */
  function beginDestinationMutation(): DestinationMutationToken {
    loadGeneration += 1;
    loading.value = false;
    return { session: sessionGeneration };
  }

  function mutationSessionIsCurrent(token: DestinationMutationToken): boolean {
    return token.session === sessionGeneration;
  }

  function mutationExpectationIsCurrent(next: MutationExpectation): boolean {
    return !expectation.value
      || next.processGeneration !== expectation.value.processGeneration
      || next.expectedRevision >= expectation.value.expectedRevision;
  }

  /** A write receipt wins over loads that started while that write was pending. */
  function beginMutationCommit(
    token: DestinationMutationToken,
    next: MutationExpectation,
  ): boolean {
    if (!mutationSessionIsCurrent(token) || !mutationExpectationIsCurrent(next)) return false;
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

  function applySnapshot(
    nextDestinations: Destination[],
    nextCredentials: DestinationCredential[],
    nextExpectation: MutationExpectation,
  ): void {
    destinations.value = nextDestinations;
    credentials.value = nextCredentials;
    expectation.value = nextExpectation;
    refusals.value = [];
    loaded.value = true;
    error.value = "";
  }

  /** Commit a fresh pair and invalidate in-flight loads, like an in-place mutation. */
  function commitSnapshot(
    nextDestinations: Destination[],
    nextCredentials: DestinationCredential[],
    nextExpectation: MutationExpectation,
  ): void {
    loadGeneration += 1;
    loading.value = false;
    applySnapshot(nextDestinations, nextCredentials, nextExpectation);
  }

  async function load(): Promise<void> {
    const generation = ++loadGeneration;
    loading.value = true;
    try {
      let destinationSnapshot: Awaited<ReturnType<typeof destinationsApi.listSnapshot>> | undefined;
      let credentialSnapshot: Awaited<ReturnType<typeof credentialsApi.listSnapshot>> | undefined;
      for (let attempt = 0; attempt < SNAPSHOT_CONSISTENCY_ATTEMPTS; attempt++) {
        const pair = await Promise.all([
          destinationsApi.listSnapshot(),
          credentialsApi.listSnapshot(),
        ]);
        if (generation !== loadGeneration) return;
        destinationSnapshot = pair[0];
        credentialSnapshot = pair[1];
        if (sameControlExpectation(destinationSnapshot.expectation, credentialSnapshot.expectation)) {
          break;
        }
        destinationSnapshot = undefined;
        credentialSnapshot = undefined;
      }
      if (generation !== loadGeneration) return;
      if (!destinationSnapshot || !credentialSnapshot) {
        throw new Error("destination credential snapshot mismatch");
      }
      applySnapshot(
        destinationSnapshot.destinations,
        credentialSnapshot.credentials,
        destinationSnapshot.expectation,
      );
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

  /** Delete an empty destination and drop it (and any stale rows) locally. */
  async function deleteDestination(id: string): Promise<void> {
    const token = beginDestinationMutation();
    try {
      const nextExpectation = await destinationsApi.delete(id, expectation.value ?? undefined);
      if (beginMutationCommit(token, nextExpectation)) {
        destinations.value = destinations.value.filter((destination) => destination.id !== id);
        credentials.value = credentials.value.filter((credential) => credential.destination_id !== id);
        expectation.value = nextExpectation;
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
    load,
    refreshAfterMutation,
    commitSnapshot,
    patchDestination,
    deleteDestination,
    explainKey,
    explainRouting,
    clear,
  };
});
