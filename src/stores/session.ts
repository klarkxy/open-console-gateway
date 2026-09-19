import { computed, ref } from "vue";
import { defineStore } from "pinia";
import {
  DashboardConflictError,
  DashboardRequestError,
  dashboardApi,
} from "../api/dashboard.ts";
import type { AuthStatus } from "../api/generated/dashboard-v3.ts";
import { useConnectionStore } from "./connection.ts";
import { useControlPlaneStore } from "./controlPlane.ts";
import { useAccountsStore } from "./accounts.ts";
import { useDestinationsStore } from "./destinations.ts";
import { useIdentitiesStore } from "./identities.ts";
import { usePlatformAccountsStore } from "./platformAccounts.ts";
import { useProvidersStore } from "./providers.ts";

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
 */
export const useSessionStore = defineStore("session", () => {
  const controlPlane = useControlPlaneStore();
  const connection = useConnectionStore();

  const phase = ref<SessionPhase>("checking");
  const status = ref<AuthStatus | null>(null);
  /** Mirrors App.vue's legacy flag: suppresses one auth-required dispatch during logout. */
  const suppressAuthRequired = ref(false);

  const localMode = computed(() => status.value?.local ?? false);
  const authenticated = computed(() => status.value?.authenticated ?? false);

  function applyStatus(next: AuthStatus): void {
    status.value = next;
    phase.value = next.authenticated ? "ready" : next.initialized ? "login" : "register";
  }

  async function loadStatus(): Promise<AuthStatus> {
    phase.value = "checking";
    const next = await dashboardApi.getAuthStatus();
    applyStatus(next);
    suppressAuthRequired.value = false;
    return next;
  }

  /** CAS tokens for auth mutations come from the latest auth status load. */
  async function authExpectation() {
    if (!controlPlane.hasTokens()) await loadStatus();
    return controlPlane.expectation();
  }

  async function login(username: string, password: string): Promise<void> {
    try {
      applyStatus(await dashboardApi.loginAdmin(username, password, await authExpectation()));
    } catch (error) {
      if (error instanceof DashboardConflictError) await loadStatus();
      throw error;
    }
    suppressAuthRequired.value = false;
  }

  async function register(username: string, password: string): Promise<void> {
    try {
      applyStatus(await dashboardApi.registerAdmin(username, password, await authExpectation()));
    } catch (error) {
      if (error instanceof DashboardConflictError) await loadStatus();
      throw error;
    }
    suppressAuthRequired.value = false;
  }

  async function logout(): Promise<void> {
    suppressAuthRequired.value = true;
    try {
      await dashboardApi.logoutAdmin(await authExpectation());
    } catch (error) {
      if (error instanceof DashboardConflictError) {
        await loadStatus();
        suppressAuthRequired.value = false;
        throw error;
      } else if (error instanceof DashboardRequestError && error.status === 401) {
        // Already unauthenticated server-side: fall through to local cleanup.
      } else {
        suppressAuthRequired.value = false;
        throw error;
      }
    }
    dropSession();
  }

  /** Local-only teardown: secrets are wiped and the shell returns to login. */
  function dropSession(): void {
    connection.clearSecrets();
    useAccountsStore().clearAccounts();
    usePlatformAccountsStore().clear();
    useIdentitiesStore().clear();
    useDestinationsStore().clear();
    useProvidersStore().clear();
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
    applyStatus,
    loadStatus,
    login,
    register,
    logout,
    dropSession,
    handleAuthRequired,
  };
});
