import { t } from "../i18n/index.ts";
import type {
  Account,
  AccountExport,
  AccountExportRequest,
  AccountImportPreview,
  AccountImportPreviewRequest,
  AccountImportRequest,
  AccountImportResult,
  AccountCreate,
  AccountCustomConfigUpdate,
  AccountList,
  AccountManagedCreate,
  AccountManagedKeyVerify,
  AccountModelTestRequest,
  AccountModelTestResponse,
  AccountModelCapabilitiesUpdate,
  AccountMutation,
  AccountOrder,
  AccountSetupUpdate,
  AccountUpdate,
  AccountUsageUpdate,
  AuthLogin,
  AuthRegister,
  AuthStatus,
  BrowserCapabilities,
  BrowserOpen,
  BrowserOpenRequest,
  ConnectionInfo,
  CpaAccountDelete,
  CpaAccountStatusUpdate,
  CpaAccounts,
  CpaCliImportRequest,
  CpaCliImportResult,
  CpaCliImports,
  CpaConnectionReport,
  CpaIntegration,
  CpaIntegrationUpdate,
  CpaModels,
  CpaOAuthSessionDelete,
  CpaOAuthStart,
  CpaOAuthStartRequest,
  CpaOAuthStatus,
  CpaQuotaReset,
  CpaRuntime,
  CpaRuntimeCheck,
  CpaRuntimeInstall,
  CpaRuntimeKeyCreated,
  CpaRuntimeKeys,
  CpaRuntimeLogs,
  CpaTestRequest,
  ContractScopeKind,
  ControlRevision,
  CustomModelDiscoveryRequest,
  CustomModelDiscoveryResponse,
  DailyTokensByModel,
  DashboardSummary,
  DesktopUpdate,
  ProviderDefinition,
  ProviderDefinitionDiscoverRequest,
  ProviderDefinitionDiscoverResponse,
  ProviderDefinitionMutation,
  ProviderDefinitionTestRequest,
  ProviderDefinitionTestResponse,
  ProviderDefinitionUpdate,
  ForwardLogKeys,
  ForwardLogModels,
  ForwardLogQuery,
  ForwardLogs,
  GatewayLogQuery,
  GatewayLogs,
  InstallUpdate,
  KeyCreate,
  KeyUpdate,
  ModelProtocolOverridesUpdate,
  MutationAck,
  MutationExpectation,
  PricingMultipliersUpdate,
  ProtocolProbeRequest,
  ProtocolProbeResponse,
  ProviderCatalog,
  ProviderContracts,
  ProviderPricing,
  ProviderPricingRefresh,
  ProviderPricingRefreshUpdate,
  ProviderUsage,
  ProxyTestRequest,
  ProxyTestResponse,
  Settings,
  SettingsUpdate,
  UpdateCheck,
  UsageMutation,
  UsageRefresh,
  UsageWindow,
  ZenFreeSettings,
  ZenFreeSettingsUpdate,
} from "./generated/dashboard-v3.ts";

/**
 * Hand-written Dashboard V3 endpoint client for the frozen
 * `/dashboard/api/v3` contract (schema/dashboard-api-v3.schema.json).
 *
 * Every non-2xx response uses the stable `V3Error` envelope
 * (`{ code, message, currentRevision, processGeneration }`); the transport
 * below maps it onto typed errors so callers can branch on 401 / 409 / 410 /
 * 429 without re-parsing bodies. Control-plane identity tokens observed on
 * any response are forwarded to the registered revision sink (the
 * controlPlane store) so later mutations always start from fresh CAS tokens.
 */

export const DASHBOARD_AUTH_REQUIRED_EVENT = "ocg-dashboard-auth-required";

/**
 * Dispatched when any V3 call answers 410 `gone`: the loaded page predates
 * the running service. Detail carries `{ message, guidance, path }`.
 */
export const DASHBOARD_GONE_EVENT = "ocg-dashboard-gone";

/** Fixed attribution id of the primary key; mirrors the backend constant. */
export const PRIMARY_KEY_ID = "00000000-0000-0000-0000-000000000001";

/** Sentinel selecting forward logs without client key attribution. */
export const UNATTRIBUTED_KEY_FILTER = "__unattributed__";

/** Control-plane identity tokens carried by (almost) every V3 payload. */
export interface ControlPlaneTokens {
  revision: number;
  processGeneration: number;
  pricingRevision?: string | null;
}

type ControlRevisionSink = (tokens: ControlPlaneTokens) => void;

let controlRevisionSink: ControlRevisionSink | null = null;

/** Registered once by the controlPlane store; replaced when a new Pinia activates. */
export function setControlRevisionSink(sink: ControlRevisionSink | null): void {
  controlRevisionSink = sink;
}

function publishTokens(body: unknown): void {
  if (!controlRevisionSink || typeof body !== "object" || body === null) return;
  const record = body as Record<string, unknown>;
  if (typeof record.revision === "number" && typeof record.processGeneration === "number") {
    controlRevisionSink({
      revision: record.revision,
      processGeneration: record.processGeneration,
      pricingRevision: typeof record.pricingRevision === "string" ? record.pricingRevision : null,
    });
    return;
  }
  // V4 listings/commits nest `{ revision: ControlRevision }`.
  const nested = record.revision;
  if (typeof nested !== "object" || nested === null) return;
  const nestedRecord = nested as Record<string, unknown>;
  if (typeof nestedRecord.revision !== "number" || typeof nestedRecord.processGeneration !== "number") return;
  controlRevisionSink({
    revision: nestedRecord.revision,
    processGeneration: nestedRecord.processGeneration,
    pricingRevision: typeof nestedRecord.pricingRevision === "string" ? nestedRecord.pricingRevision : null,
  });
}

export class DashboardAuthError extends Error {
  constructor(message: string) {
    super(message);
    this.name = "DashboardAuthError";
  }
}

export class DashboardRequestError extends Error {
  readonly status: number;
  /** Stable V3 error code (`revisionConflict`, `gone`, `throttled`, …). */
  readonly code: string;
  readonly currentRevision: number | null;
  readonly processGeneration: number | null;
  readonly retryAfterSeconds: number | null;
  readonly nextAllowedAt: string | null;

  constructor(
    message: string,
    status: number,
    code = "",
    currentRevision: number | null = null,
    processGeneration: number | null = null,
    retryAfterSeconds: number | null = null,
    nextAllowedAt: string | null = null,
  ) {
    super(message);
    this.name = "DashboardRequestError";
    this.status = status;
    this.code = code;
    this.currentRevision = currentRevision;
    this.processGeneration = processGeneration;
    this.retryAfterSeconds = retryAfterSeconds;
    this.nextAllowedAt = nextAllowedAt;
  }
}

/** 409 `revisionConflict`: the mutation was rejected before any write. */
export class DashboardConflictError extends DashboardRequestError {
  constructor(message: string, currentRevision: number | null, processGeneration: number | null) {
    super(message, 409, "revisionConflict", currentRevision, processGeneration);
    this.name = "DashboardConflictError";
  }
}

/**
 * 410 `gone`: the caller hit an endpoint the service no longer serves, which
 * means the loaded page predates the running build. The structured guidance
 * tells the user to refresh the page and, if that is not enough, upgrade.
 */
export class DashboardGoneError extends DashboardRequestError {
  /** Endpoint path that answered 410, for diagnostics. */
  readonly path: string;
  /** Localized upgrade/refresh guidance ready for display. */
  readonly guidance: string;

  constructor(message: string, path: string, currentRevision: number | null, processGeneration: number | null) {
    super(message, 410, "gone", currentRevision, processGeneration);
    this.name = "DashboardGoneError";
    this.path = path;
    this.guidance = goneGuidance();
  }
}

/** 429 `throttled` (official usage refresh): carries the absolute retry time. */
export class DashboardThrottledError extends DashboardRequestError {
  constructor(
    message: string,
    retryAfterSeconds: number | null,
    nextAllowedAt: string | null,
    currentRevision: number | null,
    processGeneration: number | null,
  ) {
    super(message, 429, "throttled", currentRevision, processGeneration, retryAfterSeconds, nextAllowedAt);
    this.name = "DashboardThrottledError";
  }
}

/** Structured upgrade/refresh guidance surfaced on old-API 410 responses. */
function goneGuidance(): string {
  // This is intentionally a stable transport-level fallback, outside the
  // generated i18n key union. Shell-level UI may localize it further.
  return "页面版本与服务不匹配，请刷新页面后重试；若仍失败请升级到最新版本";
}

export function isRevisionConflict(error: unknown): error is DashboardConflictError {
  return error instanceof DashboardConflictError
    || (error instanceof DashboardRequestError && error.status === 409 && error.code === "revisionConflict");
}

function dashboardApiBase(base: "v3" | "v4"): string {
  if (window.location.pathname.startsWith("/dashboard")) {
    return `/dashboard/api/${base}`;
  }
  // 回退仅覆盖 Gateway 监听默认端口 9042 的纯静态托管场景（如直接打开构建产物）
  return `http://127.0.0.1:9042/dashboard/api/${base}`;
}

interface V3ErrorBody {
  code?: unknown;
  message?: unknown;
  currentRevision?: unknown;
  processGeneration?: unknown;
  nextAllowedAt?: unknown;
}

export async function requestDashboard<T>(
  base: "v3" | "v4",
  path: string,
  init: RequestInit = {},
  notifyAuthRequired = true,
): Promise<T> {
  const headers = new Headers(init.headers);
  if (init.body && !headers.has("Content-Type")) {
    headers.set("Content-Type", "application/json");
  }
  const response = await fetch(`${dashboardApiBase(base)}${path}`, {
    ...init,
    headers,
    credentials: "same-origin",
  });
  if (!response.ok) {
    if (response.status === 401 && notifyAuthRequired) {
      const message = t("登录已失效，请重新登录");
      window.dispatchEvent(new CustomEvent(DASHBOARD_AUTH_REQUIRED_EVENT, { detail: message }));
      throw new DashboardAuthError(message);
    }
    let message = `${response.status} ${response.statusText}`;
    let body: V3ErrorBody | null = null;
    const responseText = await response.text().catch(() => "");
    if (responseText) {
      try {
        body = JSON.parse(responseText) as V3ErrorBody;
        if (typeof body.message === "string" && body.message) message = body.message;
      } catch {
        message = responseText;
      }
    }
    const currentRevision = typeof body?.currentRevision === "number" ? body.currentRevision : null;
    const processGeneration = typeof body?.processGeneration === "number" ? body.processGeneration : null;
    if (currentRevision !== null && processGeneration !== null) {
      publishTokens({ revision: currentRevision, processGeneration });
    }
    const retryAfterHeader = response.headers.get("Retry-After");
    const retryAfterSeconds = retryAfterHeader && /^\d+$/.test(retryAfterHeader)
      ? Number(retryAfterHeader)
      : null;
    const nextAllowedAt = typeof body?.nextAllowedAt === "string" ? body.nextAllowedAt : null;
    if (response.status === 409 && body?.code === "revisionConflict") {
      throw new DashboardConflictError(message, currentRevision, processGeneration);
    }
    if (response.status === 410) {
      const error = new DashboardGoneError(message, path, currentRevision, processGeneration);
      window.dispatchEvent(new CustomEvent(DASHBOARD_GONE_EVENT, {
        detail: { message: error.message, guidance: error.guidance, path },
      }));
      throw error;
    }
    if (response.status === 429) {
      throw new DashboardThrottledError(message, retryAfterSeconds, nextAllowedAt, currentRevision, processGeneration);
    }
    throw new DashboardRequestError(
      message,
      response.status,
      typeof body?.code === "string" ? body.code : "",
      currentRevision,
      processGeneration,
      retryAfterSeconds,
      nextAllowedAt,
    );
  }
  if (response.status === 204) return undefined as T;
  const body = await response.json() as T;
  publishTokens(body);
  return body;
}

export async function requestV3<T>(
  path: string,
  init: RequestInit = {},
  notifyAuthRequired = true,
): Promise<T> {
  return requestDashboard<T>("v3", path, init, notifyAuthRequired);
}

export async function requestV4<T>(
  path: string,
  init: RequestInit = {},
  notifyAuthRequired = true,
): Promise<T> {
  return requestDashboard<T>("v4", path, init, notifyAuthRequired);
}

function json(value: unknown): BodyInit {
  return JSON.stringify(value);
}

/** Mutation body without the CAS pair; the caller supplies it per attempt. */
export type WithoutExpectation<T> = Omit<T, "expectedRevision" | "processGeneration">;

export function withExpectation<T extends object>(body: T, expectation: MutationExpectation): BodyInit {
  return json({ ...body, ...expectation });
}

function mutation(expectation: MutationExpectation): BodyInit {
  return json(expectation);
}

function encode(segment: string): string {
  return encodeURIComponent(segment);
}

export const dashboardV3 = {
  // --- public auth/session ---
  getAuthStatus: () => requestV3<AuthStatus>("/auth/status", {}, false),
  registerAdmin: (username: string, password: string, expectation: MutationExpectation) =>
    requestV3<AuthStatus>("/auth/register", {
      method: "POST",
      body: withExpectation({ username, password } satisfies WithoutExpectation<AuthRegister>, expectation),
    }, false),
  loginAdmin: (username: string, password: string, expectation: MutationExpectation) =>
    requestV3<AuthStatus>("/auth/login", {
      method: "POST",
      body: withExpectation({ username, password } satisfies WithoutExpectation<AuthLogin>, expectation),
    }, false),
  logoutAdmin: (expectation: MutationExpectation) =>
    requestV3<AuthStatus>("/auth/logout", {
      method: "POST",
      body: mutation(expectation),
    }, false),

  // --- control plane ---
  getContract: () => requestV3<ControlRevision>("/contract"),

  // --- connection center (the only plaintext Key payload) ---
  getConnection: () => requestV3<ConnectionInfo>("/connection"),

  // --- static local external integrations ---
  getCpaIntegration: () => requestV3<CpaIntegration>("/external-integrations/cpa"),
  putCpaIntegration: (
    update: WithoutExpectation<CpaIntegrationUpdate>,
    expectation: MutationExpectation,
  ) => requestV3<CpaIntegration>("/external-integrations/cpa", {
    method: "PUT",
    body: withExpectation(update, expectation),
  }),
  deleteCpaIntegration: (expectation: MutationExpectation) =>
    requestV3<MutationAck>("/external-integrations/cpa", {
      method: "DELETE",
      body: mutation(expectation),
    }),
  testCpaIntegration: (input: CpaTestRequest) =>
    requestV3<CpaConnectionReport>("/external-integrations/cpa/test", {
      method: "POST",
      body: json(input),
    }),
  getCpaModels: () => requestV3<CpaModels>("/external-integrations/cpa/models"),
  refreshCpaModels: (expectation: MutationExpectation) =>
    requestV3<CpaModels>("/external-integrations/cpa/models/refresh", {
      method: "POST",
      body: mutation(expectation),
    }),
  getCpaAccounts: () => requestV3<CpaAccounts>("/external-integrations/cpa/accounts"),
  setCpaAccountStatus: (
    update: WithoutExpectation<CpaAccountStatusUpdate>,
    expectation: MutationExpectation,
  ) => requestV3<MutationAck>("/external-integrations/cpa/accounts/status", {
    method: "PATCH",
    body: withExpectation(update, expectation),
  }),
  deleteCpaAccount: (
    input: WithoutExpectation<CpaAccountDelete>,
    expectation: MutationExpectation,
  ) => requestV3<MutationAck>("/external-integrations/cpa/accounts", {
    method: "DELETE",
    body: withExpectation(input, expectation),
  }),
  resetCpaQuota: (
    input: WithoutExpectation<CpaQuotaReset>,
    expectation: MutationExpectation,
  ) => requestV3<MutationAck>("/external-integrations/cpa/accounts/reset-quota", {
    method: "POST",
    body: withExpectation(input, expectation),
  }),
  startCpaOAuth: (
    input: WithoutExpectation<CpaOAuthStartRequest>,
    expectation: MutationExpectation,
  ) => requestV3<CpaOAuthStart>("/external-integrations/cpa/oauth/start", {
    method: "POST",
    body: withExpectation(input, expectation),
  }),
  getCpaOAuthStatus: (state: string) =>
    requestV3<CpaOAuthStatus>(`/external-integrations/cpa/oauth/status?state=${encode(state)}`),
  getCpaCliImports: () => requestV3<CpaCliImports>("/external-integrations/cpa/cli-imports"),
  importCpaCliAccount: (
    input: WithoutExpectation<CpaCliImportRequest>,
    expectation: MutationExpectation,
  ) => requestV3<CpaCliImportResult>("/external-integrations/cpa/cli-imports", {
    method: "POST",
    body: withExpectation(input, expectation),
  }),
  cancelCpaOAuth: (
    input: WithoutExpectation<CpaOAuthSessionDelete>,
    expectation: MutationExpectation,
  ) => requestV3<MutationAck>("/external-integrations/cpa/oauth/session", {
    method: "DELETE",
    body: withExpectation(input, expectation),
  }),
  getCpaRuntime: () => requestV3<CpaRuntime>("/external-integrations/cpa/runtime"),
  checkCpaRuntimeUpdate: (expectation: MutationExpectation) =>
    requestV3<CpaRuntimeCheck>("/external-integrations/cpa/runtime/check-update", {
      method: "POST",
      body: mutation(expectation),
    }),
  installCpaRuntime: (
    input: WithoutExpectation<CpaRuntimeInstall>,
    expectation: MutationExpectation,
  ) => requestV3<CpaRuntime>("/external-integrations/cpa/runtime/install", {
    method: "POST",
    body: withExpectation(input, expectation),
  }),
  updateCpaRuntime: (
    input: WithoutExpectation<CpaRuntimeInstall>,
    expectation: MutationExpectation,
  ) => requestV3<CpaRuntime>("/external-integrations/cpa/runtime/update", {
    method: "POST",
    body: withExpectation(input, expectation),
  }),
  startCpaRuntime: (expectation: MutationExpectation) =>
    requestV3<CpaRuntime>("/external-integrations/cpa/runtime/start", {
      method: "POST",
      body: mutation(expectation),
    }),
  stopCpaRuntime: (expectation: MutationExpectation) =>
    requestV3<CpaRuntime>("/external-integrations/cpa/runtime/stop", {
      method: "POST",
      body: mutation(expectation),
    }),
  rollbackCpaRuntime: (expectation: MutationExpectation) =>
    requestV3<CpaRuntime>("/external-integrations/cpa/runtime/rollback", {
      method: "POST",
      body: mutation(expectation),
    }),
  removeCpaRuntime: (expectation: MutationExpectation) =>
    requestV3<CpaRuntime>("/external-integrations/cpa/runtime", {
      method: "DELETE",
      body: mutation(expectation),
    }),
  getCpaRuntimeLogs: () => requestV3<CpaRuntimeLogs>("/external-integrations/cpa/runtime/logs"),
  getCpaRuntimeKeys: () => requestV3<CpaRuntimeKeys>("/external-integrations/cpa/client-keys"),
  createCpaRuntimeKey: (expectation: MutationExpectation) =>
    requestV3<CpaRuntimeKeyCreated>("/external-integrations/cpa/client-keys", {
      method: "POST",
      body: mutation(expectation),
    }),
  rotateCpaRuntimeKey: (fingerprint: string, expectation: MutationExpectation) =>
    requestV3<CpaRuntimeKeyCreated>(
      `/external-integrations/cpa/client-keys/${encodeURIComponent(fingerprint)}/rotate`,
      {
        method: "POST",
        body: mutation(expectation),
      },
    ),
  deleteCpaRuntimeKey: (fingerprint: string, expectation: MutationExpectation) =>
    requestV3<MutationAck>(
      `/external-integrations/cpa/client-keys/${encodeURIComponent(fingerprint)}`,
      {
        method: "DELETE",
        body: mutation(expectation),
      },
    ),

  // --- access keys ---
  createKey: (name: string, expectation: MutationExpectation) =>
    requestV3<MutationAck>("/keys", {
      method: "POST",
      body: withExpectation({ name } satisfies WithoutExpectation<KeyCreate>, expectation),
    }),
  updateKey: (id: string, update: WithoutExpectation<KeyUpdate>, expectation: MutationExpectation) =>
    requestV3<MutationAck>(`/keys/${encode(id)}`, {
      method: "PATCH",
      body: withExpectation(update, expectation),
    }),
  deleteKey: (id: string, expectation: MutationExpectation) =>
    requestV3<MutationAck>(`/keys/${encode(id)}`, {
      method: "DELETE",
      body: mutation(expectation),
    }),
  regenerateKey: (id: string, expectation: MutationExpectation) =>
    requestV3<MutationAck>(`/keys/${encode(id)}/regenerate`, {
      method: "POST",
      body: mutation(expectation),
    }),
  regeneratePrimaryKey: (expectation: MutationExpectation) =>
    requestV3<MutationAck>("/keys/primary/regenerate", {
      method: "POST",
      body: mutation(expectation),
    }),

  // --- settings ---
  getSettings: () => requestV3<Settings>("/settings"),
  putSettings: (update: WithoutExpectation<SettingsUpdate>, expectation: MutationExpectation) =>
    requestV3<MutationAck>("/settings", {
      method: "PUT",
      body: withExpectation(update, expectation),
    }),
  testProxy: (input: ProxyTestRequest) =>
    requestV3<ProxyTestResponse>("/settings/test-proxy", {
      method: "POST",
      body: json(input),
    }),

  // --- desktop updater ---
  checkForUpdate: () => requestV3<UpdateCheck>("/settings/check-update"),
  getUpdateStatus: () => requestV3<DesktopUpdate>("/settings/update-status"),
  installUpdate: (expectedVersion: string, expectation: MutationExpectation) =>
    requestV3<DesktopUpdate>("/settings/install-update", {
      method: "POST",
      body: withExpectation({ expectedVersion } satisfies WithoutExpectation<InstallUpdate>, expectation),
    }),

  // --- pricing ---
  refreshProviderPricing: (
    providerId: string,
    refresh: WithoutExpectation<ProviderPricingRefreshUpdate>,
    expectation: MutationExpectation,
  ) => requestV3<ProviderPricingRefresh>(`/providers/${encode(providerId)}/pricing/refresh`, {
    method: "POST",
    body: withExpectation(refresh, expectation),
  }),
  putProviderPricingMultipliers: (
    providerId: string,
    update: WithoutExpectation<PricingMultipliersUpdate>,
    expectation: MutationExpectation,
  ) => requestV3<ProviderPricing>(
    `/providers/${encode(providerId)}/pricing/multipliers`,
    {
      method: "PUT",
      body: withExpectation(update, expectation),
    },
  ),
  getProviderPricing: (providerId: string) =>
    requestV3<ProviderPricing>(`/providers/${encode(providerId)}/pricing`),

  // --- accounts ---
  listAccounts: () => requestV3<AccountList>("/accounts"),
  getAccount: (id: string) => requestV3<Account>(`/accounts/${encode(id)}`),
  createAccount: (input: WithoutExpectation<AccountCreate>, expectation: MutationExpectation) =>
    requestV3<AccountMutation>("/accounts", {
      method: "POST",
      body: withExpectation(input, expectation),
    }),
  createManagedAccount: (input: WithoutExpectation<AccountManagedCreate>, expectation: MutationExpectation) =>
    requestV3<AccountMutation>("/accounts/managed", {
      method: "POST",
      body: withExpectation(input, expectation),
    }),
  updateAccount: (id: string, update: WithoutExpectation<AccountUpdate>, expectation: MutationExpectation) =>
    requestV3<AccountMutation>(`/accounts/${encode(id)}`, {
      method: "PATCH",
      body: withExpectation(update, expectation),
    }),
  deleteAccount: (id: string, expectation: MutationExpectation) =>
    requestV3<AccountMutation>(`/accounts/${encode(id)}`, {
      method: "DELETE",
      body: mutation(expectation),
    }),
  reorderAccounts: (accountIds: string[], expectation: MutationExpectation) =>
    requestV3<AccountList>("/accounts/order", {
      method: "PUT",
      body: withExpectation({ accountIds } satisfies WithoutExpectation<AccountOrder>, expectation),
    }),
  toggleAccount: (id: string, expectation: MutationExpectation) =>
    requestV3<AccountMutation>(`/accounts/${encode(id)}/toggle`, {
      method: "POST",
      body: mutation(expectation),
    }),
  advanceAccountSetup: (id: string, setupStep: WithoutExpectation<AccountSetupUpdate>["setupStep"], expectation: MutationExpectation) =>
    requestV3<AccountMutation>(`/accounts/${encode(id)}/setup`, {
      method: "PATCH",
      body: withExpectation({ setupStep } satisfies WithoutExpectation<AccountSetupUpdate>, expectation),
    }),
  verifyManagedAccountKey: (id: string, key: string, expectation: MutationExpectation) =>
    requestV3<AccountMutation>(`/accounts/${encode(id)}/setup/verify-key`, {
      method: "POST",
      body: withExpectation({ key } satisfies WithoutExpectation<AccountManagedKeyVerify>, expectation),
    }),
  resetAccountCooldown: (id: string, expectation: MutationExpectation) =>
    requestV3<AccountMutation>(`/accounts/${encode(id)}/reset-cooldown`, {
      method: "POST",
      body: mutation(expectation),
    }),
  putAccountCustomConfig: (id: string, config: WithoutExpectation<AccountCustomConfigUpdate>, expectation: MutationExpectation) =>
    requestV3<AccountMutation>(`/accounts/${encode(id)}/custom-config`, {
      method: "PUT",
      body: withExpectation(config, expectation),
    }),
  putAccountModelCapabilities: (id: string, update: WithoutExpectation<AccountModelCapabilitiesUpdate>, expectation: MutationExpectation) =>
    requestV3<AccountMutation>(`/accounts/${encode(id)}/model-capabilities`, {
      method: "PUT",
      body: withExpectation(update, expectation),
    }),
  testAccountModel: (id: string, modelId: string) =>
    requestV3<AccountModelTestResponse>(`/accounts/${encode(id)}/model-tests`, {
      method: "POST",
      body: json({ modelId } satisfies AccountModelTestRequest),
    }),
  exportAccountTransfer: (input: AccountExportRequest) =>
    requestV3<AccountExport>("/accounts/transfer/export", {
      method: "POST",
      body: json(input),
    }),
  previewAccountTransfer: (input: AccountImportPreviewRequest) =>
    requestV3<AccountImportPreview>("/accounts/transfer/preview", {
      method: "POST",
      body: json(input),
    }),
  importAccountTransfer: (
    input: WithoutExpectation<AccountImportRequest>,
    expectation: MutationExpectation,
  ) => requestV3<AccountImportResult>("/accounts/transfer/import", {
    method: "POST",
    body: withExpectation(input, expectation),
  }),

  // --- account usage ---
  getAccountUsage: (id: string) => requestV3<UsageWindow>(`/accounts/${encode(id)}/usage`),
  patchAccountUsage: (id: string, update: WithoutExpectation<AccountUsageUpdate>, expectation: MutationExpectation) =>
    requestV3<UsageMutation>(`/accounts/${encode(id)}/usage`, {
      method: "PATCH",
      body: withExpectation(update, expectation),
    }),
  refreshAccountUsage: (id: string, expectation: MutationExpectation) =>
    requestV3<UsageRefresh>(`/accounts/${encode(id)}/usage/refresh`, {
      method: "POST",
      body: mutation(expectation),
    }),
  getProviderUsage: (id: string) => requestV3<ProviderUsage>(`/accounts/${encode(id)}/provider-usage`),
  refreshProviderUsage: (id: string, expectation: MutationExpectation) =>
    requestV3<ProviderUsage>(`/accounts/${encode(id)}/provider-usage`, {
      method: "POST",
      body: mutation(expectation),
    }),

  // --- managed browser ---
  getBrowserCapabilities: () => requestV3<BrowserCapabilities>("/browser/capabilities"),
  openAccountBrowser: (id: string, target: WithoutExpectation<BrowserOpenRequest>["target"], expectation: MutationExpectation) =>
    requestV3<BrowserOpen>(`/accounts/${encode(id)}/browser`, {
      method: "POST",
      body: withExpectation({ target } satisfies WithoutExpectation<BrowserOpenRequest>, expectation),
    }),
  resetAccountBrowserProfile: (id: string, expectation: MutationExpectation) =>
    requestV3<AccountMutation>(`/accounts/${encode(id)}/browser-profile`, {
      method: "DELETE",
      body: mutation(expectation),
    }),

  // --- providers ---
  getProviders: () => requestV3<ProviderCatalog>("/providers"),
  getProviderDefinition: (providerId: string) =>
    requestV3<ProviderDefinition>(`/providers/${encode(providerId)}`),
  updateProviderDefinition: (
    providerId: string,
    input: WithoutExpectation<ProviderDefinitionUpdate>,
    expectation: MutationExpectation,
  ) => requestV3<ProviderDefinitionMutation>(`/providers/${encode(providerId)}`, {
    method: "PATCH",
    body: withExpectation(input, expectation),
  }),
  deleteProviderDefinition: (providerId: string, expectation: MutationExpectation) =>
    requestV3<MutationAck>(`/providers/${encode(providerId)}`, {
      method: "DELETE",
      body: mutation(expectation),
    }),
  discoverProviderDefinitionModels: (input: ProviderDefinitionDiscoverRequest) =>
    requestV3<ProviderDefinitionDiscoverResponse>("/providers/models/discover", {
      method: "POST",
      body: json(input),
    }),
  testProviderDefinition: (input: ProviderDefinitionTestRequest) =>
    requestV3<ProviderDefinitionTestResponse>("/providers/test", {
      method: "POST",
      body: json(input),
    }),
  getZenFreeSettings: () => requestV3<ZenFreeSettings>("/providers/zen-free"),
  patchZenFreeSettings: (enabled: boolean, expectation: MutationExpectation) =>
    requestV3<ZenFreeSettings>("/providers/zen-free", {
      method: "PATCH",
      body: withExpectation({ enabled } satisfies WithoutExpectation<ZenFreeSettingsUpdate>, expectation),
    }),
  getProviderContracts: () => requestV3<ProviderContracts>("/provider-contracts"),
  refreshContractCatalog: (
    scopeKind: ContractScopeKind,
    scopeId: string,
    expectation: MutationExpectation,
  ) => requestV3<ProviderContracts>(
    `/provider-contracts/${encode(scopeKind)}/${encode(scopeId)}/catalog/refresh`,
    { method: "POST", body: mutation(expectation) },
  ),
  resetStaticModelProtocols: (
    scopeId: string,
    expectation: MutationExpectation,
  ) => requestV3<ProviderContracts>(
    `/provider-contracts/provider/${encode(scopeId)}/model-protocols/reset-static`,
    { method: "POST", body: mutation(expectation) },
  ),
  putModelProtocolOverrides: (
    scopeKind: "provider" | "custom_endpoint",
    scopeId: string,
    update: WithoutExpectation<ModelProtocolOverridesUpdate>,
    expectation: MutationExpectation,
  ) =>
    requestV3<ProviderContracts>(
      `/provider-contracts/${scopeKind === "custom_endpoint" ? "custom-endpoint" : "provider"}/${encode(scopeId)}/model-protocol-overrides`,
      {
        method: "PUT",
        body: withExpectation(update, expectation),
      },
    ),
  runProviderProtocolProbes: (providerId: string, input: WithoutExpectation<ProtocolProbeRequest>, expectation: MutationExpectation) =>
    requestV3<ProtocolProbeResponse>(`/providers/${encode(providerId)}/protocol-probes`, {
      method: "POST",
      body: withExpectation(input, expectation),
    }),

  // --- custom model discovery (operational probe, no CAS) ---
  discoverCustomModels: (input: CustomModelDiscoveryRequest) =>
    requestV3<CustomModelDiscoveryResponse>("/custom/models/discover", {
      method: "POST",
      body: json(input),
    }),

  // --- observability (read-only, page-local state) ---
  getDashboardSummary: () => requestV3<DashboardSummary>("/dashboard/summary"),
  getDailyTokensByModel: (days?: number) =>
    requestV3<DailyTokensByModel>(`/dashboard/daily-tokens-by-model?days=${days ?? 30}`),
  getGatewayLogs: (query: GatewayLogQuery = {}) => {
    const params = new URLSearchParams({ limit: String(query.limit ?? 100) });
    if (query.requestId) params.set("requestId", query.requestId);
    return requestV3<GatewayLogs>(`/logs/gateway?${params}`);
  },
  getForwardLogs: (query: ForwardLogQuery = {}) => {
    // Filters lead the query string; the backend applies them before paging.
    const params = new URLSearchParams();
    if (query.status) params.set("status", query.status);
    if (query.accountId) params.set("accountId", query.accountId);
    if (query.model) params.set("model", query.model);
    if (query.requestId) params.set("requestId", query.requestId);
    if (query.keyId) params.set("keyId", query.keyId);
    if (query.providerId) params.set("providerId", query.providerId);
    if (query.routeAccountId) params.set("routeAccountId", query.routeAccountId);
    if (query.credentialAccountId) params.set("credentialAccountId", query.credentialAccountId);
    if (query.startTime) params.set("startTime", query.startTime);
    if (query.endTime) params.set("endTime", query.endTime);
    if (query.sortBy) params.set("sortBy", query.sortBy);
    if (query.sortOrder) params.set("sortOrder", query.sortOrder);
    params.set("limit", String(query.limit ?? 20));
    params.set("offset", String(query.offset ?? 0));
    return requestV3<ForwardLogs>(`/logs/forward?${params}`);
  },
  getForwardLogModels: () => requestV3<ForwardLogModels>("/logs/forward/models"),
  getForwardLogKeys: () => requestV3<ForwardLogKeys>("/logs/forward/keys"),
};

export function browserSessionWebSocketUrl(token: string): string {
  const url = new URL(
    `${dashboardApiBase("v3")}/browser/sessions/${encodeURIComponent(token)}/ws`,
    window.location.href,
  );
  url.protocol = url.protocol === "https:" ? "wss:" : "ws:";
  return url.toString();
}
