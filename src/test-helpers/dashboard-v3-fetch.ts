import { createPinia, setActivePinia } from "pinia";
import { useControlPlaneStore } from "../stores/controlPlane.ts";

export interface RecordedRequest {
  url: string;
  method: string;
  body: Record<string, unknown> | null;
}

export function installWindowDashboard(): void {
  Object.defineProperty(globalThis, "window", {
    configurable: true,
    value: { location: { pathname: "/dashboard" }, dispatchEvent() {} },
  });
}

export function installFetchMock(
  responder: (req: RecordedRequest) => Response | object | Promise<Response | object>,
): RecordedRequest[] {
  installWindowDashboard();
  const requests: RecordedRequest[] = [];
  Object.defineProperty(globalThis, "fetch", {
    configurable: true,
    value: async (input: string, init: RequestInit = {}) => {
      const request: RecordedRequest = {
        url: input,
        method: init.method ?? "GET",
        body: init.body ? JSON.parse(String(init.body)) as Record<string, unknown> : null,
      };
      requests.push(request);
      const result = await responder(request);
      return result instanceof Response
        ? result
        : new Response(JSON.stringify(result), { headers: { "Content-Type": "application/json" } });
    },
  });
  return requests;
}

export function setupControlPlane(
  revision = 7,
  processGeneration = 99,
  pricingRevision: string | null = null,
): void {
  setActivePinia(createPinia());
  useControlPlaneStore().sync({ revision, processGeneration, pricingRevision });
}

export function v3AccountDto(id: string, overrides: Record<string, unknown> = {}): object {
  return {
    id,
    name: "Account",
    username: "",
    password: "",
    key: "",
    enabled: true,
    accountType: "key",
    setupStep: "ready",
    providerId: "opencode",
    credentialKind: "api_key",
    quotaScope: "key",
    revision: 1,
    purchaseDate: "2026-07-15",
    expiresOn: "2026-08-15",
    cooldownUntil: null,
    cooldownGenericUntil: null,
    cooldown5hUntil: null,
    cooldownWeekUntil: null,
    cooldownMonthUntil: null,
    cooldownFreeUntil: null,
    lastError: null,
    authError: null,
    notes: "",
    usageSyncLastSuccessAt: null,
    usageSyncNextAllowedAt: null,
    createdAt: "2026-07-15T00:00:00Z",
    updatedAt: "2026-07-15T00:00:00Z",
    verificationStatus: "not_required",
    connectionVerifiedAt: null,
    verificationError: null,
    planRoutable: true,
    customConfig: null,
    modelCapabilities: [],
    ...overrides,
  };
}
