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
export type PlatformPersistOutcome = "saved" | "saved_refresh_failed" | "conflict" | "error";
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
  const refreshing = ref<Record<string, boolean>>({});
  const pendingLink = ref<PlatformPendingLink | null>(null);
  const destinationRefreshError = ref("");

  // Overlapping loads resolve out of order; only the latest operation commits
  // loading/error presentation. Mirrors the load guard in stores/accounts.ts.
  let loadGeneration = 0;

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
    if (pendingLink.value && next.links.some((link) => link.accountId === pendingLink.value!.accountId)) {
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
    try {
      acceptView(await platformAccountsApi.list());
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
    try {
      acceptView(editing
        ? await platformAccountsApi.update(editing.id, {
          name: payload.name,
          ...(payload.userCredential !== undefined ? { userCredential: payload.userCredential } : {}),
        })
        : await platformAccountsApi.create({
          kind: payload.kind,
          name: payload.name,
          baseUrl: payload.baseUrl,
          ...(payload.userCredential !== undefined ? { userCredential: payload.userCredential } : {}),
        }));
      try {
        // Both create and update change the destination projection revision.
        // Refresh the full snapshot before a card or catalog write can capture
        // its next CAS pair, including edits that leave the parent name alone.
        await useDestinationsStore().refreshAfterMutation();
        destinationRefreshError.value = "";
      } catch (error) {
        // The platform write already committed; cards keep the previous dest snapshot.
        destinationRefreshError.value = dashboardErrorDetail(error);
        return "saved_refresh_failed";
      }
      return "saved";
    } catch (e) {
      if (isRevisionConflict(e)) return recoverConflict();
      throw e;
    } finally {
      endMutation();
    }
  }

  async function remove(parentId: string): Promise<PlatformWriteOutcome> {
    if (!beginMutation()) return "error";
    try {
      await platformAccountsApi.remove(parentId);
      try {
        await load();
      } catch {
        // Deletion already committed; a list failure is presented via `error`.
      }
      return "ok";
    } catch (e) {
      if (isRevisionConflict(e)) return recoverConflict();
      throw e;
    } finally {
      endMutation();
    }
  }

  async function refreshParent(parentId: string): Promise<PlatformWriteOutcome> {
    if (refreshing.value[parentId]) return "error";
    refreshing.value[parentId] = true;
    try {
      acceptView(await platformAccountsApi.refresh(parentId));
      return "ok";
    } catch (e) {
      if (isRevisionConflict(e)) return recoverConflict();
      throw e;
    } finally {
      refreshing.value[parentId] = false;
    }
  }

  async function refreshChild(parentId: string, accountId: string): Promise<PlatformWriteOutcome> {
    const key = `${parentId}:${accountId}`;
    if (refreshing.value[key]) return "error";
    refreshing.value[key] = true;
    try {
      acceptView(await platformAccountsApi.refresh(parentId, accountId));
      return "ok";
    } catch (e) {
      if (isRevisionConflict(e)) return recoverConflict();
      throw e;
    } finally {
      refreshing.value[key] = false;
    }
  }

  /** Refresh without the per-key busy map; used by the create-and-link observation. */
  async function commitRefresh(parentId: string, accountId: string): Promise<void> {
    acceptView(await platformAccountsApi.refresh(parentId, accountId));
  }

  async function importKeys(parentId: string): Promise<PlatformKeyImportResult | "conflict" | "error"> {
    if (mutating.value || importing.value[parentId]) return "error";
    mutating.value = true;
    importing.value[parentId] = true;
    try {
      const result = await platformAccountsApi.importKeys(parentId);
      acceptView(await platformAccountsApi.list());
      return result;
    } catch (e) {
      if (isRevisionConflict(e)) return recoverConflict();
      throw e;
    } finally {
      mutating.value = false;
      importing.value[parentId] = false;
    }
  }

  async function link(
    accountId: string,
    parentId: string,
    group: Pick<PlatformGroup, "id" | "platform">,
  ): Promise<PlatformWriteOutcome> {
    try {
      acceptView(await platformAccountsApi.link(
        accountId,
        parentId,
        platformGroupWrite(group),
      ));
      return "ok";
    } catch (e) {
      if (isRevisionConflict(e)) return recoverConflict();
      throw e;
    }
  }

  async function unlink(accountId: string): Promise<PlatformWriteOutcome> {
    if (!beginMutation()) return "error";
    try {
      acceptView(await platformAccountsApi.unlink(accountId));
      return "ok";
    } catch (e) {
      if (isRevisionConflict(e)) return recoverConflict();
      throw e;
    } finally {
      endMutation();
    }
  }

  async function retryPendingLink(): Promise<PlatformWriteOutcome> {
    const pending = pendingLink.value;
    if (!pending || !beginMutation()) return "error";
    try {
      const outcome = await link(pending.accountId, pending.parentId, { id: null, platform: null });
      if (outcome === "ok") pendingLink.value = null;
      return outcome;
    } finally {
      endMutation();
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
    loadGeneration += 1;
    view.value = null;
    loaded.value = false;
    loading.value = false;
    error.value = "";
    mutating.value = false;
    importing.value = {};
    refreshing.value = {};
    pendingLink.value = null;
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
    load,
    acceptView,
    recoverConflict,
    beginMutation,
    endMutation,
    createOrUpdate,
    remove,
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
