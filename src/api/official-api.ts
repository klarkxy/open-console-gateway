import { requestV4, withExpectation } from "./dashboard-v3.ts";
import type { MutationExpectation } from "./generated/dashboard-v3.ts";
import type { OfficialApiStatus } from "./generated/dashboard-v4.ts";
import { useControlPlaneStore } from "../stores/controlPlane.ts";

export const officialApi = {
  status: (id: string) => requestV4<OfficialApiStatus>(`/accounts/${encodeURIComponent(id)}/official-api`),
  refreshBalance: (id: string, expected: MutationExpectation) => useControlPlaneStore().runMutation(
    (tokens) => requestV4<OfficialApiStatus>(`/accounts/${encodeURIComponent(id)}/official-api/balance`, {
      method: "POST", body: withExpectation({}, tokens),
    }), expected,
  ),
};
