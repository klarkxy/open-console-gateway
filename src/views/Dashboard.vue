<template>
  <div class="dashboard">
    <section class="connection-hero" aria-labelledby="connection-title">
      <div class="connection-content">
        <div class="connection-head">
          <h2 id="connection-title">{{ t("接入中心") }}</h2>
          <span class="ready-mark" :class="{ 'not-ready': summaryLoaded && !summary.gateway_running, pending: !summaryLoaded }" role="status">
            <span aria-hidden="true" />
            {{ !summaryLoaded ? t("加载中…") : summary.gateway_running ? t("就绪") : t("服务未就绪") }}
          </span>
        </div>
        <div class="connection-rows">
          <div class="connection-row">
            <n-icon size="18" aria-hidden="true"><ApiOutlined /></n-icon>
            <div class="connection-value">
              <span class="connection-label">{{ t("API 地址") }}</span>
              <code>{{ serviceApiUrl }}</code>
            </div>
            <n-tooltip trigger="hover" :delay="200">
              <template #trigger>
                <n-button circle quaternary size="small" :aria-label="t('复制 API Base URL')" @click="copyConnection('api', serviceApiUrl, t('API 地址'))">
                  <template #icon><n-icon :component="copiedTarget === 'api' ? CheckOutlined : CopyOutlined" /></template>
                </n-button>
              </template>
              {{ t("复制 API Base URL") }}
            </n-tooltip>
          </div>
          <div class="connection-row">
            <n-icon size="18" aria-hidden="true"><KeyOutlined /></n-icon>
            <div class="connection-value">
              <div class="connection-key-label">
                <span class="connection-label">{{ t("Key") }}</span>
                <n-popover v-if="enabledGatewayKeys.length > 1" trigger="click" placement="bottom-start" :show="keyMenuOpen" @update:show="keyMenuOpen = $event">
                  <template #trigger>
                    <button type="button" class="key-switcher-trigger" :disabled="refreshingKey || loading" :aria-label="t('选择 Key')" :aria-expanded="keyMenuOpen" @keydown.esc="keyMenuOpen = false">
                      <span class="key-switcher-name">{{ selectedKey?.name }}</span>
                      <n-icon size="12" aria-hidden="true"><DownOutlined /></n-icon>
                    </button>
                  </template>
                  <div class="key-switcher-menu" @keydown.esc="keyMenuOpen = false">
                    <button v-for="entry in enabledGatewayKeys" :key="entry.id" type="button" class="key-switcher-option" :class="{ selected: entry.id === selectedKey?.id }" :aria-pressed="entry.id === selectedKey?.id" @click="selectGatewayKey(entry.id)">
                      <span class="key-switcher-option-main"><span class="key-switcher-option-name">{{ entry.name }}</span><span v-if="entry.id === PRIMARY_KEY_ID" class="key-switcher-badge">{{ t("主 Key") }}</span></span>
                      <code class="key-switcher-option-value">{{ maskConnectionKey(entry.value) }}</code>
                      <n-icon v-if="entry.id === selectedKey?.id" class="key-switcher-check" size="14" aria-hidden="true"><CheckOutlined /></n-icon>
                    </button>
                  </div>
                </n-popover>
              </div>
              <code>{{ maskedKey }}</code>
            </div>
            <div class="row-actions">
              <n-popconfirm :positive-text="t('生成新 Key')" :negative-text="t('取消')" @positive-click="regenerateKey">
                <template #trigger>
                  <n-tooltip trigger="hover" :delay="200">
                    <template #trigger>
                      <n-button circle quaternary size="small" :aria-label="t('刷新 Key')" :loading="refreshingKey" :disabled="refreshingKey || loading || !selectedKey">
                        <template #icon><n-icon :component="ReloadOutlined" /></template>
                      </n-button>
                    </template>
                    {{ t("刷新 Key") }}
                  </n-tooltip>
                </template>
                {{ t("仅当前 Key 的旧值立即失效，其他 Key 不受影响。确定生成新值？") }}
              </n-popconfirm>
              <n-tooltip trigger="hover" :delay="200">
                <template #trigger>
                  <n-button circle quaternary size="small" :aria-label="t('复制 Key')" :disabled="refreshingKey || !selectedKey" @click="copyConnection('key', selectedKey?.value ?? '', t('Key'))">
                    <template #icon><n-icon :component="copiedTarget === 'key' ? CheckOutlined : CopyOutlined" /></template>
                  </n-button>
                </template>
                {{ t("复制 Key") }}
              </n-tooltip>
              <n-tooltip trigger="hover" :delay="200">
                <template #trigger>
                  <n-button circle quaternary size="small" :aria-label="t('管理接入 Key')" @click="goToKeys"><template #icon><n-icon :component="UnorderedListOutlined" /></template></n-button>
                </template>
                {{ t("管理接入 Key") }}
              </n-tooltip>
            </div>
          </div>
        </div>
        <p v-if="connectionUrls.insecureHttp" class="connection-warning" role="status">{{ t("非本机 HTTP 会明文传输 Key 与请求内容，仅在可信网络中使用。") }}</p>
      </div>
      <img :src="characterImage" alt="" class="hero-character" aria-hidden="true" />
    </section>
    <n-alert v-if="dashboardError" type="error" :title="t('仪表盘数据加载失败')"><n-button size="small" secondary :loading="loading" :disabled="refreshingKey" @click="loadDashboard">{{ t("重试") }}</n-button></n-alert>
    <section class="card attention-card" :aria-label="t('需要关注')" :aria-busy="!accountsLoaded">
      <div class="card-head">
        <div><h3 class="card-title">{{ t("需要关注") }}</h3><span v-if="accountsLoaded && attentionItems.length === 0" class="card-desc">{{ t("所有账号状态正常") }}</span><span v-else-if="accountsLoaded" class="card-desc">{{ attentionDesc }}</span></div>
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
          <span><b>{{ formatTokens(totalChartTokens) }}</b> {{ t("{days} 天合计", { days: 30 }) }}</span>
          <span><b>{{ formatTokens(Math.round(totalChartTokens / 30)) }}</b> {{ t("日均") }}</span>
        </div>
      </div>
      <div v-if="tokensLoaded" class="legend" role="list" :aria-label="t('模型图例')"><span v-for="model in legendModels" :key="model.model" class="legend-item" role="listitem"><span class="legend-dot" :style="{ background: model.color }" aria-hidden="true" />{{ model.model }}</span></div>
      <n-spin :show="loading && !tokensLoaded">
        <div v-if="!tokensLoaded" class="section-state">{{ loading ? t("加载中…") : t("仪表盘数据加载失败") }}</div>
        <n-empty v-else-if="totalChartTokens === 0" :description="t('暂无 Token 消耗数据')" />
        <StackedBarChart v-else :data="dailyTokens" :days="30" />
      </n-spin>
    </section>
  </div>
</template>

<script setup lang="ts">
import { useDestinationsStore } from "../stores/destinations.ts";
import { computed, onActivated, onDeactivated, onMounted, onUnmounted, ref, watch } from "vue";
import { NAlert, NButton, NEmpty, NIcon, NPopconfirm, NPopover, NSpin, NTag, NTooltip, useMessage } from "naive-ui";
import { ApiOutlined, CheckOutlined, CopyOutlined, DownOutlined, KeyOutlined, ReloadOutlined, UnorderedListOutlined } from "@vicons/antd";
import StackedBarChart from "../components/StackedBarChart.vue";
import { PRIMARY_KEY_ID, dashboardApi } from "../api/dashboard";
import { useAccountsStore } from "../stores/accounts.ts";
import { useConnectionStore } from "../stores/connection.ts";
import { useProvidersStore } from "../stores/providers.ts";
import { useSessionStore } from "../stores/session.ts";
import type { Account, ConnectionInfo, DailyModelTokens, DashboardSummary } from "../api/dashboard";
import { CHART_PALETTE } from "../theme";
import { t } from "../i18n/index.ts";
import { formatNumber, formatTokens, useClipboard } from "../utils/format.ts";
import { userFacingError } from "../utils/errors.ts";
import { accountExpiry } from "../domain/account-display.ts";
import { createRevalidateGate } from "../domain/revalidate.ts";
import { accountExpiryText } from "./account-status-text.ts";
import { maskConnectionKey, resolveConnectionUrls } from "./dashboard-connection";
import { buildNeedsAttention } from "./dashboard-attention.ts";
import type { AttentionItem, AttentionReason } from "./dashboard-attention.ts";

type ConnectionTarget = "api" | "key" | "upstream";
interface SwitcherKey { id: string; name: string; value: string }
const emit = defineEmits<{ navigate: [view: string] }>();
const message = useMessage();
const accountsStore = useAccountsStore();
const connectionStore = useConnectionStore();
const providersStore = useProvidersStore();
const destinationsStore = useDestinationsStore();
const sessionStore = useSessionStore();
const revalidateGate = createRevalidateGate(30_000);
watch(() => sessionStore.authenticated, (ok) => { if (!ok) revalidateGate.reset(); });
const { copiedTarget, copy, cleanup } = useClipboard();
const characterImage = new URL("../../assets/opencode-mascot.png", import.meta.url).href;
const accounts = computed(() => accountsStore.accounts);
const dailyTokens = ref<DailyModelTokens[]>([]);
const loading = ref(true);
const accountsLoaded = computed(() => accountsStore.loaded);
// Once loaded, revalidations keep the existing content rendered.
const summaryLoaded = ref(false);
const tokensLoaded = ref(false);
const dashboardError = ref(false);
const refreshingKey = ref(false);
const lifecycleNow = ref(Date.now());
const EMPTY_CONNECTION: ConnectionInfo = { gateway_port: 9042, client_root_url: "", primary_key: "", sub_keys: [], revision: 0 };
const serviceConfig = computed(() => connectionStore.info ?? EMPTY_CONNECTION);
const selectedKeyId = ref("");
const summary = ref<DashboardSummary>({ total_accounts: 0, available_accounts: 0, today_cost: 0, week_cost: 0, month_cost: 0, gateway_running: false });
const legendModels = computed(() => {
  const totals = new Map<string, number>();
  for (const row of dailyTokens.value) totals.set(row.model, (totals.get(row.model) ?? 0) + row.tokens);
  return [...totals.keys()].sort((a, b) => totals.get(b)! - totals.get(a)!).map((model, index) => ({ model, color: CHART_PALETTE[index % CHART_PALETTE.length] }));
});
const totalChartTokens = computed(() => dailyTokens.value.reduce((sum, row) => sum + row.tokens, 0));
const maskedKey = computed(() => maskConnectionKey(selectedKey.value?.value ?? ""));
const enabledGatewayKeys = computed<SwitcherKey[]>(() => [
  { id: PRIMARY_KEY_ID, name: t("主 Key"), value: serviceConfig.value.primary_key },
  ...serviceConfig.value.sub_keys.filter((entry) => entry.enabled).map((entry) => ({ id: entry.id, name: entry.name, value: entry.value })),
]);
const keyMenuOpen = ref(false);
const selectedKey = computed<SwitcherKey | null>(() => {
  const keys = enabledGatewayKeys.value;
  if (keys.length === 0 || !keys[0].value) return null;
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
const attentionItems = computed<AttentionItem[]>(() => {
  if (!accountsLoaded.value) return [];
  return buildNeedsAttention(accounts.value, lifecycleNow.value, providersStore.catalog, destinationsStore.destinationForAccount);
});
const attentionDesc = computed(() => {
  if (!accountsLoaded.value) return t("加载中…");
  const count = attentionItems.value.length;
  return count > 0 ? t("账号数：{count}", { count: formatNumber(count) }) : t("所有账号状态正常");
});
function attentionAccount(item: AttentionItem): Account | undefined { return accounts.value.find((account) => account.id === item.accountId); }
function attentionLabel(item: AttentionItem): string {
  switch (item.reason) {
    case "auth-error": return t("不可用");
    case "expired": { const account = attentionAccount(item); return account ? accountExpiryText(accountExpiry(account, lifecycleNow.value)) : t("已到期 {days} 天", { days: 0 }); }
    case "cooling": return t("冷却中");
    case "setup-incomplete": return t("注册中");
  }
}
function attentionTagType(reason: AttentionReason): "error" | "warning" | "info" | "default" {
  switch (reason) { case "auth-error": case "expired": return "error"; case "cooling": return "warning"; case "setup-incomplete": return "info"; }
}
function attentionItemAriaLabel(item: AttentionItem): string { return `${item.accountName} · ${attentionLabel(item)}`; }
async function copyConnection(target: ConnectionTarget, value: string, label: string) {
  try { await copy(target, value, label); message.success(t("已复制 {label}", { label })); }
  catch (e) { message.error(e instanceof Error ? e.message : t("复制失败")); }
}
async function regenerateKey() {
  const target = selectedKey.value;
  if (refreshingKey.value || dashboardRequestActive || !target) return;
  const isPrimary = target.id === PRIMARY_KEY_ID;
  refreshingKey.value = true;
  try {
    if (isPrimary) await connectionStore.regeneratePrimaryKey();
    else await connectionStore.regenerateKey(target.id);
    selectedKeyId.value = target.id;
    message.success(t("Key 已刷新"));
  } catch (error) {
    dashboardError.value = true;
    message.error(t("刷新 Key 失败：{error}", { error: userFacingError(error, t("无法连接到本地服务，请确认程序正在运行后重试")) }));
  } finally { refreshingKey.value = false; }
}
function goToAccounts() { emit("navigate", "accounts"); }
function goToKeys() { emit("navigate", "keys"); }
let dashboardRequestActive = false;
async function loadDashboard() {
  if (dashboardRequestActive || refreshingKey.value) return;
  dashboardRequestActive = true;
  loading.value = true;
  dashboardError.value = false;
  const [loadedAccounts, connection, loadedSummary, tokens, catalog, destinations] = await Promise.allSettled([
    accountsStore.loadPresented(), connectionStore.load(), dashboardApi.getDashboardSummary(),
    dashboardApi.getDailyTokensByModel(30), providersStore.loadCatalog(), destinationsStore.load(),
  ]);
  if (loadedSummary.status === "fulfilled") { summary.value = loadedSummary.value; summaryLoaded.value = true; }
  if (tokens.status === "fulfilled") { dailyTokens.value = tokens.value; tokensLoaded.value = true; }
  dashboardError.value = [loadedAccounts, connection, loadedSummary, tokens, catalog, destinations].some((result) => result.status === "rejected");
  if (dashboardError.value) message.error(t("部分仪表盘数据加载失败"));
  loading.value = false;
  dashboardRequestActive = false;
}
function refreshWhenVisible() { if (document.visibilityState === "visible") void loadDashboard(); }
let lifecycleClock: number | undefined;
let activatedOnce = false;
function startLifecycleClock() { if (lifecycleClock === undefined) lifecycleClock = window.setInterval(() => { lifecycleNow.value = Date.now(); }, 60_000); }
function stopLifecycleClock() { if (lifecycleClock !== undefined) { window.clearInterval(lifecycleClock); lifecycleClock = undefined; } }
function bindVisibilityRefresh() { document.addEventListener("visibilitychange", refreshWhenVisible); }
function unbindVisibilityRefresh() { document.removeEventListener("visibilitychange", refreshWhenVisible); }
onMounted(() => { bindVisibilityRefresh(); void loadDashboard(); });
onActivated(() => {
  bindVisibilityRefresh(); startLifecycleClock();
  if (!activatedOnce) { activatedOnce = true; return; }
  if (summaryLoaded.value && !revalidateGate.shouldRun()) return;
  revalidateGate.record();
  void loadDashboard();
});
onDeactivated(() => { stopLifecycleClock(); unbindVisibilityRefresh(); });
onUnmounted(() => { cleanup(); stopLifecycleClock(); unbindVisibilityRefresh(); });
</script>

<style scoped src="../styles/dashboard.css"></style>
