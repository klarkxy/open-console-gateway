import { requestV4, withExpectation } from "./dashboard-v3.ts";
import type { MutationExpectation } from "./generated/dashboard-v3.ts";
import type { OfficialApiStatus, OfficialApiPrices } from "./generated/dashboard-v4.ts";
import { useControlPlaneStore } from "../stores/controlPlane.ts";

export const officialApi = {
  status: (id: string) => requestV4<OfficialApiStatus>(`/accounts/${encodeURIComponent(id)}/official-api`),
  prices: (id: string) => requestV4<OfficialApiPrices>(`/providers/${encodeURIComponent(id)}/official-api/pricing`),
  refreshBalance: (id: string, expected: MutationExpectation) => useControlPlaneStore().runMutation(
    (tokens) => requestV4<OfficialApiStatus>(`/accounts/${encodeURIComponent(id)}/official-api/balance`, {
      method: "POST", body: withExpectation({}, tokens),
    }), expected,
  ),
  refreshPrices: (id: string, expected: MutationExpectation) => useControlPlaneStore().runMutation(
    (tokens) => requestV4<OfficialApiPrices>(`/providers/${encodeURIComponent(id)}/official-api/pricing`, {
      method: "POST", body: withExpectation({}, tokens),
    }), expected,
  ),
};
