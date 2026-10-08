/**
 * Transport wrapper for the BYOK local-client endpoints under
 * `/dashboard/api/v4/applications/byok/{client}`.
 *
 * The backend DTOs (`ByokApplication`, `ByokConfigureRequest`,
 * `ByokMutationRequest`) are generated into `generated/dashboard-v4.ts` by the
 * contract owner; this module only wires them onto the shared V4 transport.
 * Configure sends target, fingerprint, and closed-client acknowledgement —
 * never a Key id, model list, or plaintext.
 */

import { requestV4, withExpectation, type WithoutExpectation } from "./dashboard-v3.ts";
import type { MutationExpectation } from "./generated/dashboard-v3.ts";
import type {
  ByokApplication,
  ByokConfigureRequest,
  ByokMutationRequest,
  ByokPreview,
  ByokPreviewRequest,
  CopilotTokenBudget,
} from "./generated/dashboard-v4.ts";

export type ByokUpdatePreview = ByokPreview;
export type CopilotBudget = CopilotTokenBudget;
export type ByokPreviewInput = ByokPreviewRequest;
export type ByokApplicationView = ByokApplication;
export type ByokConfigureInput = WithoutExpectation<ByokConfigureRequest>;
export type ByokMutationInput = WithoutExpectation<ByokMutationRequest>;

function byokPath(client: string, suffix = ""): string {
  return `/applications/byok/${encodeURIComponent(client)}${suffix}`;
}

export const byokApplicationsApi = {
  preview: (client: string, input: ByokPreviewInput): Promise<ByokApplicationView> =>
    requestV4<ByokApplicationView>(byokPath(client, "/preview"), { method: "POST", body: JSON.stringify(input) }),
  inspect: (client: string, targetPath?: string): Promise<ByokApplicationView> => {
    const query = targetPath ? `?targetPath=${encodeURIComponent(targetPath)}` : "";
    return requestV4<ByokApplicationView>(`${byokPath(client)}${query}`);
  },
  configure: (
    client: string,
    input: ByokConfigureInput,
    expectation: MutationExpectation,
  ): Promise<ByokApplicationView> =>
    requestV4<ByokApplicationView>(byokPath(client), {
      method: "POST",
      body: withExpectation(input, expectation),
    }),
  remove: (
    client: string,
    input: ByokMutationInput,
    expectation: MutationExpectation,
  ): Promise<ByokApplicationView> =>
    requestV4<ByokApplicationView>(byokPath(client), {
      method: "DELETE",
      body: withExpectation(input, expectation),
    }),
  recover: (
    client: string,
    input: ByokMutationInput,
    expectation: MutationExpectation,
  ): Promise<ByokApplicationView> =>
    requestV4<ByokApplicationView>(byokPath(client, "/recover"), {
      method: "POST",
      body: withExpectation(input, expectation),
    }),
};
