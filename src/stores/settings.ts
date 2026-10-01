import { computed, ref } from "vue";
import { defineStore } from "pinia";
import { dashboardApi, isRevisionConflict } from "../api/dashboard.ts";
import type { AppConfig, SettingsPatch } from "../api/dashboard.ts";
import type { MutationAck, MutationExpectation } from "../api/generated/dashboard-v3.ts";

/**
 * Application settings.
 *
 * The Settings resource never carries Key plaintext (that lives in the
 * connection store). Update-check / install progress is transient and stays
 * page-local in the Settings view.
 *
 * A settings PUT receipt carries only the CAS revision: the acknowledged
 * draft is the caller's local projection, never the canonical resource (the
 * backend normalizes client root URLs, proxy model sets, and derived
 * capability/environment fields). The store stamps that revision onto the
 * last displayed snapshot and refreshes canonical fields with one background
 * GET. The confirmed CAS baseline stays on the last successful GET until
 * that refresh commits. `canonicalConfirmed` is that commit, and only that
 * commit: a PUT receipt clears it and stamps the revision onto the last
 * displayed snapshot. A failed refresh surfaces through `refreshError`
 * and does not fail the write.
 */
export const useSettingsStore = defineStore("settings", () => {
  const settings = ref<AppConfig | null>(null);
  const loading = ref(false);
  const error = ref("");
  /** Read-back failure after an acknowledged write; never a save failure. */
  const refreshError = ref("");

  // Overlapping loads resolve out of order; only the latest request commits
  // state. Stale calls still return/throw to their own caller unchanged.
  let loadGeneration = 0;
  // Bumped by clear() (logout / 401): in-flight reads and writes from the old
  // session still settle their callers, but never commit or start new reads.
  let sessionEpoch = 0;
  // Last canonical GET. A PUT ack stamps the displayed revision but does not
  // become this baseline: the next full write must not confirm last-good
  // fields under the post-ack pair.
  let confirmedBaseline: { revision: number; process_generation: number } | null = null;
  const canonicalConfirmed = ref(false);

  function commitCanonical(result: AppConfig): void {
    settings.value = result;
    confirmedBaseline = {
      revision: result.revision,
      process_generation: result.process_generation,
    };
    canonicalConfirmed.value = true;
    error.value = "";
    refreshError.value = "";
  }

  function expectationFor(update: AppConfig): MutationExpectation {
    if (
      confirmedBaseline
      && !canonicalConfirmed.value
      && (
        update.revision !== confirmedBaseline.revision
        || update.process_generation !== confirmedBaseline.process_generation
      )
    ) {
      return {
        expectedRevision: confirmedBaseline.revision,
        processGeneration: confirmedBaseline.process_generation,
      };
    }
    return {
      expectedRevision: update.revision,
      processGeneration: update.process_generation,
    };
  }

  async function load(): Promise<AppConfig> {
    const generation = ++loadGeneration;
    const session = sessionEpoch;
    loading.value = true;
    try {
      const result = await dashboardApi.getSettings();
      if (generation !== loadGeneration || session !== sessionEpoch) return result;
      commitCanonical(result);
      return result;
    } catch (e) {
      if (generation === loadGeneration && session === sessionEpoch) {
        error.value = e instanceof Error ? e.message : String(e);
      }
      throw e;
    } finally {
      if (generation === loadGeneration && session === sessionEpoch) loading.value = false;
    }
  }

  async function loadPresented(): Promise<AppConfig> {
    return load();
  }

  /**
   * Canonical read-back after an acknowledged write. Never rejects and never
   * owns `loading`: a stalled or failed refresh must not gate the next write.
   */
  async function revalidateCanonical(session: number): Promise<void> {
    const generation = ++loadGeneration;
    try {
      const result = await dashboardApi.getSettings();
      if (generation !== loadGeneration || session !== sessionEpoch) return;
      commitCanonical(result);
    } catch (cause) {
      if (generation !== loadGeneration || session !== sessionEpoch) return;
      refreshError.value = cause instanceof Error ? cause.message : String(cause);
    } finally {
      if (generation === loadGeneration) loading.value = false;
    }
  }

  /**
   * Save settings. Resolves at the PUT receipt with the acknowledged input
   * plus the new revision — the caller's local baseline projection, not the
   * normalized canonical resource. Canonical fields arrive through the
   * background revalidation, which stays out of view drafts.
   */
  function projectAck(update: AppConfig, ack: MutationAck): AppConfig {
    return {
      ...update,
      revision: ack.revision,
      process_generation: ack.processGeneration,
    };
  }

  function noteAck(ack: MutationAck): void {
    loadGeneration += 1;
    loading.value = false;
    canonicalConfirmed.value = false;
    if (!settings.value) return;
    settings.value = {
      ...settings.value,
      revision: ack.revision,
      process_generation: ack.processGeneration,
    };
  }

  async function recoverConflict(session: number, cause: unknown): Promise<void> {
    if (!isRevisionConflict(cause) || session !== sessionEpoch) return;
    try {
      await load();
    } catch {
      // A failed recovery reload never replaces the original conflict.
    }
  }

  async function putPresented(update: AppConfig): Promise<AppConfig> {
    const session = sessionEpoch;
    let ack: MutationAck;
    try {
      ack = await dashboardApi.updateSettings(update, expectationFor(update));
    } catch (cause) {
      await recoverConflict(session, cause);
      throw cause;
    }
    const projection = projectAck(update, ack);
    if (session !== sessionEpoch) {
      // The write reached the server, but the session it belonged to is
      // gone: settle the caller with its receipt and never commit the
      // projection or start a read-back into the reset store.
      return projection;
    }
    // The receipt supersedes earlier reads. Display the ack revision on the
    // last-good fields; the confirmed CAS baseline stays on the last GET.
    noteAck(ack);
    void revalidateCanonical(session);
    return projection;
  }

  /**
   * Partial presentation write for routing, invite URL, and host toggles.
   * Resolves at the PUT ack. One detached canonical GET follows when this
   * session is still current. The ack body is not a snapshot; callers that
   * only need the write to finish can ignore the resolution value.
   */
  async function patchPresented(patch: SettingsPatch): Promise<void> {
    if (
      patch.routing_mode === undefined
      && patch.conversation_sticky === undefined
      && patch.opencode_invite_url === undefined
      && patch.auto_start === undefined
      && patch.show_dock_icon === undefined
    ) return;
    const session = sessionEpoch;
    let ack: MutationAck;
    try {
      ack = await dashboardApi.patchSettings(patch);
    } catch (cause) {
      await recoverConflict(session, cause);
      throw cause;
    }
    if (session !== sessionEpoch) return;
    noteAck(ack);
    if (settings.value) settings.value = { ...settings.value, ...patch };
    void revalidateCanonical(session);
  }

  /** Session teardown (logout / 401): cached settings die with the session. */
  function clear(): void {
    sessionEpoch += 1;
    loadGeneration += 1;
    settings.value = null;
    confirmedBaseline = null;
    canonicalConfirmed.value = false;
    error.value = "";
    refreshError.value = "";
    loading.value = false;
  }

  return {
    settings: computed(() => settings.value),
    canonicalConfirmed: computed(() => canonicalConfirmed.value),
    loading: computed(() => loading.value),
    error: computed(() => error.value),
    refreshError: computed(() => refreshError.value),
    loadPresented,
    putPresented,
    patchPresented,
    clear,
  };
});
