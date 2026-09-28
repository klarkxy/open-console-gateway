import { computed, ref } from "vue";
import { defineStore } from "pinia";
import { isRevisionConflict } from "../api/dashboard-v3.ts";
import {
  platformAccountsApi,
  platformGroupWrite,
  type PlatformAccount,
  type PlatformAccountsView,
  type PlatformGroup,
  type PlatformKeyImportResult,
  type PlatformLink,
} from "../api/platform-accounts.ts";
import { dashboardErrorDetail } from "../utils/errors.ts";
import { useDestinationsStore } from "./destinations.ts";

export type PlatformPendingLink = { accountId: string; parentId: string };
export type PlatformPersistOutcome = "saved" | "conflict" | "error";
export type PlatformWriteOutcome = "ok" | "conflict" | "error";

export interface PlatformAccountWritePayload {
  kind: PlatformAccount["kind"];
  name: string;
  baseUrl: string;
  /** undefined preserves the saved credential; "" clears it. */
  userCredential?: string;
}

/**
 * Single owner of the platform-accounts list, links, and in-flight write
 * flags. Views issue API mutations here, then render outcomes; a pending
 * load can never clobber a newer accepted snapshot.
 */
export const usePlatformAccountsStore = defineStore("platformAccounts", () => {
  const view = ref<PlatformAccountsView | null>(null);
  const loaded = ref(false);
  const loading = ref(false);
  const error = ref("");
  const mutating = ref(false);
  const importing = ref<Record<string, boolean>>({});
  const nextImportPage = ref<Record<string, number>>({});
  const refreshing = ref<Record<string, boolean>>({});
  const pendingLink = ref<PlatformPendingLink | null>(null);
  const destinationRefreshError = ref("");

  // Overlapping loads resolve out of order; only the latest operation commits
  // loading/error presentation. Mirrors the load guard in stores/accounts.ts.
  let loadGeneration = 0;
  let sessionEpoch = 0;
  let destinationReadGeneration = 0;
  let refreshEpoch = 0;
  const refreshRequests = new Map<string, Promise<PlatformWriteOutcome>>();

  const parents = computed(() => view.value?.accounts ?? []);
  const links = computed(() => view.value?.links ?? []);

  /**
   * Acceptance boundary for every incoming snapshot: within the same backend
   * process generation a delayed older response must not roll the view back;
   * a different generation is an opaque identity with no comparable ordering,
   * so it is adopted as-is. Mirrors the revision sink in stores/controlPlane.ts.
   *
   * An accepted snapshot is a complete fresh list, so it also supersedes any
   * pending load: drop the obsolete loading/error presentation and invalidate
   * older load completions. A rejected stale snapshot carries no fresh state
   * and leaves an in-flight newer load untouched.
   */
  function acceptView(next: PlatformAccountsView): void {
    const current = view.value;
    if (
      current !== null
      && next.processGeneration === current.processGeneration
      && next.revision < current.revision
    ) {
      return;
    }
    view.value = next;
    loadGeneration += 1;
    loading.value = false;
    error.value = "";
    loaded.value = true;
    // A reloaded view that already contains the pending account's link settles
    // the retry state without another write.
    if (pendingLink.value && next.links.some((link) => link.accountId === pendingLink.value!.accountId && link.platformAccountId === pendingLink.value!.parentId)) {
      pendingLink.value = null;
    }
  }

  async function load(): Promise<PlatformAccountsView> {
    const generation = ++loadGeneration;
    loading.value = true;
    error.value = "";
    try {
      const next = await platformAccountsApi.list();
      if (generation !== loadGeneration) return next;
      acceptView(next);
      return next;
    } catch (e) {
      if (generation === loadGeneration) {
        error.value = dashboardErrorDetail(e);
      }
      throw e;
    } finally {
      if (generation === loadGeneration) loading.value = false;
    }
  }

  /** Revision-conflict recovery: tokens already refreshed by the CAS layer. */
  async function recoverConflict(): Promise<"conflict"> {
    const session = sessionEpoch;
    const epoch = refreshEpoch;
    try {
      const next = await platformAccountsApi.list();
      if (session === sessionEpoch && epoch === refreshEpoch) acceptView(next);
    } catch {
      // The next explicit action retries; the caller still surfaces conflict.
    }
    return "conflict";
  }

  function beginMutation(): boolean {
    if (mutating.value) return false;
    mutating.value = true;
    return true;
  }

  function endMutation(): void {
    mutating.value = false;
  }

  async function createOrUpdate(
    payload: PlatformAccountWritePayload,
    editing: PlatformAccount | null,
  ): Promise<PlatformPersistOutcome> {
    if (!beginMutation()) return "error";
    const session = sessionEpoch;
    try {
      const next = editing
        ? await platformAccountsApi.update(editing.id, {
          name: payload.name,
          ...(payload.userCredential !== undefined ? { userCredential: payload.userCredential } : {}),
        })
        : await platformAccountsApi.create({
          kind: payload.kind,
          name: payload.name,
          baseUrl: payload.baseUrl,
          ...(payload.userCredential !== undefined ? { userCredential: payload.userCredential } : {}),
        });
      if (session !== sessionEpoch) return "error";
      acceptView(next);
      void refreshDestinationProjection();
      return "saved";
    } catch (e) {
      if (session !== sessionEpoch) return "error";
      if (isRevisionConflict(e)) return recoverConflict();
      throw e;
    } finally {
      if (session === sessionEpoch) endMutation();
    }
  }

  async function refreshDestinationProjection(): Promise<void> {
    const session = sessionEpoch;
    const generation = ++destinationReadGeneration;
    destinationRefreshError.value = "";
    try {
      // Keep the previous coherent snapshot/CAS pair until this read commits.
      await useDestinationsStore().refreshAfterMutation();
    } catch (error) {
      if (session === sessionEpoch && generation === destinationReadGeneration) {
        destinationRefreshError.value = dashboardErrorDetail(error);
      }
    }
  }

  /** A confirmed deletion invalidates every older complete-list response. */
  function invalidateObservations(): void {
    refreshEpoch += 1;
    loadGeneration += 1;
    loading.value = false;
    refreshRequests.clear();
    refreshing.value = {};
  }

  /** Called after the existing account DELETE commits, not for unlinking. */
  function forgetAccount(accountId: string): void {
    invalidateObservations();
    if (view.value) {
      view.value = { ...view.value, links: view.value.links.filter(link => link.accountId !== accountId) };
    }
    if (pendingLink.value?.accountId === accountId) pendingLink.value = null;
  }

  async function remove(parentId: string): Promise<PlatformWriteOutcome> {
    if (!beginMutation()) return "error";
    const session = sessionEpoch;
    try {
      const ack = await platformAccountsApi.remove(parentId);
      if (session !== sessionEpoch) return "error";
      delete nextImportPage.value[parentId];
      invalidateObservations();
      if (view.value) {
        acceptView({
          ...view.value,
          accounts: view.value.accounts.filter(parent => parent.id !== parentId),
          links: view.value.links.filter(link => link.platformAccountId !== parentId),
          revision: ack.processGeneration === view.value.processGeneration
            ? Math.max(ack.revision, view.value.revision) : ack.revision,
          processGeneration: ack.processGeneration,
        });
      }
      if (pendingLink.value?.parentId === parentId) pendingLink.value = null;
      // Revalidation is read-only. Its failure cannot undo a confirmed DELETE
      // or cause a second destructive request; the removed row stays removed.
      await Promise.allSettled([
        load(),
        useDestinationsStore().refreshAfterMutation().then(() => {
          if (session === sessionEpoch) destinationRefreshError.value = "";
        }, (e: unknown) => {
          if (session === sessionEpoch) destinationRefreshError.value = dashboardErrorDetail(e);
        }),
      ]);
      return session === sessionEpoch ? "ok" : "error";
    } catch (e) {
      if (session !== sessionEpoch) return "error";
      if (isRevisionConflict(e)) return recoverConflict();
      throw e;
    } finally {
      if (session === sessionEpoch) endMutation();
    }
  }

  function refresh(parentId: string, accountId?: string): Promise<PlatformWriteOutcome> {
    const key = accountId === undefined ? parentId : `${parentId}:${accountId}`;
    const existing = refreshRequests.get(key);
    if (existing) return existing;
    // Parent and child observations use the same control-plane revision.
    // Serialize a platform's refreshes and coalesce repeated identical calls.
    if (mutating.value || Object.keys(refreshing.value).some(active => (
      refreshing.value[active] && (active === parentId || active.startsWith(`${parentId}:`))
    ))) return Promise.resolve("error");
    const session = sessionEpoch;
    const epoch = refreshEpoch;
    const isCurrent = () => session === sessionEpoch && epoch === refreshEpoch;
    refreshing.value[key] = true;
    const request = Promise.resolve().then(async (): Promise<PlatformWriteOutcome> => {
      try {
        if (!isCurrent()) return "error";
        const next = await platformAccountsApi.refresh(parentId, accountId);
        if (!isCurrent()) return "error";
        acceptView(next);
        return "ok";
      } catch (e) {
        if (!isCurrent()) return "error";
        if (isRevisionConflict(e)) {
          await recoverConflict();
          return isCurrent() ? "conflict" : "error";
        }
        throw e;
      } finally {
        if (isCurrent()) {
          refreshRequests.delete(key);
          delete refreshing.value[key];
        }
      }
    });
    refreshRequests.set(key, request);
    return request;
  }

  function refreshParent(parentId: string): Promise<PlatformWriteOutcome> {
    return refresh(parentId);
  }

  function refreshChild(parentId: string, accountId: string): Promise<PlatformWriteOutcome> {
    return refresh(parentId, accountId);
  }

  /** Observation during create-and-link, which already owns the mutation lock. */
  async function commitRefresh(parentId: string, accountId: string): Promise<void> {
    const session = sessionEpoch;
    const epoch = refreshEpoch;
    try {
      const next = await platformAccountsApi.refresh(parentId, accountId);
      if (session === sessionEpoch && epoch === refreshEpoch) acceptView(next);
    } catch (e) {
      if (session === sessionEpoch && epoch === refreshEpoch) throw e;
    }
  }

  async function importKeys(parentId: string): Promise<PlatformKeyImportResult | "conflict" | "error"> {
    if (mutating.value || importing.value[parentId]) return "error";
    const session = sessionEpoch;
    mutating.value = true;
    importing.value[parentId] = true;
    try {
      const result = await platformAccountsApi.importKeys(parentId, nextImportPage.value[parentId]);
      if (session !== sessionEpoch) return "error";
      if (result.nextPage != null) nextImportPage.value[parentId] = result.nextPage;
      else delete nextImportPage.value[parentId];
      try {
        const next = await platformAccountsApi.list();
        if (session === sessionEpoch) acceptView(next);
      } catch (e) {
        // Import already committed. A failed revalidation must not hide its
        // result or lose the page continuation and invite a duplicate write.
        if (session === sessionEpoch) error.value = dashboardErrorDetail(e);
      }
      return session === sessionEpoch ? result : "error";
    } catch (e) {
      if (session !== sessionEpoch) return "error";
      if (isRevisionConflict(e)) return recoverConflict();
      throw e;
    } finally {
      if (session === sessionEpoch) {
        mutating.value = false;
        importing.value[parentId] = false;
      }
    }
  }

  async function link(
    accountId: string,
    parentId: string,
    group: Pick<PlatformGroup, "id" | "platform">,
  ): Promise<PlatformWriteOutcome> {
    const session = sessionEpoch;
    const epoch = refreshEpoch;
    try {
      const next = await platformAccountsApi.link(accountId, parentId, platformGroupWrite(group));
      if (session !== sessionEpoch || epoch !== refreshEpoch) return "error";
      acceptView(next);
      return "ok";
    } catch (e) {
      if (session !== sessionEpoch || epoch !== refreshEpoch) return "error";
      if (isRevisionConflict(e)) return recoverConflict();
      throw e;
    }
  }

  async function unlink(accountId: string): Promise<PlatformWriteOutcome> {
    if (!beginMutation()) return "error";
    const session = sessionEpoch;
    try {
      const next = await platformAccountsApi.unlink(accountId);
      if (session !== sessionEpoch) return "error";
      invalidateObservations();
      acceptView(next);
      return "ok";
    } catch (e) {
      if (session !== sessionEpoch) return "error";
      if (isRevisionConflict(e)) return recoverConflict();
      throw e;
    } finally {
      if (session === sessionEpoch) endMutation();
    }
  }

  async function retryPendingLink(): Promise<PlatformWriteOutcome> {
    const pending = pendingLink.value;
    if (!pending || !beginMutation()) return "error";
    const session = sessionEpoch;
    try {
      const outcome = await link(pending.accountId, pending.parentId, { id: null, platform: null });
      if (session !== sessionEpoch) return "error";
      if (outcome === "ok") pendingLink.value = null;
      return outcome;
    } finally {
      if (session === sessionEpoch) endMutation();
    }
  }

  function setPendingLink(next: PlatformPendingLink): void {
    pendingLink.value = next;
  }

  function clearPendingLink(): void {
    pendingLink.value = null;
    destinationRefreshError.value = "";
  }

  function linksFor(parentId: string): PlatformLink[] {
    return links.value.filter((item) => item.platformAccountId === parentId);
  }

  function linkForAccount(accountId: string): PlatformLink | undefined {
    return links.value.find((item) => item.accountId === accountId);
  }

  /** Drop the cached view on 401 / logout so the next session reloads fresh. */
  function clear(): void {
    sessionEpoch += 1;
    invalidateObservations();
    view.value = null;
    loaded.value = false;
    loading.value = false;
    error.value = "";
    mutating.value = false;
    importing.value = {};
    nextImportPage.value = {};
    pendingLink.value = null;
    destinationRefreshError.value = "";
  }

  return {
    view: computed(() => view.value),
    parents,
    links,
    loaded: computed(() => loaded.value),
    loading: computed(() => loading.value),
    error: computed(() => error.value),
    mutating: computed(() => mutating.value),
    importing: computed(() => importing.value),
    refreshing: computed(() => refreshing.value),
    pendingLink: computed(() => pendingLink.value),
    destinationRefreshError: computed(() => destinationRefreshError.value),
    sessionEpoch: computed(() => sessionEpoch),
    load,
    acceptView,
    recoverConflict,
    beginMutation,
    endMutation,
    createOrUpdate,
    refreshDestinationProjection,
    remove,
    forgetAccount,
    refreshParent,
    refreshChild,
    commitRefresh,
    importKeys,
    link,
    unlink,
    retryPendingLink,
    setPendingLink,
    clearPendingLink,
    linksFor,
    linkForAccount,
    clear,
  };
});
