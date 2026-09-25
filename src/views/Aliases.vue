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
      <n-empty v-if="aliasGroups.length === 0" :description="search.trim() ? t('无匹配模型') : t('暂无 Alias')" />
      <div v-else class="aliases-table-wrap" tabindex="0" role="region" :aria-label="t('模型映射')">
        <table class="aliases-table">
          <thead>
            <tr>
              <th>{{ t("对外模型名") }}</th>
              <th>{{ t("供应商 / 方案") }}</th>
              <th>{{ t("路由顺位") }}</th>
              <th>{{ t("上游模型 ID") }}</th>
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
                  <n-tooltip trigger="hover">
                    <template #trigger>
                      <n-switch
                        size="small"
                        :value="group.published"
                        :disabled="!publicationReady || Boolean(saving[group.public_model])"
                        :loading="Boolean(saving[group.public_model])"
                        :aria-label="t('对下游展示此模型')"
                        @update:value="(published) => setPublished(group.public_model, published)"
                      />
                    </template>
                    {{ t("关闭后下游不再列出此模型，仍可用该名称调用。") }}
                  </n-tooltip>
                  <code>{{ group.public_model }}</code>
                </div>
                <p v-if="groupHasOverlap(group.rows)" class="alias-warning">{{ t('名称与其他上游 ID 重叠，请检查调用名称。') }}</p>
              </td>
              <td>
                {{ row.provider_plan }}
                <n-tag v-if="platformLabels.get(row.key)" size="tiny" :bordered="false" class="alias-platform-tag">
                  {{ platformLabels.get(row.key) }}
                </n-tag>
                <n-tag v-if="!row.routable" size="tiny" :bordered="false" class="alias-model-disabled">
                  {{ t("模型未启用") }}
                </n-tag>
              </td>
              <td class="aliases-rank">{{ rankText(row) }}</td>
              <td><code>{{ row.upstream_model }}</code></td>
            </tr>
          </tbody>
        </table>
      </div>
    </section>
  </div>
</template>

<script setup lang="ts">
import { computed, onActivated, onMounted, ref, watch } from "vue";
import { NAlert, NButton, NEmpty, NInput, NSpin, NSwitch, NTag, NTooltip } from "naive-ui";
import type { ProviderDefinitionView } from "../api/providers.ts";
import { dashboardV4 } from "../api/dashboard-v4.ts";
import type { CpaCatalogEntry } from "../api/generated/dashboard-v4.ts";
import { isDynamicCatalogEntry } from "../domain/dynamic-provider.ts";
import { flattenProviderScopes, normalizeProviderContractsResponse } from "../domain/provider-contracts.ts";
import { isRevisionConflict } from "../api/dashboard.ts";
import {
  aliasNameOverlaps,
  aliasRowPlatformLabel,
  aliasRowRoutingRanks,
  isPublicModelPublished,
  mergeProviderAliasRows,
  publicModelPublicationKey,
  sortAliasRowsByRouting,
  type ProviderAliasRow,
} from "../domain/provider-aliases.ts";
import { t } from "../i18n/index.ts";
import { useAccountsStore } from "../stores/accounts.ts";
import { useControlPlaneStore } from "../stores/controlPlane.ts";
import { useIdentitiesStore } from "../stores/identities.ts";
import { useProvidersStore } from "../stores/providers.ts";
import { useSessionStore } from "../stores/session.ts";
import { dashboardErrorDetail } from "../utils/errors.ts";

const accountsStore = useAccountsStore();
const controlPlane = useControlPlaneStore();
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
const dynamicProviders = ref<ProviderDefinitionView[]>([]);
const cpaModels = ref<CpaCatalogEntry[]>([]);
const loading = ref(false);
const search = ref("");
const loadError = ref("");
const accountsLoadError = ref("");
const dynamicLoadError = ref("");
const cpaLoadError = ref("");
const identitiesLoadError = ref("");
const unpublished = ref<string[]>([]);
const publicationReady = ref(false);
const publicationLoadError = ref("");
const publicationSaveError = ref("");
const saving = ref<Record<string, boolean>>({});
let activatedOnce = false;
let aliasesLoadedAt = 0;
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
    )
    : []
));
const routingRanks = computed(() => {
  const ranks = new Map<string, number[]>();
  for (const row of aliasRows.value) {
    ranks.set(row.key, aliasRowRoutingRanks(row, accounts.value, identitiesStore.identities));
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
  return rows.some((row) => aliasNameOverlaps(row, aliasRows.value));
}

async function setPublished(publicModel: string, published: boolean): Promise<void> {
  const key = publicModelPublicationKey(publicModel);
  const previous = unpublished.value;
  unpublished.value = published
    ? previous.filter((name) => publicModelPublicationKey(name) !== key)
    : previous.some((name) => publicModelPublicationKey(name) === key)
      ? previous
      : [...previous, key];
  saving.value = { ...saving.value, [publicModel]: true };
  try {
    if (!controlPlane.hasTokens()) await controlPlane.refresh();
    const result = await controlPlane.runMutation((expectation) => (
      dashboardV4.patchAliasPublication({ publicModel, published }, expectation)
    ));
    unpublished.value = result.unpublished;
    publicationSaveError.value = "";
  } catch (error) {
    unpublished.value = previous;
    if (isRevisionConflict(error)) {
      try {
        const snapshot = await dashboardV4.getAliasPublication();
        unpublished.value = snapshot.unpublished;
        publicationReady.value = true;
      } catch {
        // Keep the reverted optimistic state when reload also fails.
      }
    }
    publicationSaveError.value = dashboardErrorDetail(error);
  } finally {
    const next = { ...saving.value };
    delete next[publicModel];
    saving.value = next;
  }
}

async function loadAliases(options: { retain?: boolean } = {}): Promise<void> {
  if (loading.value) return;
  loading.value = true;
  if (!options.retain) {
    loadError.value = "";
    dynamicLoadError.value = "";
    cpaLoadError.value = "";
    identitiesLoadError.value = "";
    publicationLoadError.value = "";
  }
  try {
    const [contractsResult, catalogResult, accountsResult, cpaResult, publicationResult, identitiesResult] = await Promise.allSettled([
      providersStore.loadContracts(),
      providersStore.loadCatalog(),
      accountsStore.loadPresented(),
      dashboardV4.getCpaCatalog(),
      dashboardV4.getAliasPublication(),
      identitiesStore.loadPresented(),
    ]);
    if (identitiesResult.status === "fulfilled") {
      identitiesLoadError.value = "";
    } else {
      identitiesLoadError.value = dashboardErrorDetail(identitiesResult.reason);
    }
    if (publicationResult.status === "fulfilled") {
      unpublished.value = publicationResult.value.unpublished;
      publicationReady.value = true;
      publicationLoadError.value = "";
    } else {
      publicationLoadError.value = dashboardErrorDetail(publicationResult.reason);
      if (!options.retain) publicationReady.value = false;
    }
    if (cpaResult.status === "fulfilled") {
      cpaModels.value = cpaResult.value.models;
      cpaLoadError.value = "";
    } else {
      cpaLoadError.value = dashboardErrorDetail(cpaResult.reason);
    }
    if (catalogResult.status === "fulfilled") {
      const enabledProviderIds = new Set(
        (accountsResult.status === "fulfilled" ? accountsResult.value : accounts.value)
          .filter((account) => account.enabled)
          .map((account) => account.provider_id),
      );
      const entries = catalogResult.value.filter((entry) => (
        isDynamicCatalogEntry(entry) && enabledProviderIds.has(entry.provider_id)
      ));
      if (entries.length === 0) {
        dynamicProviders.value = [];
        dynamicLoadError.value = "";
      } else {
        const details = await Promise.allSettled(
          entries.map((entry) => providersStore.loadDefinition(entry.provider_id)),
        );
        const previous = new Map(dynamicProviders.value.map((provider) => [provider.id, provider]));
        const next: ProviderDefinitionView[] = [];
        const failures: string[] = [];
        details.forEach((result, index) => {
          if (result.status === "fulfilled") {
            next.push(result.value);
            return;
          }
          failures.push(dashboardErrorDetail(result.reason));
          if (options.retain) {
            const kept = previous.get(entries[index]?.provider_id ?? "");
            if (kept) next.push(kept);
          }
        });
        dynamicProviders.value = next;
        dynamicLoadError.value = failures[0] ?? "";
      }
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
  } finally {
    loading.value = false;
  }
}

watch(() => sessionStore.authenticated, (ok) => { if (!ok) aliasesLoadedAt = 0; });
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
.alias-model-disabled {
  margin-left: var(--ocg-space-xs);
  color: var(--ocg-muted);
  background-color: var(--ocg-primary-soft);
}
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
