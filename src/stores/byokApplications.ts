import { computed, reactive } from "vue";
import { defineStore } from "pinia";
import {
  byokApplicationsApi,
  type ByokApplicationView,
  type ByokConfigureInput,
  type ByokMutationInput,
  type ByokPreviewInput,
} from "../api/byok-applications.ts";
import type { MutationExpectation } from "../api/generated/dashboard-v3.ts";
import { dashboardErrorDetail } from "../utils/errors.ts";

export interface ByokEntryState {
  view: ByokApplicationView | null;
  loaded: boolean;
  loading: boolean;
  mutating: boolean;
  error: string;
}

interface ByokEntryInternal extends ByokEntryState {
  loadInFlight: number;
  mutationInFlight: number;
}

function emptyEntry(): ByokEntryInternal {
  return {
    view: null,
    loaded: false,
    loading: false,
    mutating: false,
    error: "",
    loadInFlight: 0,
    mutationInFlight: 0,
  };
}

/**
 * Single owner of BYOK application server state, keyed by client + requested
 * target path so tabs (and custom-profile targets) never clobber each other.
 * Views keep only drafts and modal flags. Every write is generation-guarded
 * per key, so a stale response cannot replace a newer snapshot or a cleared
 * session; revalidation keeps the current content rendered while loading.
 * Same-target inspects in flight share one request.
 */
export const useByokApplicationsStore = defineStore("byokApplications", () => {
  const entries = reactive(new Map<string, ByokEntryInternal>());

  let sequence = 0;
  let sessionEpoch = 0;
  const generations = new Map<string, number>();
  const inflightInspect = new Map<string, Promise<void>>();

  function keyOf(client: string, targetPath?: string | null): string {
    return `${client}::${targetPath ?? ""}`;
  }

  function isCurrent(key: string, generation: number): boolean {
    return generation > sessionEpoch && generations.get(key) === generation;
  }

  /** Busy flags unwind unless the session was dropped; supersession is not a skip. */
  function isLive(generation: number): boolean {
    return generation > sessionEpoch;
  }

  function ensureEntry(key: string): ByokEntryInternal {
    if (!entries.has(key)) {
      entries.set(key, emptyEntry());
    }
    // Read back through the reactive Map: mutations must go through the proxy,
    // otherwise async commits bypass Vue tracking and consumers never update.
    return entries.get(key)!;
  }

  function beginLoad(key: string, entry: ByokEntryInternal): number {
    entry.loadInFlight += 1;
    entry.loading = true;
    const generation = ++sequence;
    generations.set(key, generation);
    return generation;
  }

  function endLoad(_key: string, entry: ByokEntryInternal, generation: number): void {
    if (!isLive(generation)) return;
    entry.loadInFlight = Math.max(0, entry.loadInFlight - 1);
    if (entry.loadInFlight === 0) entry.loading = false;
  }

  function beginMutation(key: string, entry: ByokEntryInternal): number {
    entry.mutationInFlight += 1;
    entry.mutating = true;
    const generation = ++sequence;
    generations.set(key, generation);
    return generation;
  }

  function endMutation(_key: string, entry: ByokEntryInternal, generation: number): void {
    if (!isLive(generation)) return;
    entry.mutationInFlight = Math.max(0, entry.mutationInFlight - 1);
    if (entry.mutationInFlight === 0) entry.mutating = false;
  }

  function commit(entry: ByokEntryInternal, result: ByokApplicationView): void {
    entry.view = result;
    entry.error = "";
    entry.loaded = true;
  }

  /** Current state for one client+target, or null when never loaded. */
  function peek(client: string, targetPath?: string | null): ByokEntryState | null {
    return entries.get(keyOf(client, targetPath)) ?? null;
  }

  async function runInspect(
    client: string,
    targetPath: string | undefined,
    options: { retain?: boolean },
  ): Promise<void> {
    const key = keyOf(client, targetPath);
    const entry = ensureEntry(key);
    const generation = beginLoad(key, entry);
    if (!options.retain) entry.error = "";
    try {
      const result = await byokApplicationsApi.inspect(client, targetPath);
      if (!isCurrent(key, generation)) return;
      commit(entry, result);
    } catch (reason) {
      if (!isCurrent(key, generation)) return;
      entry.error = dashboardErrorDetail(reason);
    } finally {
      endLoad(key, entry, generation);
    }
  }

  async function inspect(
    client: string,
    targetPath?: string,
    options: { retain?: boolean } = {},
  ): Promise<void> {
    const key = keyOf(client, targetPath);
    const existing = inflightInspect.get(key);
    if (existing) return existing;
    const pending = runInspect(client, targetPath, options).finally(() => {
      if (inflightInspect.get(key) === pending) inflightInspect.delete(key);
    });
    inflightInspect.set(key, pending);
    return pending;
  }

  /** Read-only preparation shares the same view and invalidates earlier reads. */
  async function preview(client: string, input: ByokPreviewInput): Promise<boolean> {
    const key = keyOf(client, input.targetPath);
    const entry = ensureEntry(key);
    const generation = beginLoad(key, entry);
    try {
      const result = await byokApplicationsApi.preview(client, input);
      if (!isCurrent(key, generation)) return false;
      commit(entry, result);
      return true;
    } catch (reason) {
      if (isCurrent(key, generation)) entry.error = dashboardErrorDetail(reason);
      throw reason;
    } finally {
      endLoad(key, entry, generation);
    }
  }

  function discardPreview(client: string, targetPath?: string | null): void {
    const key = keyOf(client, targetPath);
    generations.set(key, ++sequence);
    const entry = entries.get(key);
    if (entry?.view?.preview) entry.view = { ...entry.view, preview: null };
  }

  async function runMutation<TInput extends { targetPath?: string | null }>(
    client: string,
    input: TInput,
    expectation: MutationExpectation,
    run: (input: TInput, expectation: MutationExpectation) => Promise<ByokApplicationView>,
  ): Promise<ByokApplicationView> {
    const key = keyOf(client, input.targetPath);
    const entry = ensureEntry(key);
    const generation = beginMutation(key, entry);
    try {
      const result = await run(input, expectation);
      if (isCurrent(key, generation)) commit(entry, result);
      return result;
    } catch (reason) {
      if (isCurrent(key, generation)) {
        entry.error = dashboardErrorDetail(reason);
      }
      throw reason;
    } finally {
      endMutation(key, entry, generation);
    }
  }

  function configure(
    client: string,
    input: ByokConfigureInput,
    expectation: MutationExpectation,
  ): Promise<ByokApplicationView> {
    return runMutation(client, input, expectation,
      (body, exp) => byokApplicationsApi.configure(client, body, exp),
    );
  }

  function remove(
    client: string,
    input: ByokMutationInput,
    expectation: MutationExpectation,
  ): Promise<ByokApplicationView> {
    return runMutation(client, input, expectation,
      (body, exp) => byokApplicationsApi.remove(client, body, exp),
    );
  }

  function recover(
    client: string,
    input: ByokMutationInput,
    expectation: MutationExpectation,
  ): Promise<ByokApplicationView> {
    return runMutation(client, input, expectation,
      (body, exp) => byokApplicationsApi.recover(client, body, exp),
    );
  }

  /** Session teardown: wipe every cached entry and invalidate in-flight work. */
  function clear(): void {
    sessionEpoch = ++sequence;
    generations.clear();
    inflightInspect.clear();
    entries.clear();
  }

  return {
    entries: computed(() => entries),
    peek,
    inspect,
    preview,
    discardPreview,
    configure,
    remove,
    recover,
    clear,
  };
});
