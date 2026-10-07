import { computed, ref, shallowRef, watch } from "vue";
import { defineStore } from "pinia";
import { pagesApi, type AccountsPage, type AccountsQuery, type AccountDetail, type AccountPageRefresh } from "../api/pages.ts";
import type { MutationExpectation } from "../api/generated/dashboard-v3.ts";
import type { BillingStatus } from "../api/billing.ts";
import { billingBinding } from "../domain/billing.ts";
import { createReadLifecycle, PAGE_READ_MAX_AGE_MS, type ReadOptions } from "./readLifecycle.ts";
import { useControlPlaneStore } from "./controlPlane.ts";
import { invalidateManagementPages } from "./managementPages.ts";
import { mapWithConcurrency } from "../utils/async.ts";

export const ACCOUNT_PAGE_SIZE = 10;
export const ACCOUNT_CARD_ROW_SIZE = 5;

export function normalizeAccountsQuery(query: AccountsQuery = {}): AccountsQuery {
  return {
    search: query.search?.trim() || undefined,
    plan: query.plan && query.plan !== "all" ? query.plan : undefined,
    status: query.status && query.status !== "all" ? query.status : undefined,
    offset: Math.max(0, Math.trunc(query.offset ?? 0)),
    limit: Math.min(100, Math.max(1, Math.trunc(query.limit ?? ACCOUNT_PAGE_SIZE))),
  };
}

export function accountsQueryKey(query: AccountsQuery): string {
  return JSON.stringify(normalizeAccountsQuery(query));
}

/** A bounded management projection, never a complete Account/Destination inventory. */
export const useAccountPageStore = defineStore("accountPage", () => {
  const page = shallowRef<AccountsPage | null>(null);
  const query = shallowRef<AccountsQuery>(normalizeAccountsQuery());
  const loading = ref(false);
  const error = ref("");
  const details = shallowRef<ReadonlyMap<string, AccountDetail>>(new Map());
  const detailLoading = ref<Record<string, boolean>>({});
  const detailErrors = ref<Record<string, string>>({});
  const refreshStates = ref<Record<string, "queued" | "running">>({});
  const cardLoading = ref<Record<string, boolean>>({});
  const cardErrors = ref<Record<string, string>>({});
  const cardPaging = ref<Record<string, { offset: number; hasMore: boolean }>>({});
  const reads = createReadLifecycle();
  const cached = new Map<string, AccountsPage>();
  const cachedPaging = new Map<string, Record<string, { offset: number; hasMore: boolean }>>();
  const keyReads = new Map<string, number>();
  let readSequence = 0;
  const detailGenerations = new Map<string, number>();
  const cardGenerations = new Map<string, number>();
  const refreshFlights = new Map<string, Promise<AccountPageRefresh | null>>();
  const controllers = new Set<AbortController>();
  let generation = 0;
  let session = 0;
  let mutationEpoch = 0;
  let pagingEpoch = 0;
  let pagingKey = "";
  const control = useControlPlaneStore();

  function abortReads(): void {
    for (const controller of controllers) controller.abort();
    controllers.clear();
  }

  function invalidate(): void {
    generation++;
    mutationEpoch++;
    reads.invalidate();
    cached.clear();
    cachedPaging.clear();
    keyReads.clear();
    abortReads();
    loading.value = false;
  }

  watch(() => control.processGeneration, (_next, previous) => {
    if (previous !== null) {
      // The transport publishes a newly discovered process before delivering
      // that GET's body. Keep its request fence intact so that current body
      // can commit; payload/process matching rejects obsolete responses.
      reads.invalidate();
      cached.clear();
      cachedPaging.clear();
      details.value = new Map();
    }
  }, { flush: "sync" });

  function currentProcess(revision: { processGeneration: number; revision?: number }): boolean {
    if (control.processGeneration !== null && revision.processGeneration !== control.processGeneration) return false;
    return revision.revision === undefined || control.revision === null || revision.revision >= control.revision;
  }

  function lifetime(snapshot: AccountsPage): number {
    if (!snapshot.validUntil) return PAGE_READ_MAX_AGE_MS;
    const expires = Date.parse(snapshot.validUntil);
    return Number.isFinite(expires) ? Math.max(0, Math.min(PAGE_READ_MAX_AGE_MS, expires - Date.now())) : 0;
  }

  function snapshotPaging(snapshot: AccountsPage) {
    return Object.fromEntries(snapshot.cards.map(card => [card.cardId, {
      offset: card.rowsOffset, hasMore: card.rowsHasMore,
    }]));
  }

  function load(next: AccountsQuery = query.value, options?: ReadOptions): Promise<AccountsPage> {
    const normalized = normalizeAccountsQuery(next);
    const key = accountsQueryKey(normalized);
    const retainedPaging = key === pagingKey ? cardPaging.value : cachedPaging.get(key) ?? {};
    const selected = ++generation;
    const capturedSession = session;
    const capturedMutation = mutationEpoch;
    const capturedPaging = pagingEpoch;
    query.value = normalized;
    const hit = cached.get(key);
    if (hit) { page.value = hit; cardPaging.value = snapshotPaging(hit); pagingKey = key; }
    const maxAgeMs = hit ? Math.min(options?.maxAgeMs ?? 0, lifetime(hit)) : 0;
    return reads.run(key, { maxAgeMs }, () => cached.get(key)!, async () => {
      const request = ++readSequence;
      keyReads.set(key, request);
      const controller = new AbortController();
      controllers.add(controller);
      loading.value = true;
      error.value = "";
      try {
        let snapshot = await pagesApi.accounts(normalized, controller.signal);
        const current = () => capturedSession === session && capturedMutation === mutationEpoch && capturedPaging === pagingEpoch
          && keyReads.get(key) === request;
        // Only explicitly selected card slices need an extra bounded read. Both
        // headers and rows are fetched afresh, even if the read version is stable
        // across a cooldown deadline. One restart reconciles an intervening write.
        for (let attempt = 0; current(); attempt++) {
          const selectedCards = snapshot.cards.filter(card => retainedPaging[card.cardId]
            && retainedPaging[card.cardId].offset !== card.rowsOffset);
          if (!selectedCards.length) break;
          const settled = await mapWithConcurrency(selectedCards, 4, card => pagesApi.accountCredentials(card.cardId,
            { ...normalized, offset: retainedPaging[card.cardId].offset, limit: ACCOUNT_CARD_ROW_SIZE }, controller.signal));
          if (!current()) return snapshot;
          const slices = settled.map(result => {
            if (result.status === "rejected") throw result.reason;
            return result.value;
          });
          if (currentProcess(snapshot.revision) && slices.every(slice => slice.readVersion === snapshot.readVersion
            && slice.asOf === snapshot.asOf && currentProcess(slice.revision))) {
            snapshot = { ...snapshot, cards: snapshot.cards.map(card => {
              const slice = slices.find(result => result.cardId === card.cardId);
              return slice ? { ...card, rows: slice.rows, rowsOffset: slice.offset, rowsHasMore: slice.hasMore } : card;
            }) };
            break;
          }
          if (attempt) throw new Error("Account page changed during credential revalidation");
          snapshot = await pagesApi.accounts(normalized, controller.signal);
        }
        if (!current() || !currentProcess(snapshot.revision)) return snapshot;
        cached.set(key, snapshot);
        if (cached.size > 8) {
          const evicted = cached.keys().next().value!;
          cached.delete(evicted); cachedPaging.delete(evicted); reads.invalidate(evicted);
        }
        reads.markSuccessful(key);
        if (selected === generation) {
          page.value = snapshot;
          cardPaging.value = snapshotPaging(snapshot);
          pagingKey = key;
          cachedPaging.set(key, cardPaging.value);
        }
        return snapshot;
      } catch (cause) {
        if (selected === generation && capturedSession === session && !controller.signal.aborted) {
          error.value = cause instanceof Error ? cause.message : String(cause);
        }
        throw cause;
      } finally {
        controllers.delete(controller);
        if (selected === generation) loading.value = false;
      }
    }).then(snapshot => {
      // A joined flight still belongs to the most recently selected query.
      if (selected === generation && capturedSession === session && capturedMutation === mutationEpoch && capturedPaging === pagingEpoch && currentProcess(snapshot.revision)) {
        page.value = snapshot;
        cardPaging.value = snapshotPaging(snapshot);
        pagingKey = key;
        cachedPaging.set(key, cardPaging.value);
        loading.value = false;
      }
      return snapshot;
    }).catch(cause => {
      if (selected === generation && capturedSession === session && capturedMutation === mutationEpoch && cause?.name !== "AbortError") {
        error.value = cause instanceof Error ? cause.message : String(cause);
      }
      throw cause;
    }).finally(() => {
      if (selected === generation) loading.value = false;
    });
  }

  async function loadCredentials(cardId: string, offset: number): Promise<void> {
    // A user's selected slice supersedes an in-flight global revalidation.
    pagingEpoch++;
    generation++;
    loading.value = false;
    const cardGeneration = (cardGenerations.get(cardId) ?? 0) + 1;
    cardGenerations.set(cardId, cardGeneration);
    const selectedKey = accountsQueryKey(query.value);
    reads.invalidate(selectedKey);
    const capturedSession = session;
    const capturedMutation = mutationEpoch;
    cardLoading.value = { ...cardLoading.value, [cardId]: true };
    cardErrors.value = { ...cardErrors.value, [cardId]: "" };
    try {
      let result = await pagesApi.accountCredentials(cardId, { ...query.value, offset, limit: ACCOUNT_CARD_ROW_SIZE });
      const current = () => capturedSession === session && capturedMutation === mutationEpoch
        && currentProcess(result.revision) && cardGenerations.get(cardId) === cardGeneration
        && selectedKey === accountsQueryKey(query.value) && !!page.value;
      const matchesHeader = () => page.value?.readVersion === result.readVersion && page.value?.asOf === result.asOf;
      if (!current()) return;
      if (!matchesHeader()) {
        // Source revisions can stay stable across a cooldown deadline. Rows
        // must also share the header's observation time before they commit.
        reads.invalidate(selectedKey);
        cached.delete(selectedKey);
        cachedPaging.delete(selectedKey);
        await load(query.value);
        if (!current()) return;
        if (!matchesHeader()) result = await pagesApi.accountCredentials(cardId, { ...query.value, offset, limit: ACCOUNT_CARD_ROW_SIZE });
        if (!current() || !matchesHeader()) return;
      }
      const header = page.value;
      if (!header) return;
      const nextPage: AccountsPage = {
        ...header,
        cards: header.cards.map(card => card.cardId === cardId ? {
          ...card, rows: result.rows, rowsOffset: result.offset, rowsHasMore: result.hasMore,
        } : card),
      };
      page.value = nextPage;
      cardPaging.value = { ...cardPaging.value, [cardId]: { offset: result.offset, hasMore: result.hasMore } };
      cachedPaging.set(selectedKey, cardPaging.value);
      cached.set(selectedKey, nextPage);
    } catch (cause) {
      if (capturedSession === session && capturedMutation === mutationEpoch && selectedKey === accountsQueryKey(query.value) && cardGenerations.get(cardId) === cardGeneration) {
        cardErrors.value = { ...cardErrors.value, [cardId]: cause instanceof Error ? cause.message : String(cause) };
      }
      throw cause;
    } finally {
      if (capturedSession === session && cardGenerations.get(cardId) === cardGeneration) {
        cardLoading.value = { ...cardLoading.value, [cardId]: false };
      }
    }
  }

  function loadDetail(id: string, options?: ReadOptions): Promise<AccountDetail> {
    const cachedDetail = details.value.get(id);
    const binding = page.value?.cards.flatMap(card => card.rows).find(row => row.account?.id === id)?.account?.updatedAt;
    const key = `detail:${id}:${binding ?? ""}`;
    return reads.run(key, cachedDetail ? options : undefined, () => details.value.get(id)!, async () => {
      const request = (detailGenerations.get(id) ?? 0) + 1;
      detailGenerations.set(id, request);
      const capturedSession = session;
      const capturedMutation = mutationEpoch;
      const controller = new AbortController();
      controllers.add(controller);
      detailLoading.value = { ...detailLoading.value, [id]: true };
      detailErrors.value = { ...detailErrors.value, [id]: "" };
      try {
        const detail = await pagesApi.accountDetail(id, controller.signal);
        if (capturedSession === session && capturedMutation === mutationEpoch && currentProcess(detail.revision) && detailGenerations.get(id) === request) {
          details.value = new Map(details.value).set(id, detail);
          reads.markSuccessful(key);
        }
        return detail;
      } catch (cause) {
        if (capturedSession === session && capturedMutation === mutationEpoch && detailGenerations.get(id) === request && !controller.signal.aborted) {
          detailErrors.value = { ...detailErrors.value, [id]: cause instanceof Error ? cause.message : String(cause) };
        }
        throw cause;
      } finally {
        controllers.delete(controller);
        if (capturedSession === session && detailGenerations.get(id) === request) {
          detailLoading.value = { ...detailLoading.value, [id]: false };
        }
      }
    });
  }

  function refreshAccount(id: string, mode: "automatic" | "manual", expectation?: MutationExpectation): Promise<AccountPageRefresh | null> {
    const flight = refreshFlights.get(id);
    if (flight) return flight;
    const capturedSession = session;
    const boundVersion = page.value?.cards.flatMap(card => card.rows).find(row => row.account?.id === id)?.account?.updatedAt;
    refreshStates.value = { ...refreshStates.value, [id]: "running" };
    let promise!: Promise<AccountPageRefresh | null>;
    promise = (async () => {
      try {
        if (!expectation && !control.hasTokens()) await control.refresh();
        const result = await pagesApi.refreshAccount(id, mode, expectation ?? control.expectation());
        if (capturedSession !== session || !currentProcess(result.revision)) return null;
        const rowVersion = page.value?.cards.flatMap(card => card.rows).find(row => row.account?.id === id)?.account?.updatedAt;
        if (boundVersion !== rowVersion || (page.value?.revision.processGeneration === result.revision.processGeneration
          && page.value.revision.revision > result.revision.revision)) return null;
        if (result.outcome === "refreshed" || result.outcome === "partial") {
          invalidate();
          invalidateManagementPages("accountPage");
          details.value = new Map([...details.value].filter(([key]) => key !== id));
          if (page.value) page.value = { ...page.value, revision: result.revision,
            cards: page.value.cards.map(card => ({ ...card, rows: card.rows.map(row => row.account?.id === id
              ? { ...row, billing: result.billing ?? row.billing, refresh: result.refresh,
                  account: result.account ? { ...row.account, updatedAt: result.account.updated_at,
                    cooldownUntil: result.account.cooldown_until, authError: result.account.auth_error,
                    enabled: result.account.enabled } : row.account }
              : row) })) };
        }
        return result;
      } finally {
        if (capturedSession === session) {
          const next = { ...refreshStates.value };
          delete next[id];
          refreshStates.value = next;
        }
        if (refreshFlights.get(id) === promise) refreshFlights.delete(id);
      }
    })();
    refreshFlights.set(id, promise);
    return promise;
  }

  /** Fence old GETs immediately at a write receipt; revalidation stays off the save lock. */
  function noteMutation(): void {
    invalidate();
    details.value = new Map();
  }

  function removeAccount(id: string): void {
    noteMutation();
    if (page.value) page.value = {
      ...page.value,
      cards: page.value.cards.map(card => ({ ...card, rows: card.rows.filter(row => row.account?.id !== id) })),
    };
  }

  function commitBilling(accountId: string, binding: string, status: BillingStatus): void {
    if (!page.value || !currentProcess(status)) return;
    const matches = (row: AccountsPage["cards"][number]["rows"][number]) => row.account?.id === accountId
      && billingBinding(row.account.updatedAt, row.inferenceEndpointUrl) === binding
      && (!row.billing || row.billing.processGeneration !== status.processGeneration || row.billing.revision <= status.revision);
    if (!page.value.cards.some(card => card.rows.some(matches))) return;
    invalidate();
    page.value = { ...page.value, cards: page.value.cards.map(card => ({ ...card,
      rows: card.rows.map(row => matches(row) ? { ...row, billing: status } : row) })) };
  }

  function clear(): void {
    session++;
    invalidate();
    page.value = null;
    query.value = normalizeAccountsQuery();
    details.value = new Map();
    detailLoading.value = {};
    detailErrors.value = {};
    cardLoading.value = {};
    cardErrors.value = {};
    cardPaging.value = {};
    pagingKey = "";
    refreshStates.value = {};
    refreshFlights.clear();
    error.value = "";
  }

  return { page: computed(() => page.value), query: computed(() => query.value),
    loaded: computed(() => page.value !== null), loading, error, details: computed(() => details.value),
    detailLoading, detailErrors, cardLoading, cardErrors, cardPaging, refreshStates,
    load, loadCredentials, loadDetail, refreshAccount, noteMutation, invalidate: noteMutation, removeAccount, commitBilling, clear, clearAccountPage: clear };
});
