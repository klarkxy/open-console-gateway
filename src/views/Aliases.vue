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
      :title="t('加载供应商失败: {error}', { error: loadError })"
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
        :title="t('加载供应商失败: {error}', { error: loadError })"
      >
        <n-button size="small" secondary :loading="loading" @click="loadAliases({ retain: true })">
          {{ t("重试") }}
        </n-button>
      </n-alert>
      <n-alert
        v-if="accountsLoadError"
        type="warning"
        :title="t('加载 Custom Alias 账号失败: {error}', { error: accountsLoadError })"
      >
        <n-button size="small" secondary :loading="loading" @click="loadAliases({ retain: true })">
          {{ t("重试") }}
        </n-button>
      </n-alert>
      <n-alert
        v-if="dynamicLoadError"
        type="warning"
        :title="t('加载供应商失败: {error}', { error: dynamicLoadError })"
      >
        <n-button size="small" secondary :loading="loading" @click="loadAliases({ retain: true })">
          {{ t("重试") }}
        </n-button>
      </n-alert>
      <n-alert
        v-if="cpaLoadError"
        type="warning"
        :title="t('加载 CPA 模型目录失败: {error}', { error: cpaLoadError })"
      >
        <n-button size="small" secondary :loading="loading" @click="loadAliases({ retain: true })">
          {{ t("重试") }}
        </n-button>
      </n-alert>

      <n-alert
        v-if="publicationLoadError"
        type="warning"
        :title="t('加载对外展示失败: {error}', { error: publicationLoadError })"
      >
        <n-button size="small" secondary :loading="loading" @click="loadAliases({ retain: true })">
          {{ t("重试") }}
        </n-button>
      </n-alert>
      <n-alert
        v-if="publicationSaveError"
        type="warning"
        :title="t('更新对外展示失败: {error}', { error: publicationSaveError })"
      />
      <n-empty v-if="aliasGroups.length === 0" :description="search.trim() ? t('无匹配模型') : t('暂无 Alias')" />
      <div v-else class="aliases-table-wrap" tabindex="0" role="region" :aria-label="t('模型映射')">
        <table class="aliases-table">
          <thead>
            <tr>
              <th>{{ t("对外模型名") }}</th>
              <th>{{ t("供应商 / 方案") }}</th>
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
              <td>{{ row.provider_plan }}</td>
              <td><code>{{ row.upstream_model }}</code></td>
            </tr>
          </tbody>
        </table>
      </div>
    </section>
  </div>
</template>

<script setup lang="ts">
import { computed, onActivated, onMounted, ref } from "vue";
import { NAlert, NButton, NEmpty, NInput, NSpin, NSwitch, NTooltip } from "naive-ui";
import type { Account } from "../api/dashboard.ts";
import type {
  ProviderDefinitionView,
  ProviderCatalogEntry,
  ProviderContractsResponse,
} from "../api/providers.ts";
import { dashboardV4 } from "../api/dashboard-v4.ts";
import type { CpaCatalogEntry } from "../api/generated/dashboard-v4.ts";
import { providerApi } from "../api/providers.ts";
import { isDynamicCatalogEntry } from "../domain/dynamic-provider.ts";
import { flattenProviderScopes, normalizeProviderContractsResponse } from "../domain/provider-contracts.ts";
import { isRevisionConflict } from "../api/dashboard.ts";
import {
  aliasNameOverlaps,
  isPublicModelPublished,
  mergeProviderAliasRows,
  publicModelPublicationKey,
  type ProviderAliasRow,
} from "../domain/provider-aliases.ts";
import { t } from "../i18n/index.ts";
import { useAccountsStore } from "../stores/accounts.ts";
import { useControlPlaneStore } from "../stores/controlPlane.ts";
import { useProvidersStore } from "../stores/providers.ts";
import { dashboardErrorDetail } from "../utils/errors.ts";

const accountsStore = useAccountsStore();
const controlPlane = useControlPlaneStore();
const providersStore = useProvidersStore();
const contracts = ref<ProviderContractsResponse | null>(null);
const catalog = ref<ProviderCatalogEntry[] | null>(null);
const accounts = ref<Account[]>([]);
const dynamicProviders = ref<ProviderDefinitionView[]>([]);
const cpaModels = ref<CpaCatalogEntry[]>([]);
const loading = ref(false);
const search = ref("");
const loadError = ref("");
const accountsLoadError = ref("");
const dynamicLoadError = ref("");
const cpaLoadError = ref("");
const unpublished = ref<string[]>([]);
const publicationReady = ref(false);
const publicationLoadError = ref("");
const publicationSaveError = ref("");
const saving = ref<Record<string, boolean>>({});
let activatedOnce = false;

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
      rows,
    }))
    .sort((left, right) => left.public_model.localeCompare(right.public_model));
});

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
    publicationLoadError.value = "";
  }
  try {
    const [contractsResult, catalogResult, accountsResult, cpaResult, publicationResult] = await Promise.allSettled([
      providersStore.loadContracts(),
      providersStore.loadCatalog(),
      accountsStore.loadPresented(),
      dashboardV4.getCpaCatalog(),
      dashboardV4.getAliasPublication(),
    ]);
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
      catalog.value = catalogResult.value;
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
          entries.map((entry) => providerApi.getProviderDefinition(entry.provider_id)),
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
      accounts.value = accountsResult.value;
      accountsLoadError.value = "";
    } else {
      accountsLoadError.value = dashboardErrorDetail(accountsResult.reason);
    }
    if (contractsResult.status === "fulfilled") {
      contracts.value = normalizeProviderContractsResponse(contractsResult.value);
      loadError.value = "";
    } else {
      loadError.value = dashboardErrorDetail(contractsResult.reason);
    }
  } finally {
    loading.value = false;
  }
}

onMounted(() => void loadAliases());
onActivated(() => {
  if (activatedOnce) void loadAliases({ retain: true });
  else activatedOnce = true;
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
  padding: 16px;
  border: 1px solid var(--ocg-border);
  border-radius: 14px;
  background: var(--ocg-surface);
  box-shadow: var(--ocg-shadow-sm);
}
.aliases-section > .n-alert {
  margin-bottom: 12px;
}
.aliases-table-wrap {
  overflow-x: auto;
}
.aliases-search { margin-bottom: 16px; }
.aliases-name-row {
  display: flex;
  align-items: center;
  gap: 8px;
}
.aliases-unpublished {
  opacity: 0.55;
}
.alias-warning { color: var(--ocg-warning); margin: 4px 0 0; }
.aliases-table {
  width: 100%;
  min-width: 520px;
  border-collapse: collapse;
  font-size: var(--ocg-font-sm);
}
.aliases-table th,
.aliases-table td {
  padding: 10px 12px;
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
