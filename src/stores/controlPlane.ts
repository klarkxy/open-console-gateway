import { computed, ref } from "vue";
import { defineStore } from "pinia";
import {
  DashboardConflictError,
  dashboardV3,
  isRevisionConflict,
  setControlRevisionSink,
  type ControlPlaneTokens,
} from "../api/dashboard-v3.ts";
import type { MutationExpectation } from "../api/generated/dashboard-v3.ts";

/** Queued local write dropped before dispatch (logout / session reset). */
export class LocalMutationCancelledError extends Error {
  constructor() {
    super("local mutation cancelled before dispatch");
    this.name = "LocalMutationCancelledError";
  }
}

export function isLocalMutationCancelled(error: unknown): error is LocalMutationCancelledError {
  return error instanceof LocalMutationCancelledError;
}

/**
 * A second write for an already queued/in-flight target. Rejection is the
 * duplicate-protection signal: callers must never treat it as committed.
 */
export class LocalMutationBusyError extends Error {
  constructor(target: string) {
    super(`a local mutation is already in flight for ${target}`);
    this.name = "LocalMutationBusyError";
  }
}

export function isLocalMutationBusy(error: unknown): error is LocalMutationBusyError {
  return error instanceof LocalMutationBusyError;
}

/**
 * Control-plane identity tokens (`revision` / `processGeneration`). The
 * client transport forwards each published pair here through the revision
 * sink, so mutations always start from the freshest tokens the session has
 * seen. This store remains the owner: the transport only reads the current
 * process. It withholds a response that still names the process captured at
 * dispatch after a different process became current. A published different
 * process is adopted as an opaque identity, and a lower revision of the same
 * process is ignored. A historical `pricingRevision` on the same payload is
 * not a CAS token and is not stored.
 *
 * 409 recovery deliberately does not replay mutations. A conflict refreshes
 * the tokens from `GET /contract` and then surfaces the original error so the
 * owning page/store can reload the affected resource and ask the user to
 * re-apply their change. This avoids turning an apparently generic mutation
 * into an unsafe automatic retry.
 */
export const useControlPlaneStore = defineStore("controlPlane", () => {
  const revision = ref<number | null>(null);
  const processGeneration = ref<number | null>(null);

  let backendEpoch = 0;

  function sync(tokens: ControlPlaneTokens): void {
    // A delayed older GET must not roll the CAS revision back within the same
    // backend process generation. Across generations there is no comparable
    // ordering, so a different generation is always adopted as-is.
    if (
      processGeneration.value !== null
      && tokens.processGeneration === processGeneration.value
      && revision.value !== null
      && tokens.revision < revision.value
    ) {
      return;
    }
    if (processGeneration.value !== null && tokens.processGeneration !== processGeneration.value) {
      backendEpoch += 1;
    }
    revision.value = tokens.revision;
    processGeneration.value = tokens.processGeneration;
  }

  function currentProcess(): number | null {
    return processGeneration.value;
  }

  // The transport calls `sync` for a response it is willing to publish, and
  // reads the live process through `currentProcess` without writing it.
  setControlRevisionSink(sync, currentProcess);

  function clearTokens(): void {
    revision.value = null;
    processGeneration.value = null;
  }

  /**
   * Fresh CAS tokens from `GET /contract`. The transport is the sole publisher
   * of that body: it has already applied the pair, or withheld an origin
   * process that is no longer current. Re-applying the raw contract would
   * put the withheld process back.
   */
  async function refresh(): Promise<MutationExpectation> {
    const session = localSession;
    await dashboardV3.getContract();
    // A reset that landed during the await wins: its response must not
    // repopulate the new session's tokens, and waiters must not dispatch.
    if (session !== localSession) {
      throw new LocalMutationCancelledError();
    }
    return expectation();
  }

  function hasTokens(): boolean {
    return revision.value !== null && processGeneration.value !== null;
  }

  /**
   * Current CAS tokens for a mutation. Throws when nothing has been loaded
   * yet; callers that can hit this should `await refresh()` first.
   */
  function expectation(): MutationExpectation {
    if (revision.value === null || processGeneration.value === null) {
      throw new Error("control-plane tokens are not loaded yet");
    }
    return { expectedRevision: revision.value, processGeneration: processGeneration.value };
  }

  /**
   * Run once. `captured` pins the tokens the editor loaded with; omit it to
   * use the store's current pair. On 409 refresh tokens, but never replay.
   * A failed `GET /contract` must not replace the original conflict.
   */
  async function runMutation<T>(
    mutate: (expectation: MutationExpectation) => Promise<T>,
    captured?: MutationExpectation,
  ): Promise<T> {
    const session = localSession;
    try {
      return await mutate(captured ?? expectation());
    } catch (error) {
      if (session !== localSession || !isRevisionConflict(error)) throw error;
      try {
        await refresh();
      } catch {
        // Keep the original 409 when the contract GET fails.
      }
      throw error;
    }
  }

  // Short local config/publication writes are serialized on this lane so two
  // quick independent edits cannot self-conflict on the shared CAS pair: the
  // next write dispatches only after the previous receipt synced its tokens.
  // Only fast local writes belong here — never network refreshes, probes,
  // usage reads, or installation work, which would reintroduce global waiting.
  let localLane: Promise<unknown> = Promise.resolve();
  let localSession = 0;
  const localTargets = new Map<string, Promise<unknown>>();

  /**
   * Run a short local CAS write on the serial lane. `target` gives per-target
   * duplicate protection: a second submission for the same target while one
   * is queued/in flight is rejected (`LocalMutationBusyError`) — distinct
   * verbs/payloads are never coalesced into one promise, so a queued delete
   * behind an update can never report the update's success. Uncaptured
   * intents read the freshest tokens at dispatch time; `captured` editor
   * expectations stay exactly as captured and are never rebased or replayed.
   * Work queued before a logout/session reset is dropped without dispatching,
   * and any known backend process-generation change between enqueue and
   * dispatch drops the queued write instead of sending it.
   */
  function runLocalMutation<T>(
    target: string,
    mutate: (expectation: MutationExpectation) => Promise<T>,
    captured?: MutationExpectation,
  ): Promise<T> {
    if (localTargets.has(target)) {
      return Promise.reject(new LocalMutationBusyError(target));
    }
    const session = localSession;
    const knownBackendEpoch = backendEpoch;
    const dispatched = localLane.then(async (): Promise<T> => {
      if (session !== localSession) throw new LocalMutationCancelledError();
      if (
        knownBackendEpoch !== backendEpoch
      ) {
        throw new DashboardConflictError(
          "the queued write predates the current backend process",
          revision.value,
          processGeneration.value,
        );
      }
      if (!captured && !hasTokens()) await refresh();
      // The refresh await may have spanned a reset or a generation change.
      if (session !== localSession) throw new LocalMutationCancelledError();
      if (
        knownBackendEpoch !== backendEpoch
        || (captured
          && processGeneration.value !== null
          && captured.processGeneration !== processGeneration.value)
      ) {
        throw new DashboardConflictError(
          "the queued edit predates the current backend process",
          revision.value,
          processGeneration.value,
        );
      }
      return runMutation(mutate, captured);
    });
    localLane = dispatched.catch(() => undefined);
    localTargets.set(target, dispatched);
    void dispatched.catch(() => undefined).finally(() => {
      // Identity check: after a reset, a new write for the same target owns
      // the slot; this stale finally must not clear it.
      if (localTargets.get(target) === dispatched) localTargets.delete(target);
    });
    return dispatched;
  }

  function reset(): void {
    // Queued local writes must never dispatch into a new session, and new
    // session writes must not wait on the prior session's in-flight work.
    localSession += 1;
    localTargets.clear();
    localLane = Promise.resolve();
    setControlRevisionSink(sync, currentProcess);
    clearTokens();
  }

  return {
    revision: computed(() => revision.value),
    processGeneration: computed(() => processGeneration.value),
    sync,
    refresh,
    hasTokens,
    expectation,
    runMutation,
    runLocalMutation,
    reset,
  };
});
