import { requestV4, requestDashboard, withExpectation, type WithoutExpectation } from "./dashboard-v3.ts";
import type { MutationExpectation } from "./generated/dashboard-v3.ts";
import type { CopilotApplication, CopilotTarget, CopilotInstallRequest, CopilotMutationRequest } from "./generated/dashboard-v4.ts";
export type { CopilotTarget };
export type CopilotApplicationView = CopilotApplication;
export type CopilotInstallInput = WithoutExpectation<CopilotInstallRequest>;
export type CopilotMutationInput = WithoutExpectation<CopilotMutationRequest>;
const PATH = "/applications/copilot-extension";
export const copilotApplicationApi = {
  inspect: (target: CopilotTarget): Promise<CopilotApplicationView> => {
    const query = new URLSearchParams();
    for (const [key, value] of Object.entries(target)) if (value) query.set(key, value);
    return requestV4(`${PATH}${query.size ? `?${query}` : ""}`);
  },
  install: (input: CopilotInstallInput, expectation: MutationExpectation): Promise<CopilotApplicationView> =>
    requestV4(PATH, { method: "POST", body: withExpectation(input, expectation) }),
  disconnect: (input: CopilotMutationInput, expectation: MutationExpectation): Promise<CopilotApplicationView> =>
    requestV4(`${PATH}/disconnect`, { method: "POST", body: withExpectation(input, expectation) }),
  uninstall: (input: CopilotMutationInput, expectation: MutationExpectation): Promise<CopilotApplicationView> =>
    requestV4(PATH, { method: "DELETE", body: withExpectation(input, expectation) }),
  downloadPackage: (): Promise<Blob> => requestDashboard("v4", `${PATH}/package`, {}, true, "blob"),
};
