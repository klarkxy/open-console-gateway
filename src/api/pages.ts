/** Bounded, Rust-owned management read models on the existing V4 transport. */
import { requestV4, withExpectation } from "./dashboard-v3.ts";
import type { MutationExpectation } from "./generated/dashboard-v3.ts";
import type {
  AccountsPage as WireAccountsPage,
  AccountCardPageItem,
  AccountPageRow,
  AccountPageDetail,
  AccountPageRefresh as WireAccountPageRefresh,
  AccountCardCredentialsPage,
  ProvidersPage,
  ProviderPageItem,
  ProviderPageDetail,
  ProviderModelsPage,
  ProviderEditDetail,
  AliasesPage,
  AccountSummary,
  DestinationSummary,
  AccountCredentialSummary,
  DashboardPage, DashboardPageSummary, DashboardAttentionItem, DashboardAttentionReason, DashboardModelTotal,
  AccountOperationDetail as WireAccountOperationDetail,
  DashboardChartDay,
} from "./generated/dashboard-v4.ts";
import { presentAccount, type Account } from "./dashboard-presenters.ts";
import {
  presentDestination, presentDestinationCredential,
  type Destination, type DestinationCredential,
} from "./destinations.ts";
import { presentIdentity, type Identity } from "./identities.ts";
import { presentConnection, type Connection } from "./connections.ts";

export type {
  DashboardChartDay,
  DashboardPage, DashboardPageSummary, DashboardAttentionItem, DashboardAttentionReason, DashboardModelTotal,
  AccountPageRow, AccountCardCredentialsPage,
  ProvidersPage, ProviderPageItem, ProviderPageDetail, ProviderModelsPage,
  ProviderEditDetail, AliasesPage, AccountSummary, DestinationSummary,
};
export type CredentialSummary = AccountCredentialSummary;
export type AccountsPage = WireAccountsPage & {
  planFilters: { providerId: string; label: string; count: number }[];
};
export type AccountPageRefresh = Omit<WireAccountPageRefresh, "account"> & { account: Account | null };
export type AccountPageCard = AccountCardPageItem;
export type ProviderModelRow = ProviderModelsPage["models"][number];
export type AliasPageGroup = AliasesPage["groups"][number];
export type AliasPageRow = AliasPageGroup["rows"][number];
export type AccountPageAction = AccountPageRow["actions"][number];
export type AccountOperationDetail = Omit<WireAccountOperationDetail, "allowedConnections"> & { allowedConnections: Connection[] };
export type AccountDetail = Omit<AccountPageDetail, "account" | "destination" | "credential" | "identity" | "connection" | "operations"> & {
  operations: AccountOperationDetail;
  account: Account;
  destination: Destination | null;
  credential: DestinationCredential | null;
  identity: Identity | null;
  connection: Connection | null;
};

export interface PageQuery {
  search?: string;
  offset?: number;
  limit?: number;
}
export interface AccountsPageQuery extends PageQuery {
  plan?: string;
  status?: string;
}
export type AccountsQuery = AccountsPageQuery;
export interface ProvidersPageQuery extends PageQuery {
  sort?: string;
}
export interface ProviderModelsQuery extends PageQuery {
  model?: string;
  enabledOnly?: boolean;
}

function pagePath(path: string, query: PageQuery & Record<string, unknown> = {}): string {
  const params = new URLSearchParams();
  for (const [key, value] of Object.entries(query)) {
    if (value === undefined || value === null || value === "") continue;
    params.set(key, String(value));
  }
  const search = params.toString();
  return search ? `${path}?${search}` : path;
}

function accountDetail(value: AccountPageDetail): AccountDetail {
  return {
    ...value,
    operations: { ...value.operations, allowedConnections: value.operations.allowedConnections.map(presentConnection) },
    account: presentAccount(value.account),
    destination: value.destination ? presentDestination(value.destination) : null,
    credential: value.credential ? presentDestinationCredential(value.credential) : null,
    identity: value.identity ? presentIdentity(value.identity) : null,
    connection: value.connection ? presentConnection(value.connection) : null,
  };
}

function accountPage(value: WireAccountsPage): AccountsPage {
  return {
    ...value,
    planFilters: value.planOptions.map(option => ({
      providerId: option.value, label: option.label, count: option.credentialCount,
    })),
  };
}

export const pagesApi = {
  dashboard: (query: { utcOffsetMinutes?: number } = {}, signal?: AbortSignal) =>
    requestV4<DashboardPage>(pagePath("/pages/dashboard", query), { signal }),
  accounts: async (query: AccountsPageQuery = {}, signal?: AbortSignal): Promise<AccountsPage> =>
    accountPage(await requestV4<WireAccountsPage>(pagePath("/pages/accounts", { ...query }), { signal })),
  accountCredentials: (cardId: string, query: AccountsPageQuery = {}, signal?: AbortSignal) =>
    requestV4<AccountCardCredentialsPage>(pagePath(`/pages/accounts/cards/${encodeURIComponent(cardId)}/credentials`, { ...query }), { signal }),
  accountDetail: async (id: string, signal?: AbortSignal): Promise<AccountDetail> =>
    accountDetail(await requestV4<AccountPageDetail>(`/pages/accounts/${encodeURIComponent(id)}/detail`, { signal })),
  refreshAccount: async (id: string, mode: "automatic" | "manual", expectation: MutationExpectation, signal?: AbortSignal): Promise<AccountPageRefresh> => {
    const value = await requestV4<WireAccountPageRefresh>(`/pages/accounts/${encodeURIComponent(id)}/refresh`, {
      method: "POST", body: withExpectation({ mode }, expectation), signal,
    });
    return { ...value, account: value.account ? presentAccount(value.account) : null };
  },
  providers: (query: ProvidersPageQuery = {}, signal?: AbortSignal) =>
    requestV4<ProvidersPage>(pagePath("/pages/providers", { ...query }), { signal }),
  providerDetail: (railKey: string, signal?: AbortSignal) =>
    requestV4<ProviderPageDetail>(`/pages/providers/${encodeURIComponent(railKey)}`, { signal }),
  providerModels: (railKey: string, query: ProviderModelsQuery = {}, signal?: AbortSignal) =>
    requestV4<ProviderModelsPage>(pagePath(`/pages/providers/${encodeURIComponent(railKey)}/models`, { ...query }), { signal }),
  providerEditDetail: (railKey: string, signal?: AbortSignal) =>
    requestV4<ProviderEditDetail>(`/pages/providers/${encodeURIComponent(railKey)}/edit-detail`, { signal }),
  aliases: (query: PageQuery = {}, signal?: AbortSignal) =>
    requestV4<AliasesPage>(pagePath("/pages/aliases", { ...query }), { signal }),
};
