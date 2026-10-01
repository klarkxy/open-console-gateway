import { computed, ref } from "vue";
import { defineStore } from "pinia";
import { dashboardV3 } from "../api/dashboard-v3.ts";
import { dashboardV4 } from "../api/dashboard-v4.ts";
import type {
  CpaAccount,
  CpaIntegration,
  CpaRuntime,
  CpaRuntimeKey,
  MutationAck,
} from "../api/generated/dashboard-v3.ts";
import type { CpaCatalog, CpaCatalogEntry } from "../api/generated/dashboard-v4.ts";
import { cpaAccountKey, cpaCardStatus, type CpaCardStatus } from "../domain/cpa-runtime.ts";
import { dashboardErrorDetail } from "../utils/errors.ts";

/** Epoch tagging one catalog selection intent inside a catalog write generation. */
export type CpaCatalogWriteEpoch = { generation: number; seq: number };

/**
 * External CPA address after the saved URL is removed and no env URL is pinned.
 * Same literal as `cpa::DEFAULT_CPA_BASE_URL`.
 */
const CLEARED_EXTERNAL_CPA_BASE_URL = "http://127.0.0.1:8317";

/**
 * Single owner of the CPA server read models: the integration + runtime
 * snapshots, the model catalog, the CPA account list, and the runtime client
 * keys. The CPA page drives mutations and keeps only UI-local state (drafts,
 * OAuth progress, action flags, the one-time secret reveal).
 *
 * Every resource keeps its own read generation so a slow GET can never
 * clobber fresher state and reloading one resource never cancels another.
 * Loads always issue their own request: invalidating an older pending read
 * leaves the newest intent with a committable GET of its own, and a stale
 * finally never releases a newer load's flag. A caller that passes
 * `expectedSession` is refused before the GET and again before commit, so a
 * detached read cannot start after `clear()`. Mutation receipts commit in
 * place through the `commit*`/`project*`/`remove*`/`invalidate*` actions,
 * which also kill older in-flight reads. Each resource has a loaded flag:
 * the first successful commit sets it, `clear()` drops it, and a later read
 * keeps the current rows. `sessionEpoch` is bumped by `clear()` on
 * logout/401.
 *
 * Secrets never live here: only secret-free DTOs are stored.
 */
export const useCpaStore = defineStore("cpa", () => {
  const integration = ref<CpaIntegration | null>(null);
  const runtime = ref<CpaRuntime | null>(null);
  const loaded = ref(false);
  const loading = ref(false);
  /** Integration-leg failure of the last `load()`; drives the page-level alert. */
  const error = ref("");
  /** Runtime-leg failure of the last `load()`; the page keeps rendering. */
  const runtimeError = ref("");

  let loadGeneration = 0;
  let integrationReadSeq = 0;
  let runtimeReadSeq = 0;
  /** Session invalidation token; bumped only by `clear()`. */
  let sessionEpoch = 0;

  const cardStatus = computed((): CpaCardStatus | null => (
    cpaCardStatus(integration.value, runtime.value)
  ));

  /** Keep the integration snapshot truthful while runtime state moves. */
  function applyRuntimeSnapshot(snapshot: CpaRuntime): void {
    if (!integration.value) return;
    integration.value = {
      ...integration.value,
      currentOperation: snapshot.currentOperation,
      installedVersion: snapshot.currentVersion,
      latestVersion: snapshot.latestVersion,
      runtimeOwned: snapshot.owned,
      runtimeRunning: snapshot.running,
      runtimeSupported: snapshot.supported,
      runtimeUnavailableReason: snapshot.unavailableReason,
      updateAvailable: snapshot.updateAvailable,
    };
  }

  /**
   * Guarded integration read. A captured session that has already ended
   * returns before the GET. Resolves null when a newer read or a session
   * change owns the snapshot.
   */
  async function refreshIntegration(expectedSession?: number): Promise<CpaIntegration | null> {
    if (expectedSession !== undefined && expectedSession !== sessionEpoch) return null;
    const readSeq = ++integrationReadSeq;
    const session = expectedSession ?? sessionEpoch;
    const value = await dashboardV3.getCpaIntegration();
    if (readSeq !== integrationReadSeq || session !== sessionEpoch) return null;
    integration.value = value;
    error.value = "";
    return value;
  }

  /** Guarded runtime read; resolves null when a newer read superseded it. */
  async function refreshRuntime(): Promise<CpaRuntime | null> {
    const readSeq = ++runtimeReadSeq;
    const snapshot = await dashboardV3.getCpaRuntime();
    if (readSeq !== runtimeReadSeq) return null;
    runtime.value = snapshot;
    applyRuntimeSnapshot(snapshot);
    runtimeError.value = "";
    return snapshot;
  }

  /** Integration mutation receipts commit in place and kill older reads. */
  function commitIntegration(value: CpaIntegration): void {
    integrationReadSeq += 1;
    integration.value = value;
    error.value = "";
  }

  /**
   * DELETE receipt for an external connection. The saved URL, keys, account,
   * and catalog are gone. A read-only base URL stays (env pin). Runtime
   * snapshot fields stay; a managed runtime is not disconnected by this path.
   * Older integration, account, and catalog reads lose their commit.
   */
  function commitClearedIntegration(ack: MutationAck | null | undefined): CpaIntegration | null {
    const current = integration.value;
    let cleared: CpaIntegration | null = null;
    if (current) {
      const revision = ack && typeof ack.revision === "number" ? ack.revision : current.revision;
      const processGeneration = ack && typeof ack.processGeneration === "number"
        ? ack.processGeneration
        : current.processGeneration;
      cleared = {
        accountId: null,
        baseUrl: current.baseUrlReadOnly ? current.baseUrl : CLEARED_EXTERNAL_CPA_BASE_URL,
        baseUrlReadOnly: current.baseUrlReadOnly,
        configured: false,
        currentOperation: current.currentOperation,
        enabled: false,
        inferenceKeyConfigured: false,
        installedVersion: null,
        latestVersion: current.latestVersion,
        managementKeyConfigured: false,
        modelCount: 0,
        modelsRefreshedAt: null,
        processGeneration,
        revision,
        runtimeOwned: false,
        runtimeRunning: current.runtimeRunning,
        runtimeSupported: current.runtimeSupported,
        runtimeUnavailableReason: current.runtimeSupported ? null : current.runtimeUnavailableReason,
        updateAvailable: current.updateAvailable,
      };
    }
    integrationReadSeq += 1;
    integration.value = cleared;
    error.value = "";
    resetAccounts();
    resetCatalog();
    return cleared;
  }

  /** Runtime lifecycle receipts commit in place and kill older reads. */
  function commitRuntimeSnapshot(snapshot: CpaRuntime): void {
    runtimeReadSeq += 1;
    runtime.value = snapshot;
    applyRuntimeSnapshot(snapshot);
    runtimeError.value = "";
  }

  async function load(): Promise<void> {
    const generation = ++loadGeneration;
    const session = sessionEpoch;
    loading.value = true;
    error.value = "";
    try {
      const [integrationResult, runtimeResult] = await Promise.allSettled([
        refreshIntegration(session),
        refreshRuntime(),
      ]);
      if (generation !== loadGeneration) return;
      if (integrationResult.status === "rejected") {
        error.value = dashboardErrorDetail(integrationResult.reason);
        return;
      }
      // A newer integration read owns the state; stay quiet.
      if (integrationResult.value === null) return;
      if (runtimeResult.status === "rejected") {
        runtime.value = null;
        runtimeError.value = dashboardErrorDetail(runtimeResult.reason);
      }
      error.value = "";
      loaded.value = true;
    } finally {
      if (generation === loadGeneration) loading.value = false;
    }
  }

  // --- model catalog ---

  const catalogModels = ref<CpaCatalogEntry[]>([]);
  const catalogSourceUrl = ref<string | null>(null);
  const catalogLoading = ref(false);
  const catalogLoaded = ref(false);
  const catalogError = ref("");

  // Catalog writes stay serial in the view, but every click captures its own
  // target epoch at enqueue time: a slow PUT response must never overwrite a
  // newer selection. Only the latest write of the current generation may
  // apply a response or run an error resync. Full loads, model refresh,
  // disconnect, session teardown, and page exit bump the generation so queued
  // writes from before them return quietly instead of resurrecting cleared
  // state or overwriting freshly loaded data.
  let catalogWriteGeneration = 0;
  let catalogWriteSeq = 0;
  // Reads track their own sequence so the newest load always has a
  // committable request and a stale finally never releases a newer load.
  let catalogReadSeq = 0;

  function bumpCatalogWriteGeneration(): void {
    catalogWriteGeneration += 1;
  }

  function captureCatalogWrite(): CpaCatalogWriteEpoch {
    return { generation: catalogWriteGeneration, seq: catalogWriteSeq };
  }

  /** A new selection intent: invalidates reads and writes started before it. */
  function nextCatalogWriteEpoch(): CpaCatalogWriteEpoch {
    catalogWriteSeq += 1;
    return captureCatalogWrite();
  }

  function isCurrentCatalogWrite(epoch: CpaCatalogWriteEpoch): boolean {
    return epoch.generation === catalogWriteGeneration && epoch.seq === catalogWriteSeq;
  }

  function applyCatalog(snapshot: Pick<CpaCatalog, "models" | "sourceUrl" | "refreshedAt">): void {
    catalogModels.value = snapshot.models;
    catalogSourceUrl.value = snapshot.sourceUrl;
    catalogError.value = "";
    catalogLoaded.value = true;
    if (integration.value) {
      integration.value = {
        ...integration.value,
        modelCount: snapshot.models.length,
        modelsRefreshedAt: snapshot.refreshedAt,
      };
    }
  }

  /** Commit a catalog snapshot only while its epoch still owns the selection. */
  function commitCatalog(
    snapshot: Pick<CpaCatalog, "models" | "sourceUrl" | "refreshedAt">,
    epoch: CpaCatalogWriteEpoch,
  ): boolean {
    if (!isCurrentCatalogWrite(epoch)) return false;
    applyCatalog(snapshot);
    return true;
  }

  /** Optimistic local selection draft; the view's write chain persists it. */
  function setCatalogModels(models: CpaCatalogEntry[]): void {
    catalogModels.value = models;
  }

  /** The write chain reports an error resync failure into the catalog slot. */
  function setCatalogError(detail: string): void {
    catalogError.value = detail;
  }

  /**
   * Catalog read for loads and retries. Always issues its own GET: when a
   * refresh invalidates an older pending read, the newest intent still has a
   * committable request in flight. A read started before a newer selection
   * intent or write generation cannot apply, but the latest read always
   * releases the loading flag.
   */
  async function loadCatalog(expectedSession?: number): Promise<boolean> {
    if (expectedSession !== undefined && expectedSession !== sessionEpoch) return false;
    const readSeq = ++catalogReadSeq;
    const epoch = captureCatalogWrite();
    catalogLoading.value = true;
    try {
      const snapshot = await dashboardV4.getCpaCatalog();
      if (readSeq !== catalogReadSeq || !isCurrentCatalogWrite(epoch)) return false;
      if (expectedSession !== undefined && expectedSession !== sessionEpoch) return false;
      applyCatalog(snapshot);
      return true;
    } catch (error) {
      if (
        readSeq === catalogReadSeq
        && isCurrentCatalogWrite(epoch)
        && (expectedSession === undefined || expectedSession === sessionEpoch)
      ) {
        catalogError.value = dashboardErrorDetail(error);
      }
      return false;
    } finally {
      if (readSeq === catalogReadSeq) catalogLoading.value = false;
    }
  }

  /** Epoch-guarded catalog GET for the write chain's error resync. */
  async function fetchCatalog(epoch: CpaCatalogWriteEpoch): Promise<void> {
    const snapshot = await dashboardV4.getCpaCatalog();
    commitCatalog(snapshot, epoch);
  }

  function resetCatalog(): void {
    catalogReadSeq += 1;
    catalogLoading.value = false;
    catalogLoaded.value = false;
    catalogModels.value = [];
    catalogSourceUrl.value = null;
    catalogError.value = "";
  }

  // --- CPA accounts ---

  const cpaAccounts = ref<CpaAccount[]>([]);
  const accountsLoading = ref(false);
  const accountsLoaded = ref(false);
  const accountsError = ref("");
  let accountsReadSeq = 0;

  function accountStillCurrent(expectedSession: number | undefined, readSeq: number): boolean {
    return readSeq === accountsReadSeq
      && (expectedSession === undefined || expectedSession === sessionEpoch);
  }

  /**
   * Account list read. Resolves true only when this call commits.
   * A mismatched captured session returns before the GET.
   */
  async function loadAccounts(expectedSession?: number): Promise<boolean> {
    if (expectedSession !== undefined && expectedSession !== sessionEpoch) return false;
    const readSeq = ++accountsReadSeq;
    accountsLoading.value = true;
    try {
      const accounts = (await dashboardV3.getCpaAccounts()).accounts;
      if (!accountStillCurrent(expectedSession, readSeq)) return false;
      cpaAccounts.value = accounts;
      accountsError.value = "";
      accountsLoaded.value = true;
      return true;
    } catch (error) {
      if (accountStillCurrent(expectedSession, readSeq)) {
        accountsError.value = dashboardErrorDetail(error);
      }
      return false;
    } finally {
      if (readSeq === accountsReadSeq) accountsLoading.value = false;
    }
  }

  /** Status receipt: write the acknowledged disabled bit on that account only. */
  function projectAccountDisabled(
    account: Pick<CpaAccount, "name" | "authIndex">,
    disabled: boolean,
  ): void {
    const key = cpaAccountKey(account);
    accountsReadSeq += 1;
    accountsLoading.value = false;
    accountsError.value = "";
    cpaAccounts.value = cpaAccounts.value.map((row) => (
      cpaAccountKey(row) === key ? { ...row, disabled } : row
    ));
  }

  /**
   * Quota-reset receipt carries no replacement tracker. Clear that account's
   * quota and leave every other field and row untouched.
   */
  function projectAccountQuotaReset(account: Pick<CpaAccount, "name" | "authIndex">): void {
    const key = cpaAccountKey(account);
    accountsReadSeq += 1;
    accountsLoading.value = false;
    accountsError.value = "";
    cpaAccounts.value = cpaAccounts.value.map((row) => (
      cpaAccountKey(row) === key ? { ...row, quota: null } : row
    ));
  }

  /** Delete receipt: drop that account immediately so an older list cannot restore it. */
  function removeAccount(account: Pick<CpaAccount, "name" | "authIndex">): void {
    const key = cpaAccountKey(account);
    accountsReadSeq += 1;
    accountsLoading.value = false;
    accountsError.value = "";
    cpaAccounts.value = cpaAccounts.value.filter((row) => cpaAccountKey(row) !== key);
  }

  function resetAccounts(): void {
    accountsReadSeq += 1;
    accountsLoading.value = false;
    accountsLoaded.value = false;
    accountsError.value = "";
    cpaAccounts.value = [];
  }

  // --- runtime client keys (secret-free rows) ---

  const runtimeKeys = ref<CpaRuntimeKey[]>([]);
  const keysLoading = ref(false);
  const keysLoaded = ref(false);
  const keysError = ref("");
  let keysReadSeq = 0;

  function keysStillCurrent(expectedSession: number | undefined, readSeq: number): boolean {
    return readSeq === keysReadSeq
      && (expectedSession === undefined || expectedSession === sessionEpoch);
  }

  /** Client-key list read. A mismatched captured session returns before the GET. */
  async function loadRuntimeKeys(expectedSession?: number): Promise<boolean> {
    if (expectedSession !== undefined && expectedSession !== sessionEpoch) return false;
    const readSeq = ++keysReadSeq;
    keysLoading.value = true;
    try {
      const keys = (await dashboardV3.getCpaRuntimeKeys()).keys;
      if (!keysStillCurrent(expectedSession, readSeq)) return false;
      runtimeKeys.value = keys;
      keysError.value = "";
      keysLoaded.value = true;
      return true;
    } catch (error) {
      if (keysStillCurrent(expectedSession, readSeq)) keysError.value = dashboardErrorDetail(error);
      return false;
    } finally {
      if (readSeq === keysReadSeq) keysLoading.value = false;
    }
  }

  /**
   * A create/rotate receipt carries no full list row, but it is newer state
   * than any list read already in flight: kill those reads (and release their
   * loading flag) so an old list can never erase the accepted mutation.
   */
  function invalidateRuntimeKeys(): void {
    keysReadSeq += 1;
    keysLoading.value = false;
  }

  /** Rotate receipt carries a new hint for an existing fingerprint and nothing else. */
  function projectRuntimeKeyHint(fingerprint: string, hint: string): void {
    keysReadSeq += 1;
    keysLoading.value = false;
    keysError.value = "";
    runtimeKeys.value = runtimeKeys.value.map((key) => (
      key.fingerprint === fingerprint ? { ...key, hint } : key
    ));
  }

  /** Receipt-confirmed delete commits in place; older in-flight reads die. */
  function removeRuntimeKey(fingerprint: string): void {
    keysReadSeq += 1;
    keysLoading.value = false;
    keysError.value = "";
    runtimeKeys.value = runtimeKeys.value.filter((key) => key.fingerprint !== fingerprint);
  }

  function resetRuntimeKeys(): void {
    keysReadSeq += 1;
    keysLoading.value = false;
    keysLoaded.value = false;
    keysError.value = "";
    runtimeKeys.value = [];
  }

  /**
   * Page exit (deactivate/unmount): in-flight reads for the page-owned
   * resources lose their commit right and their loading flags, so a response
   * landing while the page is gone cannot change what a kept-alive visit
   * shows on return. Committed data stays cached for the revalidation window.
   */
  function invalidateReads(): void {
    accountsReadSeq += 1;
    keysReadSeq += 1;
    catalogReadSeq += 1;
    accountsLoading.value = false;
    keysLoading.value = false;
    catalogLoading.value = false;
  }

  // --- session ---

  function currentSession(): number {
    return sessionEpoch;
  }

  function clear(): void {
    sessionEpoch += 1;
    loadGeneration += 1;
    integrationReadSeq += 1;
    runtimeReadSeq += 1;
    catalogReadSeq += 1;
    catalogWriteGeneration += 1;
    accountsReadSeq += 1;
    keysReadSeq += 1;
    integration.value = null;
    runtime.value = null;
    loaded.value = false;
    loading.value = false;
    error.value = "";
    runtimeError.value = "";
    catalogModels.value = [];
    catalogSourceUrl.value = null;
    catalogLoading.value = false;
    catalogLoaded.value = false;
    catalogError.value = "";
    cpaAccounts.value = [];
    accountsLoading.value = false;
    accountsLoaded.value = false;
    accountsError.value = "";
    runtimeKeys.value = [];
    keysLoading.value = false;
    keysLoaded.value = false;
    keysError.value = "";
  }

  return {
    integration: computed(() => integration.value),
    runtime: computed(() => runtime.value),
    cardStatus,
    loaded: computed(() => loaded.value),
    loading: computed(() => loading.value),
    error: computed(() => error.value),
    runtimeError: computed(() => runtimeError.value),
    catalogModels: computed(() => catalogModels.value),
    catalogSourceUrl: computed(() => catalogSourceUrl.value),
    catalogLoading: computed(() => catalogLoading.value),
    catalogLoaded: computed(() => catalogLoaded.value),
    catalogError: computed(() => catalogError.value),
    cpaAccounts: computed(() => cpaAccounts.value),
    accountsLoading: computed(() => accountsLoading.value),
    accountsLoaded: computed(() => accountsLoaded.value),
    accountsError: computed(() => accountsError.value),
    runtimeKeys: computed(() => runtimeKeys.value),
    keysLoading: computed(() => keysLoading.value),
    keysLoaded: computed(() => keysLoaded.value),
    keysError: computed(() => keysError.value),
    load,
    refreshIntegration,
    refreshRuntime,
    commitIntegration,
    commitClearedIntegration,
    commitRuntimeSnapshot,
    bumpCatalogWriteGeneration,
    captureCatalogWrite,
    nextCatalogWriteEpoch,
    isCurrentCatalogWrite,
    commitCatalog,
    setCatalogModels,
    setCatalogError,
    loadCatalog,
    fetchCatalog,
    resetCatalog,
    loadAccounts,
    projectAccountDisabled,
    projectAccountQuotaReset,
    removeAccount,
    resetAccounts,
    loadRuntimeKeys,
    invalidateRuntimeKeys,
    projectRuntimeKeyHint,
    removeRuntimeKey,
    resetRuntimeKeys,
    invalidateReads,
    currentSession,
    clear,
  };
});
