import { computed, ref } from "vue";
import { defineStore } from "pinia";
import { DashboardRequestError } from "../api/dashboard-v3.ts";
import {
  credentialsApi,
  destinationsApi,
  type Destination,
  type DestinationCredential,
} from "../api/destinations.ts";
import type { MutationExpectation } from "../api/generated/dashboard-v3.ts";
import type {
  MappingErrorCodeDto,
  RefusedRowKindDto,
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
  const destinations = ref<Destination[]>([]);
  const credentials = ref<DestinationCredential[]>([]);
  const expectation = ref<MutationExpectation | null>(null);
  const loaded = ref(false);
  const loading = ref(false);
  const error = ref("");
  const refusals = ref<DestinationProjectionRefusal[]>([]);

  let loadGeneration = 0;

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

  async function load(): Promise<void> {
    const generation = ++loadGeneration;
    loading.value = true;
    try {
      const [destinationSnapshot, credentialSnapshot] = await Promise.all([
        destinationsApi.listSnapshot(),
        credentialsApi.listSnapshot(),
      ]);
      if (generation !== loadGeneration) return;
      destinations.value = destinationSnapshot.destinations;
      credentials.value = credentialSnapshot.credentials;
      expectation.value = destinationSnapshot.expectation;
      refusals.value = [];
      loaded.value = true;
      error.value = "";
    } catch (e) {
      if (generation === loadGeneration) {
        error.value = e instanceof Error ? e.message : String(e);
        if (isDestinationProjectionRefused(e)) {
          refusals.value = refusalsFromError(e);
        }
      }
      throw e;
    } finally {
      if (generation === loadGeneration) loading.value = false;
    }
  }

  /** Drop the cached projection on 401 / logout so the next session reloads fresh. */
  function clear(): void {
    loadGeneration++;
    destinations.value = [];
    credentials.value = [];
    expectation.value = null;
    loaded.value = false;
    loading.value = false;
    error.value = "";
    refusals.value = [];
  }

  return {
    destinations: computed(() => destinations.value),
    credentials: computed(() => credentials.value),
    expectation: computed(() => expectation.value),
    loaded: computed(() => loaded.value),
    loading: computed(() => loading.value),
    error: computed(() => error.value),
    refusals: computed(() => refusals.value),
    byId: destinationsById,
    credentialsByLegacyAccountId,
    load,
    clear,
  };
});
