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
      v-else-if="loadError && !pageStore.page"
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
        v-if="loadError && pageStore.page"
        type="warning"
        :title="t('加载供应商失败：{error}', { error: loadError })"
      >
        <n-button size="small" secondary :loading="loading" @click="loadAliases({ retain: true })">
          {{ t("重试") }}
        </n-button>
      </n-alert>
      <n-alert v-for="issue in pageStore.page?.errors ?? []" :key="`${issue.resource}:${issue.id}`" type="warning"
        :title="t(aliasPageIssueKey(issue.resource), { error: issue.code })" />
      <n-alert v-for="(failure, key) in pageStore.publicationErrors" :key="key" type="warning"
        :title="t('更新对外展示失败：{error}', { error: failure })" />
      <n-spin v-if="loading" size="small" />
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
          <tbody v-for="group in aliasGroups" :key="group.publicModel">
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
                    :disabled="publicationSaving(group.publicModel)"
                    :loading="publicationSaving(group.publicModel)"
                    :aria-label="t('对下游展示此模型')"
                    :title="t('关闭后下游不再列出此模型，仍可用该名称调用。')"
                    @update:value="(published) => setPublished(group.publicModel, published)"
                  />
                  <code>{{ group.publicModel }}</code>
                  <span v-if="group.continued" class="aliases-capability-none">{{ t('本页 {shown} / {total} 条映射', { shown: group.rows.length, total: group.matchingRows }) }}</span>
                </div>
                <p v-if="group.hasOverlap" class="alias-warning">{{ t('名称与其他上游 ID 重叠，请检查调用名称。') }}</p>
              </td>
              <td>
                {{ row.providerPlan }}
                <n-tag v-if="row.platformLabel" size="tiny" :bordered="false" class="alias-platform-tag">
                  {{ row.platformLabel }}
                </n-tag>
              </td>
              <td class="aliases-rank">{{ rankText(row) }}</td>
              <td><code>{{ row.upstreamModel }}</code></td>
              <td class="aliases-capability">
                <template v-if="row.capability.state === 'ready'">
                  <n-tag
                    v-for="modality in row.capability.inputModalities"
                    :key="modality"
                    size="tiny"
                    :bordered="false"
                  >
                    {{ modalityLabel(modality) }}
                  </n-tag>
                  <n-tag size="tiny" :bordered="false" class="aliases-capability-source">
                    {{ sourceLabel(row.capability.source) }}
                  </n-tag>
                </template>
                <template v-else-if="row.capability.state === 'unknown'">
                  <n-tag size="tiny" type="warning" :bordered="false">{{ t("未知") }}</n-tag>
                  <n-button
                    v-if="aliasPageTarget(row.capabilityTarget)"
                    text
                    size="tiny"
                    type="primary"
                    :aria-label="`${t('去声明')} ${row.publicModel}`"
                    @click="openCapabilityTarget(row)"
                  >
                    {{ t("去声明") }}
                  </n-button>
                </template>
                <n-tag
                  v-else-if="row.capability.state === 'error'"
                  size="tiny"
                  type="error"
                  :bordered="false"
                >
                  {{ t("加载失败") }}
                </n-tag>
                <span v-else-if="row.capability.state === 'unavailable'" class="aliases-capability-none">—</span>
                <span v-else class="aliases-capability-none">{{ t("加载中…") }}</span>
              </td>
              <td class="aliases-action">
                <n-button
                  v-if="aliasPageTarget(row.target)"
                  circle
                  quaternary
                  size="small"
                  :aria-label="row.customAccountId ? t('打开相关账号') : t('打开相关供应商模型')"
                  :title="row.customAccountId ? t('打开相关账号') : t('打开相关供应商模型')"
                  @click="openAliasRowTarget(row)"
                >
                  <template #icon><n-icon :component="LinkOutlined" /></template>
                </n-button>
              </td>
            </tr>
          </tbody>
        </table>
      </div>
      <div class="aliases-pagination">
        <span>{{ t('已显示 {shown} 条映射，共 {total} 条', { shown: pageStore.page?.groups.reduce((count, group) => count + group.rows.length, 0) ?? 0, total: pageStore.page?.filteredRows ?? 0 }) }}</span>
        <n-pagination :page="Math.floor(offset / PAGE_SIZE) + 1" :page-size="PAGE_SIZE"
          :item-count="pageStore.page?.filteredRows ?? 0" :disabled="loading"
          @update:page="goToPage">
          <template #prev>
            <n-button size="small" quaternary :disabled="loading || offset === 0">{{ t('上一页') }}</n-button>
          </template>
          <template #next>
            <n-button size="small" quaternary :disabled="loading || !pageStore.page?.hasMore">{{ t('下一页') }}</n-button>
          </template>
          <template #label="{ type, node, active }">
            <n-button v-if="type === 'page'" size="small" quaternary :disabled="loading" :aria-current="active ? 'page' : undefined">{{ node }}</n-button>
            <component :is="() => node" v-else />
          </template>
        </n-pagination>
      </div>
    </section>
  </div>
</template>

<script setup lang="ts">
import { computed, onActivated, onMounted, onUnmounted, ref, watch } from "vue";
import { useRouter, useRoute } from "vue-router";
import { NAlert, NButton, NEmpty, NIcon, NInput, NPagination, NSpin, NSwitch, NTag } from "naive-ui";
import { LinkOutlined } from "@vicons/antd";
import { ALIAS_CAPABILITY_SOURCE_KEYS, ALIAS_MODALITY_KEYS } from "../domain/alias-capabilities.ts";
import { aliasPageTarget, aliasPageIssueKey, aliasPagePublicationKey, aliasPageRankText, type AliasPageRow } from "../domain/alias-page.ts";
import { useAliasPageStore } from "../stores/aliasPage.ts";
import { PAGE_READ_MAX_AGE_MS } from "../stores/readLifecycle.ts";
import { t } from "../i18n/index.ts";

const pageStore = useAliasPageStore();
const router = useRouter();
const route = useRoute();
const search = ref("");
const offset = ref(0);
const PAGE_SIZE = 50;
const loading = computed(() => pageStore.loading);
const loadError = computed(() => pageStore.error);
const initialLoading = computed(() => loading.value && !pageStore.page);
const aliasGroups = computed(() => pageStore.groups);
let activeOnce = false;
let searchTimer: ReturnType<typeof setTimeout> | undefined;

function rankText(row: AliasPageRow): string { return aliasPageRankText(row); }
function modalityLabel(modality: string): string {
  const key = ALIAS_MODALITY_KEYS[modality];
  return key ? t(key) : modality;
}
function sourceLabel(source: string | null): string {
  const key = source ? ALIAS_CAPABILITY_SOURCE_KEYS[source] : null;
  return key ? t(key) : source ?? "";
}
function openCapabilityTarget(row: AliasPageRow): void {
  const target = aliasPageTarget(row.capabilityTarget);
  if (target) void router.push(target);
}
function openAliasRowTarget(row: AliasPageRow): void {
  const target = aliasPageTarget(row.target);
  if (target) void router.push(target);
}
function publicationSaving(name: string): boolean { return pageStore.pending.includes(aliasPagePublicationKey(name)); }
function setPublished(name: string, published: boolean): void {
  void pageStore.setPublished(name, published);
}
async function loadAliases(options: { retain?: boolean; maxAgeMs?: number } = {}): Promise<void> {
  try { await pageStore.load({ search: search.value.trim(), offset: offset.value, limit: PAGE_SIZE }, options); }
  catch { /* The store retains the last successful page and exposes the failure. */ }
}
function goToPage(page: number): void { offset.value = (page - 1) * PAGE_SIZE; void loadAliases(); }
watch(search, () => {
  offset.value = 0;
  pageStore.invalidate();
  clearTimeout(searchTimer);
  searchTimer = setTimeout(() => void loadAliases(), 180);
});
function onForeground(): void { if (route.name === "aliases") void loadAliases(); }
onMounted(() => { void loadAliases(); window.addEventListener("focus", onForeground); });
onActivated(() => {
  if (activeOnce) void loadAliases({ maxAgeMs: PAGE_READ_MAX_AGE_MS });
  activeOnce = true;
});
onUnmounted(() => { window.removeEventListener("focus", onForeground); clearTimeout(searchTimer); });
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
.aliases-pagination { display: flex; flex-wrap: wrap; gap: var(--ocg-space-md); align-items: center; justify-content: space-between; margin-top: var(--ocg-space-lg); color: var(--ocg-muted); }
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
