import { computed, ref } from "vue";
import { defineStore, getActivePinia } from "pinia";
import {
  DashboardConflictError,
  DashboardRequestError,
  dashboardApi,
} from "../api/dashboard.ts";
import type { AuthStatus } from "../api/generated/dashboard-v3.ts";
import { useControlPlaneStore } from "./controlPlane.ts";
import { dropAllSnapshots } from "./persistence.ts";

export type SessionPhase = "checking" | "login" | "register" | "ready";

/**
 * Dashboard session: auth status, login/register/logout, and the global 401 /
 * 410 reactions.
 *
 * - A 401 surfaced by any V3 call dispatches the auth-required event (kept
 *   from the V2 bus); `handleAuthRequired` drops the session back to the
 *   login screen and wipes the memory-only connection secrets.
 * - A 410 `gone` means the loaded page predates the running service; the
 *   transport dispatches a separate gone event with structured
 *   refresh/upgrade guidance that the shell turns into a banner.
 * - `sessionEpoch` is the shared invalidation contract: `dropSession`
 *   increments it synchronously, and every async auth continuation captured
 *   before the bump must settle without reviving the dropped session.
 */
export const useSessionStore = defineStore("session", () => {
  const controlPlane = useControlPlaneStore();

  const phase = ref<SessionPhase>("checking");
  const status = ref<AuthStatus | null>(null);
  /** Mirrors App.vue's legacy flag: suppresses one auth-required dispatch during logout. */
  const suppressAuthRequired = ref(false);
  // Readonly to the outside; only dropSession advances it.
  const epoch = ref(0);

  const localMode = computed(() => status.value?.local ?? false);
  const authenticated = computed(() => status.value?.authenticated ?? false);

  function applyStatus(next: AuthStatus): void {
    status.value = next;
    phase.value = next.authenticated ? "ready" : next.initialized ? "login" : "register";
  }

  async function loadStatus(): Promise<AuthStatus> {
    const session = epoch.value;
    phase.value = "checking";
    const next = await dashboardApi.getAuthStatus();
    // A reset that landed during the await wins: the late response still
    // reaches its own caller, but it must not re-enter store state.
    if (session !== epoch.value) return next;
    applyStatus(next);
    suppressAuthRequired.value = false;
    return next;
  }

  /**
   * Load tokens when this session has none, then re-check the epoch before
   * the auth request. A reset during the status read must not borrow the
   * replacement session's tokens.
   */
  async function prepareAuth(session: number) {
    if (!controlPlane.hasTokens()) await loadStatus();
    if (session !== epoch.value) throw new Error("session ended");
    return controlPlane.expectation();
  }

  async function login(username: string, password: string): Promise<void> {
    const session = epoch.value;
    const expectation = await prepareAuth(session);
    try {
      const next = await dashboardApi.loginAdmin(username, password, expectation);
      if (session !== epoch.value) throw new Error("session ended");
      applyStatus(next);
      suppressAuthRequired.value = false;
    } catch (error) {
      if (session !== epoch.value) throw error;
      if (error instanceof DashboardConflictError) {
        try {
          await loadStatus();
        } catch {
          // A failed recovery reload never replaces the original conflict.
        }
      }
      throw error;
    }
  }

  async function register(username: string, password: string): Promise<void> {
    const session = epoch.value;
    const expectation = await prepareAuth(session);
    try {
      const next = await dashboardApi.registerAdmin(username, password, expectation);
      if (session !== epoch.value) throw new Error("session ended");
      applyStatus(next);
      suppressAuthRequired.value = false;
    } catch (error) {
      if (session !== epoch.value) throw error;
      if (error instanceof DashboardConflictError) {
        try {
          await loadStatus();
        } catch {
          // A failed recovery reload never replaces the original conflict.
        }
      }
      throw error;
    }
  }

  async function logout(): Promise<void> {
    const session = epoch.value;
    const expectation = await prepareAuth(session);
    if (session !== epoch.value) throw new Error("session ended");
    suppressAuthRequired.value = true;
    try {
      await dashboardApi.logoutAdmin(expectation);
    } catch (error) {
      if (session !== epoch.value) throw error;
      if (error instanceof DashboardConflictError) {
        try {
          await loadStatus();
        } catch {
          // A failed recovery reload never replaces the original conflict.
        }
        suppressAuthRequired.value = false;
        throw error;
      } else if (error instanceof DashboardRequestError && error.status === 401) {
        // Already unauthenticated server-side: fall through to local cleanup.
      } else {
        suppressAuthRequired.value = false;
        throw error;
      }
    }
    if (session !== epoch.value) return;
    dropSession();
  }

  /**
   * Session-owned teardown that clears every business store without importing
   * one: a static import here would pull the whole store → api → domain graph
   * (including the provider preset data) into the entry chunk. A store
   * registers itself in the Pinia instance on first use, and a store that was
   * never used has no cached state to wipe, so clearing through the registry
   * keeps the exact dropSession semantics without the dependency edge.
   */
  const SESSION_RESETTERS = {
    connection: "clearSecrets",
    controlPlane: "reset",
    accounts: "clearAccounts",
    accountPage: "clear",
    dashboardPage: "clear",
    providerPage: "clear",
    aliasPage: "clear",
    platformAccounts: "clear",
    identities: "clear",
    destinations: "clear",
    providers: "clear",
    settings: "clear",
    cpa: "clear",
    billing: "clear",
    dsh: "clear",
    byokApplications: "clear",
    temporaryPolicy: "clear",
    observability: "clear",
  } as const;

  /** Local-only teardown: secrets are wiped and the shell returns to login. */
  function dropSession(): void {
    // Advance the epoch first so every in-flight auth continuation captured
    // before this point is stale by the time teardown finishes.
    epoch.value += 1;
    const registry = getActivePinia()?._s;
    for (const [storeId, method] of Object.entries(SESSION_RESETTERS)) {
      const store = registry?.get(storeId) as
        | { clear?: () => void; clearAccounts?: () => void; clearSecrets?: () => void; reset?: () => void }
        | undefined;
      store?.[method]?.();
    }
    // Persisted read-model snapshots die with the session too: the next
    // login must never render the previous session's data.
    dropAllSnapshots();
    status.value = null;
    phase.value = "login";
  }

  /** Auth-required event from the transport: session is gone server-side. */
  function handleAuthRequired(): void {
    if (suppressAuthRequired.value) return;
    dropSession();
  }

  return {
    phase,
    status,
    localMode,
    authenticated,
    suppressAuthRequired,
    sessionEpoch: computed(() => epoch.value),
    applyStatus,
    loadStatus,
    login,
    register,
    logout,
    dropSession,
    handleAuthRequired,
  };
});
