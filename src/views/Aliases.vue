<template>
  <div class="aliases-page">
    <div
      v-if="initialLoading"
      class="aliases-state"
      role="status"
      aria-live="polite"
      :aria-label="t('加载中…')"
    >
      <n-spin size="small" />
    </div>

    <n-alert
      v-else-if="loadError && !contracts"
      type="error"
      :title="t('加载供应商失败：{error}', { error: loadError })"
    >
      <n-button size="small" secondary :loading="loading" @click="loadAliases()">
        {{ t("重试") }}
      </n-button>
    </n-alert>

    <section v-else class="aliases-section" aria-labelledby="alias-table-title">
      <h2 id="alias-table-title" class="sr-only">{{ t("别名") }}</h2>
      <n-input v-model:value="search" clearable :input-props="{ 'aria-label': t('搜索模型或供应商') }" :placeholder="t('搜索模型或供应商')" class="aliases-search" />
      <n-alert
        v-if="loadError && contracts"
        type="warning"
        :title="t('加载供应商失败：{error}', { error: loadError })"
      >
        <n-button size="small" secondary :loading="loading" @click="loadAliases({ retain: true })">
          {{ t("重试") }}
        </n-button>
      </n-alert>
      <n-alert
        v-if="catalogLoadError"
        type="warning"
        :title="t('加载供应商目录失败：{error}', { error: catalogLoadError })"
      >
        <n-button size="small" secondary :loading="loading" @click="loadAliases({ retain: true })">
          {{ t("重试") }}
        </n-button>
      </n-alert>
      <n-alert
        v-if="accountsLoadError"
        type="warning"
        :title="t('加载 Custom Alias 账号失败：{error}', { error: accountsLoadError })"
      >
        <n-button size="small" secondary :loading="loading" @click="loadAliases({ retain: true })">
          {{ t("重试") }}
        </n-button>
      </n-alert>
      <n-alert
        v-if="dynamicLoadError"
        type="warning"
        :title="t('加载供应商失败：{error}', { error: dynamicLoadError })"
      >
        <n-button size="small" secondary :loading="loading" @click="loadAliases({ retain: true })">
          {{ t("重试") }}
        </n-button>
      </n-alert>
      <n-alert
        v-if="destinationsLoadError"
        type="warning"
        :title="t('目的地投影刷新失败：{error}', { error: destinationsLoadError })"
      >
        <n-button size="small" secondary :loading="loading" @click="loadAliases({ retain: true })">
          {{ t("重试") }}
        </n-button>
      </n-alert>
      <n-alert
        v-if="identitiesLoadError"
        type="warning"
        :title="t('加载路由顺位失败：{error}', { error: identitiesLoadError })"
      >
        <n-button size="small" secondary :loading="loading" @click="loadAliases({ retain: true })">
          {{ t("重试") }}
        </n-button>
      </n-alert>
      <n-alert
        v-if="cpaLoadError"
        type="warning"
        :title="t('加载 CPA 模型目录失败：{error}', { error: cpaLoadError })"
      >
        <n-button size="small" secondary :loading="loading" @click="loadAliases({ retain: true })">
          {{ t("重试") }}
        </n-button>
      </n-alert>

      <n-alert
        v-if="publicationLoadError"
        type="warning"
        :title="t('加载对外展示失败：{error}', { error: publicationLoadError })"
      >
        <n-button size="small" secondary :loading="loading" @click="loadAliases({ retain: true })">
          {{ t("重试") }}
        </n-button>
      </n-alert>
      <n-alert
        v-if="publicationSaveError"
        type="warning"
        :title="t('更新对外展示失败：{error}', { error: publicationSaveError })"
      />
      <n-alert
        v-if="capabilitiesLoadError"
        type="warning"
        :title="t('加载模型能力失败：{error}', { error: capabilitiesLoadError })"
      >
        <n-button size="small" secondary :loading="loading" @click="loadAliases({ retain: true })">
          {{ t("重试") }}
        </n-button>
      </n-alert>
      <n-empty v-if="aliasGroups.length === 0" :description="search.trim() ? t('无匹配模型') : t('暂无 Alias')" />
      <div v-else class="aliases-table-wrap" tabindex="0" role="region" :aria-label="t('模型映射')">
        <table class="aliases-table">
          <thead>
            <tr>
              <th>{{ t("对外模型名") }}</th>
              <th>{{ t("供应商 / 方案") }}</th>
              <th>{{ t("路由顺位") }}</th>
              <th>{{ t("上游模型 ID") }}</th>
              <th>{{ t("能力") }}</th>
              <th><span class="sr-only">{{ t("打开相关目标") }}</span></th>
            </tr>
          </thead>
          <tbody v-for="group in aliasGroups" :key="group.public_model">
            <tr v-for="(row, index) in group.rows" :key="row.key">
              <td
                v-if="index === 0"
                :rowspan="group.rows.length"
                class="aliases-name"
                :class="{ 'aliases-unpublished': !group.published }"
              >
                <div class="aliases-name-row">
                  <!-- Hover hints stay native: an NTooltip per group would
                       instantiate a Popover/Follower chain per row group, which
                       dominates first paint on large catalogs. Same copy, same
                       hover affordance, aria-label unchanged. -->
                  <n-switch
                    size="small"
                    :value="group.published"
                    :disabled="!publicationReady || publicationSaving(group.public_model)"
                    :loading="publicationSaving(group.public_model)"
                    :aria-label="t('对下游展示此模型')"
                    :title="t('关闭后下游不再列出此模型，仍可用该名称调用。')"
                    @update:value="(published) => setPublished(group.public_model, published)"
                  />
                  <code>{{ group.public_model }}</code>
                </div>
                <p v-if="groupHasOverlap(group.rows)" class="alias-warning">{{ t('名称与其他上游 ID 重叠，请检查调用名称。') }}</p>
              </td>
              <td>
                {{ row.provider_plan }}
                <n-tag v-if="platformLabels.get(row.key)" size="tiny" :bordered="false" class="alias-platform-tag">
                  {{ platformLabels.get(row.key) }}
                </n-tag>
              </td>
              <td class="aliases-rank">{{ rankText(row) }}</td>
              <td><code>{{ row.upstream_model }}</code></td>
              <td class="aliases-capability">
                <template v-if="capabilityFor(row).state === 'ready'">
                  <n-tag
                    v-for="modality in capabilityFor(row).input_modalities"
                    :key="modality"
                    size="tiny"
                    :bordered="false"
                  >
                    {{ modalityLabel(modality) }}
                  </n-tag>
                  <n-tag size="tiny" :bordered="false" class="aliases-capability-source">
                    {{ sourceLabel(capabilityFor(row).source) }}
                  </n-tag>
                </template>
                <template v-else-if="capabilityFor(row).state === 'unknown'">
                  <n-tag size="tiny" type="warning" :bordered="false">{{ t("未知") }}</n-tag>
                  <n-button
                    v-if="aliasCapabilityTarget(row)"
                    text
                    size="tiny"
                    type="primary"
                    :aria-label="`${t('去声明')} ${row.public_model}`"
                    @click="openCapabilityTarget(row)"
                  >
                    {{ t("去声明") }}
                  </n-button>
                </template>
                <n-tag
                  v-else-if="capabilityFor(row).state === 'error'"
                  size="tiny"
                  type="error"
                  :bordered="false"
                >
                  {{ t("加载失败") }}
                </n-tag>
                <span v-else-if="capabilityFor(row).state === 'unavailable'" class="aliases-capability-none">—</span>
                <span v-else class="aliases-capability-none">{{ t("加载中…") }}</span>
              </td>
              <td class="aliases-action">
                <n-button
                  v-if="aliasRowTarget(row)"
                  circle
                  quaternary
                  size="small"
                  :aria-label="row.custom_account_id ? t('打开相关账号') : t('打开相关供应商模型')"
                  :title="row.custom_account_id ? t('打开相关账号') : t('打开相关供应商模型')"
                  @click="openAliasRowTarget(row)"
                >
                  <template #icon><n-icon :component="LinkOutlined" /></template>
                </n-button>
              </td>
            </tr>
          </tbody>
        </table>
      </div>
    </section>
  </div>
</template>

<script setup lang="ts">
import { computed, onActivated, onMounted, onUnmounted, ref, watch } from "vue";
import { useRouter, type RouteLocationRaw } from "vue-router";
import { NAlert, NButton, NEmpty, NIcon, NInput, NSpin, NSwitch, NTag } from "naive-ui";
import { LinkOutlined } from "@vicons/antd";
import { isDynamicCatalogEntry } from "../domain/dynamic-provider.ts";
import {
  ALIAS_CAPABILITY_SOURCE_KEYS,
  ALIAS_MODALITY_KEYS,
  aliasCapabilityView,
  type AliasCapabilityView,
} from "../domain/alias-capabilities.ts";
import { flattenProviderScopes, normalizeProviderContractsResponse } from "../domain/provider-contracts.ts";
import { CPA_PROVIDER_ID } from "../domain/destination-providers.ts";
import {
  aliasOverlapFlags,
  aliasRowPlatformLabel,
  aliasRoutingRankIndex,
  aliasRoutingRanksFromIndex,
  isPublicModelPublished,
  mergeProviderAliasRows,
  publicModelPublicationKey,
  sortAliasRowsByRouting,
  type ProviderAliasRow,
} from "../domain/provider-aliases.ts";
import { t } from "../i18n/index.ts";
import { useAccountsStore } from "../stores/accounts.ts";
import { appViewRoute } from "./app-navigation.ts";
import { useDestinationsStore } from "../stores/destinations.ts";
import { useIdentitiesStore } from "../stores/identities.ts";
import { useProvidersStore } from "../stores/providers.ts";
import { useSessionStore } from "../stores/session.ts";
import { dashboardErrorDetail } from "../utils/errors.ts";

const accountsStore = useAccountsStore();
const destinationsStore = useDestinationsStore();
const identitiesStore = useIdentitiesStore();
const providersStore = useProvidersStore();
const sessionStore = useSessionStore();
// Server state lives in the stores; these are read-through projections.
const contracts = computed(() => {
  const raw = providersStore.contracts;
  return raw ? normalizeProviderContractsResponse(raw) : null;
});
const catalog = computed(() => providersStore.catalog);
const accounts = computed(() => accountsStore.accounts);
const dynamicProviders = computed(() => {
  const enabled = new Set(accounts.value.filter(account => account.enabled).map(account => account.provider_id));
  return (catalog.value ?? []).flatMap(entry => {
    if (!isDynamicCatalogEntry(entry) || !enabled.has(entry.provider_id)) return [];
    const definition = providersStore.definitions.get(entry.provider_id);
    return definition ? [definition] : [];
  });
});
const cpaModels = computed(() => providersStore.cpaModels ?? []);
const loading = ref(false);
const search = ref("");
const loadError = ref("");
const catalogLoadError = ref("");
const capabilitiesLoadError = ref("");
const accountsLoadError = ref("");
const dynamicLoadError = ref("");
const destinationsLoadError = ref("");
const cpaLoadError = ref("");
const identitiesLoadError = ref("");
// Alias publication lives in the providers store; these are read-through views.
const unpublished = computed(() => providersStore.aliasUnpublished);
const publicationReady = computed(() => providersStore.aliasPublicationReady);
const publicationLoadError = computed(() => providersStore.aliasPublicationLoadError);
const publicationSaveError = computed(() => providersStore.aliasPublicationSaveError);
const router = useRouter();
let activatedOnce = false;
let aliasesLoadedAt = 0;
let loadGeneration = 0;
let capabilitiesGeneration = 0;
// Activation refreshes skip data loaded recently; user actions call
// loadAliases directly and stay immediate.
const ACTIVATED_REFRESH_FRESHNESS_MS = 30_000;

const initialLoading = computed(() => loading.value && !contracts.value);
const aliasRows = computed(() => (
  contracts.value
    ? mergeProviderAliasRows(
      flattenProviderScopes(contracts.value, catalog.value),
      accounts.value,
      dynamicProviders.value,
      cpaModels.value,
      destinationsStore.destinations,
    )
    : []
));
// Overlap is a whole-table fact, so it is resolved once per row set instead of
// rescanning every row for each rendered group.
const overlapFlags = computed(() => aliasOverlapFlags(aliasRows.value));
const routingRanks = computed(() => {
  const ranks = new Map<string, number[]>();
  const index = aliasRoutingRankIndex(accounts.value, identitiesStore.identities);
  for (const row of aliasRows.value) {
    ranks.set(row.key, aliasRoutingRanksFromIndex(row, index));
  }
  return ranks;
});
const platformLabels = computed(() => {
  const labels = new Map<string, string>();
  for (const row of aliasRows.value) {
    const label = aliasRowPlatformLabel(row, identitiesStore.identities);
    if (label) labels.set(row.key, label);
  }
  return labels;
});
const aliasGroups = computed(() => {
  const groups = new Map<string, typeof aliasRows.value>();
  const query = search.value.trim().toLocaleLowerCase();
  for (const row of aliasRows.value) {
    if (query && ![row.public_model, row.upstream_model, row.provider_plan, row.custom_account ?? ""].some((value) => value.toLocaleLowerCase().includes(query))) continue;
    const key = row.public_model.toLocaleLowerCase();
    const existing = groups.get(key);
    if (existing) existing.push(row);
    else groups.set(key, [row]);
  }
  return [...groups.values()]
    .map((rows) => ({
      public_model: rows[0]?.public_model ?? "",
      published: isPublicModelPublished(rows[0]?.public_model ?? "", unpublished.value),
      rows: sortAliasRowsByRouting(rows, (row) => routingRanks.value.get(row.key) ?? []),
    }))
    .sort((left, right) => left.public_model.localeCompare(right.public_model));
});

function rankText(row: ProviderAliasRow): string {
  const ranks = routingRanks.value.get(row.key) ?? [];
  return ranks.length > 0 ? ranks.join(" · ") : "—";
}

function groupHasOverlap(rows: readonly ProviderAliasRow[]): boolean {
  return rows.some((row) => overlapFlags.value.has(row.key));
}

// Effective capabilities are read-through projections of the destinations
// store's per-destination metadata snapshots; the view never copies them.
const capabilities = computed(() => {
  const views = new Map<string, AliasCapabilityView>();
  for (const row of aliasRows.value) {
    views.set(row.key, aliasCapabilityView(
      row,
      destinationsStore.destinations,
      destinationsStore.modelMetadata,
      destinationsStore.modelMetadataErrors,
    ));
  }
  return views;
});
const PENDING_CAPABILITY: AliasCapabilityView = {
  state: "pending",
  destination_id: null,
  source: null,
  input_modalities: [],
  output_modalities: [],
};

function capabilityFor(row: ProviderAliasRow): AliasCapabilityView {
  return capabilities.value.get(row.key) ?? PENDING_CAPABILITY;
}

function modalityLabel(modality: string): string {
  const key = ALIAS_MODALITY_KEYS[modality];
  return key ? t(key) : modality;
}

function sourceLabel(source: string | null): string {
  if (!source) return "";
  const key = ALIAS_CAPABILITY_SOURCE_KEYS[source];
  return key ? t(key) : source;
}

/** One aggregate read covers every destination behind the visible rows. */
async function loadAliasCapabilities(): Promise<void> {
  const generation = ++capabilitiesGeneration;
  try {
    await destinationsStore.loadAllModelMetadata();
    if (generation !== capabilitiesGeneration) return;
    capabilitiesLoadError.value = "";
  } catch (error) {
    if (generation !== capabilitiesGeneration) return;
    capabilitiesLoadError.value = dashboardErrorDetail(error);
  }
}

/** Declare deep-link: the provider models tab with the capabilities editor open. */
function aliasCapabilityTarget(row: ProviderAliasRow): RouteLocationRaw | null {
  if (row.custom_account_id || !row.provider_id || row.provider_id === CPA_PROVIDER_ID) return null;
  return appViewRoute("providers", {
    provider: row.provider_id,
    tab: "models",
    model: row.public_model,
    capabilities: row.public_model,
  });
}

function openCapabilityTarget(row: ProviderAliasRow): void {
  const target = aliasCapabilityTarget(row);
  if (target) void router.push(target);
}

// The store owns the write: per-row duplicate guard, optimistic overlay,
// and overlay-scoped failure reconciliation.
function setPublished(publicModel: string, published: boolean): void {
  void providersStore.setAliasPublished(publicModel, published);
}

function publicationSaving(publicModel: string): boolean {
  return providersStore.aliasPublicationPending.includes(publicModelPublicationKey(publicModel));
}

/** Reach the exact relevant account (Custom API rows) or provider models tab. */
function aliasRowTarget(row: ProviderAliasRow): RouteLocationRaw | null {
  if (row.custom_account_id) {
    return appViewRoute("accounts", undefined, { account_id: row.custom_account_id });
  }
  if (row.provider_id && row.provider_id !== CPA_PROVIDER_ID) {
    return appViewRoute("providers", { provider: row.provider_id, tab: "models", model: row.public_model });
  }
  return null;
}

function openAliasRowTarget(row: ProviderAliasRow): void {
  const target = aliasRowTarget(row);
  if (target) void router.push(target);
}

async function loadAliases(options: { retain?: boolean } = {}): Promise<void> {
  if (loading.value) return;
  const generation = ++loadGeneration;
  loading.value = true;
  if (!options.retain) {
    loadError.value = "";
    catalogLoadError.value = "";
    capabilitiesLoadError.value = "";
    dynamicLoadError.value = "";
    destinationsLoadError.value = "";
    cpaLoadError.value = "";
    identitiesLoadError.value = "";
  }
  try {
    // Every slot is named: the publication read (store-owned error) must
    // never be mistaken for the destination catalog read, and each read
    // propagates its own failure independently.
    const [
      contractsResult,
      catalogResult,
      accountsResult,
      cpaResult,
      identitiesResult,
      publicationResult,
      destinationsResult,
    ] = await Promise.allSettled([
      providersStore.loadContracts(),
      providersStore.loadCatalog(),
      accountsStore.loadPresented(),
      providersStore.loadCpaModels(),
      identitiesStore.loadPresented(),
      providersStore.loadAliasPublication(),
      // Dynamic Provider rows project through the destination catalog; without
      // it their enablement is unknown and the rows stay hidden.
      destinationsStore.load(),
    ]);
    if (generation !== loadGeneration) return;
    // Alias publication failures surface through the store-owned load error.
    void publicationResult;
    if (identitiesResult.status === "fulfilled") {
      identitiesLoadError.value = "";
    } else {
      identitiesLoadError.value = dashboardErrorDetail(identitiesResult.reason);
    }
    if (cpaResult.status === "fulfilled") {
      cpaLoadError.value = "";
    } else {
      cpaLoadError.value = dashboardErrorDetail(cpaResult.reason);
    }
    if (catalogResult.status === "fulfilled") {
      catalogLoadError.value = "";
      const enabledProviderIds = new Set(
        (accountsResult.status === "fulfilled" ? accountsResult.value : accounts.value)
          .filter((account) => account.enabled)
          .map((account) => account.provider_id),
      );
      const entries = catalogResult.value.filter((entry) => (
        isDynamicCatalogEntry(entry) && enabledProviderIds.has(entry.provider_id)
      ));
      if (entries.length === 0) {
        dynamicLoadError.value = "";
        destinationsLoadError.value = "";
      } else {
        // Destination catalog enablement decides dynamic row routability, so
        // its failure hides those rows and is reported apart from definition
        // failures.
        destinationsLoadError.value = destinationsResult.status === "rejected"
          ? dashboardErrorDetail(destinationsResult.reason)
          : "";
        const details = await Promise.allSettled(
          entries.map((entry) => providersStore.loadDefinition(entry.provider_id)),
        );
        if (generation !== loadGeneration) return;
        const failure = details.find((result) => result.status === "rejected");
        dynamicLoadError.value = failure?.status === "rejected"
          ? dashboardErrorDetail(failure.reason)
          : "";
      }
    } else {
      catalogLoadError.value = dashboardErrorDetail(catalogResult.reason);
    }
    if (accountsResult.status === "fulfilled") {
      accountsLoadError.value = "";
    } else {
      accountsLoadError.value = dashboardErrorDetail(accountsResult.reason);
    }
    if (contractsResult.status === "fulfilled") {
      loadError.value = "";
      aliasesLoadedAt = Date.now();
    } else {
      loadError.value = dashboardErrorDetail(contractsResult.reason);
    }
    // Capability tags fan out per destination behind the visible rows; they
    // render as pending chips instead of holding the table hostage.
    void loadAliasCapabilities();
  } finally {
    if (generation === loadGeneration) loading.value = false;
  }
}

watch(() => sessionStore.authenticated, ok => {
  if (ok) return;
  loadGeneration += 1;
  capabilitiesGeneration += 1;
  aliasesLoadedAt = 0;
  loading.value = false;
  loadError.value = catalogLoadError.value = accountsLoadError.value = dynamicLoadError.value = destinationsLoadError.value = cpaLoadError.value = identitiesLoadError.value = "";
  capabilitiesLoadError.value = "";
}, { flush: "sync" });
onUnmounted(() => { loadGeneration += 1; capabilitiesGeneration += 1; });
onMounted(() => void loadAliases());
onActivated(() => {
  if (activatedOnce) {
    if (Date.now() - aliasesLoadedAt >= ACTIVATED_REFRESH_FRESHNESS_MS) void loadAliases({ retain: true });
  } else {
    activatedOnce = true;
  }
});
</script>

<style scoped>
.aliases-page {
  min-width: 0;
  max-width: 1440px;
  margin: 0 auto;
  overflow-x: hidden;
}
.aliases-state {
  min-height: 160px;
  display: grid;
  place-items: center;
}
.aliases-section {
  min-width: 0;
  padding: var(--ocg-space-lg);
  border: 1px solid var(--ocg-border);
  border-radius: var(--ocg-radius-lg);
  background: var(--ocg-surface);
  box-shadow: var(--ocg-shadow-sm);
}
.aliases-section > .n-alert {
  margin-bottom: var(--ocg-space-md);
}
.aliases-table-wrap {
  overflow-x: auto;
}
.aliases-search { margin-bottom: var(--ocg-space-lg); }
.aliases-name-row {
  display: flex;
  align-items: center;
  gap: var(--ocg-space-sm);
}
.aliases-unpublished {
  opacity: 0.55;
}
.alias-warning { color: var(--ocg-warning); margin: var(--ocg-space-xs) 0 0; }
.alias-platform-tag {
  margin-left: var(--ocg-space-xs);
  color: var(--ocg-muted);
}
.aliases-table {
  width: 100%;
  min-width: 520px;
  border-collapse: collapse;
  font-size: var(--ocg-font-sm);
}
.aliases-table th,
.aliases-table td {
  padding: 10px var(--ocg-space-md);
  border-bottom: 1px solid var(--ocg-border);
  text-align: left;
  vertical-align: middle;
}
.aliases-table th {
  color: var(--ocg-muted);
  font-size: var(--ocg-font-xs);
  font-weight: 600;
}
.aliases-table .aliases-name {
  vertical-align: top;
}
.aliases-table .aliases-rank {
  white-space: nowrap;
  color: var(--ocg-muted);
}
.aliases-capability {
  white-space: nowrap;
}
.aliases-capability .n-tag {
  margin-right: var(--ocg-space-xs);
}
.aliases-capability-source,
.aliases-capability-none {
  color: var(--ocg-muted);
}
@media (max-width: 720px) {
  .aliases-table th:first-child,
  .aliases-name {
    position: sticky;
    left: 0;
    z-index: 1;
    background: var(--ocg-surface);
    max-width: 140px;
    overflow-wrap: anywhere;
    box-shadow: 1px 0 var(--ocg-border);
  }
}
</style>
