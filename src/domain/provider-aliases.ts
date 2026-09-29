import type { Account } from "../api/dashboard.ts";
import type { Destination } from "../api/destinations.ts";
import type { Identity, ModelScope } from "../api/identities.ts";
import type { ProviderDefinitionView } from "../api/providers.ts";
import { projectDestinationCatalog } from "./destination-catalog.ts";
import { CPA_PROVIDER_ID } from "./destination-providers.ts";
import type { ProviderScopeView } from "./provider-contracts.ts";

export interface ProviderAliasRow {
  provider_id: string;
  key: string;
  public_model: string;
  provider_plan: string;
  custom_account: string | null;
  upstream_model: string;
  routable: boolean;
  custom_account_id: string | null;
}

export type CpaAliasModel = {
  id: string;
  enabled: boolean;
};

/** Provider ids that currently have at least one enabled account. */
export function enabledAliasProviderIds(accounts: readonly Account[]): Set<string> {
  return new Set(
    accounts.filter((account) => account.enabled).map((account) => account.provider_id),
  );
}

function scopeProviderId(scope: ProviderScopeView): string {
  return scope.provider_id || scope.scope_id;
}

/** Prefer a code-owned Alias when a CPA catalog ID can join one. */
export function cpaPublicModelName(
  scopes: readonly ProviderScopeView[],
  modelId: string,
): string {
  const needle = modelId.toLocaleLowerCase();
  for (const scope of scopes) {
    if (scope.scope_kind !== "provider") continue;
    for (const model of scope.models) {
      if (model.model_id === modelId && model.alias) return model.alias;
      if (model.alias && model.alias.toLocaleLowerCase() === needle) return model.alias;
    }
  }
  return modelId;
}

export function cpaAliasRows(
  models: readonly CpaAliasModel[],
  scopes: readonly ProviderScopeView[],
): ProviderAliasRow[] {
  return models
    .filter((model) => model.enabled)
    .map((model) => ({
      provider_id: CPA_PROVIDER_ID,
      key: `cpa:${model.id}`,
      public_model: cpaPublicModelName(scopes, model.id),
      provider_plan: "CPA",
      custom_account: null,
      upstream_model: model.id,
      routable: true,
      custom_account_id: null,
    }));
}

function providerPlanLabel(scope: ProviderScopeView): string {
  return scope.label;
}

/** Case-folded public name used by downstream publication. */
export function publicModelPublicationKey(name: string): string {
  return name.trim().toLowerCase();
}

/** Default on: missing names stay visible to downstream `GET /v1/models`. */
export function isPublicModelPublished(
  name: string,
  unpublished: readonly string[],
): boolean {
  const key = publicModelPublicationKey(name);
  return !unpublished.some((item) => publicModelPublicationKey(item) === key);
}

/**
 * This is a read-only cross-reference. Provider contracts describe built-in
 * Alias resolution; account capabilities describe Custom mappings. Downstream
 * listing publication is separate operator state.
 */
export function providerAliasRows(
  scopes: readonly ProviderScopeView[],
  accounts: readonly Account[],
): ProviderAliasRow[] {
  const rows: ProviderAliasRow[] = [];
  const enabledProviders = enabledAliasProviderIds(accounts);
  const providerRawModels = new Set(
    scopes
      .filter((scope) => scope.scope_kind === "provider")
      .flatMap((scope) => scope.models.map((model) => model.model_id)),
  );
  for (const scope of scopes) {
    if (scope.scope_kind !== "provider") continue;
    const providerId = scopeProviderId(scope);
    if (!enabledProviders.has(providerId)) continue;
    for (const model of scope.models) {
      const publicModel = model.alias || (providerId === "opencode" ? model.model_id : "");
      if (!publicModel) continue;
      rows.push({
        provider_id: providerId,
        key: `${scope.key}:${publicModel}:${model.model_id}`,
        public_model: publicModel,
        provider_plan: providerPlanLabel(scope),
        custom_account: null,
        upstream_model: model.model_id,
        routable: model.routable,
        custom_account_id: null,
      });
    }
  }

  for (const account of accounts) {
    if (account.provider_id !== "custom" || !account.enabled) continue;
    const scope = scopes.find((candidate) => (
      candidate.scope_kind === "custom_endpoint" && candidate.scope_id === account.id
    ));
    // Capabilities are stored per protocol; the Alias table has no protocol
    // column, so identical mappings collapse to one row per account.
    const seenMappings = new Set<string>();
    for (const capability of account.model_capabilities) {
      const mappingKey = `${capability.public_model.toLocaleLowerCase()}:${capability.upstream_model}`;
      if (seenMappings.has(mappingKey)) continue;
      seenMappings.add(mappingKey);
      const contract = scope?.models.find((model) => (
        (model.alias || model.model_id).toLocaleLowerCase()
          === capability.public_model.toLocaleLowerCase()
      ));
      const conflictsWithProviderRaw = providerRawModels.has(capability.public_model);
      rows.push({
        provider_id: account.provider_id,
        key: `custom:${account.id}:${capability.public_model}:${capability.upstream_model}`,
        public_model: capability.public_model,
        provider_plan: scope?.label || "Custom API",
        custom_account: account.name,
        upstream_model: capability.upstream_model,
        routable: !conflictsWithProviderRaw
          && account.enabled
          && account.setup_step === "ready"
          && account.plan_routable
          && Boolean(contract?.routable),
        custom_account_id: account.id,
      });
    }
  }
  return rows;
}

/** The Configurable HTTP destination backing one user-defined Provider, if any. */
function dynamicDestinationFor(
  providerId: string,
  destinations: readonly Destination[],
): Destination | null {
  for (const destination of destinations) {
    if (destination.legacy.kind === "dynamic" && destination.legacy.id === providerId) {
      return destination;
    }
  }
  return null;
}

/**
 * User-defined Provider mappings, projected through the same destination
 * catalog the Providers page renders, so per-model enablement and protocol
 * state decide routability here too. A Provider without a loaded destination
 * has no catalog facts at all, so it yields no rows rather than claiming
 * routes it cannot prove.
 */
export function dynamicProviderAliasRows(
  providers: readonly ProviderDefinitionView[],
  destinations: readonly Destination[] = [],
): ProviderAliasRow[] {
  return providers.flatMap((provider) => {
    const destination = dynamicDestinationFor(provider.id, destinations);
    if (!destination) return [];
    const scope = projectDestinationCatalog(destination);
    return provider.models.map((model) => {
      const contract = scope.models.find((entry) => entry.model_id === model.public_model);
      return {
        provider_id: provider.id,
        key: `dynamic:${provider.id}:${model.public_model}:${model.upstream_model}`,
        public_model: model.public_model,
        provider_plan: provider.name,
        custom_account: null,
        upstream_model: model.upstream_model,
        // `production_inference` is the destination's own enablement, the same
        // pair the gateway requires before it lists a model downstream.
        routable: scope.production_inference && Boolean(contract?.routable),
        custom_account_id: null,
      };
    });
  });
}

/** Production Alias table: enabled mappings from enabled-account providers and CPA. */
export function mergeProviderAliasRows(
  scopes: readonly ProviderScopeView[],
  accounts: readonly Account[],
  providers: readonly ProviderDefinitionView[],
  cpaModels: readonly CpaAliasModel[] = [],
  destinations: readonly Destination[] = [],
): ProviderAliasRow[] {
  const enabled = enabledAliasProviderIds(accounts);
  return [
    ...providerAliasRows(scopes, accounts),
    ...dynamicProviderAliasRows(providers.filter((provider) => enabled.has(provider.id)), destinations),
    ...(enabled.has(CPA_PROVIDER_ID) ? cpaAliasRows(cpaModels, scopes) : []),
  ].filter((row) => row.routable);
}

/** Configuration inventory only; these counts do not predict request-time eligibility. */
export function aliasAccountCounts(row: ProviderAliasRow, accounts: readonly Account[]) {
  const matching = accounts.filter((account) => row.custom_account_id
    ? account.id === row.custom_account_id
    : account.provider_id === row.provider_id);
  return { total: matching.length, enabled: matching.filter((account) => account.enabled).length };
}

/** Flag cross-provider names that can be interpreted as another exact upstream ID. */
export function aliasNameOverlaps(row: ProviderAliasRow, rows: readonly ProviderAliasRow[]): boolean {
  return rows.some((other) => other.provider_id !== row.provider_id
    && other.upstream_model === row.public_model
    && other.public_model.toLocaleLowerCase() !== row.public_model.toLocaleLowerCase());
}

/** Separator that cannot appear in a provider id or a folded public name. */
const OVERLAP_TAG_SEPARATOR = "\u0000";

/** Identity of one row as an overlap candidate: provider plus folded public name. */
function overlapTag(row: ProviderAliasRow): string {
  return `${row.provider_id}${OVERLAP_TAG_SEPARATOR}${row.public_model.toLocaleLowerCase()}`;
}

/** True when a candidate tagged from an upstream-model bucket overlaps `row`. */
function bucketOverlapsRow(bucket: ReadonlySet<string>, row: ProviderAliasRow): boolean {
  const folded = row.public_model.toLocaleLowerCase();
  for (const tag of bucket) {
    const separator = tag.indexOf(OVERLAP_TAG_SEPARATOR);
    if (tag.slice(0, separator) !== row.provider_id && tag.slice(separator + 1) !== folded) return true;
  }
  return false;
}

/**
 * Row keys whose public name can also be read as another provider's raw
 * upstream ID — the same predicate {@link aliasNameOverlaps} applies, resolved
 * for the whole table in one indexed pass. The Alias page renders the warning
 * per group on every re-render, so the per-row table scan this replaces is
 * quadratic in the number of rows.
 */
export function aliasOverlapFlags(rows: readonly ProviderAliasRow[]): Set<string> {
  const byUpstreamModel = new Map<string, Set<string>>();
  for (const row of rows) {
    const tag = overlapTag(row);
    const bucket = byUpstreamModel.get(row.upstream_model);
    if (!bucket) byUpstreamModel.set(row.upstream_model, new Set([tag]));
    else bucket.add(tag);
  }
  const flags = new Set<string>();
  for (const row of rows) {
    const bucket = byUpstreamModel.get(row.public_model);
    if (!bucket) continue;
    if (bucketOverlapsRow(bucket, row)) flags.add(row.key);
  }
  return flags;
}

/** Platform label for a platform-linked Custom Key row; null for anything else. */
export function aliasRowPlatformLabel(
  row: ProviderAliasRow,
  identities: readonly Identity[],
): string | null {
  if (!row.custom_account_id) return null;
  const accountIdentity = identities.find((identity) => (
    identity.legacy.kind === "account" && identity.legacy.id === row.custom_account_id
  ));
  const platformId = accountIdentity?.declared_relations[0]?.platform_account_id;
  if (!platformId) return null;
  const label = identities.find((identity) => (
    identity.legacy.kind === "platform_account" && identity.legacy.id === platformId
  ))?.identity.label.trim();
  return label || null;
}

/** Config-level scope check mirroring the gateway's binding model scope. */
export function aliasModelScopeAllows(scope: ModelScope, publicModel: string): boolean {
  if (scope.kind === "all") return true;
  const needle = publicModel.toLocaleLowerCase();
  return scope.models.some((model) => model.toLocaleLowerCase() === needle);
}

/**
 * Ascending routing ranks of the inference credentials that serve one Alias
 * row at config level: enabled accounts, enabled bindings, scope allowing the
 * public name. Non-routable rows serve nothing. Runtime state (cooldowns,
 * quota) is not reflected, so this is the configured order, not a live pick.
 */
interface AliasRankBinding {
  rank: number;
  scope: ModelScope;
}

interface AliasRoutingRankIndex {
  enabledAccountIds: ReadonlySet<string>;
  providerAccountIds: ReadonlyMap<string, ReadonlySet<string>>;
  bindingsByAccountId: ReadonlyMap<string, readonly AliasRankBinding[]>;
}

/**
 * Account and credential facts shared by every row. Building this once keeps
 * a full Alias table from rescanning every identity for each model.
 */
export function aliasRoutingRankIndex(
  accounts: readonly Account[],
  identities: readonly Identity[],
): AliasRoutingRankIndex {
  const enabledAccountIds = new Set<string>();
  const providerAccountIds = new Map<string, Set<string>>();
  for (const account of accounts) {
    if (!account.enabled) continue;
    enabledAccountIds.add(account.id);
    const ids = providerAccountIds.get(account.provider_id);
    if (ids) ids.add(account.id);
    else providerAccountIds.set(account.provider_id, new Set([account.id]));
  }
  const bindingsByAccountId = new Map<string, AliasRankBinding[]>();
  for (const identity of identities) {
    for (const credential of identity.credentials) {
      if (credential.legacy.kind !== "account" || !enabledAccountIds.has(credential.legacy.id)) continue;
      if (credential.credential.purpose !== "inference" || !credential.credential.enabled) continue;
      const bindings = credential.bindings.flatMap((binding) => (
        binding.enabled ? [{ rank: binding.routing_rank, scope: binding.model_scope }] : []
      ));
      if (bindings.length === 0) continue;
      const existing = bindingsByAccountId.get(credential.legacy.id);
      if (existing) existing.push(...bindings);
      else bindingsByAccountId.set(credential.legacy.id, bindings);
    }
  }
  return { enabledAccountIds, providerAccountIds, bindingsByAccountId };
}

function ranksForAccounts(
  accountIds: ReadonlySet<string> | undefined,
  bindingsByAccountId: ReadonlyMap<string, readonly AliasRankBinding[]>,
  publicModel: string,
): number[] {
  if (!accountIds || accountIds.size === 0) return [];
  const ranks = new Set<number>();
  for (const accountId of accountIds) {
    for (const binding of bindingsByAccountId.get(accountId) ?? []) {
      if (aliasModelScopeAllows(binding.scope, publicModel)) ranks.add(binding.rank);
    }
  }
  return [...ranks].sort((left, right) => left - right);
}

/** Routing ranks for one row using an index shared by the whole Alias table. */
export function aliasRoutingRanksFromIndex(
  row: ProviderAliasRow,
  index: AliasRoutingRankIndex,
): number[] {
  if (!row.routable) return [];
  const accountIds = row.custom_account_id
    ? (index.enabledAccountIds.has(row.custom_account_id) ? new Set([row.custom_account_id]) : undefined)
    : index.providerAccountIds.get(row.provider_id);
  return ranksForAccounts(accountIds, index.bindingsByAccountId, row.public_model);
}

export function aliasRowRoutingRanks(
  row: ProviderAliasRow,
  accounts: readonly Account[],
  identities: readonly Identity[],
): number[] {
  return aliasRoutingRanksFromIndex(row, aliasRoutingRankIndex(accounts, identities));
}

/** Order one group's rows by first serving rank; unrouted rows keep their relative order at the end. */
export function sortAliasRowsByRouting(
  rows: readonly ProviderAliasRow[],
  ranksOf: (row: ProviderAliasRow) => readonly number[],
): ProviderAliasRow[] {
  return rows
    .map((row, index) => ({ row, index, rank: ranksOf(row)[0] ?? Number.POSITIVE_INFINITY }))
    .sort((left, right) => left.rank - right.rank || left.index - right.index)
    .map((entry) => entry.row);
}
