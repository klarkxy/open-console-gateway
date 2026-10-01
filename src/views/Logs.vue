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
          <n-select
            v-model:value="gatewayLevelFilter"
            class="gateway-level-filter"
            :options="gatewayLevelOptions"
            :aria-label="t('级别')"
          />
          <n-input
            v-model:value="gatewayCategoryFilter"
            clearable
            class="gateway-category-filter"
            :placeholder="t('按分类精确搜索')"
            :input-props="{ 'aria-label': t('分类') }"
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
          :loading="gatewayLoading && !gatewayLoaded"
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
          :loading="forwardLoading && !forwardLoaded"
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
import { computed, h, nextTick, onActivated, onMounted, onUnmounted, ref, watch } from "vue";
import { useRoute, useRouter, onBeforeRouteUpdate, type LocationQuery } from "vue-router";
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
import { UNATTRIBUTED_KEY_FILTER } from "../api/dashboard";
import type {
  ForwardLog,
  GatewayLog,
} from "../api/dashboard";
import { t } from "../i18n/index.ts";
import { locale } from "../i18n/index.ts";
import { useAccountsStore } from "../stores/accounts.ts";
import { useProvidersStore } from "../stores/providers.ts";
import { useObservabilityStore } from "../stores/observability.ts";
import { storeToRefs } from "pinia";
import { formatCost, formatNumber, useClipboard } from "../utils/format.ts";
import { computeTimeRange, resolveTimeRange, timePresetValues } from "./log-time-range.ts";
import { routeQuerySearch } from "./app-navigation.ts";
import type { TimePreset } from "./log-time-range.ts";
import { gatewayLogMessage } from "./gateway-log-message.ts";
import { gatewayLogLevelTag, parseGatewayLogLevel, type GatewayLogLevel } from "./gateway-log-level.ts";
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
const message = useMessage();
const accountsStore = useAccountsStore();
const providersStore = useProvidersStore();
const observabilityStore = useObservabilityStore();
const {
  gatewayLogs, gatewayLoaded, gatewayLoading, gatewayError, gatewayLoadedAt,
  forwardLogs, forwardTotals, forwardLoaded, forwardLoading, forwardError, forwardLoadedAt,
  models, clientKeys,
} = storeToRefs(observabilityStore);
const { copiedTarget, copy, cleanup } = useClipboard();
const activeTab = ref<LogTab>("forward");
const accounts = computed(() => accountsStore.accounts);
const providerCatalog = computed(() => providersStore.catalog);
const statusFilter = ref<string>("");
const accountFilter = ref<string>("");
const modelFilter = ref<string>("");
const keyFilter = ref<string>("");
const providerFilter = ref<string>("");
const routeAccountFilter = ref<string>("");
const credentialAccountFilter = ref<string>("");
const requestIdFilter = ref<string>("");
const gatewayLevelFilter = ref<GatewayLogLevel>("");
const gatewayCategoryFilter = ref("");
const sortBy = ref<SortBy>("timestamp");
const sortOrder = ref<SortOrder>("desc");
const advancedFilterCount = computed(() => [
  requestIdFilter.value, accountFilter.value, keyFilter.value, providerFilter.value,
  routeAccountFilter.value, credentialAccountFilter.value,
  sortBy.value !== 'timestamp' ? sortBy.value : '',
].filter(Boolean).length);
const timeRange = ref<[number, number] | null>(resolveTimeRange("last24h", null));
const activePreset = ref<TimePreset>("last24h");
const customTimeRange = ref<[number, number] | null>(timeRange.value);
const showTimePanel = ref(false);

function parseQueryTimeRange(params: URLSearchParams): [number, number] | null {
  const start = params.get("start");
  const end = params.get("end");
  if (!start || !end) return null;
  const startMs = Date.parse(start);
  const endMs = Date.parse(end);
  if (Number.isNaN(startMs) || Number.isNaN(endMs) || startMs > endMs) return null;
  return [startMs, endMs];
}

function sameTimeRange(a: [number, number] | null, b: [number, number] | null): boolean {
  return a === b || (a !== null && b !== null && a[0] === b[0] && a[1] === b[1]);
}

// Restore filter state from a logs URL. Setup runs this once for the inbound
// deep link; the route watcher further down reuses it for same-instance
// navigations. Values that round-trip our own syncQueryState writes compare
// equal and are kept as-is, so router.replace echoes never look like changes.
function applyRouteQuery(search: string): void {
  const params = new URLSearchParams(search);
  activeTab.value = params.get("tab") === "gateway" ? "gateway" : "forward";
  const status = params.get("status") ?? "";
  statusFilter.value = status === "success_unpriced" ? "success" : status;
  accountFilter.value = params.get("account") ?? "";
  modelFilter.value = params.get("model") ?? "";
  keyFilter.value = params.get("key") ?? "";
  providerFilter.value = params.get("provider") ?? "";
  routeAccountFilter.value = params.get("route_account") ?? "";
  credentialAccountFilter.value = params.get("credential_account") ?? "";
  requestIdFilter.value = params.get("request_id") ?? "";
  gatewayLevelFilter.value = parseGatewayLogLevel(params.get("level"));
  gatewayCategoryFilter.value = params.get("category") ?? "";
  const sort = params.get("sort");
  sortBy.value = sort !== null && sortValues.has(sort as SortBy) ? sort as SortBy : "timestamp";
  const order = params.get("order");
  sortOrder.value = order === "asc" || order === "desc" ? order : "desc";
  const queryRange = parseQueryTimeRange(params);
  const preset = params.get("range");
  const nextPreset: TimePreset = queryRange
    ? "custom"
    : preset !== null
        && preset !== "custom"
        && timePresetValues.has(preset as TimePreset)
      ? preset as TimePreset
      : "last24h";
  // A preset window re-anchors only when the preset changes; loaders resolve
  // the live window from the preset, so the stored range is just a display
  // anchor and reusing it avoids echo writes that would read as a change.
  const nextRange = queryRange
    ?? (nextPreset === activePreset.value ? timeRange.value : resolveTimeRange(nextPreset, null));
  activePreset.value = nextPreset;
  if (!sameTimeRange(timeRange.value, nextRange)) timeRange.value = nextRange;
  if (!sameTimeRange(customTimeRange.value, nextRange)) customTimeRange.value = nextRange;
}

applyRouteQuery(routeQuerySearch("logs", route.query));
const showAdvancedFilters = ref(advancedFilterCount.value > 0);
const forwardPage = ref(1);
const gatewayPage = ref(1);
const pageSize = 20;
const gatewayPagination = computed(() => ({
  page: gatewayPage.value,
  pageSize,
}));
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
const gatewayLevelOptions = computed(() => [allOption.value, ...(["TRACE", "DEBUG", "INFO", "WARN", "ERROR"] as const)
  .map((value) => ({ label: value, value }))]);
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
  { label: t("旧口径"), value: "cost" },
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
  const cost = row.cost;
  if (
    cost === null
    || row.cost_state === "free"
    || row.cost_state === "unpriced"
    || row.cost_state === "outcome_unknown"
    || !Number.isFinite(cost)
    || cost <= 0
  ) {
    return "—";
  }
  return formatCost(cost, 5);
}

// Stored native amount from an earlier record. Missing and non-positive values stay blank.
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
  { title: t("级别"), key: "level", width: 90, render: (row: GatewayLog) => h(NTag, {
    type: gatewayLogLevelTag(row.level), size: "small", bordered: false,
  }, { default: () => row.level }) },
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
      if (row.cost_state === "legacy_estimate" && formatQuotaCost(row) !== "—") {
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
  { title: t("旧口径"), key: "cost", width: 152, align: "right" as const, render: formatQuotaCost },
  { title: t("原始供应商成本"), key: "native_cost", width: 150, align: "right" as const, render: formatNativeCost },
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
  if (gatewayLevelFilter.value) query.level = gatewayLevelFilter.value;
  if (gatewayCategoryFilter.value.trim()) query.category = gatewayCategoryFilter.value.trim();
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

// Query keys this view writes and reads back; a navigation carrying any of
// them is a logs deep link that owns the filter state.
const LOGS_QUERY_KEYS = [
  "tab", "status", "account", "model", "key", "provider", "route_account",
  "credential_account", "request_id", "level", "category", "start", "end",
  "range", "sort", "order",
];

// Set while a route-driven restore assigns filters; the filter watchers skip
// their own sync/load then, so one navigation commits one consolidated round
// of requests instead of one per watcher.
let applyingRouteQuery = false;

function forwardQuerySignature(): string {
  const range = timeRange.value;
  return [
    statusFilter.value, accountFilter.value, modelFilter.value, keyFilter.value,
    providerFilter.value, routeAccountFilter.value, credentialAccountFilter.value,
    requestIdFilter.value, activePreset.value,
    range ? `${range[0]}:${range[1]}` : "", sortBy.value, sortOrder.value,
  ].join(" ");
}

function gatewayQuerySignature(): string {
  return [gatewayLevelFilter.value, gatewayCategoryFilter.value, requestIdFilter.value].join(" ");
}

// KeepAlive reuses this instance across /logs navigations, so the URL can
// change under a live view (deep links, Back/Forward, leaving and returning
// with a different query). Restore the filters an inbound query carries and
// reload once per real change; navigations without logs params (plain menu
// revisits) and round-trips of our own writes keep local filters untouched.
// Neither path writes back to the router — UI→URL sync stays with the filter
// watchers — so an inbound navigation cannot loop or strand a pending replace
// that would collide with a following Back/Forward.
function applyInboundQuery(query: LocationQuery): void {
  if (!LOGS_QUERY_KEYS.some((key) => key in query)) return;
  const tabBefore = activeTab.value;
  const forwardBefore = forwardQuerySignature();
  const gatewayBefore = gatewayQuerySignature();
  applyingRouteQuery = true;
  applyRouteQuery(routeQuerySearch("logs", query));
  void nextTick(() => {
    applyingRouteQuery = false;
  });
  const tabChanged = activeTab.value !== tabBefore;
  const forwardChanged = forwardQuerySignature() !== forwardBefore;
  const gatewayChanged = gatewayQuerySignature() !== gatewayBefore;
  if (!tabChanged && !forwardChanged && !gatewayChanged) return;
  if (forwardChanged) forwardPage.value = 1;
  if (gatewayChanged) gatewayPage.value = 1;
  if ((tabChanged && activeTab.value === "forward") || forwardChanged) void loadForwardLogs();
  if ((tabChanged && activeTab.value === "gateway") || gatewayChanged) void loadGatewayLogs();
}

// Same-record navigations (another /logs link, Back/Forward between logs
// entries) are applied from the update guard, which runs while the navigation
// is still resolving instead of only after the route commits.
onBeforeRouteUpdate((to) => {
  applyInboundQuery(to.query);
});

// Re-entry (leave, then return with a different query) is not an update of
// the cached record, so the watcher covers it after the route commits; the
// signature check keeps it from duplicating a navigation the guard applied.
watch(() => route.query, (query) => {
  if (route.name !== "logs") return;
  applyInboundQuery(query);
});

// Auto-refresh on activation skips resources loaded recently, and never
// duplicates a load that is already in flight (the loading flags cover
// user-driven triggers too, since those carry the current filters).
const ACTIVATED_REFRESH_FRESHNESS_MS = 30_000;

async function loadGatewayLogs() {
  const error = await observabilityStore.loadGateway({
    limit: 200,
    requestId: requestIdFilter.value || null,
    level: gatewayLevelFilter.value || null,
    category: gatewayCategoryFilter.value.trim() || null,
  });
  if (error) message.error(t("加载运行日志失败：{error}", { error }));
}

async function loadForwardLogs() {
  const requestRange = resolveTimeRange(activePreset.value, timeRange.value);
  const error = await observabilityStore.loadForward({
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
  if (error) message.error(t("加载请求日志失败：{error}", { error }));
}

async function loadAccounts() {
  try {
    await accountsStore.loadPresented();
  } catch (e) {
    message.error(t("加载账号筛选失败：{error}", { error: String(e) }));
  }
}

async function loadForwardLogModels() {
  const error = await observabilityStore.loadModels();
  if (error) message.error(t("加载模型筛选失败：{error}", { error }));
}

async function loadForwardLogKeys() {
  const error = await observabilityStore.loadKeys();
  if (error) message.error(t("加载 Key 筛选失败：{error}", { error }));
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

watch(gatewayLevelFilter, () => {
  if (applyingRouteQuery) return;
  gatewayPage.value = 1;
  syncQueryState();
  void loadGatewayLogs();
});

let categoryDebounce: ReturnType<typeof setTimeout> | null = null;
watch(gatewayCategoryFilter, () => {
  if (applyingRouteQuery) return;
  if (categoryDebounce !== null) clearTimeout(categoryDebounce);
  gatewayPage.value = 1;
  categoryDebounce = setTimeout(() => {
    categoryDebounce = null;
    syncQueryState();
    void loadGatewayLogs();
  }, 300);
});

watch(activeTab, (tab) => {
  if (applyingRouteQuery) return;
  syncQueryState();
  // The shared request ID may have changed while this tab was hidden.
  if (tab === "gateway") void loadGatewayLogs();
  if (tab === "forward") void loadForwardLogs();
});
watch(
  [statusFilter, accountFilter, modelFilter, keyFilter, providerFilter, routeAccountFilter, credentialAccountFilter, timeRange, activePreset, sortBy, sortOrder],
  () => {
    if (applyingRouteQuery) return;
    forwardPage.value = 1;
    syncQueryState();
    void loadForwardLogs();
  },
);
// Typing a request id fires both list loads; debounce so each keystroke
// batch turns into at most one round-trip per list.
let requestIdDebounce: ReturnType<typeof setTimeout> | null = null;
watch(requestIdFilter, () => {
  if (applyingRouteQuery) return;
  if (requestIdDebounce !== null) clearTimeout(requestIdDebounce);
  forwardPage.value = 1;
  gatewayPage.value = 1;
  requestIdDebounce = setTimeout(() => {
    requestIdDebounce = null;
    syncQueryState();
    void loadForwardLogs();
    void loadGatewayLogs();
  }, 300);
});
onUnmounted(() => {
  if (requestIdDebounce !== null) clearTimeout(requestIdDebounce);
  if (categoryDebounce !== null) clearTimeout(categoryDebounce);
});

let activatedOnce = false;
// Logs accumulate server-side while another tab is active (this view is kept
// alive by App.vue); refresh both lists when returning, keeping filters.
onActivated(() => {
  if (activatedOnce) {
    // Only the automatic refresh is gated; user actions (search, filters,
    // paging, refresh buttons) call the loaders directly and stay immediate.
    if (activeTab.value === "gateway" && !gatewayLoading.value && Date.now() - gatewayLoadedAt.value >= ACTIVATED_REFRESH_FRESHNESS_MS) {
      void loadGatewayLogs();
    }
    if (activeTab.value === "forward" && !forwardLoading.value && Date.now() - forwardLoadedAt.value >= ACTIVATED_REFRESH_FRESHNESS_MS) {
      void loadForwardLogs();
    }
  } else {
    activatedOnce = true;
  }
});

onMounted(() => {
  syncQueryState();
  if (activeTab.value === "gateway") void loadGatewayLogs();
  if (activeTab.value === "forward") void loadForwardLogs();
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
.gateway-level-filter {
  width: 130px;
}
.gateway-category-filter {
  width: min(220px, 100%);
}
@media (max-width: 650px) {
  .log-toolbar {
    flex-wrap: wrap;
  }
  .request-id-filter {
    width: 100%;
  }
  .gateway-category-filter {
    flex: 1 1 140px;
  }
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
