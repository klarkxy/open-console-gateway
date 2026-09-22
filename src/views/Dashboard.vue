<template>
  <div class="dashboard">
    <section class="connection-hero" aria-labelledby="connection-title">
      <div class="connection-content">
        <div class="connection-head">
          <h2 id="connection-title">⚡ {{ t("接入中心") }}</h2>
          <span
            class="ready-mark"
            :class="{
              'not-ready': summaryLoaded && !summary.gateway_running,
              pending: !summaryLoaded,
            }"
            role="status"
          >
            <span aria-hidden="true" />
            {{ !summaryLoaded ? t("加载中…") : summary.gateway_running ? t("就绪") : t("服务未就绪") }}
          </span>
        </div>

        <div class="connection-rows">
          <div class="connection-row">
            <n-icon size="18" aria-hidden="true"><ApiOutlined /></n-icon>
            <div class="connection-value">
              <span class="sr-only">{{ t("API 地址") }}</span>
              <code>{{ serviceApiUrl }}</code>
            </div>
            <n-tooltip trigger="hover" :delay="200">
              <template #trigger>
                <n-button
                  circle
                  quaternary
                  size="small"
                  :aria-label="t('复制 API Base URL')"
                  @click="copyConnection('api', serviceApiUrl, t('API 地址'))"
                >
                  <template #icon>
                    <n-icon :component="copiedTarget === 'api' ? CheckOutlined : CopyOutlined" />
                  </template>
                </n-button>
              </template>
              {{ t("复制 API Base URL") }}
            </n-tooltip>
          </div>

          <div class="connection-row">
            <n-icon size="18" aria-hidden="true"><KeyOutlined /></n-icon>
            <n-popover
              v-if="enabledGatewayKeys.length > 1"
              trigger="click"
              placement="bottom-start"
              :show="keyMenuOpen"
              @update:show="keyMenuOpen = $event"
            >
              <template #trigger>
                <button
                  type="button"
                  class="key-switcher-trigger"
                  :disabled="refreshingKey || loading"
                  :aria-label="t('选择 Key')"
                  :aria-expanded="keyMenuOpen"
                  aria-haspopup="menu"
                  @keydown.esc="keyMenuOpen = false"
                >
                  <span class="key-switcher-name">{{ selectedKey?.name }}</span>
                  <span v-if="selectedKey?.id === PRIMARY_KEY_ID" class="key-switcher-badge">{{ t("主 Key") }}</span>
                  <n-icon size="12" aria-hidden="true"><DownOutlined /></n-icon>
                </button>
              </template>
              <div class="key-switcher-menu" @keydown.esc="keyMenuOpen = false">
                <button
                  v-for="entry in enabledGatewayKeys"
                  :key="entry.id"
                  type="button"
                  class="key-switcher-option"
                  :class="{ selected: entry.id === selectedKey?.id }"
                  @click="selectGatewayKey(entry.id)"
                >
                  <span class="key-switcher-option-main">
                    <span class="key-switcher-option-name">{{ entry.name }}</span>
                    <span v-if="entry.id === PRIMARY_KEY_ID" class="key-switcher-badge">{{ t("主 Key") }}</span>
                  </span>
                  <code class="key-switcher-option-value">{{ maskConnectionKey(entry.value) }}</code>
                  <n-icon
                    v-if="entry.id === selectedKey?.id"
                    class="key-switcher-check"
                    size="14"
                    aria-hidden="true"
                  ><CheckOutlined /></n-icon>
                </button>
              </div>
            </n-popover>
            <div class="connection-value">
              <span class="sr-only">{{ t("Key") }}</span>
              <code>{{ maskedKey }}</code>
            </div>
            <div class="row-actions">
              <n-popconfirm
                :positive-text="t('生成新 Key')"
                :negative-text="t('取消')"
                @positive-click="regenerateKey"
              >
                <template #trigger>
                  <n-tooltip trigger="hover" :delay="200">
                    <template #trigger>
                      <n-button
                        circle
                        quaternary
                        size="small"
                        :aria-label="t('刷新 Key')"
                        :loading="refreshingKey"
                        :disabled="refreshingKey || loading || !selectedKey"
                      >
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
                  <n-button
                    circle
                    quaternary
                    size="small"
                    :aria-label="t('复制 Key')"
                    :disabled="refreshingKey || !selectedKey"
                    @click="copyConnection('key', selectedKey?.value ?? '', t('Key'))"
                  >
                    <template #icon>
                      <n-icon :component="copiedTarget === 'key' ? CheckOutlined : CopyOutlined" />
                    </template>
                  </n-button>
                </template>
                {{ t("复制 Key") }}
              </n-tooltip>
              <n-tooltip trigger="hover" :delay="200">
                <template #trigger>
                  <n-button
                    circle
                    quaternary
                    size="small"
                    :aria-label="t('管理接入 Key')"
                    @click="goToKeys"
                  >
                    <template #icon><n-icon :component="UnorderedListOutlined" /></template>
                  </n-button>
                </template>
                {{ t("管理接入 Key") }}
              </n-tooltip>
            </div>
          </div>
        </div>
        <p v-if="connectionUrls.insecureHttp" class="connection-warning" role="status">
          {{ t("非本机 HTTP 会明文传输 Key 与请求内容，仅在可信网络中使用。") }}
        </p>
      </div>
      <img :src="characterImage" alt="" class="hero-character" aria-hidden="true" />
    </section>

    <n-alert v-if="dashboardError" type="error" :title="t('仪表盘数据加载失败')">
      <n-button size="small" secondary :loading="loading" :disabled="refreshingKey" @click="loadDashboard">
        {{ t("重试") }}
      </n-button>
    </n-alert>

    <section class="card attention-card" :aria-label="t('需要关注')" :aria-busy="!accountsLoaded">
      <div class="card-head">
        <div>
          <h3 class="card-title">{{ t("需要关注") }}</h3>
          <span v-if="accountsLoaded && attentionItems.length === 0" class="card-desc">{{ t("所有账号状态正常") }}</span>
          <span v-else-if="accountsLoaded" class="card-desc">{{ attentionDesc }}</span>
        </div>
        <n-button
          v-if="accountsLoaded && attentionItems.length > 0"
          size="small"
          @click="goToAccounts"
        >
          {{ t("去处理") }}
        </n-button>
      </div>
      <div v-if="!accountsLoaded" class="section-state">
        {{ loading ? t("加载中…") : t("仪表盘数据加载失败") }}
      </div>
      <div v-else-if="attentionItems.length > 0" class="attention-list" role="list">
        <button
          v-for="item in attentionItems"
          :key="item.accountId"
          type="button"
          class="attention-item"
          role="listitem"
          :aria-label="attentionItemAriaLabel(item)"
          @click="goToAccounts"
        >
          <span class="attention-name">{{ item.accountName }}</span>
          <n-tag size="small" :type="attentionTagType(item.reason)">
            {{ attentionLabel(item) }}
          </n-tag>
        </button>
      </div>
    </section>

    <section class="card chart-card">
      <div class="card-head chart-head">
        <div>
          <h3 class="card-title">{{ t("每日 Token 消耗") }}</h3>
        </div>
        <div v-if="tokensLoaded" class="chart-stats" role="group" :aria-label="t('图表摘要')">
          <span>{{ t("模型：{count}", { count: formatNumber(legendModels.length) }) }}</span>
          <span><b>{{ formatTokens(totalChartTokens) }}</b> {{ t("{days} 天合计", { days: 30 }) }}</span>
          <span><b>{{ formatTokens(Math.round(totalChartTokens / 30)) }}</b> {{ t("日均") }}</span>
        </div>
      </div>
      <div v-if="tokensLoaded" class="legend" role="list" :aria-label="t('模型图例')">
        <span v-for="model in legendModels" :key="model.model" class="legend-item" role="listitem">
          <span class="legend-dot" :style="{ background: model.color }" aria-hidden="true" />
          {{ model.model }}
        </span>
      </div>
      <n-spin :show="loading && !tokensLoaded">
        <div v-if="!tokensLoaded" class="section-state">
          {{ loading ? t("加载中…") : t("仪表盘数据加载失败") }}
        </div>
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
import {
  ApiOutlined,
  CheckOutlined,
  CopyOutlined,
  DownOutlined,
  KeyOutlined,
  ReloadOutlined,
  UnorderedListOutlined,
} from "@vicons/antd";
import StackedBarChart from "../components/StackedBarChart.vue";
import { PRIMARY_KEY_ID, dashboardApi } from "../api/dashboard";
import { useAccountsStore } from "../stores/accounts.ts";
import { useConnectionStore } from "../stores/connection.ts";
import { useProvidersStore } from "../stores/providers.ts";
import type {
  Account,
  ConnectionInfo,
  DailyModelTokens,
  DashboardSummary,
} from "../api/dashboard";
import { CHART_PALETTE } from "../theme";
import { t } from "../i18n/index.ts";
import { formatNumber, formatTokens, useClipboard } from "../utils/format.ts";
import { userFacingError } from "../utils/errors.ts";
import { accountExpiry } from "../domain/account-display.ts";
import { accountExpiryText } from "./account-status-text.ts";
import { maskConnectionKey, resolveConnectionUrls } from "./dashboard-connection";
import { buildNeedsAttention } from "./dashboard-attention.ts";
import type { AttentionItem, AttentionReason } from "./dashboard-attention.ts";

type ConnectionTarget = "api" | "key" | "upstream";

/** One selectable credential in the connection switcher. */
interface SwitcherKey {
  id: string;
  name: string;
  value: string;
}

const emit = defineEmits<{
  navigate: [view: string];
}>();

const message = useMessage();
const accountsStore = useAccountsStore();
const connectionStore = useConnectionStore();
const providersStore = useProvidersStore();
const destinationsStore = useDestinationsStore();
const { copiedTarget, copy, cleanup } = useClipboard();
const characterImage = new URL("../../assets/opencode-mascot.png", import.meta.url).href;
const accounts = computed(() => accountsStore.accounts);
const dailyTokens = ref<DailyModelTokens[]>([]);
const loading = ref(true);
const accountsLoaded = computed(() => accountsStore.loaded);
// summary/tokens latches: once true they stay true, so revalidations
// (tab return, window refocus) keep the current content rendered.
const summaryLoaded = ref(false);
const tokensLoaded = ref(false);
const dashboardError = ref(false);
const refreshingKey = ref(false);
const lifecycleNow = ref(Date.now());

// ponytail: keep this pre-load fallback in sync with ConnectionInfo.
const EMPTY_CONNECTION: ConnectionInfo = {
  gateway_port: 9042,
  client_root_url: "",
  primary_key: "",
  sub_keys: [],
  revision: 0,
};
const serviceConfig = computed(() => connectionStore.info ?? EMPTY_CONNECTION);
const selectedKeyId = ref("");
const summary = ref<DashboardSummary>({
  total_accounts: 0,
  available_accounts: 0,
  today_cost: 0,
  week_cost: 0,
  month_cost: 0,
  gateway_running: false,
});

const legendModels = computed(() => {
  const totals = new Map<string, number>();
  for (const row of dailyTokens.value) totals.set(row.model, (totals.get(row.model) ?? 0) + row.tokens);
  return [...totals.keys()]
    .sort((a, b) => totals.get(b)! - totals.get(a)!)
    .map((model, index) => ({ model, color: CHART_PALETTE[index % CHART_PALETTE.length] }));
});
const totalChartTokens = computed(() => dailyTokens.value.reduce((sum, row) => sum + row.tokens, 0));
const maskedKey = computed(() => maskConnectionKey(selectedKey.value?.value ?? ""));
// The primary key is pinned first; only enabled sub keys join the switcher.
const enabledGatewayKeys = computed<SwitcherKey[]>(() => [
  { id: PRIMARY_KEY_ID, name: t("主 Key"), value: serviceConfig.value.primary_key },
  ...serviceConfig.value.sub_keys
    .filter((entry) => entry.enabled)
    .map((entry) => ({ id: entry.id, name: entry.name, value: entry.value })),
]);
const keyMenuOpen = ref(false);
// Default to the primary key and keep the selection valid across connection
// reloads and key lifecycle changes.
const selectedKey = computed<SwitcherKey | null>(() => {
  const keys = enabledGatewayKeys.value;
  if (keys.length === 0 || !keys[0].value) return null;
  return keys.find((entry) => entry.id === selectedKeyId.value) ?? keys[0];
});
// Keep the switcher explicitly on the primary key until the user picks
// another one, and re-validate whenever the enabled list changes.
watch(enabledGatewayKeys, (keys) => {
  if (keys.length > 0 && !keys.some((entry) => entry.id === selectedKeyId.value)) {
    selectedKeyId.value = keys[0].id;
  }
});
function selectGatewayKey(id: string): void {
  selectedKeyId.value = id;
  keyMenuOpen.value = false;
}
const connectionUrls = computed(() => {
  try {
    return resolveConnectionUrls(
      serviceConfig.value.client_root_url,
      window.location.origin,
      serviceConfig.value.gateway_port,
      import.meta.env.DEV,
    );
  } catch {
    return resolveConnectionUrls(
      "",
      window.location.origin,
      serviceConfig.value.gateway_port,
      import.meta.env.DEV,
    );
  }
});
const serviceApiUrl = computed(() => connectionUrls.value.apiBaseUrl);

const attentionItems = computed<AttentionItem[]>(() => {
  if (!accountsLoaded.value) return [];
  return buildNeedsAttention(accounts.value, lifecycleNow.value, providersStore.catalog, destinationsStore.destinationForAccount);
});

const attentionDesc = computed(() => {
  if (!accountsLoaded.value) return t("加载中…");
  const count = attentionItems.value.length;
  return count > 0
    ? t("账号数：{count}", { count: formatNumber(count) })
    : t("所有账号状态正常");
});

function attentionAccount(item: AttentionItem): Account | undefined {
  return accounts.value.find((account) => account.id === item.accountId);
}

function attentionLabel(item: AttentionItem): string {
  switch (item.reason) {
    case "auth-error":
      return t("不可用");
    case "expired": {
      const account = attentionAccount(item);
      return account
        ? accountExpiryText(accountExpiry(account, lifecycleNow.value))
        : t("已到期 {days} 天", { days: 0 });
    }
    case "cooling":
      return t("冷却中");
    case "setup-incomplete":
      return t("注册中");
  }
}

function attentionTagType(
  reason: AttentionReason,
): "error" | "warning" | "info" | "default" {
  switch (reason) {
    case "auth-error":
    case "expired":
      return "error";
    case "cooling":
      return "warning";
    case "setup-incomplete":
      return "info";
  }
}

function attentionItemAriaLabel(item: AttentionItem): string {
  return `${item.accountName} · ${attentionLabel(item)}`;
}

async function copyConnection(target: ConnectionTarget, value: string, label: string) {
  try {
    await copy(target, value, label);
    message.success(t("已复制 {label}", { label }));
  } catch (e) {
    message.error(e instanceof Error ? e.message : t("复制失败"));
  }
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
    message.error(t("刷新 Key 失败：{error}", {
      error: userFacingError(error, t("无法连接到本地服务，请确认程序正在运行后重试")),
    }));
  } finally {
    refreshingKey.value = false;
  }
}

function goToAccounts() {
  emit("navigate", "accounts");
}

function goToKeys() {
  emit("navigate", "keys");
}

let dashboardRequestActive = false;

async function loadDashboard() {
  if (dashboardRequestActive || refreshingKey.value) return;
  dashboardRequestActive = true;
  loading.value = true;
  dashboardError.value = false;
  // Revalidation keeps the current content: successful responses replace
  // state in place, failures keep the previous snapshot visible.
  const [loadedAccounts, connection, loadedSummary, tokens, catalog, destinations] = await Promise.allSettled([
    accountsStore.loadPresented(),
    connectionStore.load(),
    dashboardApi.getDashboardSummary(),
    dashboardApi.getDailyTokensByModel(30),
    providersStore.loadCatalog(),
    destinationsStore.load(),
  ]);
  if (loadedSummary.status === "fulfilled") {
    summary.value = loadedSummary.value;
    summaryLoaded.value = true;
  }
  if (tokens.status === "fulfilled") {
    dailyTokens.value = tokens.value;
    tokensLoaded.value = true;
  }
  dashboardError.value = [loadedAccounts, connection, loadedSummary, tokens, catalog, destinations].some((result) => result.status === "rejected");
  if (dashboardError.value) {
    message.error(t("部分仪表盘数据加载失败"));
  }
  loading.value = false;
  dashboardRequestActive = false;
}

function refreshWhenVisible() {
  if (document.visibilityState === "visible") void loadDashboard();
}

let lifecycleClock: number | undefined;
let activatedOnce = false;

function startLifecycleClock() {
  if (lifecycleClock === undefined) {
    lifecycleClock = window.setInterval(() => {
      lifecycleNow.value = Date.now();
    }, 60_000);
  }
}

function stopLifecycleClock() {
  if (lifecycleClock !== undefined) {
    window.clearInterval(lifecycleClock);
    lifecycleClock = undefined;
  }
}

// The view stays cached by App.vue's KeepAlive; pause its clock and
// visibility-driven refreshes while another tab is active. Bind/unbind on
// activate/deactivate so leave/return cycles re-register the listener.
function bindVisibilityRefresh() {
  document.addEventListener("visibilitychange", refreshWhenVisible);
}

function unbindVisibilityRefresh() {
  document.removeEventListener("visibilitychange", refreshWhenVisible);
}

onMounted(() => {
  bindVisibilityRefresh();
  void loadDashboard();
});
onActivated(() => {
  bindVisibilityRefresh();
  startLifecycleClock();
  if (activatedOnce) void loadDashboard();
  else activatedOnce = true;
});
onDeactivated(() => {
  stopLifecycleClock();
  unbindVisibilityRefresh();
});
onUnmounted(() => {
  cleanup();
  stopLifecycleClock();
  unbindVisibilityRefresh();
});
</script>

<style scoped>
.dashboard {
  display: flex;
  flex-direction: column;
  gap: var(--ocg-space-lg);
  max-width: 1480px;
  margin: 0 auto;
}

.connection-hero {
  position: relative;
  min-height: 262px;
  overflow: hidden;
  border: 1px solid var(--ocg-border);
  border-radius: var(--ocg-radius-lg);
  background: var(--ocg-surface);
  box-shadow: var(--ocg-shadow-sm);
}
.connection-hero::before {
  content: "";
  position: absolute;
  inset: 0 0 0 54%;
  opacity: 0.42;
  background-image:
    linear-gradient(var(--ocg-border) 1px, transparent 1px),
    linear-gradient(90deg, var(--ocg-border) 1px, transparent 1px);
  background-size: 24px 24px;
  mask-image: linear-gradient(90deg, transparent, #000 35%);
}
.connection-hero::after {
  content: "";
  position: absolute;
  z-index: 0;
  right: -12px;
  bottom: -28px;
  width: 390px;
  height: 300px;
  background: radial-gradient(ellipse at center, var(--ocg-mascot-halo, transparent), transparent 70%);
  pointer-events: none;
}
.connection-content {
  position: relative;
  z-index: 2;
  width: min(760px, calc(100% - 300px));
  padding: var(--ocg-space-xl);
}
.connection-head {
  display: flex;
  align-items: center;
  justify-content: space-between;
  margin-bottom: 14px;
}
.connection-head h2 {
  margin: 0;
  color: var(--ocg-ink);
  font: 700 var(--ocg-font-xl)/1.2 "Bahnschrift", "Segoe UI Variable Display", sans-serif;
}
.ready-mark {
  display: inline-flex;
  align-items: center;
  gap: 6px;
  color: var(--ocg-success);
  font: 700 var(--ocg-font-sm)/1 "Cascadia Mono", Consolas, monospace;
  letter-spacing: 0.08em;
}
.ready-mark > span {
  width: 7px;
  height: 7px;
  border-radius: 50%;
  background: var(--ocg-success);
  box-shadow: 0 0 0 4px var(--ocg-success-soft);
}
.ready-mark.not-ready {
  color: var(--ocg-error);
}
.ready-mark.not-ready > span {
  background: var(--ocg-error);
  box-shadow: 0 0 0 4px color-mix(in srgb, var(--ocg-error) 16%, transparent);
}
.ready-mark.pending {
  color: var(--ocg-subtle);
}
.ready-mark.pending > span {
  background: var(--ocg-subtle);
  box-shadow: 0 0 0 4px color-mix(in srgb, var(--ocg-subtle) 16%, transparent);
}
.connection-rows {
  display: grid;
  gap: var(--ocg-space-sm);
}
.connection-row {
  display: grid;
  grid-template-columns: 28px minmax(0, 1fr) auto;
  align-items: center;
  min-height: 44px;
  padding: 6px var(--ocg-space-sm) 6px var(--ocg-space-md);
  border: 1px solid var(--ocg-border);
  border-radius: var(--ocg-radius-md);
  background: color-mix(in srgb, var(--ocg-canvas) 72%, var(--ocg-surface));
  color: var(--ocg-primary);
}
.connection-row:has(.key-switcher-trigger) {
  grid-template-columns: 28px auto minmax(0, 1fr) auto;
}
.key-switcher-trigger {
  display: inline-flex;
  align-items: center;
  gap: 6px;
  max-width: 200px;
  padding: 3px var(--ocg-space-sm);
  border: 1px solid var(--ocg-border);
  border-radius: var(--ocg-radius-sm);
  background: var(--ocg-surface);
  color: var(--ocg-ink);
  font-size: var(--ocg-font-sm);
  font-weight: 600;
  line-height: 1.4;
  cursor: pointer;
}
.key-switcher-trigger:hover:not(:disabled) {
  border-color: var(--ocg-primary);
}
.key-switcher-trigger:focus-visible {
  outline: 2px solid var(--ocg-primary);
  outline-offset: 2px;
}
.key-switcher-trigger:disabled {
  cursor: default;
  opacity: 0.6;
}
.key-switcher-name {
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}
.key-switcher-badge {
  flex: none;
  padding: 0 6px;
  border: 1px solid var(--ocg-border);
  border-radius: 999px;
  color: var(--ocg-subtle);
  font-size: var(--ocg-font-xs);
  font-weight: 400;
}
.key-switcher-menu {
  display: grid;
  gap: var(--ocg-space-xs);
  min-width: 260px;
}
.key-switcher-option {
  display: grid;
  grid-template-columns: minmax(0, 1fr) auto;
  align-items: center;
  column-gap: var(--ocg-space-md);
  padding: 6px 10px;
  border: none;
  border-radius: var(--ocg-radius-sm);
  background: none;
  color: var(--ocg-ink);
  cursor: pointer;
  text-align: left;
}
.key-switcher-option:hover,
.key-switcher-option.selected {
  background: var(--ocg-primary-soft);
}
.key-switcher-option:focus-visible {
  outline: 2px solid var(--ocg-primary);
  outline-offset: -2px;
}
.key-switcher-option-main {
  display: flex;
  align-items: center;
  gap: 6px;
  grid-column: 1;
  min-width: 0;
}
.key-switcher-option-name {
  overflow: hidden;
  font-size: var(--ocg-font-sm);
  font-weight: 600;
  line-height: 1.4;
  text-overflow: ellipsis;
  white-space: nowrap;
}
.key-switcher-option-value {
  grid-column: 1;
  color: var(--ocg-muted);
  font: var(--ocg-font-xs)/1.4 "Cascadia Mono", Consolas, monospace;
}
.key-switcher-check {
  grid-column: 2;
  grid-row: 1 / span 2;
  color: var(--ocg-primary);
}
.connection-value {
  min-width: 0;
  color: var(--ocg-ink);
}
.connection-value code {
  display: block;
  overflow: hidden;
  font: var(--ocg-font-md)/1.4 "Cascadia Mono", Consolas, monospace;
  text-overflow: ellipsis;
  white-space: nowrap;
}
.row-actions {
  display: flex;
  align-items: center;
  gap: 2px;
}
.connection-warning {
  margin: 10px 2px 0;
  color: var(--ocg-warning);
  font-size: var(--ocg-font-sm);
  line-height: 1.5;
}
.hero-character {
  position: absolute;
  z-index: 1;
  top: var(--ocg-space-xs);
  right: 28px;
  height: 380px;
  max-width: 34%;
  object-fit: contain;
  filter:
    drop-shadow(0 0 1px var(--ocg-mascot-rim, transparent))
    drop-shadow(0 18px 20px rgba(31, 27, 56, 0.14));
  pointer-events: none;
  user-select: none;
}

.card {
  border: 1px solid var(--ocg-border);
  border-radius: var(--ocg-radius-lg);
  background: var(--ocg-surface);
  box-shadow: var(--ocg-shadow-sm);
}
.card-head {
  display: flex;
  align-items: flex-start;
  justify-content: space-between;
  gap: var(--ocg-space-md);
  padding: var(--ocg-space-lg) 18px 10px;
}
.card-title {
  margin: 0;
  color: var(--ocg-ink);
  font: 650 var(--ocg-font-lg)/1.3 "Bahnschrift", "Segoe UI Variable Display", sans-serif;
}
.card-desc {
  margin: 3px 0 0;
  color: var(--ocg-subtle);
  font-size: var(--ocg-font-sm);
}
.chart-card {
  padding-bottom: var(--ocg-space-md);
}
.chart-stats {
  display: flex;
  flex-wrap: wrap;
  justify-content: flex-end;
  gap: var(--ocg-space-sm) var(--ocg-space-lg);
  color: var(--ocg-subtle);
  font-size: var(--ocg-font-sm);
}
.chart-stats b {
  color: var(--ocg-ink);
  font-family: "Cascadia Mono", Consolas, monospace;
  font-weight: 600;
}
.legend {
  display: flex;
  flex-wrap: wrap;
  gap: 7px 14px;
  padding: 0 18px var(--ocg-space-xs);
}
.legend-item {
  display: inline-flex;
  align-items: center;
  gap: 5px;
  color: var(--ocg-muted);
  font-size: var(--ocg-font-sm);
}
.legend-dot {
  width: 7px;
  height: 7px;
  border-radius: 50%;
}
.chart-card :deep(.n-spin-content) {
  padding: var(--ocg-space-xs) var(--ocg-space-md) 0;
  overflow: hidden;
}

.section-state {
  min-height: 96px;
  display: grid;
  place-items: center;
  color: var(--ocg-subtle);
  font-size: var(--ocg-font-sm);
}

@media (max-width: 900px) {
  .connection-content {
    width: calc(100% - 210px);
  }
  .hero-character {
    right: 6px;
    max-width: 36%;
  }
}

@media (max-width: 640px) {
  .dashboard {
    gap: var(--ocg-space-md);
  }
  .connection-hero {
    min-height: 256px;
  }
  .connection-content {
    width: 100%;
    padding: 18px 14px;
  }
  .hero-character {
    z-index: 0;
    top: auto;
    right: -50px;
    bottom: -58px;
    height: 282px;
    max-width: 58%;
    opacity: 0.12;
  }
  .connection-hero::after {
    right: -48px;
    bottom: -34px;
    width: 300px;
    height: 250px;
  }
  .connection-hero::before {
    inset: 0;
    opacity: 0.18;
  }
  .connection-row {
    background: color-mix(in srgb, var(--ocg-surface) 88%, transparent);
  }
  .chart-head {
    align-items: flex-start;
  }
  .chart-stats {
    display: grid;
    gap: 2px;
  }
}

.attention-card {
  padding-bottom: var(--ocg-space-lg);
}

.attention-list {
  display: grid;
  gap: var(--ocg-space-sm);
  padding: var(--ocg-space-xs) 18px 0;
}

.attention-item {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: var(--ocg-space-md);
  width: 100%;
  padding: 10px var(--ocg-space-md);
  border: 1px solid var(--ocg-border);
  border-radius: var(--ocg-radius-md);
  background: color-mix(in srgb, var(--ocg-canvas) 70%, var(--ocg-surface));
  color: var(--ocg-ink);
  font: inherit;
  text-align: left;
  cursor: pointer;
  transition: border-color 0.16s ease, background-color 0.16s ease;
}

.attention-item:hover {
  border-color: var(--ocg-primary);
  background: color-mix(in srgb, var(--ocg-primary-soft) 40%, var(--ocg-surface));
}

.attention-item:focus-visible {
  outline: 2px solid var(--ocg-primary);
  outline-offset: 2px;
}

.attention-name {
  overflow: hidden;
  min-width: 0;
  font-size: var(--ocg-font-md);
  font-weight: 600;
  text-overflow: ellipsis;
  white-space: nowrap;
}

@media (prefers-reduced-motion: reduce) {
  .attention-item {
    transition: none;
  }
}

@media (max-width: 640px) {
  .attention-list {
    padding: var(--ocg-space-xs) 14px 0;
  }

  .attention-item {
    padding: 9px 11px;
  }
}
</style>
