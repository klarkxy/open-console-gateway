/**
 * Explicit presentation client for the frozen Dashboard V3 contract.
 *
 * Every request is made by `dashboardV3`; each endpoint below projects only
 * the fields the existing page needs. There is no V2 import, route fallback,
 * recursive case conversion, or compatibility cast.
 */
export type * from "./dashboard-presenters.ts";

import { useControlPlaneStore } from "../stores/controlPlane.ts";
import { isVersionAtLeast } from "../utils/version.ts";
import type {
  AccountCustomConfigUpdate,
  AccountExport,
  AccountExportRequest,
  AccountImportPreview,
  AccountImportPreviewRequest,
  AccountImportRequest,
  AccountImportResult,
  AccountManagedCreate,
  AccountModelCapabilitiesUpdate,
  AccountSetupStep,
  AccountUsageUpdate,
  AuthStatus,
  ForwardLogQuery as V3ForwardLogQuery,
  GatewayLogQuery,
  KeyUpdate,
  MutationAck,
  MutationExpectation,
  ProxyTestRequest,
} from "./generated/dashboard-v3.ts";
import {
  DASHBOARD_AUTH_REQUIRED_EVENT,
  DASHBOARD_GONE_EVENT,
  DashboardAuthError,
  DashboardConflictError,
  DashboardGoneError,
  DashboardRequestError,
  DashboardThrottledError,
  PRIMARY_KEY_ID,
  UNATTRIBUTED_KEY_FILTER,
  browserSessionWebSocketUrl,
  dashboardV3,
  isRevisionConflict,
  type WithoutExpectation,
} from "./dashboard-v3.ts";
import {
  getOperationLogs,
  getRequestLogAttempts,
  getRequestLogs,
} from "./log-ledger.ts";
import {
  accountCreateInput,
  accountUpdateInput,
  presentAccount,
  presentBrowserCapabilities,
  presentBrowserOpen,
  presentConnection,
  presentDailyModelTokens,
  presentDashboardSummary,
  presentForwardLogs,
  presentGatewayLog,
  presentProxyTest,
  presentSettings,
  settingsPatchInput,
  settingsUpdateInput,
  presentUpdateCheck,
  presentUpdateStatus,
  presentUsage,
  presentUsageRefresh,
  type Account,
  type AccountInput,
  type AccountCustomConfigUpdateInput,
  type AccountModelCapabilityInput,
  type AccountModelTestResponse,
  type AccountUpdate,
  type AppConfig,
  type SettingsPatch,
  type BrowserTarget,
  type ConnectionInfo,
  type CustomModelDiscoveryInput,
  type ForwardLogQuery,
  type ManagedAccountInput,
} from "./dashboard-presenters.ts";

export {
  DASHBOARD_AUTH_REQUIRED_EVENT,
  DASHBOARD_GONE_EVENT,
  DashboardAuthError,
  DashboardConflictError,
  DashboardGoneError,
  DashboardRequestError,
  DashboardThrottledError,
  PRIMARY_KEY_ID,
  UNATTRIBUTED_KEY_FILTER,
  browserSessionWebSocketUrl,
  isRevisionConflict,
  isVersionAtLeast,
};

async function withCas<T>(
  run: (expectation: { expectedRevision: number; processGeneration: number }) => Promise<T>,
): Promise<T> {
  const controlPlane = useControlPlaneStore();
  if (!controlPlane.hasTokens()) await controlPlane.refresh();
  return controlPlane.runMutation(run);
}

// Short local account writes only: the serial lane orders them on fresh CAS
// tokens so two quick independent edits cannot self-conflict. Network
// refreshes/verifications stay on plain withCas and never enter the lane.
async function withLocalCas<T>(
  target: string,
  run: (expectation: { expectedRevision: number; processGeneration: number }) => Promise<T>,
): Promise<T> {
  return useControlPlaneStore().runLocalMutation(target, run);
}

async function mutatedAccount(result: Promise<{ account: Parameters<typeof presentAccount>[0] | null }>): Promise<Account> {
  const mutation = await result;
  if (mutation.account === null) throw new Error("account mutation returned no account");
  return presentAccount(mutation.account);
}

const SETTINGS_PATCH_FIELDS = [
  "auto_start",
  "conversation_sticky",
  "opencode_invite_url",
  "routing_mode",
  "show_dock_icon",
] as const satisfies readonly (keyof SettingsPatch)[];

function settingsPatchFields(patch: SettingsPatch): string[] {
  return SETTINGS_PATCH_FIELDS.filter((field) => patch[field] !== undefined);
}

function forwardLogQuery(value: ForwardLogQuery): V3ForwardLogQuery {
  return {
    limit: value.limit,
    offset: value.offset,
    status: value.status,
    accountId: value.account_id,
    model: value.model,
    requestId: value.request_id,
    keyId: value.key_id,
    providerId: value.provider_id,
    routeAccountId: value.route_account_id,
    credentialAccountId: value.credential_account_id,
    startTime: value.start_time,
    endTime: value.end_time,
    sortBy: value.sort_by,
    sortOrder: value.sort_order,
  };
}

export const dashboardApi = {
  getAuthStatus: async (): Promise<AuthStatus> => dashboardV3.getAuthStatus(),
  registerAdmin: (username: string, password: string, expectation: MutationExpectation): Promise<AuthStatus> =>
    dashboardV3.registerAdmin(username, password, expectation),
  loginAdmin: (username: string, password: string, expectation: MutationExpectation): Promise<AuthStatus> =>
    dashboardV3.loginAdmin(username, password, expectation),
  logoutAdmin: (expectation: MutationExpectation): Promise<AuthStatus> =>
    dashboardV3.logoutAdmin(expectation),

  getConnection: async (): Promise<ConnectionInfo> => presentConnection(await dashboardV3.getConnection()),
  createKey: async (name: string, expectation: MutationExpectation): Promise<void> => {
    await dashboardV3.createKey(name, expectation);
  },
  updateKey: async (id: string, update: { name?: string; enabled?: boolean }, expectation: MutationExpectation): Promise<void> => {
    const body: WithoutExpectation<KeyUpdate> = {};
    if (update.name !== undefined) body.name = update.name;
    if (update.enabled !== undefined) body.enabled = update.enabled;
    await dashboardV3.updateKey(id, body, expectation);
  },
  deleteKey: async (id: string, expectation: MutationExpectation): Promise<void> => {
    await dashboardV3.deleteKey(id, expectation);
  },
  regenerateKey: async (id: string, expectation: MutationExpectation): Promise<void> => {
    await dashboardV3.regenerateKey(id, expectation);
  },
  regeneratePrimaryKey: async (expectation: MutationExpectation): Promise<void> => {
    await dashboardV3.regeneratePrimaryKey(expectation);
  },

  getAccounts: async (): Promise<Account[]> =>
    (await dashboardV3.listAccounts()).accounts.map(presentAccount),

  createAccount: (input: AccountInput): Promise<Account> =>
    mutatedAccount(withLocalCas("account:create", (expectation) => dashboardV3.createAccount(accountCreateInput(input), expectation))),

  createManagedAccount: (input: ManagedAccountInput): Promise<Account> =>
    mutatedAccount(withLocalCas("account:create-managed", (expectation) => dashboardV3.createManagedAccount({
      name: input.name,
      username: input.username,
      notes: input.notes,
    } satisfies WithoutExpectation<AccountManagedCreate>, expectation))),

  exportAccountTransfer: (input: AccountExportRequest): Promise<AccountExport> =>
    dashboardV3.exportAccountTransfer(input),

  previewAccountTransfer: (input: AccountImportPreviewRequest): Promise<AccountImportPreview> =>
    dashboardV3.previewAccountTransfer(input),

  importAccountTransfer: (
    input: WithoutExpectation<AccountImportRequest>,
  ): Promise<AccountImportResult> => withCas((expectation) => (
    dashboardV3.importAccountTransfer(input, expectation)
  )),

  updateAccount: (id: string, update: AccountUpdate): Promise<Account> =>
    mutatedAccount(withLocalCas(`account:${id}`, (expectation) => dashboardV3.updateAccount(id, accountUpdateInput(update), expectation))),

  reorderAccounts: async (accountIds: string[]): Promise<Account[]> =>
    (await withLocalCas("account:reorder", (expectation) => dashboardV3.reorderAccounts(accountIds, expectation))).accounts.map(presentAccount),

  deleteAccount: async (id: string): Promise<void> => {
    await withLocalCas(`account:${id}`, (expectation) => dashboardV3.deleteAccount(id, expectation));
  },

  toggleAccount: (id: string): Promise<Account> =>
    mutatedAccount(withLocalCas(`account:${id}`, (expectation) => dashboardV3.toggleAccount(id, expectation))),

  resetAccountCooldown: (id: string): Promise<Account> =>
    mutatedAccount(withLocalCas(`account:${id}`, (expectation) => dashboardV3.resetAccountCooldown(id, expectation))),

  advanceAccountSetup: (id: string, setupStep: AccountSetupStep): Promise<Account> =>
    mutatedAccount(withLocalCas(`account:${id}`, (expectation) => dashboardV3.advanceAccountSetup(id, setupStep, expectation))),

  verifyManagedAccountKey: (id: string, key: string): Promise<Account> =>
    mutatedAccount(withCas((expectation) => dashboardV3.verifyManagedAccountKey(id, key, expectation))),

  testAccountModel: (id: string, modelId: string): Promise<AccountModelTestResponse> =>
    dashboardV3.testAccountModel(id, modelId),

  updateAccountCustomConfig: (
    id: string,
    config: AccountCustomConfigUpdateInput,
  ): Promise<Account> => mutatedAccount(withLocalCas(`account:${id}`, (expectation) => {
    const payload = {
      endpointUrl: config.endpoint_url,
      upstreamProtocol: config.upstream_protocol,
      modelCapabilities: config.model_capabilities.map((capability) => ({
        publicModel: capability.public_model,
        protocol: capability.protocol,
        source: capability.source,
        upstreamModel: capability.upstream_model,
      })),
    };
    return dashboardV3.putAccountCustomConfig(
      id,
      payload satisfies WithoutExpectation<AccountCustomConfigUpdate>,
      expectation,
    );
  })),

  updateAccountModelCapabilities: (
    id: string,
    capabilities: AccountModelCapabilityInput[],
  ): Promise<Account> => mutatedAccount(withLocalCas(`account:${id}`, (expectation) => dashboardV3.putAccountModelCapabilities(id, {
    capabilities: capabilities.map((capability) => ({
      publicModel: capability.public_model,
      protocol: capability.protocol,
      source: capability.source,
      upstreamModel: capability.upstream_model,
    })),
  } satisfies WithoutExpectation<AccountModelCapabilitiesUpdate>, expectation))),

  discoverCustomModels: async (input: CustomModelDiscoveryInput) => {
    const result = await dashboardV3.discoverCustomModels({
      endpointUrl: input.endpoint_url,
      upstreamProtocol: input.upstream_protocol,
      apiKey: input.api_key,
      accountId: input.account_id,
    });
    return { models: result.models, truncated: result.truncated };
  },

  getBrowserCapabilities: async () => presentBrowserCapabilities(await dashboardV3.getBrowserCapabilities()),
  openAccountBrowser: async (id: string, target: BrowserTarget) =>
    presentBrowserOpen(await withCas((expectation) => dashboardV3.openAccountBrowser(id, target, expectation))),
  resetAccountBrowserProfile: (id: string): Promise<Account> =>
    mutatedAccount(withCas((expectation) => dashboardV3.resetAccountBrowserProfile(id, expectation))),

  getAccountUsage: async (id: string) => presentUsage(await dashboardV3.getAccountUsage(id)),
  updateAccountUsage: async (
    id: string,
    window: "window_5h" | "window_week" | "window_month",
    percent: number,
    resetsInMinutes?: number | null,
  ) => presentUsage((await withCas((expectation) => dashboardV3.patchAccountUsage(id, {
    window,
    percent,
    resetsInMinutes: resetsInMinutes ?? null,
  } satisfies WithoutExpectation<AccountUsageUpdate>, expectation))).usage),
  refreshAccountUsage: async (id: string) =>
    presentUsageRefresh(await withCas((expectation) => dashboardV3.refreshAccountUsage(id, expectation))),

  getSettings: async () => presentSettings(await dashboardV3.getSettings()),
  // Receipt-only: the PUT ack carries just the CAS revision. The submitted
  // draft is the caller's local projection, never the canonical resource;
  // canonical fields come from a separate GET owned by the caller.
  // `captured` is the editor baseline. Omit it to send the revision and
  // process generation already stored on the snapshot — never the live
  // global tokens, which may have moved since the editor loaded.
  updateSettings: (settings: AppConfig, captured?: MutationExpectation): Promise<MutationAck> =>
    dashboardV3.putSettings(settingsUpdateInput(settings), captured ?? {
      expectedRevision: settings.revision,
      processGeneration: settings.process_generation,
    }),
  // Explicit partial intent on the local CAS lane. The lane reads the
  // current expectation at dispatch and does not replay a conflict.
  patchSettings: (patch: SettingsPatch): Promise<MutationAck> =>
    withLocalCas(
      `settings-patch:${settingsPatchFields(patch).join("+")}`,
      (expectation) => dashboardV3.putSettings(settingsPatchInput(patch), expectation),
    ),
  testProxy: async (input: {
    proxy_mode: AppConfig["proxy_mode"];
    proxy_url?: string;
    proxy_list_direction?: AppConfig["proxy_list_direction"];
  }) => presentProxyTest(await dashboardV3.testProxy({
    proxyMode: input.proxy_mode,
    proxyUrl: input.proxy_url,
    proxyListDirection: input.proxy_list_direction,
  } satisfies ProxyTestRequest)),

  checkForUpdate: async () => presentUpdateCheck(await dashboardV3.checkForUpdate()),
  getUpdateStatus: async () => presentUpdateStatus(await dashboardV3.getUpdateStatus()),
  installUpdate: async (expectedVersion: string) =>
    presentUpdateStatus(await withCas((expectation) => dashboardV3.installUpdate(expectedVersion, expectation))),

  getGatewayLogs: async (query: GatewayLogQuery = {}, signal?: AbortSignal) =>
    (await dashboardV3.getGatewayLogs(query, signal)).items.map(presentGatewayLog),
  getForwardLogs: async (query: ForwardLogQuery = {}, signal?: AbortSignal) =>
    presentForwardLogs(await dashboardV3.getForwardLogs(forwardLogQuery(query), signal)),
  getOperationLogs,
  getRequestLogs,
  getRequestLogAttempts,
  getForwardLogModels: async () => (await dashboardV3.getForwardLogModels()).models,
  getForwardLogKeys: async () => (await dashboardV3.getForwardLogKeys()).keys,
  getDashboardSummary: async () => presentDashboardSummary(await dashboardV3.getDashboardSummary()),
  getDailyTokensByModel: async (days?: number) =>
    (await dashboardV3.getDailyTokensByModel(days)).items.map(presentDailyModelTokens),
};
