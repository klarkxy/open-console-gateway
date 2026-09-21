/**
 * Account billing client. Wire DTOs are the generated dashboard-v4 types.
 */

import { requestV4, withExpectation, type WithoutExpectation } from "./dashboard-v3.ts";
import type { MutationExpectation } from "./generated/dashboard-v3.ts";
import type {
  BillingStatus,
  CreditCalibrationRequest,
  CreditConfigureRequest,
  CreditGrantRequest,
} from "./generated/dashboard-v4.ts";
import { useControlPlaneStore } from "../stores/controlPlane.ts";

export type {
  BillingModel,
  BillingSource,
  BillingStatus,
  CreditBalanceCorrection,
  CreditBucket,
  CreditBucketKind,
  CreditCalibrationRequest,
  CreditConfigureRequest,
  CreditConfiguration,
  CreditGrantRequest,
  CreditMeterView,
  CreditPreset,
  CreditRate,
  MonthlyCredits,
  ProviderUsage,
} from "./generated/dashboard-v4.ts";

function billingPath(id: string): string {
  return `/accounts/${encodeURIComponent(id)}/billing`;
}

function creditsPath(id: string): string {
  return `${billingPath(id)}/credits`;
}

async function withCas<T>(
  run: (expectation: MutationExpectation) => Promise<T>,
  captured?: MutationExpectation,
): Promise<T> {
  const control = useControlPlaneStore();
  if (!captured && !control.hasTokens()) await control.refresh();
  return control.runMutation(run, captured);
}

export const billingApi = {
  status: (id: string) => requestV4<BillingStatus>(billingPath(id)),
  configureCredits: (
    id: string,
    input: WithoutExpectation<CreditConfigureRequest>,
    expectation?: MutationExpectation,
  ) => withCas(
    (tokens) => requestV4<BillingStatus>(creditsPath(id), {
      method: "PUT",
      body: withExpectation(input, tokens),
    }),
    expectation,
  ),
  calibrateCredits: (
    id: string,
    input: WithoutExpectation<CreditCalibrationRequest>,
    expectation?: MutationExpectation,
  ) => withCas(
    (tokens) => requestV4<BillingStatus>(`${creditsPath(id)}/calibrate`, {
      method: "POST",
      body: withExpectation(input, tokens),
    }),
    expectation,
  ),
  grantCredits: (
    id: string,
    input: WithoutExpectation<CreditGrantRequest>,
    expectation?: MutationExpectation,
  ) => withCas(
    (tokens) => requestV4<BillingStatus>(`${creditsPath(id)}/grants`, {
      method: "POST",
      body: withExpectation(input, tokens),
    }),
    expectation,
  ),
  disableCredits: (id: string, expectation?: MutationExpectation) => withCas(
    (tokens) => requestV4<BillingStatus>(creditsPath(id), {
      method: "DELETE",
      body: withExpectation({}, tokens),
    }),
    expectation,
  ),
};
