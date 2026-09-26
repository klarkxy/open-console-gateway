<template>
  <section class="logs-card">
    <n-tabs v-model:value="activeTab" type="line" animated>
      <n-tab-pane name="gateway" :tab="t('运行日志')">
        <div class="log-toolbar">
          <n-input
            v-model:value="requestIdFilter"
            clearable
            class="request-id-filter"
            :placeholder="t('按请求 ID 精确搜索')"
              :input-props="{ 'aria-label': t('请求 ID') }"
          />
          <n-tooltip trigger="hover">
            <template #trigger>
              <n-button
                circle
                quaternary
                :loading="gatewayLoading"
                :aria-label="t('刷新运行日志')"
                @click="loadGatewayLogs"
              >
                <template #icon><n-icon :component="ReloadOutlined" /></template>
              </n-button>
            </template>
            {{ t("刷新运行日志") }}
          </n-tooltip>
        </div>
        <n-alert v-if="gatewayError" type="error" :title="t('加载运行日志失败：{error}', { error: gatewayError })">
          <n-button size="small" secondary @click="loadGatewayLogs">{{ t("重试") }}</n-button>
        </n-alert>
        <p class="log-limit-note">{{ t("仅显示最近 {count} 条运行日志", { count: 200 }) }}</p>
        <n-data-table
          :columns="gatewayColumns"
          :data="gatewayLogs"
          :row-key="logRowKey"
          :loading="gatewayLoading"
          :pagination="gatewayPagination"
          :scroll-x="1200"
          :virtual-scroll="true"
          max-height="560"
          size="small"
          @update:page="changeGatewayPage"
        />
      </n-tab-pane>
      <n-tab-pane name="forward" :tab="t('请求日志')">
        <div class="stats-row">
          <div class="stat-card">
            <div class="stat-label">{{ t("请求数") }}</div>
            <div class="stat-value">{{ formatNumber(forwardTotals.total_requests) }}</div>
          </div>
          <div class="stat-card">
            <div class="stat-label">{{ t("输入") }}</div>
            <div class="stat-value">{{ formatNumber(forwardTotals.prompt_tokens) }}</div>
          </div>
          <div class="stat-card">
            <div class="stat-label">{{ t("输出") }}</div>
            <div class="stat-value">{{ formatNumber(forwardTotals.completion_tokens) }}</div>
          </div>
          <div class="stat-card">
            <div class="stat-label">{{ t("缓存") }}</div>
            <div class="stat-value">{{ formatNumber(forwardTotals.cached_tokens) }}</div>
          </div>
          <div class="stat-card">
            <div class="stat-label">{{ t("总 Tokens") }}</div>
            <div class="stat-value">{{ formatNumber(forwardTotals.prompt_tokens + forwardTotals.completion_tokens) }}</div>
          </div>
        </div>
        <n-button class="advanced-filter-toggle" size="small" :aria-expanded="showAdvancedFilters" aria-controls="request-log-filters" @click="showAdvancedFilters = !showAdvancedFilters">
          {{ showAdvancedFilters ? t('收起筛选') : t('更多筛选（{count}）', { count: advancedFilterCount }) }}
        </n-button>
        <div id="request-log-filters" class="filter-bar" :class="{ 'show-advanced': showAdvancedFilters }">
          <div class="filter-field request-id-field advanced-filter">
            <span class="filter-label">{{ t("请求 ID") }}</span>
            <n-input
              v-model:value="requestIdFilter"
              clearable
              :placeholder="t('按请求 ID 精确搜索')"
              :input-props="{ 'aria-label': t('请求 ID') }"
            />
          </div>
          <div class="filter-field">
            <span class="filter-label">{{ t("状态") }}</span>
            <n-select
              v-model:value="statusFilter"
              :options="statusOptions"
              :placeholder="t('状态')"
              :aria-label="t('状态')"
            />
          </div>
          <div class="filter-field advanced-filter">
            <span class="filter-label">{{ t("账号") }}</span>
            <n-select
              v-model:value="accountFilter"
              :options="accountOptions"
              :placeholder="t('账号')"
              :aria-label="t('账号')"
            />
          </div>
          <div class="filter-field">
            <span class="filter-label">{{ t("模型") }}</span>
            <n-select
              v-model:value="modelFilter"
              :options="modelOptions"
              :placeholder="t('模型')"
              :aria-label="t('模型')"
            />
          </div>
          <div class="filter-field key-filter-field advanced-filter">
            <span class="filter-label">{{ t("接入 Key") }}</span>
            <n-select
              v-model:value="keyFilter"
              :options="keyOptions"
              :placeholder="t('接入 Key')"
              :aria-label="t('接入 Key')"
              :consistent-menu-width="false"
            />
          </div>
          <div class="filter-field advanced-filter">
            <span class="filter-label">{{ t("服务商") }}</span>
            <n-select
              v-model:value="providerFilter"
              :options="providerOptions"
              :placeholder="t('服务商')"
              :aria-label="t('服务商')"
              :consistent-menu-width="false"
            />
          </div>
          <div class="filter-field advanced-filter">
            <span class="filter-label">{{ t("路由账号") }}</span>
            <n-select
              v-model:value="routeAccountFilter"
              :options="routeAccountOptions"
              :placeholder="t('路由账号')"
              :aria-label="t('路由账号')"
              :consistent-menu-width="false"
            />
          </div>
          <div class="filter-field advanced-filter">
            <span class="filter-label">{{ t("凭证账号") }}</span>
            <n-select
              v-model:value="credentialAccountFilter"
              :options="credentialAccountOptions"
              :placeholder="t('凭证账号')"
              :aria-label="t('凭证账号')"
              :consistent-menu-width="false"
            />
          </div>
          <div class="filter-field time-range-field">
            <span class="filter-label">{{ t("时间范围") }}</span>
            <n-popover
              trigger="click"
              placement="bottom-start"
              :show="showTimePanel"
              @update:show="showTimePanel = $event"
            >
              <template #trigger>
                <n-button class="time-range-trigger">
                  <template #icon>
                    <n-icon :component="CalendarOutlined" />
                  </template>
                  {{ timeRangeLabel }}
                </n-button>
              </template>
              <div class="time-range-panel">
                <div class="preset-list">
                  <n-button
                    v-for="item in timePresetOptions"
                    :key="item.value"
                    quaternary
                    :type="activePreset === item.value ? 'primary' : 'default'"
                    class="preset-item"
                    @click="applyTimePreset(item.value)"
                  >
                    {{ item.label }}
                  </n-button>
                </div>
                <div
                  class="custom-range-wrapper"
                  :class="{ 'is-visible': activePreset === 'custom' }"
                >
                  <span class="custom-range-title">{{ t("自定义范围") }}</span>
                  <n-date-picker
                    v-model:value="customTimeRange"
                    type="daterange"
                    :panel="true"
                    :actions="null"
                    class="custom-time-picker"
                    @update:value="applyCustomTimeRange"
                  />
                </div>
              </div>
            </n-popover>
          </div>
          <div class="filter-field advanced-filter">
            <span class="filter-label">{{ t("排序") }}</span>
            <n-select
              v-model:value="sortBy"
              :options="sortOptions"
              :placeholder="t('排序')"
              :aria-label="t('排序')"
              :consistent-menu-width="false"
              class="sort-select"
            />
          </div>
          <div class="filter-actions">
            <n-tooltip trigger="hover">
              <template #trigger>
                <n-button
                  circle
                  quaternary
                  :aria-label="sortOrder === 'asc' ? t('升序') : t('降序')"
                  @click="toggleSortOrder"
                >
                  <template #icon>
                    <n-icon :component="sortOrder === 'asc' ? ArrowUpOutlined : ArrowDownOutlined" />
                  </template>
                </n-button>
              </template>
              {{ sortOrder === "asc" ? t("升序") : t("降序") }}
            </n-tooltip>
            <n-tooltip v-if="hasFilters" trigger="hover">
              <template #trigger>
                <n-button circle quaternary :aria-label="t('清除筛选')" @click="clearFilters">
                  <template #icon><n-icon :component="ClearOutlined" /></template>
                </n-button>
              </template>
              {{ t("清除筛选") }}
            </n-tooltip>
            <n-tooltip trigger="hover">
              <template #trigger>
                <n-button
                  circle
                  quaternary
                  :loading="forwardLoading"
                  :aria-label="t('刷新请求日志')"
                  @click="refreshForwardLogs"
                >
                  <template #icon><n-icon :component="ReloadOutlined" /></template>
                </n-button>
              </template>
              {{ t("刷新请求日志") }}
            </n-tooltip>
          </div>
        </div>
        <p v-if="keyFilter" class="key-filter-note" role="status">
          {{ t("升级前用量统一计入主 Key") }}
        </p>
        <n-alert v-if="forwardError" type="error" :title="t('加载请求日志失败：{error}', { error: forwardError })">
          <n-button size="small" secondary @click="loadForwardLogs">{{ t("重试") }}</n-button>
        </n-alert>
        <n-data-table
          :columns="forwardColumns"
          :data="forwardLogs"
          :row-key="logRowKey"
          :loading="forwardLoading"
          :pagination="forwardPagination"
          :scroll-x="1870"
          remote
          size="small"
          @update:page="changeForwardPage"
        >
          <template #empty>
            <n-empty :description="t('暂无请求日志')" />
          </template>
        </n-data-table>
      </n-tab-pane>
    </n-tabs>
  </section>
</template>

<script setup lang="ts">
import { computed, h, onActivated, onMounted, onUnmounted, ref, watch } from "vue";
import { useRoute, useRouter } from "vue-router";
import {
  NAlert,
  NButton,
  NDataTable,
  NDatePicker,
  NEmpty,
  NIcon,
  NInput,
  NPopover,
  NSelect,
  NTabPane,
  NTabs,
  NTag,
  NTooltip,
  useMessage,
} from "naive-ui";
import { ArrowDownOutlined, ArrowUpOutlined, CalendarOutlined, CheckOutlined, ClearOutlined, CopyOutlined, ReloadOutlined } from "@vicons/antd";
import { UNATTRIBUTED_KEY_FILTER, dashboardApi } from "../api/dashboard";
import type {
  ForwardLog,
  ForwardLogClientKey,
  ForwardLogSummary,
  GatewayLog,
} from "../api/dashboard";
import { t } from "../i18n/index.ts";
import { locale } from "../i18n/index.ts";
import { useAccountsStore } from "../stores/accounts.ts";
import { useProvidersStore } from "../stores/providers.ts";
import { useSessionStore } from "../stores/session.ts";
import { formatCost, formatNumber, useClipboard } from "../utils/format.ts";
import { dashboardErrorDetail } from "../utils/errors.ts";
import { computeTimeRange, resolveTimeRange, timePresetValues } from "./log-time-range.ts";
import { routeQuerySearch } from "./app-navigation.ts";
import type { TimePreset } from "./log-time-range.ts";
import { gatewayLogMessage } from "./gateway-log-message.ts";
import {
  forwardLogAlias,
  forwardLogLatencyMs,
  forwardLogPresentedStatus,
  forwardLogTotalTokens,
} from "./forward-log-display.ts";
import {
  renderDiagnostic,
  renderForwardDetail,
  renderRequestId,
  type LogsColumnContext,
} from "./logs-columns.ts";
import { formatNativeCostEstimate, forwardLogNativeEstimate } from "../domain/native-cost.ts";

type LogTab = "gateway" | "forward";
type SortBy = "timestamp" | "attempt" | "prompt_tokens" | "completion_tokens" | "cached_tokens" | "cost";
type SortOrder = "asc" | "desc";
const sortValues = new Set<SortBy>([
  "timestamp",
  "attempt",
  "prompt_tokens",
  "completion_tokens",
  "cached_tokens",
  "cost",
]);

const route = useRoute();
const router = useRouter();
const query = new URLSearchParams(routeQuerySearch("logs", route.query));
const message = useMessage();
const accountsStore = useAccountsStore();
const providersStore = useProvidersStore();
const sessionStore = useSessionStore();
watch(() => sessionStore.authenticated, (ok) => {
  if (!ok) {
    gatewayLoadedAt = 0;
    forwardLoadedAt = 0;
  }
});
const { copiedTarget, copy, cleanup } = useClipboard();
const activeTab = ref<LogTab>(query.get("tab") === "gateway" ? "gateway" : "forward");
const gatewayLogs = ref<GatewayLog[]>([]);
const forwardLogs = ref<ForwardLog[]>([]);
// Server state lives in the stores; these are read-through projections.
const accounts = computed(() => accountsStore.accounts);
const models = ref<string[]>([]);
const clientKeys = ref<ForwardLogClientKey[]>([]);
const providerCatalog = computed(() => providersStore.catalog);
const gatewayLoading = ref(false);
const gatewayError = ref("");
const forwardLoading = ref(false);
const forwardError = ref("");
const queryStatus = query.get("status") ?? "";
const statusFilter = ref<string>(queryStatus === "success_unpriced" ? "success" : queryStatus);
const accountFilter = ref<string>(query.get("account") ?? "");
const modelFilter = ref<string>(query.get("model") ?? "");
const keyFilter = ref<string>(query.get("key") ?? "");
const providerFilter = ref<string>(query.get("provider") ?? "");
const routeAccountFilter = ref<string>(query.get("route_account") ?? "");
const credentialAccountFilter = ref<string>(query.get("credential_account") ?? "");
const requestIdFilter = ref<string>(query.get("request_id") ?? "");
const querySort = query.get("sort");
const queryOrder = query.get("order");
const sortBy = ref<SortBy>(
  querySort !== null && sortValues.has(querySort as SortBy) ? querySort as SortBy : "timestamp",
);
const sortOrder = ref<SortOrder>(queryOrder === "asc" || queryOrder === "desc" ? queryOrder : "desc");
const advancedFilterCount = computed(() => [
  requestIdFilter.value, accountFilter.value, keyFilter.value, providerFilter.value,
  routeAccountFilter.value, credentialAccountFilter.value,
  sortBy.value !== 'timestamp' ? sortBy.value : '',
].filter(Boolean).length);
const showAdvancedFilters = ref(advancedFilterCount.value > 0);

function parseQueryTimeRange(): [number, number] | null {
  const start = query.get("start");
  const end = query.get("end");
  if (!start || !end) return null;
  const startMs = Date.parse(start);
  const endMs = Date.parse(end);
  if (Number.isNaN(startMs) || Number.isNaN(endMs) || startMs > endMs) return null;
  return [startMs, endMs];
}

const initialTimeRange = parseQueryTimeRange();
const queryPreset = query.get("range");
const initialPreset: TimePreset = initialTimeRange
  ? "custom"
  : queryPreset !== null
      && queryPreset !== "custom"
      && timePresetValues.has(queryPreset as TimePreset)
    ? queryPreset as TimePreset
    : "last24h";
const initialRange = initialTimeRange ?? resolveTimeRange(initialPreset, null);
const timeRange = ref<[number, number] | null>(initialRange);
const activePreset = ref<TimePreset>(initialPreset);
const customTimeRange = ref<[number, number] | null>(initialRange);
const showTimePanel = ref(false);
const forwardPage = ref(1);
const gatewayPage = ref(1);
const pageSize = 20;
const gatewayPagination = computed(() => ({
  page: gatewayPage.value,
  pageSize,
}));
const emptySummary = (): ForwardLogSummary => ({
  total_requests: 0,
  prompt_tokens: 0,
  completion_tokens: 0,
  cached_tokens: 0,
  cost: 0,
});
const forwardTotals = ref<ForwardLogSummary>(emptySummary());
const forwardPagination = computed(() => ({
  page: forwardPage.value,
  pageSize,
  itemCount: forwardTotals.value.total_requests,
}));

const dateFormatter = computed(() => new Intl.DateTimeFormat(locale.value, {
  year: "numeric",
  month: "2-digit",
  day: "2-digit",
  hour: "2-digit",
  minute: "2-digit",
  second: "2-digit",
}));
const dateOnlyFormatter = computed(() => new Intl.DateTimeFormat(locale.value, {
  year: "numeric",
  month: "2-digit",
  day: "2-digit",
}));
const timePresetOptions = computed(() => [
  { label: t("24 小时内"), value: "last24h" as TimePreset },
  { label: t("最近 7 天"), value: "last7d" as TimePreset },
  { label: t("最近 30 天"), value: "last30d" as TimePreset },
  { label: t("本月"), value: "thisMonth" as TimePreset },
  { label: t("上月"), value: "lastMonth" as TimePreset },
  { label: t("全部"), value: "all" as TimePreset },
  { label: t("自定义"), value: "custom" as TimePreset },
]);
const timeRangeLabel = computed(() => {
  if (!timeRange.value || activePreset.value === "all") return t("全部");
  const preset = timePresetOptions.value.find((item) => item.value === activePreset.value);
  if (preset && activePreset.value !== "custom") return preset.label;
  const [start, end] = timeRange.value;
  return `${dateOnlyFormatter.value.format(new Date(start))} ~ ${dateOnlyFormatter.value.format(new Date(end))}`;
});
const statusMeta = computed<Record<string, { label: string; type: "success" | "warning" | "error" | "default" }>>(() => ({
  success: { label: t("成功"), type: "success" },
  success_no_usage: { label: t("成功·无用量"), type: "success" },
  outcome_unknown: { label: t("结果未知"), type: "warning" },
  streaming: { label: t("进行中"), type: "warning" },
  client_error: { label: t("客户端错误"), type: "error" },
  error: { label: t("错误"), type: "error" },
}));
const allOption = computed(() => ({ label: t("全部"), value: "" }));
const statusOptions = computed(() => [allOption.value, ...Object.entries(statusMeta.value).map(([value, meta]) => ({ label: meta.label, value }))]);
const accountOptions = computed(() => [allOption.value, ...accounts.value.map((account) => ({ label: account.name, value: account.id }))]);
const modelOptions = computed(() => [allOption.value, ...models.value.map((model) => ({ label: model, value: model }))]);
// Keys come from the log table itself (not config) so disabled, deleted, and
// dangling ids stay filterable exactly as they appear on the rows.
const keyOptions = computed(() => [
  allOption.value,
  ...clientKeys.value.map((key) => ({ label: key.name, value: key.id })),
  { label: t("未归因"), value: UNATTRIBUTED_KEY_FILTER },
]);
const sortOptions = computed(() => [
  { label: t("时间"), value: "timestamp" },
  { label: t("尝试次数"), value: "attempt" },
  { label: t("输入"), value: "prompt_tokens" },
  { label: t("输出"), value: "completion_tokens" },
  { label: t("缓存"), value: "cached_tokens" },
  { label: t("额度消耗（估算）"), value: "cost" },
]);
// Provider options come from the loaded accounts' provider ids;
// route/credential account options reuse the same account list. All three are
// sent as exact remote query params — rows are never filtered client-side.
const providerOptions = computed(() => [
  allOption.value,
  ...[...new Set(accounts.value.map((account) => account.provider_id).filter(Boolean))]
    .map((providerId) => ({ label: providerId, value: providerId })),
]);
const routeAccountOptions = computed(() => accountOptions.value);
const credentialAccountOptions = computed(() => accountOptions.value);
const hasFilters = computed(() =>
  !!statusFilter.value
  || !!accountFilter.value
  || !!modelFilter.value
  || !!keyFilter.value
  || !!providerFilter.value
  || !!routeAccountFilter.value
  || !!credentialAccountFilter.value
  || !!requestIdFilter.value
  || !!timeRange.value,
);

function formatDate(value: string): string {
  const date = new Date(value);
  return Number.isNaN(date.getTime()) ? value : dateFormatter.value.format(date);
}

function toIsoString(ms: number): string {
  return new Date(ms).toISOString();
}

function applyTimePreset(preset: TimePreset) {
  if (preset === "custom") {
    const currentRange = resolveTimeRange(activePreset.value, timeRange.value);
    activePreset.value = "custom";
    timeRange.value = currentRange;
    customTimeRange.value = currentRange;
    showTimePanel.value = true;
    return;
  }
  if (preset === "all") {
    activePreset.value = "all";
    timeRange.value = null;
    customTimeRange.value = null;
    showTimePanel.value = false;
    return;
  }
  const range = computeTimeRange(preset);
  activePreset.value = preset;
  timeRange.value = range;
  customTimeRange.value = range;
  showTimePanel.value = false;
}

function applyCustomTimeRange(value: [number, number] | null) {
  if (!value) return;
  const start = new Date(value[0]);
  start.setHours(0, 0, 0, 0);
  const end = new Date(value[1]);
  end.setHours(23, 59, 59, 999);
  const range: [number, number] = [start.getTime(), end.getTime()];
  activePreset.value = "custom";
  timeRange.value = range;
  customTimeRange.value = range;
  showTimePanel.value = false;
}

function formatQuotaCost(row: ForwardLog): string {
  if (row.cost_state === "free") {
    return t("免费");
  }
  if (row.cost === null || row.cost_state === "unpriced" || row.cost_state === "outcome_unknown") {
    return "—";
  }
  return formatCost(row.cost, 5);
}

// Original-currency platform token estimate; never a quota debit or wallet
// charge, so it renders in its own column and is never summed with USD.
function formatNativeCost(row: ForwardLog): string {
  const estimate = forwardLogNativeEstimate(row);
  return estimate ? formatNativeCostEstimate(estimate, locale.value) : "—";
}

async function copyText(target: string, value: string, label: string) {
  try {
    await copy(target, value, label);
    message.success(t("已复制 {label}", { label }));
  } catch (e) {
    message.error(e instanceof Error ? e.message : t("复制失败"));
  }
}

function logRowKey(row: GatewayLog | ForwardLog): number {
  return row.id;
}

function focusRequestChain(requestId: string) {
  requestIdFilter.value = requestId;
  sortBy.value = "attempt";
  sortOrder.value = "asc";
}

const logsColumnContext: LogsColumnContext = {
  components: { NButton, NIcon, CheckOutlined, CopyOutlined },
  copiedTarget,
  copyText,
  focusRequestChain,
  accounts,
  catalog: providerCatalog,
};

const gatewayColumns = computed(() => [
  {
    type: "expand" as const,
    width: 44,
    expandable: (row: GatewayLog) => !!row.diagnostic || !!row.error_source,
    renderExpand: renderDiagnostic,
  },
  { title: t("时间"), key: "created_at", width: 150, render: (row: GatewayLog) => formatDate(row.created_at) },
  { title: t("请求 ID"), key: "request_id", width: 170, render: (row: GatewayLog) => renderRequestId(row, logsColumnContext) },
  { title: t("级别"), key: "level", width: 80 },
  { title: t("分类"), key: "category", width: 100 },
  { title: t("消息"), key: "message", minWidth: 480, ellipsis: { tooltip: true }, render: (row: GatewayLog) => gatewayLogMessage(row.message) },
]);
const forwardColumns = computed(() => [
  {
    type: "expand" as const,
    width: 44,
    expandable: () => true,
    renderExpand: (row: ForwardLog) => renderForwardDetail(row, logsColumnContext),
  },
  { title: t("时间"), key: "timestamp", width: 150, render: (row: ForwardLog) => formatDate(row.timestamp) },
  {
    title: t("尝试次数"),
    key: "attempt",
    width: 82,
    align: "center" as const,
    render: (row: ForwardLog) => row.attempt ? `#${row.attempt}` : "—",
  },
  { title: t("模型别名"), key: "model_alias", width: 180, ellipsis: { tooltip: true }, render: (row: ForwardLog) => forwardLogAlias(row) },
  {
    title: t("状态"),
    key: "status",
    width: 112,
    render: (row: ForwardLog) => {
      const sourceLabel = row.error_source === "upstream"
        ? t("上游拒绝")
        : row.error_source === "transport"
          ? t("上游连接错误")
          : row.error_source === "client" || (row.error_source === "gateway" && ["auth", "parse", "validation", "body_limit"].includes(row.error_stage ?? ""))
            ? t("请求错误")
            : row.error_source === "downstream"
              ? t("下游断开")
              : null;
      const presentedStatus = forwardLogPresentedStatus(row.status);
      const meta = sourceLabel
        ? { label: sourceLabel, type: row.error_source === "downstream" ? "warning" as const : "error" as const }
        : statusMeta.value[presentedStatus] ?? { label: presentedStatus, type: "default" as const };
      const tags = [h(NTag, { type: meta.type, size: "small", bordered: false }, { default: () => meta.label })];
      if (row.cost_state === "free") {
        tags.push(h(NTag, { type: "success", size: "small", bordered: false }, { default: () => t("免费") }));
      }
      if (row.cost_state === "legacy_estimate") {
        tags.push(h(NTag, { type: "default", size: "small", bordered: false }, { default: () => t("旧口径") }));
      }
      return h("div", { class: "status-tags" }, tags);
    },
  },
  { title: t("总 Tokens"), key: "total_tokens", width: 110, align: "right" as const, render: (row: ForwardLog) => formatNumber(forwardLogTotalTokens(row)) },
  { title: t("耗时"), key: "latency", width: 100, align: "right" as const, render: (row: ForwardLog) => { const ms = forwardLogLatencyMs(row); return ms === null ? "—" : `${ms} ms`; } },
  { title: "HTTP", key: "http_status", width: 72 },
  { title: t("输入"), key: "prompt_tokens", width: 92, align: "right" as const, render: (row: ForwardLog) => formatNumber(row.prompt_tokens) },
  { title: t("输出"), key: "completion_tokens", width: 92, align: "right" as const, render: (row: ForwardLog) => formatNumber(row.completion_tokens) },
  { title: t("缓存"), key: "cached_tokens", width: 92, align: "right" as const, render: (row: ForwardLog) => formatNumber(row.cached_tokens) },
  { title: t("缓存写"), key: "cache_creation_tokens", width: 92, align: "right" as const, render: (row: ForwardLog) => formatNumber(row.cache_creation_tokens) },
  { title: t("额度消耗（估算）"), key: "cost", width: 152, align: "right" as const, render: formatQuotaCost },
  { title: t("平台估算（原始货币）"), key: "native_cost", width: 150, align: "right" as const, render: formatNativeCost },
  { title: t("错误"), key: "error_message", minWidth: 220, ellipsis: { tooltip: true } },
]);

function clearFilters() {
  statusFilter.value = "";
  accountFilter.value = "";
  modelFilter.value = "";
  keyFilter.value = "";
  providerFilter.value = "";
  routeAccountFilter.value = "";
  credentialAccountFilter.value = "";
  requestIdFilter.value = "";
  activePreset.value = "all";
  timeRange.value = null;
  customTimeRange.value = null;
  showTimePanel.value = false;
}

function toggleSortOrder() {
  sortOrder.value = sortOrder.value === "asc" ? "desc" : "asc";
}

function syncQueryState() {
  const query: Record<string, string> = { tab: activeTab.value };
  if (statusFilter.value) query.status = statusFilter.value;
  if (accountFilter.value) query.account = accountFilter.value;
  if (modelFilter.value) query.model = modelFilter.value;
  if (keyFilter.value) query.key = keyFilter.value;
  if (providerFilter.value) query.provider = providerFilter.value;
  if (routeAccountFilter.value) query.route_account = routeAccountFilter.value;
  if (credentialAccountFilter.value) query.credential_account = credentialAccountFilter.value;
  if (requestIdFilter.value) query.request_id = requestIdFilter.value;
  if (activePreset.value === "custom" && timeRange.value) {
    query.start = toIsoString(timeRange.value[0]);
    query.end = toIsoString(timeRange.value[1]);
  } else {
    query.range = activePreset.value;
  }
  if (sortBy.value) query.sort = sortBy.value;
  if (sortOrder.value) query.order = sortOrder.value;
  void router.replace({ query });
}

let gatewayRequest = 0;
let gatewayLoadedAt = 0;
// Auto-refresh on activation skips resources loaded recently, and never
// duplicates a load that is already in flight (the loading flags cover
// user-driven triggers too, since those carry the current filters).
const ACTIVATED_REFRESH_FRESHNESS_MS = 30_000;

async function loadGatewayLogs() {
  const request = ++gatewayRequest;
  gatewayLoading.value = true;
  gatewayError.value = "";
  try {
    const logs = await dashboardApi.getGatewayLogs(200, requestIdFilter.value);
    if (request !== gatewayRequest) return;
    gatewayLogs.value = logs;
    gatewayLoadedAt = Date.now();
    gatewayPage.value = 1;
  } catch (e) {
    if (request === gatewayRequest) {
      gatewayError.value = e instanceof Error ? e.message : String(e);
      message.error(t("加载运行日志失败：{error}", { error: gatewayError.value }));
    }
  } finally {
    if (request === gatewayRequest) gatewayLoading.value = false;
  }
}

let forwardRequest = 0;
let forwardLoadedAt = 0;

async function loadForwardLogs() {
  const request = ++forwardRequest;
  forwardLoading.value = true;
  forwardError.value = "";
  forwardLogs.value = [];
  forwardTotals.value = emptySummary();
  try {
    const requestRange = resolveTimeRange(activePreset.value, timeRange.value);
    const result = await dashboardApi.getForwardLogs({
      limit: pageSize,
      offset: (forwardPage.value - 1) * pageSize,
      status: statusFilter.value,
      account_id: accountFilter.value,
      model: modelFilter.value,
      key_id: keyFilter.value,
      provider_id: providerFilter.value,
      route_account_id: routeAccountFilter.value,
      credential_account_id: credentialAccountFilter.value,
      request_id: requestIdFilter.value,
      start_time: requestRange ? toIsoString(requestRange[0]) : null,
      end_time: requestRange ? toIsoString(requestRange[1]) : null,
      sort_by: sortBy.value,
      sort_order: sortOrder.value,
    });
    if (request !== forwardRequest) return;
    forwardLogs.value = result.items;
    forwardTotals.value = result.summary;
    forwardLoadedAt = Date.now();
  } catch (e) {
    if (request === forwardRequest) {
      forwardLogs.value = [];
      forwardTotals.value = emptySummary();
      forwardError.value = dashboardErrorDetail(e);
      message.error(t("加载请求日志失败：{error}", { error: forwardError.value }));
    }
  } finally {
    if (request === forwardRequest) forwardLoading.value = false;
  }
}

async function loadAccounts() {
  try {
    await accountsStore.loadPresented();
  } catch (e) {
    message.error(t("加载账号筛选失败：{error}", { error: String(e) }));
  }
}

async function loadForwardLogModels() {
  try {
    models.value = await dashboardApi.getForwardLogModels();
  } catch (e) {
    message.error(t("加载模型筛选失败：{error}", { error: String(e) }));
  }
}

async function loadForwardLogKeys() {
  try {
    clientKeys.value = await dashboardApi.getForwardLogKeys();
  } catch (e) {
    message.error(t("加载 Key 筛选失败：{error}", { error: String(e) }));
  }
}

async function loadProviderCatalog() {
  try {
    await providersStore.loadCatalog();
  } catch {
    // Catalog failure only disables plan labels; logs remain usable.
  }
}

async function refreshForwardLogs() {
  await Promise.all([loadForwardLogs(), loadForwardLogModels(), loadForwardLogKeys()]);
}

function changeForwardPage(page: number) {
  forwardPage.value = page;
  void loadForwardLogs();
}

function changeGatewayPage(page: number) {
  gatewayPage.value = page;
}

watch(activeTab, syncQueryState);
watch(
  [statusFilter, accountFilter, modelFilter, keyFilter, providerFilter, routeAccountFilter, credentialAccountFilter, timeRange, activePreset, sortBy, sortOrder],
  () => {
    forwardPage.value = 1;
    syncQueryState();
    void loadForwardLogs();
  },
);
// Typing a request id fires both list loads; debounce so each keystroke
// batch turns into at most one round-trip per list.
let requestIdDebounce: ReturnType<typeof setTimeout> | null = null;
watch(requestIdFilter, () => {
  if (requestIdDebounce !== null) clearTimeout(requestIdDebounce);
  requestIdDebounce = setTimeout(() => {
    requestIdDebounce = null;
    forwardPage.value = 1;
    gatewayPage.value = 1;
    syncQueryState();
    void loadForwardLogs();
    void loadGatewayLogs();
  }, 300);
});
onUnmounted(() => {
  if (requestIdDebounce !== null) clearTimeout(requestIdDebounce);
});

let activatedOnce = false;
// Logs accumulate server-side while another tab is active (this view is kept
// alive by App.vue); refresh both lists when returning, keeping filters.
onActivated(() => {
  if (activatedOnce) {
    // Only the automatic refresh is gated; user actions (search, filters,
    // paging, refresh buttons) call the loaders directly and stay immediate.
    if (!gatewayLoading.value && Date.now() - gatewayLoadedAt >= ACTIVATED_REFRESH_FRESHNESS_MS) {
      void loadGatewayLogs();
    }
    if (!forwardLoading.value && Date.now() - forwardLoadedAt >= ACTIVATED_REFRESH_FRESHNESS_MS) {
      void loadForwardLogs();
    }
  } else {
    activatedOnce = true;
  }
});

onMounted(() => {
  syncQueryState();
  void loadGatewayLogs();
  void loadForwardLogs();
  void loadAccounts();
  void loadForwardLogModels();
  void loadForwardLogKeys();
  void loadProviderCatalog();
});

onUnmounted(cleanup);
</script>

<style scoped>
.key-filter-note {
  margin: -6px 0 10px;
  color: var(--ocg-subtle);
  font-size: var(--ocg-font-xs);
}
.log-limit-note {
  margin: 6px 0 10px;
  color: var(--ocg-subtle);
  font-size: var(--ocg-font-xs);
}

.logs-card {
  max-width: 1480px;
  margin: 0 auto;
  padding: var(--ocg-space-xs) 18px 18px;
  border: 1px solid var(--ocg-border);
  border-radius: var(--ocg-radius-lg);
  background: var(--ocg-surface);
  box-shadow: var(--ocg-shadow-sm);
}
.stats-row {
  display: grid;
  grid-template-columns: repeat(5, 1fr);
  gap: var(--ocg-space-md);
  margin-bottom: var(--ocg-space-lg);
}
.stat-card {
  padding: var(--ocg-space-md) 14px;
  border: 1px solid var(--ocg-border);
  border-radius: var(--ocg-radius-md);
  background: var(--ocg-surface);
}
.stat-label {
  margin-bottom: 6px;
  font-size: var(--ocg-font-xs);
  color: var(--ocg-muted);
}
.stat-value {
  font-size: var(--ocg-font-xl);
  font-weight: 600;
  color: var(--ocg-ink);
  white-space: nowrap;
  overflow: hidden;
  text-overflow: ellipsis;
}
.filter-bar {
  display: flex;
  flex-wrap: wrap;
  align-items: flex-end;
  gap: var(--ocg-space-sm);
  margin-bottom: var(--ocg-space-md);
}
.filter-field {
  display: flex;
  flex-direction: column;
  gap: var(--ocg-space-xs);
  flex: 1 1 160px;
  min-width: 0;
}
.filter-field.request-id-field {
  flex: 2 1 220px;
  max-width: 320px;
}
.filter-field.time-range-field {
  flex: 1 1 200px;
}
.filter-actions {
  display: flex;
  gap: var(--ocg-space-xs);
  flex: 0 0 auto;
  margin-left: auto;
}
.filter-label {
  font-size: var(--ocg-font-xs);
  color: var(--ocg-subtle);
  line-height: 1.2;
}
.time-range-trigger {
  width: 100%;
  min-width: 120px;
  max-width: 240px;
  justify-content: flex-start;
}
.time-range-trigger :deep(.n-button__content) {
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}
.time-range-panel {
  display: inline-flex;
  flex-direction: row;
  gap: var(--ocg-space-sm);
  max-width: calc(100vw - 48px);
}
.preset-list {
  display: flex;
  flex-direction: column;
  gap: 2px;
  min-width: 100px;
}
.preset-item {
  justify-content: flex-start;
}
.preset-item :deep(.n-button__content) {
  white-space: nowrap;
}
.custom-range-wrapper {
  display: flex;
  flex-direction: column;
  gap: var(--ocg-space-sm);
  width: auto;
  max-width: 0;
  opacity: 0;
  overflow: hidden;
  transition: max-width 0.2s ease, opacity 0.2s ease;
  border-left: 1px solid transparent;
}
.custom-range-wrapper.is-visible {
  max-width: 600px;
  opacity: 1;
  padding-left: var(--ocg-space-sm);
  border-left-color: var(--ocg-border);
}
.custom-range-title {
  font-size: var(--ocg-font-sm);
  color: var(--ocg-muted);
  white-space: nowrap;
}
.custom-time-picker {
  min-width: 0;
}
.custom-time-picker :deep(.n-date-panel) {
  box-shadow: none;
  background: transparent;
}
.custom-time-picker :deep(.n-date-panel-header),
.custom-time-picker :deep(.n-date-panel-calendar__picker-col),
.custom-time-picker :deep(.n-date-panel-actions) {
  background: transparent;
}
.sort-select {
  min-width: 110px;
}
.log-toolbar,
.filter-actions {
  display: flex;
  justify-content: flex-end;
  gap: var(--ocg-space-xs);
}
.log-toolbar {
  margin-bottom: var(--ocg-space-sm);
}
.request-id-filter {
  width: min(360px, 100%);
  margin-right: auto;
}
:deep(.request-id-cell) {
  display: flex;
  align-items: center;
  gap: 6px;
}
:deep(.request-id-cell code) {
  overflow: hidden;
  font: var(--ocg-font-xs)/1.4 "Cascadia Mono", Consolas, monospace;
  text-overflow: ellipsis;
  white-space: nowrap;
}
:deep(.status-tags) {
  display: flex;
  flex-wrap: wrap;
  gap: 3px;
}
.diagnostic-detail {
  display: grid;
  gap: var(--ocg-space-md);
  padding: var(--ocg-space-sm) 0;
}
.diagnostic-detail h4 {
  margin: 0 0 5px;
  color: var(--ocg-muted);
  font-size: var(--ocg-font-sm);
}
.diagnostic-meta {
  display: grid;
  grid-template-columns: max-content minmax(120px, 1fr) max-content minmax(120px, 1fr);
  gap: 5px var(--ocg-space-md);
  margin: 0;
}
.diagnostic-meta dt {
  color: var(--ocg-subtle);
}
.diagnostic-meta dd {
  margin: 0;
  font-family: "Cascadia Mono", Consolas, monospace;
  word-break: break-word;
}
.diagnostic-json,
.error-text {
  margin: 0;
  padding: 10px var(--ocg-space-md);
  border: 1px solid var(--ocg-border);
  border-radius: var(--ocg-radius-sm);
  background: var(--ocg-canvas);
  color: var(--ocg-ink);
  font-family: "Cascadia Mono", Consolas, monospace;
  font-size: var(--ocg-font-sm);
  line-height: 1.5;
  white-space: pre-wrap;
  word-break: break-word;
}
.diagnostic-json {
  max-height: 320px;
  overflow: auto;
}

.advanced-filter-toggle { display: inline-flex; margin-bottom: var(--ocg-space-md); }
.filter-bar:not(.show-advanced) .advanced-filter { display: none; }

@media (max-width: 860px) {
  .time-range-panel {
    flex-direction: column;
  }
  .custom-range-wrapper.is-visible {
    width: auto;
    max-width: 100%;
    border-left: none;
    border-top: 1px solid var(--ocg-border);
    padding-left: 0;
    padding-top: var(--ocg-space-sm);
  }
  .custom-time-picker {
    overflow-x: auto;
  }
}

@media (max-width: 760px) {
  .stats-row {
    grid-template-columns: repeat(3, 1fr);
    gap: var(--ocg-space-sm);
  }
}

@media (max-width: 560px) {
  .stat-card { padding: 10px; }
  .stats-row { margin-bottom: var(--ocg-space-md); }
  .logs-card {
    padding: 2px var(--ocg-space-md) var(--ocg-space-md);
  }
  .stats-row {
    grid-template-columns: repeat(3, minmax(0, 1fr));
  }
  .filter-field {
    flex: 1 1 140px;
  }
  .filter-actions {
    margin-left: 0;
  }
}
</style>
