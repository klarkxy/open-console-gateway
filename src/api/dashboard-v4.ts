/**
 * Hand-written Dashboard V4 endpoint client for the additive
 * `/dashboard/api/v4` contract (schema/dashboard-api-v4.schema.json).
 *
 * Transport, error classes, and CAS token publishing are shared with V3.
 * Generated types stay in `generated/dashboard-v4.ts` (types only).
 */

import { requestV4, withExpectation, type WithoutExpectation } from "./dashboard-v3.ts";
import type { MutationExpectation } from "./generated/dashboard-v3.ts";
import type {
  ConnectionList,
  ControlRevision,
  CredentialRotateRequest,
  CredentialRotateResult,
  IdentityList,
  OnboardingCommitRequest,
  OnboardingCommitResult,
  TemplateList,
} from "./generated/dashboard-v4.ts";

export const dashboardV4 = {
  getContract: () => requestV4<ControlRevision>("/contract"),
  getTemplates: () => requestV4<TemplateList>("/templates"),
  getConnections: () => requestV4<ConnectionList>("/connections"),
  getAccounts: () => requestV4<IdentityList>("/accounts"),
  commitOnboarding: (
    input: WithoutExpectation<OnboardingCommitRequest>,
    expectation: MutationExpectation,
  ) => requestV4<OnboardingCommitResult>("/onboarding/commit", {
    method: "POST",
    body: withExpectation(input, expectation),
  }),
  rotateCredential: (
    id: string,
    input: WithoutExpectation<CredentialRotateRequest>,
    expectation: MutationExpectation,
  ) => requestV4<CredentialRotateResult>(
    `/credentials/${encodeURIComponent(id)}/rotate`,
    {
      method: "POST",
      body: withExpectation(input, expectation),
    },
  ),
};
