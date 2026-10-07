import type { pagesApi } from "../api/pages.ts";
import { appViewRoute } from "../views/app-navigation.ts";
import type { MessageKey } from "../i18n/index.ts";

export type AliasPage = Awaited<ReturnType<typeof pagesApi.aliases>>;
export type AliasPageGroup = AliasPage["groups"][number];
export type AliasPageRow = AliasPageGroup["rows"][number];

export const ALIAS_PAGE_ISSUE_KEYS = {
  catalog: "加载供应商目录失败：{error}",
  accounts: "加载 Custom Alias 账号失败：{error}",
  destinations: "目的地投影刷新失败：{error}",
  identities: "加载路由顺位失败：{error}",
  cpa: "加载 CPA 模型目录失败：{error}",
  publication: "加载对外展示失败：{error}",
  metadata: "加载模型能力失败：{error}",
  contracts: "加载供应商失败：{error}",
} as const satisfies Record<string, MessageKey>;

export function aliasPageIssueKey(resource: string): MessageKey {
  return ALIAS_PAGE_ISSUE_KEYS[resource as keyof typeof ALIAS_PAGE_ISSUE_KEYS] ?? ALIAS_PAGE_ISSUE_KEYS.contracts;
}

export function aliasPagePublicationKey(name: string): string {
  return name.trim().toLocaleLowerCase();
}

/** Targets are server decisions; the view only translates them into a route. */
export function aliasPageTarget(target: AliasPageRow["target"] | AliasPageRow["capabilityTarget"]) {
  if (!target) return null;
  if (target.accountId) return appViewRoute("accounts", undefined, { account_id: target.accountId });
  if (!target.providerId && !target.destinationId) return null;
  return appViewRoute("providers", {
    ...(target.destinationId ? { destination: target.destinationId } : { provider: target.providerId! }),
    tab: "models",
    model: target.model,
  }, target.capabilities ? { capabilities: target.model } : undefined);
}

export function aliasPageRankText(row: Pick<AliasPageRow, "routingRanks">): string {
  return row.routingRanks.length ? row.routingRanks.join(" · ") : "—";
}

/** A publication receipt changes every visible segment of that public name. */
export function applyAliasPagePublication(page: AliasPage, unpublished: readonly string[]): AliasPage {
  const hidden = new Set(unpublished.map(aliasPagePublicationKey));
  return {
    ...page,
    groups: page.groups.map(group => ({ ...group, published: !hidden.has(group.publicationKey) })),
  };
}
