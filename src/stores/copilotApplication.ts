import { computed, ref } from "vue";
import { defineStore } from "pinia";
import { copilotApplicationApi, type CopilotApplicationView, type CopilotTarget, type CopilotInstallInput, type CopilotMutationInput } from "../api/copilot-application.ts";
import type { MutationExpectation } from "../api/generated/dashboard-v3.ts";
import { copilotTargetKey } from "../domain/copilot-application.ts";
import { dashboardErrorDetail } from "../utils/errors.ts";
export const useCopilotApplicationStore = defineStore("copilotApplication", () => {
  const application = ref<CopilotApplicationView | null>(null);
  const snapshotKey = ref<string | null>(null), loading = ref(false), mutating = ref(false), error = ref("");
  let generation = 0, session = 0, loads = 0;
  let inflight: { key: string; promise: Promise<void> } | null = null;
  async function runInspect(target: CopilotTarget): Promise<void> {
    const epoch = ++generation, origin = session; loading.value = true; loads++;
    try {
      const result = await copilotApplicationApi.inspect(target);
      if (epoch === generation && origin === session) { application.value = result; snapshotKey.value = copilotTargetKey(target); error.value = ""; }
    } catch (reason) { if (epoch === generation && origin === session) error.value = dashboardErrorDetail(reason); }
    finally { if (origin === session) { loads = Math.max(0, loads - 1); loading.value = loads > 0; } }
  }
  function inspect(target: CopilotTarget): Promise<void> {
    const key = copilotTargetKey(target); if (inflight?.key === key) return inflight.promise;
    const promise = runInspect(target).finally(() => { if (inflight?.promise === promise) inflight = null; });
    inflight = { key, promise }; return promise;
  }
  async function mutate(action: "install" | "disconnect" | "uninstall", input: CopilotInstallInput | CopilotMutationInput, expectation: MutationExpectation): Promise<CopilotApplicationView> {
    if (mutating.value || loading.value || application.value?.fingerprint !== input.expectedFingerprint || copilotTargetKey(application.value.target) !== copilotTargetKey(input.target)) throw new Error("Copilot target needs inspection");
    const epoch = ++generation, origin = session; mutating.value = true;
    try {
      const result = action === "install" ? await copilotApplicationApi.install(input as CopilotInstallInput, expectation) : await copilotApplicationApi[action](input, expectation);
      if (epoch === generation && origin === session) { application.value = result; error.value = ""; }
      return result;
    } catch (reason) { if (epoch === generation && origin === session) { error.value = dashboardErrorDetail(reason); if ((reason as { status?: number })?.status === 409) snapshotKey.value = null; } throw reason; }
    finally { if (origin === session) mutating.value = false; }
  }
  function clear(): void { session++; generation++; loads = 0; inflight = null; application.value = null; snapshotKey.value = null; error.value = ""; loading.value = false; mutating.value = false; }
  return { application: computed(() => application.value), snapshotKey: computed(() => snapshotKey.value), loading: computed(() => loading.value), mutating: computed(() => mutating.value), error: computed(() => error.value), inspect, mutate, clear };
});
