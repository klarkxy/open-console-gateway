import { useControlPlaneStore } from "../stores/controlPlane.ts";
import { requestV3, type WithoutExpectation } from "./dashboard-v3.ts";
import type {
  MutationAck,
  MutationExpectation,
  PlatformAccount,
  PlatformAccounts,
  PlatformCreate,
  PlatformGroup,
  PlatformLink,
  PlatformLinkWrite,
  PlatformModel,
  PlatformPrice,
  PlatformQuota,
  PlatformQuotaKind,
  PlatformRefresh,
  PlatformSnapshot,
  PlatformUpdate,
} from "./generated/dashboard-v3.ts";

// The wire shape is the shared generated contract; re-exported here so views
// and domain code import platform types from one place.
export type {
  PlatformAccount,
  PlatformGroup,
  PlatformKind,
  PlatformLink,
  PlatformModel,
  PlatformPrice,
  PlatformQuota,
  PlatformQuotaKind,
  PlatformSnapshot,
} from "./generated/dashboard-v3.ts";

/**
 * New API / Sub2API platform-account client for `/dashboard/api/v3`.
 *
 * Responses are validated field-by-field by the adapters below before they are
 * handed to the UI: a malformed payload degrades to explicit defaults instead
 * of an unchecked cast pretending to be a valid snapshot.
 */

/** List/create/update/link/unlink/refresh all return this full view. */
export type PlatformAccountsView = PlatformAccounts;

export interface PlatformAccountCreateInput {
  kind: PlatformAccount["kind"];
  name: string;
  baseUrl: string;
  userCredential?: string;
}

export interface PlatformAccountUpdateInput {
  name: string;
  /** Omitted/undefined preserves the saved credential; empty string clears it. */
  userCredential?: string;
}

function str(value: unknown, fallback = ""): string {
  return typeof value === "string" ? value : fallback;
}

function strOrNull(value: unknown): string | null {
  return typeof value === "string" && value ? value : null;
}

function numOrNull(value: unknown): number | null {
  return typeof value === "number" && Number.isFinite(value) ? value : null;
}

function num(value: unknown, fallback = 0): number {
  return typeof value === "number" && Number.isFinite(value) ? value : fallback;
}

function strList(value: unknown): string[] {
  return Array.isArray(value) ? value.filter((item): item is string => typeof item === "string") : [];
}

function record(value: unknown): Record<string, unknown> {
  return typeof value === "object" && value !== null ? value as Record<string, unknown> : {};
}

function adaptGroup(value: unknown): PlatformGroup {
  const dto = record(value);
  return {
    id: strOrNull(dto.id),
    platform: strOrNull(dto.platform),
    subscriptionType: strOrNull(dto.subscriptionType),
    autoGroups: strList(dto.autoGroups),
    verified: dto.verified === true,
  };
}

function adaptQuotaKind(value: unknown): PlatformQuotaKind {
  return value === "subscription" || value === "key_limit" ? value : "wallet";
}

function adaptQuota(value: unknown): PlatformQuota {
  const dto = record(value);
  return {
    kind: adaptQuotaKind(dto.kind),
    scopeId: str(dto.scopeId),
    unit: str(dto.unit),
    used: numOrNull(dto.used),
    remaining: numOrNull(dto.remaining),
    limit: numOrNull(dto.limit),
    unlimited: dto.unlimited === true,
    period: strOrNull(dto.period),
    resetsAt: numOrNull(dto.resetsAt),
    expiresAt: numOrNull(dto.expiresAt),
    source: str(dto.source),
  };
}

function adaptModel(value: unknown): PlatformModel {
  const dto = record(value);
  return {
    id: str(dto.id),
    platform: strOrNull(dto.platform),
    groupId: strOrNull(dto.groupId),
    source: str(dto.source),
  };
}

function adaptPrice(value: unknown): PlatformPrice {
  const dto = record(value);
  return {
    model: str(dto.model),
    groupId: strOrNull(dto.groupId),
    currency: str(dto.currency),
    input: numOrNull(dto.input),
    output: numOrNull(dto.output),
    cacheRead: numOrNull(dto.cacheRead),
    cacheWrite: numOrNull(dto.cacheWrite),
    source: str(dto.source),
    officialReference: dto.officialReference === true,
    unavailableReason: strOrNull(dto.unavailableReason),
    validUntil: num(dto.validUntil),
  };
}

function adaptSnapshot(value: unknown): PlatformSnapshot | null {
  if (typeof value !== "object" || value === null) return null;
  const dto = record(value);
  return {
    observedAt: num(dto.observedAt),
    stale: dto.stale === true,
    errors: strList(dto.errors),
    quotas: Array.isArray(dto.quotas) ? dto.quotas.map(adaptQuota) : [],
    models: Array.isArray(dto.models) ? dto.models.map(adaptModel) : [],
    prices: Array.isArray(dto.prices) ? dto.prices.map(adaptPrice) : [],
    groups: Array.isArray(dto.groups) ? dto.groups.map(adaptGroup) : [],
    billingPreference: strOrNull(dto.billingPreference),
    walletOverflow: typeof dto.walletOverflow === "boolean" ? dto.walletOverflow : null,
  };
}

function adaptPlatformAccount(value: unknown): PlatformAccount {
  const dto = record(value);
  return {
    id: str(dto.id),
    kind: dto.kind === "sub2api" ? "sub2api" : "new_api",
    name: str(dto.name),
    baseUrl: str(dto.baseUrl),
    hasUserCredential: dto.hasUserCredential === true,
    version: num(dto.version),
    snapshot: adaptSnapshot(dto.snapshot),
  };
}

function adaptPlatformLink(value: unknown): PlatformLink {
  const dto = record(value);
  return {
    accountId: str(dto.accountId),
    platformAccountId: str(dto.platformAccountId),
    group: adaptGroup(dto.group),
    snapshot: adaptSnapshot(dto.snapshot),
  };
}

function adaptView(value: unknown): PlatformAccountsView {
  const dto = record(value);
  return {
    accounts: Array.isArray(dto.accounts) ? dto.accounts.map(adaptPlatformAccount) : [],
    links: Array.isArray(dto.links) ? dto.links.map(adaptPlatformLink) : [],
    revision: num(dto.revision),
    processGeneration: num(dto.processGeneration),
  };
}

async function withCas<T>(
  run: (expectation: MutationExpectation) => Promise<T>,
): Promise<T> {
  const controlPlane = useControlPlaneStore();
  if (!controlPlane.hasTokens()) await controlPlane.refresh();
  return controlPlane.runMutation(run);
}

function encode(segment: string): string {
  return encodeURIComponent(segment);
}

/** Group written on link: id/platform come from the user pick; the observed subscription type is not trusted config. */
export function platformGroupWrite(group: Pick<PlatformGroup, "id" | "platform">): PlatformGroup {
  return { id: group.id, platform: group.platform, subscriptionType: null, autoGroups: [], verified: false };
}

export const platformAccountsApi = {
  list: async (): Promise<PlatformAccountsView> => adaptView(await requestV3<unknown>("/platform-accounts")),

  create: (input: PlatformAccountCreateInput): Promise<PlatformAccountsView> =>
    withCas(async (expectation) => adaptView(await requestV3<unknown>("/platform-accounts", {
      method: "POST",
      body: JSON.stringify({
        kind: input.kind,
        name: input.name,
        baseUrl: input.baseUrl,
        ...(input.userCredential !== undefined ? { userCredential: input.userCredential } : {}),
        ...expectation,
      } satisfies WithoutExpectation<PlatformCreate> & MutationExpectation),
    }))),

  update: (id: string, input: PlatformAccountUpdateInput): Promise<PlatformAccountsView> =>
    withCas(async (expectation) => adaptView(await requestV3<unknown>(`/platform-accounts/${encode(id)}`, {
      method: "PUT",
      body: JSON.stringify({
        name: input.name,
        // null and omission both preserve the saved credential; "" clears it.
        ...(input.userCredential !== undefined ? { userCredential: input.userCredential } : {}),
        ...expectation,
      } satisfies WithoutExpectation<PlatformUpdate> & MutationExpectation),
    }))),

  remove: (id: string): Promise<MutationAck> =>
    withCas((expectation) => requestV3<MutationAck>(`/platform-accounts/${encode(id)}`, {
      method: "DELETE",
      body: JSON.stringify(expectation),
    })),

  link: (accountId: string, platformAccountId: string, group: PlatformGroup): Promise<PlatformAccountsView> =>
    withCas(async (expectation) => adaptView(await requestV3<unknown>(`/accounts/${encode(accountId)}/platform-link`, {
      method: "PUT",
      body: JSON.stringify({
        platformAccountId,
        group,
        ...expectation,
      } satisfies WithoutExpectation<PlatformLinkWrite> & MutationExpectation),
    }))),

  unlink: (accountId: string): Promise<PlatformAccountsView> =>
    withCas(async (expectation) => adaptView(await requestV3<unknown>(`/accounts/${encode(accountId)}/platform-link`, {
      method: "DELETE",
      body: JSON.stringify(expectation),
    }))),

  /** Without `accountId` refreshes the parent; with it, only that linked Key's view. */
  refresh: (id: string, accountId?: string): Promise<PlatformAccountsView> =>
    withCas(async (expectation) => adaptView(await requestV3<unknown>(`/platform-accounts/${encode(id)}/refresh`, {
      method: "POST",
      body: JSON.stringify({
        ...(accountId !== undefined ? { accountId } : {}),
        ...expectation,
      } satisfies WithoutExpectation<PlatformRefresh> & MutationExpectation),
    }))),
};
