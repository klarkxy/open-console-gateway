<template>
  <div class="dashboard">
    <section class="connection-hero" aria-labelledby="connection-title">
      <div class="connection-content">
        <div class="connection-head">
          <h2 id="connection-title">{{ t("接入中心") }}</h2>
          <span class="ready-mark" :class="{ 'not-ready': summaryLoaded && !summary.gatewayRunning, pending: !summaryLoaded }" role="status">
            <span aria-hidden="true" />
            {{ !summaryLoaded ? t("加载中…") : summary.gatewayRunning ? t("就绪") : t("服务未就绪") }}
          </span>
        </div>
        <div class="connection-rows">
          <div class="connection-row">
            <n-icon size="18" aria-hidden="true"><ApiOutlined /></n-icon>
            <div class="connection-value">
              <span class="connection-label">{{ t("API 地址") }}</span>
              <code>{{ serviceApiUrl }}</code>
            </div>
            <OcgTooltip :delay="200">
              <template #trigger>
                <n-button circle quaternary size="small" :aria-label="t('复制 API Base URL')" @click="copyConnection('api', serviceApiUrl, t('API 地址'))">
                  <template #icon><n-icon :component="copiedTarget === 'api' ? CheckOutlined : CopyOutlined" /></template>
                </n-button>
              </template>
              {{ t("复制 API Base URL") }}
            </OcgTooltip>
          </div>
          <div class="connection-row">
            <n-icon size="18" aria-hidden="true"><KeyOutlined /></n-icon>
            <div class="connection-value">
              <div class="connection-key-label">
                <span class="connection-label">{{ t("Key") }}</span>
                <OcgPopover v-if="enabledGatewayKeys.length > 1" v-model:open="keyMenuOpen">
                  <template #trigger>
                    <button
                      type="button"
                      class="inline-flex max-w-[min(220px,70%)] cursor-pointer items-center gap-1 rounded-sm border-0 bg-transparent px-1 [font:inherit] text-[length:var(--ocg-font-xs)] leading-[1.6] text-primary transition-[background-color] duration-[var(--ocg-motion-fast)] ease-[var(--ocg-ease)] not-disabled:hover:bg-primary-soft disabled:cursor-default disabled:opacity-60"
                      :disabled="refreshingKey || loading"
                      :aria-label="t('选择 Key')"
                    >
                      <span class="min-w-0 overflow-hidden text-ellipsis whitespace-nowrap">{{ selectedKey?.name }}</span>
                      <n-icon size="12" aria-hidden="true"><DownOutlined /></n-icon>
                    </button>
                  </template>
                  <div class="grid max-h-[min(360px,60vh)] w-[min(320px,calc(100vw-64px))] gap-1 overflow-auto">
                    <button
                      v-for="entry in enabledGatewayKeys"
                      :key="entry.id"
                      type="button"
                      class="grid cursor-pointer grid-cols-[minmax(0,1fr)_auto] items-center gap-x-3 gap-y-0.5 rounded-sm border-0 px-3 py-2 text-left [font:inherit] text-ink transition-[background-color] duration-[var(--ocg-motion-fast)] ease-[var(--ocg-ease)] hover:bg-primary-soft"
                      :class="entry.id === selectedKey?.id ? 'bg-primary-soft' : 'bg-transparent'"
                      :aria-pressed="entry.id === selectedKey?.id"
                      @click="selectGatewayKey(entry.id)"
                    >
                      <span class="col-start-1 flex min-w-0 items-center gap-2"><span class="overflow-hidden text-ellipsis whitespace-nowrap text-[length:var(--ocg-font-sm)]">{{ entry.name }}</span><span v-if="entry.id === PRIMARY_KEY_ID" class="flex-none text-[length:var(--ocg-font-xs)] text-muted">{{ t("主 Key") }}</span></span>
                      <code class="col-start-1 font-mono text-[length:var(--ocg-font-xs)] text-muted">{{ presentConnectionKey(entry.value) }}</code>
                      <n-icon v-if="entry.id === selectedKey?.id" class="col-start-2 row-span-2 row-start-1 text-primary" size="14" aria-hidden="true"><CheckOutlined /></n-icon>
                    </button>
                  </div>
                </OcgPopover>
              </div>
              <code>{{ maskedKey }}</code>
            </div>
            <div class="row-actions">
              <n-popconfirm :positive-text="t('生成新 Key')" :negative-text="t('取消')" @positive-click="regenerateKey">
                <template #trigger>
                  <n-button circle quaternary size="small" :aria-label="t('刷新 Key')" :title="t('刷新 Key')" :loading="refreshingKey" :disabled="refreshingKey || loading || !selectedKey">
                    <template #icon><n-icon :component="ReloadOutlined" /></template>
                  </n-button>
                </template>
                {{ t("仅当前 Key 的旧值立即失效，其他 Key 不受影响。确定生成新值？") }}
              </n-popconfirm>
              <OcgTooltip :delay="200">
                <template #trigger>
                  <n-button circle quaternary size="small" :aria-label="t('复制 Key')" :disabled="refreshingKey || !selectedKey?.value" @click="copyConnection('key', selectedKey?.value ?? '', t('Key'))">
                    <template #icon><n-icon :component="copiedTarget === 'key' ? CheckOutlined : CopyOutlined" /></template>
                  </n-button>
                </template>
                {{ t("复制 Key") }}
              </OcgTooltip>
              <OcgTooltip :delay="200">
                <template #trigger>
                  <n-button circle quaternary size="small" :aria-label="t('管理接入 Key')" @click="goToKeys"><template #icon><n-icon :component="UnorderedListOutlined" /></template></n-button>
                </template>
                {{ t("管理接入 Key") }}
              </OcgTooltip>
            </div>
          </div>
        </div>
        <n-alert v-if="connectionStore.refreshError" type="warning" :title="t('接入 Key 加载失败，请重试')">
          <div class="flex flex-wrap items-center justify-between gap-3">
            <span>{{ t("加载接入 Key 失败：{error}", { error: connectionStore.refreshError }) }}</span>
            <n-button size="small" secondary :loading="connectionReadLoading" @click="reloadConnection">{{ t("重试") }}</n-button>
          </div>
        </n-alert>
        <p v-if="connectionUrls.insecureHttp" class="connection-warning" role="status">{{ t("非本机 HTTP 会明文传输 Key 与请求内容，仅在可信网络中使用。") }}</p>
      </div>
      <img :src="characterImage" alt="" class="hero-character" aria-hidden="true" />
    </section>
    <n-alert v-if="dashboardError" type="error" :title="t('仪表盘数据加载失败')"><n-button size="small" secondary :loading="loading" :disabled="refreshingKey" @click="loadDashboard">{{ t("重试") }}</n-button></n-alert>
    <section class="card attention-card" :aria-label="t('需要关注')" :aria-busy="!accountsLoaded">
      <div class="card-head">
        <div><h3 class="card-title">{{ t("需要关注") }}</h3><span v-if="accountsLoaded && attentionReadComplete && attentionItems.length === 0" class="card-desc">{{ t("所有账号状态正常") }}</span><span v-else-if="accountsLoaded && attentionItems.length > 0" class="card-desc">{{ attentionDesc }}</span></div>
        <n-button v-if="accountsLoaded && attentionItems.length > 0" size="small" @click="goToAccounts">{{ t("去处理") }}</n-button>
      </div>
      <div v-if="!accountsLoaded" class="section-state">{{ loading ? t("加载中…") : t("仪表盘数据加载失败") }}</div>
      <div v-else-if="attentionItems.length > 0" class="attention-list" role="list">
        <div v-for="item in attentionItems" :key="item.accountId" role="listitem">
          <button type="button" class="attention-item" :aria-label="attentionItemAriaLabel(item)" @click="goToAccounts"><span class="attention-name">{{ item.accountName }}</span><n-tag size="small" :type="attentionTagType(item.reason)">{{ attentionLabel(item) }}</n-tag></button>
        </div>
      </div>
    </section>
    <section class="card chart-card">
      <div class="card-head chart-head">
        <div><h3 class="card-title">{{ t("每日 Token 消耗") }}</h3></div>
        <div v-if="tokensLoaded" class="chart-stats" role="group" :aria-label="t('图表摘要')">
          <span>{{ t("模型：{count}", { count: formatNumber(legendModels.length) }) }}</span>
          <span><b>{{ formatTokens(totalChartTokens) }}</b> {{ t("{days} 天合计", { days: chartDays }) }}</span>
          <span><b>{{ formatTokens(dailyAverageTokens) }}</b> {{ t("日均") }}</span>
        </div>
      </div>
      <div v-if="tokensLoaded" class="legend" role="list" :aria-label="t('模型图例')"><span v-for="model in legendModels" :key="model.model" class="legend-item" role="listitem"><span class="legend-dot" :style="{ background: model.color }" aria-hidden="true" />{{ model.model }}</span></div>
      <n-spin :show="loading && !tokensLoaded">
        <div v-if="!tokensLoaded" class="section-state">{{ loading ? t("加载中…") : t("仪表盘数据加载失败") }}</div>
        <n-empty v-else-if="totalChartTokens === 0" :description="t('暂无 Token 消耗数据')" />
        <StackedBarChart v-else :series="chartSeries" :model-totals="modelTotals" :total-tokens="totalChartTokens" :days="chartDays" />
      </n-spin>
    </section>
  </div>
</template>

<script setup lang="ts">
import { computed, onActivated, onDeactivated, onMounted, onUnmounted, ref, watch } from "vue";
import { NAlert, NButton, NEmpty, NIcon, NPopconfirm, NSpin, NTag, useMessage } from "naive-ui";
import { ApiOutlined, CheckOutlined, CopyOutlined, DownOutlined, KeyOutlined, ReloadOutlined, UnorderedListOutlined } from "@vicons/antd";
import StackedBarChart from "../components/StackedBarChart.vue";
import OcgPopover from "../components/ocg/OcgPopover.vue";
import OcgTooltip from "../components/ocg/OcgTooltip.vue";
import { PRIMARY_KEY_ID } from "../api/dashboard";
import { useDashboardPageStore } from "../stores/dashboardPage.ts";
import { useConnectionStore } from "../stores/connection.ts";
import { useSessionStore } from "../stores/session.ts";
import type { ConnectionInfo } from "../api/dashboard";
import { CHART_PALETTE } from "../theme";
import { t } from "../i18n/index.ts";
import { formatNumber, formatTokens, useClipboard } from "../utils/format.ts";
import { maskConnectionKey, resolveConnectionUrls } from "./dashboard-connection";
import { ATTENTION_REASON_KEYS, attentionTagType, type AttentionItem } from "./dashboard-attention.ts";

type ConnectionTarget = "api" | "key" | "upstream";
interface SwitcherKey { id: string; name: string; value: string }
const emit = defineEmits<{ navigate: [view: string] }>();
const message = useMessage();
const dashboardStore = useDashboardPageStore();
const connectionStore = useConnectionStore();
const sessionStore = useSessionStore();
const { copiedTarget, copy, cleanup } = useClipboard();
const characterImage = new URL("../../assets/opencode-mascot-sm.webp", import.meta.url).href;
const chartSeries = computed(() => dashboardStore.page?.chartSeries ?? []);
const modelTotals = computed(() => dashboardStore.page?.modelTotals ?? []);
const loading = computed(() => dashboardStore.loading);
const accountsLoaded = computed(() => dashboardStore.page !== null);
// Once loaded, revalidations keep the existing content rendered.
const summaryLoaded = accountsLoaded;
const tokensLoaded = accountsLoaded;
const dashboardError = computed(() => Boolean(dashboardStore.error) || Boolean(dashboardStore.page?.errors.length));
const refreshingKey = ref(false);
const connectionReadLoading = ref(false);
let connectionReadGeneration = 0;
const EMPTY_CONNECTION: ConnectionInfo = { gateway_port: 9042, client_root_url: "", primary_key: "", sub_keys: [], revision: 0 };
const serviceConfig = computed(() => connectionStore.info ?? EMPTY_CONNECTION);
const selectedKeyId = ref("");
const summary = computed(() => dashboardStore.page?.summary ?? { gatewayRunning: false });
const legendModels = computed(() => modelTotals.value.map((row, index) => ({ model: row.model, color: CHART_PALETTE[index % CHART_PALETTE.length] })));
const totalChartTokens = computed(() => dashboardStore.page?.totalTokens ?? 0);
const dailyAverageTokens = computed(() => dashboardStore.page?.dailyAverageTokens ?? 0);
const chartDays = computed(() => dashboardStore.page?.chartDays ?? 30);
/** A loaded key with no cached secret is unknown plaintext, not an unconfigured key. */
function presentConnectionKey(value: string): string {
  if (!value && connectionStore.info) return t("未知");
  return maskConnectionKey(value);
}
const maskedKey = computed(() => presentConnectionKey(selectedKey.value?.value ?? ""));
const enabledGatewayKeys = computed<SwitcherKey[]>(() => [
  { id: PRIMARY_KEY_ID, name: t("主 Key"), value: serviceConfig.value.primary_key },
  ...serviceConfig.value.sub_keys.filter((entry) => entry.enabled).map((entry) => ({ id: entry.id, name: entry.name, value: entry.value })),
]);
const keyMenuOpen = ref(false);
const selectedKey = computed<SwitcherKey | null>(() => {
  if (!connectionStore.info) return null;
  const keys = enabledGatewayKeys.value;
  if (keys.length === 0) return null;
  return keys.find((entry) => entry.id === selectedKeyId.value) ?? keys[0];
});
watch(enabledGatewayKeys, (keys) => {
  if (keys.length > 0 && !keys.some((entry) => entry.id === selectedKeyId.value)) selectedKeyId.value = keys[0].id;
});
function selectGatewayKey(id: string): void { selectedKeyId.value = id; keyMenuOpen.value = false; }
const connectionUrls = computed(() => {
  try { return resolveConnectionUrls(serviceConfig.value.client_root_url, window.location.origin, serviceConfig.value.gateway_port, import.meta.env.DEV); }
  catch { return resolveConnectionUrls("", window.location.origin, serviceConfig.value.gateway_port, import.meta.env.DEV); }
});
const serviceApiUrl = computed(() => connectionUrls.value.apiBaseUrl);
const attentionItems = computed(() => dashboardStore.page?.attentionItems ?? []);
const attentionReadComplete = computed(() => !dashboardStore.page?.errors.some(issue => issue.resource === "account"));
const attentionDesc = computed(() => {
  if (!accountsLoaded.value) return t("加载中…");
  const count = dashboardStore.page?.attentionTotal ?? 0;
  if (count > attentionItems.value.length) return t("已显示 {shown} 个，共 {total} 个账号", { shown: formatNumber(attentionItems.value.length), total: formatNumber(count) });
  return count > 0 ? t("账号数：{count}", { count: formatNumber(count) }) : t("所有账号状态正常");
});
function attentionLabel(item: AttentionItem): string {
  return t(ATTENTION_REASON_KEYS[item.reason], { days: item.expiredDays ?? 0 });
}
function attentionItemAriaLabel(item: AttentionItem): string { return `${item.accountName} · ${attentionLabel(item)}`; }
async function copyConnection(target: ConnectionTarget, value: string, label: string) {
  if (typeof value !== "string" || (target === "key" && value.length === 0)) return;
  try { await copy(target, value, label); message.success(t("已复制 {label}", { label })); }
  catch (e) { message.error(e instanceof Error ? e.message : t("复制失败")); }
}
let keyFlow = 0;
function dashboardKeyCurrent(flow: number, connectionEpoch: number | null, shellEpoch: number | undefined): boolean {
  if (flow !== keyFlow) return false;
  const current = connectionStore.currentSession;
  if (typeof current === "function" && connectionEpoch !== null && current() !== connectionEpoch) return false;
  if (typeof sessionStore.sessionEpoch === "number" && shellEpoch !== undefined && sessionStore.sessionEpoch !== shellEpoch) return false;
  return true;
}
async function regenerateKey() {
  const target = selectedKey.value;
  if (refreshingKey.value || loading.value || !target) return;
  const flow = ++keyFlow;
  const connectionEpoch = typeof connectionStore.currentSession === "function" ? connectionStore.currentSession() : null;
  const shellEpoch = typeof sessionStore.sessionEpoch === "number" ? sessionStore.sessionEpoch : undefined;
  const isPrimary = target.id === PRIMARY_KEY_ID;
  refreshingKey.value = true;
  try {
    if (isPrimary) await connectionStore.regeneratePrimaryKey();
    else await connectionStore.regenerateKey(target.id);
    if (!dashboardKeyCurrent(flow, connectionEpoch, shellEpoch)) return;
    selectedKeyId.value = target.id;
    message.success(t("Key 已刷新"));
  } catch (error) {
    if (!dashboardKeyCurrent(flow, connectionEpoch, shellEpoch)) return;
    const detail = error instanceof Error
      ? error.message
      : t("无法连接到本地服务，请确认程序正在运行后重试");
    message.error(t("刷新 Key 失败：{error}", { error: detail }));
  } finally { refreshingKey.value = false; }
}
async function reloadConnection(): Promise<void> {
  if (connectionReadLoading.value) return;
  const generation = ++connectionReadGeneration;
  connectionReadLoading.value = true;
  try {
    await connectionStore.load();
  } catch {
    // A failed read keeps the last connection snapshot and the selected key.
  } finally {
    if (generation === connectionReadGeneration) connectionReadLoading.value = false;
  }
}
function goToAccounts() { emit("navigate", "accounts"); }
function goToKeys() { emit("navigate", "keys"); }
const ACTIVATED_REFRESH_FRESHNESS_MS = 15_000;
let active = false;
let refreshTimer: number | undefined;
function stopRefreshTimer(): void {
  if (refreshTimer !== undefined) { window.clearTimeout(refreshTimer); refreshTimer = undefined; }
}
function scheduleRefresh(): void {
  stopRefreshTimer();
  if (!active || document.visibilityState !== "visible" || !sessionStore.authenticated) return;
  const remaining = dashboardStore.page ? Date.parse(dashboardStore.page.validUntil) - Date.now() : 0;
  const delay = remaining > 0 && !dashboardStore.error ? remaining : ACTIVATED_REFRESH_FRESHNESS_MS;
  refreshTimer = window.setTimeout(() => { void loadPage(); }, Math.max(250, delay));
}
async function loadPage(maxAgeMs = 0): Promise<void> {
  if (!sessionStore.authenticated) return;
  if (refreshingKey.value) { scheduleRefresh(); return; }
  try { await dashboardStore.load({ utcOffsetMinutes: -new Date().getTimezoneOffset() }, { maxAgeMs }); }
  catch { /* The store retains the snapshot and exposes the read failure. */ }
  finally { scheduleRefresh(); }
}
async function loadDashboard(): Promise<void> {
  await Promise.allSettled([loadPage(), connectionStore.load()]);
}
function refreshWhenVisible(): void {
  if (document.visibilityState === "visible") void loadPage(ACTIVATED_REFRESH_FRESHNESS_MS);
  else stopRefreshTimer();
}
function activate(): void {
  active = true;
  document.addEventListener("visibilitychange", refreshWhenVisible);
  void loadPage(ACTIVATED_REFRESH_FRESHNESS_MS);
}
function deactivate(): void {
  active = false;
  stopRefreshTimer();
  document.removeEventListener("visibilitychange", refreshWhenVisible);
}
watch(() => sessionStore.authenticated, (ok) => { if (!ok) stopRefreshTimer(); });
onMounted(() => { activate(); void reloadConnection(); });
onActivated(activate);
onDeactivated(deactivate);
onUnmounted(() => { cleanup(); deactivate(); });
</script>

<style scoped src="../styles/dashboard.css"></style>
