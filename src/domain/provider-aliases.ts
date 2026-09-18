import type { Account } from "../api/dashboard.ts";
import type { ProviderDefinitionView } from "../api/providers.ts";
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
      if (!model.alias) continue;
      rows.push({
        provider_id: providerId,
        key: `${scope.key}:${model.alias}:${model.model_id}`,
        public_model: model.alias,
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
    for (const capability of account.model_capabilities) {
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

export function dynamicProviderAliasRows(
  providers: readonly ProviderDefinitionView[],
): ProviderAliasRow[] {
  return providers.flatMap((provider) => provider.models.map((model) => ({
    provider_id: provider.id,
    key: `dynamic:${provider.id}:${model.public_model}:${model.upstream_model}`,
    public_model: model.public_model,
    provider_plan: provider.name,
    custom_account: null,
    upstream_model: model.upstream_model,
    routable: true,
    custom_account_id: null,
  })));
}

/** Production Alias table: enabled-account providers, then CPA catalog pins. */
export function mergeProviderAliasRows(
  scopes: readonly ProviderScopeView[],
  accounts: readonly Account[],
  providers: readonly ProviderDefinitionView[],
  cpaModels: readonly CpaAliasModel[] = [],
): ProviderAliasRow[] {
  const enabled = enabledAliasProviderIds(accounts);
  return [
    ...providerAliasRows(scopes, accounts),
    ...dynamicProviderAliasRows(providers.filter((provider) => enabled.has(provider.id))),
    ...(enabled.has(CPA_PROVIDER_ID) ? cpaAliasRows(cpaModels, scopes) : []),
  ];
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
