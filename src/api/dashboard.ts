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
  KeyUpdate,
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
  presentPricing,
  presentProviderPricingRefresh,
  presentProxyTest,
  presentSettings,
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
  type BrowserTarget,
  type ConnectionInfo,
  type CustomModelDiscoveryInput,
  type ForwardLogQuery,
  type ManagedAccountInput,
  type PricingMultiplierUpdate,
  type ProviderPricingRefreshRequest,
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

async function mutatedAccount(result: Promise<{ account: Parameters<typeof presentAccount>[0] | null }>): Promise<Account> {
  const mutation = await result;
  if (mutation.account === null) throw new Error("account mutation returned no account");
  return presentAccount(mutation.account);
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
  regeneratePrimaryKey: async (expectation: MutationExpectation): Promise<string> => {
    await dashboardV3.regeneratePrimaryKey(expectation);
    return (await dashboardV3.getConnection()).primaryKey;
  },

  getAccounts: async (): Promise<Account[]> =>
    (await dashboardV3.listAccounts()).accounts.map(presentAccount),

  createAccount: (input: AccountInput): Promise<Account> =>
    mutatedAccount(withCas((expectation) => dashboardV3.createAccount(accountCreateInput(input), expectation))),

  createManagedAccount: (input: ManagedAccountInput): Promise<Account> =>
    mutatedAccount(withCas((expectation) => dashboardV3.createManagedAccount({
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
    mutatedAccount(withCas((expectation) => dashboardV3.updateAccount(id, accountUpdateInput(update), expectation))),

  reorderAccounts: async (accountIds: string[]): Promise<Account[]> =>
    (await withCas((expectation) => dashboardV3.reorderAccounts(accountIds, expectation))).accounts.map(presentAccount),

  deleteAccount: async (id: string): Promise<void> => {
    await withCas((expectation) => dashboardV3.deleteAccount(id, expectation));
  },

  toggleAccount: (id: string): Promise<Account> =>
    mutatedAccount(withCas((expectation) => dashboardV3.toggleAccount(id, expectation))),

  resetAccountCooldown: (id: string): Promise<Account> =>
    mutatedAccount(withCas((expectation) => dashboardV3.resetAccountCooldown(id, expectation))),

  advanceAccountSetup: (id: string, setupStep: AccountSetupStep): Promise<Account> =>
    mutatedAccount(withCas((expectation) => dashboardV3.advanceAccountSetup(id, setupStep, expectation))),

  verifyManagedAccountKey: (id: string, key: string): Promise<Account> =>
    mutatedAccount(withCas((expectation) => dashboardV3.verifyManagedAccountKey(id, key, expectation))),

  testAccountModel: (id: string, modelId: string): Promise<AccountModelTestResponse> =>
    dashboardV3.testAccountModel(id, modelId),

  updateAccountCustomConfig: (
    id: string,
    config: AccountCustomConfigUpdateInput,
  ): Promise<Account> => mutatedAccount(withCas((expectation) => {
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
  ): Promise<Account> => mutatedAccount(withCas((expectation) => dashboardV3.putAccountModelCapabilities(id, {
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
  updateSettings: async (settings: AppConfig) => {
    await withCas((expectation) => dashboardV3.putSettings(settingsUpdateInput(settings), expectation));
    return presentSettings(await dashboardV3.getSettings());
  },
  testProxy: async (input: {
    proxy_mode: AppConfig["proxy_mode"];
    proxy_url?: string;
    proxy_list_direction?: AppConfig["proxy_list_direction"];
  }) => presentProxyTest(await dashboardV3.testProxy({
    proxyMode: input.proxy_mode,
    proxyUrl: input.proxy_url,
    proxyListDirection: input.proxy_list_direction,
  } satisfies ProxyTestRequest)),

  getPricing: async () => {
    const result = await dashboardV3.getProviderPricing("opencode");
    if (!result.snapshot) throw new Error("OpenCode Go pricing is not available");
    return presentPricing(result.snapshot);
  },
  refreshProviderPricing: async (
    providerId: string,
    refresh: ProviderPricingRefreshRequest = {},
  ) => {
    const controlPlane = useControlPlaneStore();
    if (!controlPlane.hasTokens()) await controlPlane.refresh();
    const expectedProviderPricingRevision = refresh.expected_provider_revision;
    if (!expectedProviderPricingRevision) {
      throw new Error("provider pricing revision is not loaded yet");
    }
    const result = await controlPlane.runMutation((expectation) => (
      dashboardV3.refreshProviderPricing(providerId, {
        expectedProviderPricingRevision,
        policy: refresh.policy,
        expectedOfficialContentHash: refresh.expected_official_content_hash,
      }, expectation)
    ));
    return presentProviderPricingRefresh(result);
  },
  updatePricingMultipliers: async (expectedPricingRevision: string, multipliers: PricingMultiplierUpdate[]) => {
    const controlPlane = useControlPlaneStore();
    if (!controlPlane.hasTokens()) await controlPlane.refresh();
    const result = await controlPlane.runMutation((expectation) => dashboardV3.putProviderPricingMultipliers(
      "opencode",
      {
        expectedPricingRevision,
        multipliers: multipliers.map((multiplier) => ({
          modelId: multiplier.model_id,
          multiplier: multiplier.multiplier,
        })),
      },
      expectation,
    ));
    if (!result.snapshot) throw new Error("OpenCode Go pricing snapshot is not available");
    return presentPricing(result.snapshot);
  },

  checkForUpdate: async () => presentUpdateCheck(await dashboardV3.checkForUpdate()),
  getUpdateStatus: async () => presentUpdateStatus(await dashboardV3.getUpdateStatus()),
  installUpdate: async (expectedVersion: string) =>
    presentUpdateStatus(await withCas((expectation) => dashboardV3.installUpdate(expectedVersion, expectation))),

  getGatewayLogs: async (limit?: number, requestId?: string | null) =>
    (await dashboardV3.getGatewayLogs({ limit, requestId: requestId ?? null })).items.map(presentGatewayLog),
  getForwardLogs: async (query: ForwardLogQuery = {}) =>
    presentForwardLogs(await dashboardV3.getForwardLogs(forwardLogQuery(query))),
  getForwardLogModels: async () => (await dashboardV3.getForwardLogModels()).models,
  getForwardLogKeys: async () => (await dashboardV3.getForwardLogKeys()).keys,
  getDashboardSummary: async () => presentDashboardSummary(await dashboardV3.getDashboardSummary()),
  getDailyTokensByModel: async (days?: number) =>
    (await dashboardV3.getDailyTokensByModel(days)).items.map(presentDailyModelTokens),
};
