/**
 * Account billing client. Wire DTOs are the generated dashboard-v4 types.
 */

import { requestV4, withExpectation, type WithoutExpectation } from "./dashboard-v3.ts";
import type { MutationExpectation } from "./generated/dashboard-v3.ts";
import type {
  BillingStatus,
  CreditBucket,
  BillingSnapshots,
  CreditCalibrationRequest,
  CreditGrantRequest,
  MonthlyCredits,
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
  CreditConfiguration,
  CreditGrantRequest,
  MonthlyCredits,
  CreditMeterView,
  CreditPreset,
  CreditRate,
  ProviderUsage,
} from "./generated/dashboard-v4.ts";

function billingPath(id: string): string {
  return `/accounts/${encodeURIComponent(id)}/billing`;
}

function creditsPath(id: string): string {
  return `${billingPath(id)}/credits`;
}

/**
 * Manual credit setup body. Matches `CreditConfigurationWrite`: name, currency,
 * monthly, and sourceUrl. Historical rates and creditsPerCurrency stay on the
 * stored meter and are not part of this write.
 */
export interface CreditConfigurationWrite {
  name: string;
  currency: string;
  monthly: MonthlyCredits | null;
  sourceUrl: string | null;
}

export interface CreditConfigureWrite {
  configuration: CreditConfigurationWrite;
  /** Required for initial setup; omitted when a settings edit must keep balances. */
  initialBuckets?: CreditBucket[] | null;
}

export function creditConfigurationWrite(input: {
  name: string;
  currency: string;
  monthly: MonthlyCredits | null;
  sourceUrl: string | null;
}): CreditConfigurationWrite {
  return {
    name: input.name,
    currency: input.currency,
    monthly: input.monthly,
    sourceUrl: input.sourceUrl,
  };
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
  snapshots: (accountIds: string[]) => requestV4<BillingSnapshots>("/billing/snapshots", {
    method: "POST", body: JSON.stringify({ accountIds }),
  }),
  status: (id: string) => requestV4<BillingStatus>(billingPath(id)),
  configureCredits: (
    id: string,
    input: CreditConfigureWrite,
    expectation?: MutationExpectation,
  ) => withCas(
    (tokens) => {
      const body: CreditConfigureWrite = {
        configuration: creditConfigurationWrite(input.configuration),
      };
      if (input.initialBuckets !== undefined) body.initialBuckets = input.initialBuckets;
      return requestV4<BillingStatus>(creditsPath(id), {
        method: "PUT",
        body: withExpectation(body, tokens),
      });
    },
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
