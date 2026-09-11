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
  OnboardingCommitRequest,
  OnboardingCommitResult,
  TemplateList,
} from "./generated/dashboard-v4.ts";

export const dashboardV4 = {
  getContract: () => requestV4<ControlRevision>("/contract"),
  getTemplates: () => requestV4<TemplateList>("/templates"),
  getConnections: () => requestV4<ConnectionList>("/connections"),
  commitOnboarding: (
    input: WithoutExpectation<OnboardingCommitRequest>,
    expectation: MutationExpectation,
  ) => requestV4<OnboardingCommitResult>("/onboarding/commit", {
    method: "POST",
    body: withExpectation(input, expectation),
  }),
};
