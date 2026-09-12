import { computed, ref } from "vue";
import { defineStore } from "pinia";
import {
  dashboardV3,
  isRevisionConflict,
  setControlRevisionSink,
  type ControlPlaneTokens,
} from "../api/dashboard-v3.ts";
import type { MutationExpectation } from "../api/generated/dashboard-v3.ts";

/**
 * Control-plane identity tokens (`revision` / `processGeneration` /
 * `pricingRevision`). Every V3 payload carries them; the client transport
 * forwards each observed pair here through the revision sink, so mutations
 * always start from the freshest tokens the session has seen.
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
  const pricingRevision = ref<string | null>(null);

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
    revision.value = tokens.revision;
    processGeneration.value = tokens.processGeneration;
    if (typeof tokens.pricingRevision === "string") {
      pricingRevision.value = tokens.pricingRevision;
    }
  }

  // The V3 transport calls this for every response that carries tokens.
  setControlRevisionSink(sync);

  /** Fresh CAS tokens from `GET /contract`. */
  async function refresh(): Promise<MutationExpectation> {
    const contract = await dashboardV3.getContract();
    sync(contract);
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
    try {
      return await mutate(captured ?? expectation());
    } catch (error) {
      if (!isRevisionConflict(error)) throw error;
      try {
        await refresh();
      } catch {
        // Keep the original 409 when the contract GET fails.
      }
      throw error;
    }
  }

  function reset(): void {
    revision.value = null;
    processGeneration.value = null;
    pricingRevision.value = null;
  }

  return {
    revision: computed(() => revision.value),
    processGeneration: computed(() => processGeneration.value),
    pricingRevision: computed(() => pricingRevision.value),
    sync,
    refresh,
    hasTokens,
    expectation,
    runMutation,
    reset,
  };
});
