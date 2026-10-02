<template>
  <section class="logs-card">
    <n-tabs v-model:value="activeTab" type="line" animated>
      <n-tab-pane name="requests" :tab="t('逻辑请求')">
        <p class="log-limit-note">{{ t("逻辑请求说明") }}</p>
        <div class="stats-row">
          <div class="stat-card">
            <div class="stat-label">{{ t("逻辑请求数") }}</div>
            <div class="stat-value">{{ formatNumber(requestUsage.totalRequests) }}</div>
          </div>
          <div class="stat-card">
            <div class="stat-label">{{ t("上游尝试") }}</div>
            <div class="stat-value">{{ formatNumber(requestUsage.totalAttempts) }}</div>
          </div>
          <div class="stat-card">
            <div class="stat-label">{{ t("输入") }}</div>
            <div class="stat-value">{{ formatNumber(requestUsage.inputTokens) }}</div>
          </div>
          <div class="stat-card">
            <div class="stat-label">{{ t("输出") }}</div>
            <div class="stat-value">{{ formatNumber(requestUsage.outputTokens) }}</div>
          </div>
          <div class="stat-card">
            <div class="stat-label">{{ t("缓存已包含在输入中") }}</div>
            <div class="stat-value">{{ formatNumber(requestUsage.cachedTokens) }}</div>
          </div>
          <div class="stat-card">
            <div class="stat-label">{{ t("总 Tokens") }}</div>
            <div class="stat-value">{{ formatNumber(requestUsage.totalTokens) }}</div>
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
            <n-select v-model:value="statusFilter" :options="requestStatusOptions" :placeholder="t('状态')" :aria-label="t('状态')" />
          </div>
          <div class="filter-field advanced-filter">
            <span class="filter-label">{{ t("账号") }}</span>
            <n-select v-model:value="accountFilter" :options="accountOptions" :placeholder="t('账号')" :aria-label="t('账号')" />
          </div>
          <div class="filter-field">
            <span class="filter-label">{{ t("模型") }}</span>
            <n-select v-model:value="modelFilter" :options="modelOptions" :placeholder="t('模型')" :aria-label="t('模型')" />
          </div>
          <div class="filter-field key-filter-field advanced-filter">
            <span class="filter-label">{{ t("接入 Key") }}</span>
            <n-select v-model:value="keyFilter" :options="keyOptions" :placeholder="t('接入 Key')" :aria-label="t('接入 Key')" :consistent-menu-width="false" />
          </div>
          <div class="filter-field advanced-filter">
            <span class="filter-label">{{ t("服务商") }}</span>
            <n-select v-model:value="providerFilter" :options="providerOptions" :placeholder="t('服务商')" :aria-label="t('服务商')" :consistent-menu-width="false" />
          </div>
          <div class="filter-field advanced-filter">
            <span class="filter-label">{{ t("路由账号") }}</span>
            <n-select v-model:value="routeAccountFilter" :options="routeAccountOptions" :placeholder="t('路由账号')" :aria-label="t('路由账号')" :consistent-menu-width="false" />
          </div>
          <div class="filter-field advanced-filter">
            <span class="filter-label">{{ t("凭证账号") }}</span>
            <n-select v-model:value="credentialAccountFilter" :options="credentialAccountOptions" :placeholder="t('凭证账号')" :aria-label="t('凭证账号')" :consistent-menu-width="false" />
          </div>
          <div class="filter-field time-range-field">
            <span class="filter-label">{{ t("时间范围") }}</span>
            <n-popover trigger="click" placement="bottom-start" :show="showTimePanel" @update:show="showTimePanel = $event">
              <template #trigger>
                <n-button class="time-range-trigger">
                  <template #icon><n-icon :component="CalendarOutlined" /></template>
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
                <div class="custom-range-wrapper" :class="{ 'is-visible': activePreset === 'custom' }">
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
          <div class="filter-actions">
            <n-tooltip v-if="hasRequestFilters" trigger="hover">
              <template #trigger>
                <n-button circle quaternary :aria-label="t('清除筛选')" @click="clearRequestFilters">
                  <template #icon><n-icon :component="ClearOutlined" /></template>
                </n-button>
              </template>
              {{ t("清除筛选") }}
            </n-tooltip>
            <n-tooltip trigger="hover">
              <template #trigger>
                <n-button circle quaternary :loading="requestLoading" :aria-label="t('刷新逻辑请求')" @click="refreshRequestLogs">
                  <template #icon><n-icon :component="ReloadOutlined" /></template>
                </n-button>
              </template>
              {{ t("刷新逻辑请求") }}
            </n-tooltip>
          </div>
        </div>
        <p v-if="keyFilter" class="key-filter-note" role="status">{{ t("升级前用量统一计入主 Key") }}</p>
        <n-alert v-if="requestError" type="error" :title="t('加载逻辑请求失败：{error}', { error: requestError })">
          <n-button size="small" secondary @click="loadRequestLogs">{{ t("重试") }}</n-button>
        </n-alert>
        <div class="requests-table">
          <n-data-table
            :columns="requestColumns"
            :data="requestLogs"
            :row-key="requestRowKey"
            :loading="showResourceSkeleton(requestLoading, requestLoaded)"
            :pagination="requestPagination"
            :expanded-row-keys="expandedRequestKeys"
            :scroll-x="1680"
            remote
            size="small"
            @update:page="changeRequestPage"
            @update:expanded-row-keys="onRequestExpanded"
          >
            <template #empty>
              <n-empty :description="t('暂无逻辑请求')" />
            </template>
          </n-data-table>
        </div>
      </n-tab-pane>
      <n-tab-pane name="operations" :tab="t('用户操作')">
        <p class="log-limit-note">{{ t("用户操作说明") }}</p>
        <div class="filter-bar">
          <div class="filter-field">
            <span class="filter-label">{{ t("操作结果") }}</span>
            <n-select v-model:value="outcomeFilter" :options="outcomeOptions" :aria-label="t('操作结果')" />
          </div>
          <div class="filter-field">
            <span class="filter-label">{{ t("来源") }}</span>
            <n-select v-model:value="sourceFilter" :options="sourceOptions" :aria-label="t('来源')" />
          </div>
          <div class="filter-field">
            <span class="filter-label">{{ t("动作码") }}</span>
            <n-input v-model:value="actionFilter" clearable :placeholder="t('动作码')" :input-props="{ 'aria-label': t('动作码') }" />
          </div>
          <div class="filter-field">
            <span class="filter-label">{{ t("主体类型") }}</span>
            <n-input v-model:value="subjectTypeFilter" clearable :input-props="{ 'aria-label': t('主体类型') }" />
          </div>
          <div class="filter-field">
            <span class="filter-label">{{ t("主体标识") }}</span>
            <n-input v-model:value="subjectIdFilter" clearable :input-props="{ 'aria-label': t('主体标识') }" />
          </div>
          <div class="filter-field time-range-field">
            <span class="filter-label">{{ t("时间范围") }}</span>
            <n-popover trigger="click" placement="bottom-start" :show="showOperationTimePanel" @update:show="showOperationTimePanel = $event">
              <template #trigger>
                <n-button class="time-range-trigger">
                  <template #icon><n-icon :component="CalendarOutlined" /></template>
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
                <div class="custom-range-wrapper" :class="{ 'is-visible': activePreset === 'custom' }">
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
          <div class="filter-actions">
            <n-tooltip v-if="hasOperationFilters" trigger="hover">
              <template #trigger>
                <n-button circle quaternary :aria-label="t('清除筛选')" @click="clearOperationFilters">
                  <template #icon><n-icon :component="ClearOutlined" /></template>
                </n-button>
              </template>
              {{ t("清除筛选") }}
            </n-tooltip>
            <n-tooltip trigger="hover">
              <template #trigger>
                <n-button circle quaternary :loading="operationLoading" :aria-label="t('刷新用户操作')" @click="loadOperationLogs">
                  <template #icon><n-icon :component="ReloadOutlined" /></template>
                </n-button>
              </template>
              {{ t("刷新用户操作") }}
            </n-tooltip>
          </div>
        </div>
        <n-alert v-if="operationError" type="error" :title="t('加载用户操作失败：{error}', { error: operationError })">
          <n-button size="small" secondary @click="loadOperationLogs">{{ t("重试") }}</n-button>
        </n-alert>
        <div class="operations-table">
          <n-data-table
            :columns="operationColumns"
            :data="operationLogs"
            :row-key="operationRowKey"
            :loading="showResourceSkeleton(operationLoading, operationLoaded)"
            :pagination="operationPagination"
            :expanded-row-keys="expandedOperationIds"
            :scroll-x="1280"
            remote
            size="small"
            @update:page="changeOperationPage"
            @update:expanded-row-keys="onOperationExpanded"
          >
            <template #empty>
              <n-empty :description="t('暂无用户操作')" />
            </template>
          </n-data-table>
        </div>
      </n-tab-pane>
      <n-tab-pane name="history" :tab="t('历史混合日志')">
        <p class="log-limit-note">{{ t("历史混合日志说明") }}</p>
        <div class="log-toolbar">
          <n-input
            v-model:value="requestIdFilter"
            clearable
            class="request-id-filter"
            :placeholder="t('按请求 ID 精确搜索')"
            :input-props="{ 'aria-label': t('请求 ID') }"
          />
          <n-select v-model:value="gatewayLevelFilter" class="gateway-level-filter" :options="gatewayLevelOptions" :aria-label="t('级别')" />
          <n-input
            v-model:value="gatewayCategoryFilter"
            clearable
            class="gateway-category-filter"
            :placeholder="t('按分类精确搜索')"
            :input-props="{ 'aria-label': t('分类') }"
          />
          <n-tooltip trigger="hover">
            <template #trigger>
              <n-button circle quaternary :loading="gatewayLoading" :aria-label="t('刷新历史混合日志')" @click="loadGatewayLogs">
                <template #icon><n-icon :component="ReloadOutlined" /></template>
              </n-button>
            </template>
            {{ t("刷新历史混合日志") }}
          </n-tooltip>
        </div>
        <n-alert v-if="gatewayError" type="error" :title="t('加载历史混合日志失败：{error}', { error: gatewayError })">
          <n-button size="small" secondary @click="loadGatewayLogs">{{ t("重试") }}</n-button>
        </n-alert>
        <p class="log-limit-note">{{ t("仅显示最近 {count} 条历史混合日志", { count: 200 }) }}</p>
        <n-data-table
          :columns="gatewayColumns"
          :data="gatewayLogs"
          :row-key="logRowKey"
          :loading="showResourceSkeleton(gatewayLoading, gatewayLoaded)"
          :pagination="gatewayPagination"
          :scroll-x="1200"
          :virtual-scroll="true"
          max-height="560"
          size="small"
          @update:page="changeGatewayPage"
        >
          <template #empty>
            <n-empty :description="t('暂无历史混合日志')" />
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
import { CalendarOutlined, CheckOutlined, ClearOutlined, CopyOutlined, ReloadOutlined } from "@vicons/antd";
import { UNATTRIBUTED_KEY_FILTER } from "../api/dashboard";
import type { GatewayLog } from "../api/dashboard";
import type { OperationLog, OperationOutcome, OperationSource, RequestLog } from "../api/log-ledger-types.ts";
import { t } from "../i18n/index.ts";
import { locale } from "../i18n/index.ts";
import { useAccountsStore } from "../stores/accounts.ts";
import { useProvidersStore } from "../stores/providers.ts";
import { useObservabilityStore } from "../stores/observability.ts";
import { storeToRefs } from "pinia";
import { formatNumber, useClipboard } from "../utils/format.ts";
import { computeTimeRange, resolveTimeRange, timePresetValues } from "./log-time-range.ts";
import { routeQuerySearch } from "./app-navigation.ts";
import type { TimePreset } from "./log-time-range.ts";
import { gatewayLogMessage } from "./gateway-log-message.ts";
import { gatewayLogLevelTag, parseGatewayLogLevel, type GatewayLogLevel } from "./gateway-log-level.ts";
import { renderDiagnostic, renderForwardDetail, renderRequestId, type LogsColumnContext } from "./logs-columns.ts";
import {
  OPERATION_OUTCOME_KEYS,
  OPERATION_SOURCE_KEYS,
  REQUEST_STATUS_KEYS,
  attemptPanelId,
  describeOperationAction,
  logicalRequestStatus,
  operationOutcomeKey,
  operationSourceKey,
  requestStatusKey,
  requestUsageTotals,
  showResourceSkeleton,
} from "../domain/log-ledger.ts";

type LogTab = "requests" | "operations" | "history";

const route = useRoute();
const router = useRouter();
const message = useMessage();
const accountsStore = useAccountsStore();
const providersStore = useProvidersStore();
const observabilityStore = useObservabilityStore();
const {
  gatewayLogs, gatewayLoaded, gatewayLoading, gatewayError, gatewayLoadedAt,
  requestLogs, requestSummary, requestTotal, requestLoaded, requestLoading, requestError, requestLoadedAt,
  requestDetails,
  operationLogs, operationTotal, operationLoaded, operationLoading, operationError, operationLoadedAt,
  models, clientKeys,
} = storeToRefs(observabilityStore);
const { copiedTarget, copy, cleanup } = useClipboard();
const activeTab = ref<LogTab>("requests");
const accounts = computed(() => accountsStore.accounts);
const providerCatalog = computed(() => providersStore.catalog);
const statusFilter = ref("");
const accountFilter = ref("");
const modelFilter = ref("");
const keyFilter = ref("");
const providerFilter = ref("");
const routeAccountFilter = ref("");
const credentialAccountFilter = ref("");
const requestIdFilter = ref("");
const outcomeFilter = ref<OperationOutcome | "">("");
const sourceFilter = ref<OperationSource | "">("");
const actionFilter = ref("");
const subjectTypeFilter = ref("");
const subjectIdFilter = ref("");
const gatewayLevelFilter = ref<GatewayLogLevel>("");
const gatewayCategoryFilter = ref("");
const expandedRequestKeys = ref<string[]>([]);
const expandedOperationIds = ref<string[]>([]);
const advancedFilterCount = computed(() => [
  requestIdFilter.value, accountFilter.value, keyFilter.value, providerFilter.value,
  routeAccountFilter.value, credentialAccountFilter.value,
].filter(Boolean).length);
const timeRange = ref<[number, number] | null>(resolveTimeRange("last24h", null));
const activePreset = ref<TimePreset>("last24h");
const customTimeRange = ref<[number, number] | null>(timeRange.value);
const showTimePanel = ref(false);
const showOperationTimePanel = ref(false);
const requestUsage = computed(() => requestUsageTotals(requestSummary.value));

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

function parseLogTab(value: string | null): LogTab {
  if (value === "history" || value === "gateway") return "history";
  if (value === "operations") return "operations";
  return "requests";
}

function parseOutcome(value: string | null): OperationOutcome | "" {
  if (value && Object.hasOwn(OPERATION_OUTCOME_KEYS, value)) return value as OperationOutcome;
  return "";
}

function parseSource(value: string | null): OperationSource | "" {
  if (value && Object.hasOwn(OPERATION_SOURCE_KEYS, value)) return value as OperationSource;
  return "";
}

function applyRouteQuery(search: string): void {
  const params = new URLSearchParams(search);
  activeTab.value = parseLogTab(params.get("tab"));
  const status = params.get("status") ?? "";
  statusFilter.value = status === "success_unpriced" ? "success" : status;
  accountFilter.value = params.get("account") ?? "";
  modelFilter.value = params.get("model") ?? "";
  keyFilter.value = params.get("key") ?? "";
  providerFilter.value = params.get("provider") ?? "";
  routeAccountFilter.value = params.get("route_account") ?? "";
  credentialAccountFilter.value = params.get("credential_account") ?? "";
  requestIdFilter.value = params.get("request_id") ?? "";
  outcomeFilter.value = parseOutcome(params.get("outcome"));
  sourceFilter.value = parseSource(params.get("op_source"));
  actionFilter.value = params.get("action") ?? "";
  subjectTypeFilter.value = params.get("subject_type") ?? "";
  subjectIdFilter.value = params.get("subject_id") ?? "";
  gatewayLevelFilter.value = parseGatewayLogLevel(params.get("level"));
  gatewayCategoryFilter.value = params.get("category") ?? "";
  const queryRange = parseQueryTimeRange(params);
  const preset = params.get("range");
  const nextPreset: TimePreset = queryRange
    ? "custom"
    : preset !== null && preset !== "custom" && timePresetValues.has(preset as TimePreset)
      ? preset as TimePreset
      : "last24h";
  const nextRange = queryRange
    ?? (nextPreset === activePreset.value ? timeRange.value : resolveTimeRange(nextPreset, null));
  activePreset.value = nextPreset;
  if (!sameTimeRange(timeRange.value, nextRange)) timeRange.value = nextRange;
  if (!sameTimeRange(customTimeRange.value, nextRange)) customTimeRange.value = nextRange;
}

applyRouteQuery(routeQuerySearch("logs", route.query));
const showAdvancedFilters = ref(advancedFilterCount.value > 0);
const requestPage = ref(1);
const operationPage = ref(1);
const gatewayPage = ref(1);
const pageSize = 20;
const requestPagination = computed(() => ({ page: requestPage.value, pageSize, itemCount: requestTotal.value }));
const operationPagination = computed(() => ({ page: operationPage.value, pageSize, itemCount: operationTotal.value }));
const gatewayPagination = computed(() => ({ page: gatewayPage.value, pageSize }));

const dateFormatter = computed(() => new Intl.DateTimeFormat(locale.value, {
  year: "numeric", month: "2-digit", day: "2-digit",
  hour: "2-digit", minute: "2-digit", second: "2-digit",
}));
const dateOnlyFormatter = computed(() => new Intl.DateTimeFormat(locale.value, {
  year: "numeric", month: "2-digit", day: "2-digit",
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
const allOption = computed(() => ({ label: t("全部"), value: "" }));
const requestStatusOptions = computed(() => [
  allOption.value,
  ...Object.entries(REQUEST_STATUS_KEYS).map(([value, key]) => ({ label: t(key), value })),
]);
const outcomeOptions = computed(() => [
  allOption.value,
  ...Object.entries(OPERATION_OUTCOME_KEYS).map(([value, key]) => ({ label: t(key), value })),
]);
const sourceOptions = computed(() => [
  allOption.value,
  ...Object.entries(OPERATION_SOURCE_KEYS).map(([value, key]) => ({ label: t(key), value })),
]);
const gatewayLevelOptions = computed(() => [
  allOption.value,
  ...(["TRACE", "DEBUG", "INFO", "WARN", "ERROR"] as const).map((value) => ({ label: value, value })),
]);
const accountOptions = computed(() => [allOption.value, ...accounts.value.map((account) => ({ label: account.name, value: account.id }))]);
const modelOptions = computed(() => [allOption.value, ...models.value.map((model) => ({ label: model, value: model }))]);
const keyOptions = computed(() => [
  allOption.value,
  ...clientKeys.value.map((key) => ({ label: key.name, value: key.id })),
  { label: t("未归因"), value: UNATTRIBUTED_KEY_FILTER },
]);
const providerOptions = computed(() => [
  allOption.value,
  ...[...new Set(accounts.value.map((account) => account.provider_id).filter(Boolean))]
    .map((providerId) => ({ label: providerId, value: providerId })),
]);
const routeAccountOptions = computed(() => accountOptions.value);
const credentialAccountOptions = computed(() => accountOptions.value);
const hasRequestFilters = computed(() =>
  !!statusFilter.value || !!accountFilter.value || !!modelFilter.value || !!keyFilter.value
  || !!providerFilter.value || !!routeAccountFilter.value || !!credentialAccountFilter.value
  || !!requestIdFilter.value || !!timeRange.value);
const hasOperationFilters = computed(() =>
  !!outcomeFilter.value || !!sourceFilter.value || !!actionFilter.value.trim()
  || !!subjectTypeFilter.value.trim() || !!subjectIdFilter.value.trim() || !!timeRange.value);

function formatDate(value: string): string {
  const date = new Date(value);
  return Number.isNaN(date.getTime()) ? value : dateFormatter.value.format(date);
}

function toIsoString(ms: number): string {
  return new Date(ms).toISOString();
}

function blank(value: string): string | null {
  const trimmed = value.trim();
  return trimmed ? trimmed : null;
}

function applyTimePreset(preset: TimePreset) {
  if (preset === "custom") {
    const currentRange = resolveTimeRange(activePreset.value, timeRange.value);
    activePreset.value = "custom";
    timeRange.value = currentRange;
    customTimeRange.value = currentRange;
    showTimePanel.value = true;
    showOperationTimePanel.value = true;
    return;
  }
  if (preset === "all") {
    activePreset.value = "all";
    timeRange.value = null;
    customTimeRange.value = null;
    showTimePanel.value = false;
    showOperationTimePanel.value = false;
    return;
  }
  const range = computeTimeRange(preset);
  activePreset.value = preset;
  timeRange.value = range;
  customTimeRange.value = range;
  showTimePanel.value = false;
  showOperationTimePanel.value = false;
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
  showOperationTimePanel.value = false;
}

async function copyText(target: string, value: string, label: string) {
  try {
    await copy(target, value, label);
    message.success(t("已复制 {label}", { label }));
  } catch (e) {
    message.error(e instanceof Error ? e.message : t("复制失败"));
  }
}

function logRowKey(row: GatewayLog): number {
  return row.id;
}
function requestRowKey(row: RequestLog): string {
  return row.requestKey;
}
function operationRowKey(row: OperationLog): string {
  return row.operationId;
}

function focusRequestChain(requestId: string) {
  requestIdFilter.value = requestId;
  activeTab.value = "requests";
}

const logsColumnContext: LogsColumnContext = {
  components: { NButton, NIcon, CheckOutlined, CopyOutlined },
  copiedTarget,
  copyText,
  focusRequestChain,
  accounts,
  catalog: providerCatalog,
};

function actionLabel(action: string): string {
  const described = describeOperationAction(action);
  if (described.kind === "mapped") return t(described.key);
  if (described.kind === "domain") return `${t(described.domainKey)} · ${described.verb}`;
  return t(described.key, { action: described.action });
}

function textOrDash(value: string | null | undefined): string {
  const trimmed = value?.trim();
  return trimmed ? trimmed : "—";
}

function toggleRequest(requestKey: string) {
  const open = expandedRequestKeys.value.includes(requestKey);
  onRequestExpanded(open
    ? expandedRequestKeys.value.filter((key) => key !== requestKey)
    : [...expandedRequestKeys.value, requestKey]);
}

function onRequestExpanded(keys: Array<string | number>) {
  const next = keys.map(String);
  const added = next.filter((key) => !expandedRequestKeys.value.includes(key));
  expandedRequestKeys.value = next;
  for (const key of added) void observabilityStore.loadRequestAttempts(key);
}

function toggleOperation(operationId: string) {
  onOperationExpanded(expandedOperationIds.value.includes(operationId)
    ? expandedOperationIds.value.filter((id) => id !== operationId)
    : [...expandedOperationIds.value, operationId]);
}

function onOperationExpanded(keys: Array<string | number>) {
  expandedOperationIds.value = keys.map(String);
}

function renderModelIdentity(row: RequestLog) {
  const requested = row.requestedModel?.trim() || row.model;
  const alias = row.resolvedAlias?.trim() || "";
  const upstream = row.upstreamModel?.trim() || "";
  return h("div", { class: "model-identity" }, [
    h("div", requested || "—"),
    alias ? h("div", { class: "model-identity-meta" }, `${t("解析别名")} ${alias}`) : null,
    upstream ? h("div", { class: "model-identity-meta" }, `${t("上游模型")} ${upstream}`) : null,
  ]);
}

function renderRequestAttempts(row: RequestLog) {
  const detail = requestDetails.value[row.requestKey];
  if (!detail || (!detail.loaded && !detail.error)) return h("p", { class: "log-limit-note" }, t("正在加载上游尝试"));
  if (detail.error && detail.items.length === 0) {
    return h("div", [
      h(NAlert, { type: "error", title: t("加载上游尝试失败：{error}", { error: detail.error }) }),
      h(NButton, {
        size: "small",
        secondary: true,
        onClick: () => void observabilityStore.loadRequestAttempts(row.requestKey, true),
      }, { default: () => t("重试") }),
    ]);
  }
  return h("div", { id: attemptPanelId(row.requestKey), class: "attempt-list" }, [
    h("p", { class: "log-limit-note" }, t("上游尝试 {attempts}，记录行 {rows}", {
      attempts: row.attemptCount,
      rows: row.recordedRowCount,
    })),
    ...detail.items.map((item) => h("article", { class: "attempt-row", key: item.id }, [
      h("dl", { class: "diagnostic-meta" }, [
        [t("时间"), formatDate(item.timestamp)],
        [t("状态"), t(requestStatusKey(logicalRequestStatus(item.status)) ?? "未知")],
        ["HTTP", item.http_status === null ? "—" : String(item.http_status)],
        [t("输入"), formatNumber(item.prompt_tokens)],
        [t("输出"), formatNumber(item.completion_tokens)],
        [t("缓存已包含在输入中"), formatNumber(item.cached_tokens)],
        [t("接入 Key"), item.client_key_name ?? item.client_key_id ?? "—"],
      ].flatMap(([label, value]) => [h("dt", label), h("dd", value)])),
      renderForwardDetail(item, logsColumnContext),
    ])),
  ]);
}

function renderOperationFacts(row: OperationLog) {
  const metadata = row.metadata;
  const facts: Array<[string, string]> = [];
  if (row.completedAt) facts.push([t("完成时间"), formatDate(row.completedAt)]);
  if (row.reasonCode) facts.push([t("原因码"), row.reasonCode]);
  if (metadata?.changedFields?.length) facts.push([t("变更字段"), metadata.changedFields.join(", ")]);
  if (metadata?.requestedCount !== null && metadata?.requestedCount !== undefined) {
    facts.push([t("请求数量"), String(metadata.requestedCount)]);
  }
  if (metadata?.completedCount !== null && metadata?.completedCount !== undefined) {
    facts.push([t("完成数"), String(metadata.completedCount)]);
  }
  if (metadata?.failedCount !== null && metadata?.failedCount !== undefined) {
    facts.push([t("失败数"), String(metadata.failedCount)]);
  }
  if (metadata?.revision !== null && metadata?.revision !== undefined) facts.push([t("修订"), String(metadata.revision)]);
  if (metadata?.compensated !== null && metadata?.compensated !== undefined) {
    facts.push([t("已补偿"), metadata.compensated ? t("已补偿") : "—"]);
  }
  if (metadata?.relatedIds?.length) facts.push([t("相关标识"), metadata.relatedIds.join(", ")]);
  if (!facts.length) return h("p", { class: "log-limit-note" }, t("无附加事实"));
  return h("dl", { class: "diagnostic-meta" }, facts.flatMap(([label, value]) => [h("dt", label), h("dd", value)]));
}

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

const requestColumns = computed(() => [
  {
    type: "expand" as const,
    width: 0,
    expandable: () => true,
    renderExpand: renderRequestAttempts,
  },
  {
    key: "expand-request",
    width: 56,
    render: (row: RequestLog) => {
      const open = expandedRequestKeys.value.includes(row.requestKey);
      return h(NButton, {
        quaternary: true,
        size: "small",
        "aria-expanded": open ? "true" : "false",
        "aria-controls": attemptPanelId(row.requestKey),
        "aria-label": open ? t("收起上游尝试") : t("展开上游尝试"),
        onClick: () => toggleRequest(row.requestKey),
      }, { default: () => (open ? "▾" : "▸") });
    },
  },
  { title: t("时间"), key: "timestamp", width: 150, render: (row: RequestLog) => formatDate(row.timestamp) },
  {
    title: t("请求 ID"),
    key: "requestId",
    width: 170,
    render: (row: RequestLog) => row.requestId
      ? renderRequestId({ id: row.requestKey, request_id: row.requestId }, logsColumnContext)
      : "—",
  },
  {
    title: t("状态"),
    key: "status",
    width: 132,
    render: (row: RequestLog) => {
      const logical = logicalRequestStatus(row.status);
      const key = requestStatusKey(logical);
      const type = logical === "success"
        ? "success" as const
        : logical === "error" || logical === "client_error"
          ? "error" as const
          : logical === "streaming" || logical === "outcome_unknown" || logical === "cancelled"
            ? "warning" as const
            : "default" as const;
      const tags = [h(NTag, { type, size: "small", bordered: false }, { default: () => key ? t(key) : logical })];
      if (row.isLegacy) tags.push(h(NTag, { size: "small", bordered: false }, { default: () => t("历史行") }));
      return h("div", { class: "status-tags" }, tags);
    },
  },
  { title: "HTTP", key: "httpStatus", width: 72, render: (row: RequestLog) => row.httpStatus ?? "—" },
  { title: t("请求模型"), key: "requestedModel", width: 200, render: renderModelIdentity },
  { title: t("上游尝试"), key: "attemptCount", width: 96, align: "right" as const, render: (row: RequestLog) => formatNumber(row.attemptCount) },
  { title: t("输入"), key: "promptTokens", width: 90, align: "right" as const, render: (row: RequestLog) => formatNumber(row.promptTokens) },
  { title: t("输出"), key: "completionTokens", width: 90, align: "right" as const, render: (row: RequestLog) => formatNumber(row.completionTokens) },
  { title: t("缓存"), key: "cachedTokens", width: 90, align: "right" as const, render: (row: RequestLog) => formatNumber(row.cachedTokens) },
  {
    title: t("总 Tokens"),
    key: "totalTokens",
    width: 110,
    align: "right" as const,
    render: (row: RequestLog) => formatNumber(row.promptTokens + row.completionTokens),
  },
  { title: t("耗时"), key: "durationMs", width: 100, align: "right" as const, render: (row: RequestLog) => row.durationMs === null ? "—" : `${row.durationMs} ms` },
  { title: t("接入 Key"), key: "clientKeyName", width: 140, ellipsis: { tooltip: true }, render: (row: RequestLog) => textOrDash(row.clientKeyName || row.clientKeyId) },
  { title: t("路由账号"), key: "routeAccountId", width: 140, ellipsis: { tooltip: true }, render: (row: RequestLog) => textOrDash(row.routeAccountId || row.accountName) },
  { title: t("凭证账号"), key: "credentialAccountId", width: 140, ellipsis: { tooltip: true }, render: (row: RequestLog) => textOrDash(row.credentialAccountId) },
  { title: t("服务商"), key: "providerId", width: 120, ellipsis: { tooltip: true }, render: (row: RequestLog) => textOrDash(row.providerId) },
]);

const operationColumns = computed(() => [
  {
    type: "expand" as const,
    width: 0,
    expandable: () => true,
    renderExpand: renderOperationFacts,
  },
  {
    key: "expand-operation",
    width: 56,
    render: (row: OperationLog) => {
      const open = expandedOperationIds.value.includes(row.operationId);
      return h(NButton, {
        quaternary: true,
        size: "small",
        "aria-expanded": open ? "true" : "false",
        "aria-label": open ? t("收起操作事实") : t("展开操作事实"),
        onClick: () => toggleOperation(row.operationId),
      }, { default: () => (open ? "▾" : "▸") });
    },
  },
  { title: t("时间"), key: "startedAt", width: 160, render: (row: OperationLog) => formatDate(row.startedAt) },
  { title: t("动作"), key: "action", minWidth: 180, ellipsis: { tooltip: true }, render: (row: OperationLog) => actionLabel(row.action) },
  {
    title: t("来源"),
    key: "source",
    width: 120,
    render: (row: OperationLog) => {
      const key = operationSourceKey(row.source);
      return key ? t(key) : row.source;
    },
  },
  { title: t("执行者"), key: "actorId", width: 140, ellipsis: { tooltip: true }, render: (row: OperationLog) => textOrDash(row.actorId) },
  {
    title: t("主体"),
    key: "subject",
    minWidth: 180,
    ellipsis: { tooltip: true },
    render: (row: OperationLog) => [row.subjectType, row.subjectId].filter(Boolean).join(" ") || "—",
  },
  {
    title: t("操作结果"),
    key: "outcome",
    width: 140,
    render: (row: OperationLog) => {
      const key = operationOutcomeKey(row.outcome);
      const type = row.outcome === "success" || row.outcome === "compensated"
        ? "success" as const
        : row.outcome === "failed"
          ? "error" as const
          : row.outcome === "rejected" || row.outcome === "partial" || row.outcome === "pending"
            ? "warning" as const
            : "default" as const;
      return h(NTag, { type, size: "small", bordered: false }, { default: () => key ? t(key) : row.outcome });
    },
  },
  { title: t("原因码"), key: "reasonCode", width: 160, ellipsis: { tooltip: true }, render: (row: OperationLog) => textOrDash(row.reasonCode) },
]);

function clearRequestFilters() {
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
  showOperationTimePanel.value = false;
}

function clearOperationFilters() {
  outcomeFilter.value = "";
  sourceFilter.value = "";
  actionFilter.value = "";
  subjectTypeFilter.value = "";
  subjectIdFilter.value = "";
  activePreset.value = "all";
  timeRange.value = null;
  customTimeRange.value = null;
  showTimePanel.value = false;
  showOperationTimePanel.value = false;
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
  if (outcomeFilter.value) query.outcome = outcomeFilter.value;
  if (sourceFilter.value) query.op_source = sourceFilter.value;
  if (actionFilter.value.trim()) query.action = actionFilter.value.trim();
  if (subjectTypeFilter.value.trim()) query.subject_type = subjectTypeFilter.value.trim();
  if (subjectIdFilter.value.trim()) query.subject_id = subjectIdFilter.value.trim();
  if (gatewayLevelFilter.value) query.level = gatewayLevelFilter.value;
  if (gatewayCategoryFilter.value.trim()) query.category = gatewayCategoryFilter.value.trim();
  if (activePreset.value === "custom" && timeRange.value) {
    query.start = toIsoString(timeRange.value[0]);
    query.end = toIsoString(timeRange.value[1]);
  } else {
    query.range = activePreset.value;
  }
  void router.replace({ query });
}

const LOGS_QUERY_KEYS = [
  "tab", "status", "account", "model", "key", "provider", "route_account",
  "credential_account", "request_id", "outcome", "op_source", "action",
  "subject_type", "subject_id", "level", "category", "start", "end", "range",
];

let applyingRouteQuery = false;

function requestQuerySignature(): string {
  const range = timeRange.value;
  return [
    statusFilter.value, accountFilter.value, modelFilter.value, keyFilter.value,
    providerFilter.value, routeAccountFilter.value, credentialAccountFilter.value,
    requestIdFilter.value, activePreset.value,
    range ? `${range[0]}:${range[1]}` : "",
  ].join(" ");
}

function operationQuerySignature(): string {
  const range = timeRange.value;
  return [
    outcomeFilter.value, sourceFilter.value, actionFilter.value.trim(),
    subjectTypeFilter.value.trim(), subjectIdFilter.value.trim(), activePreset.value,
    range ? `${range[0]}:${range[1]}` : "",
  ].join(" ");
}

function gatewayQuerySignature(): string {
  return [gatewayLevelFilter.value, gatewayCategoryFilter.value, requestIdFilter.value].join(" ");
}

function applyInboundQuery(query: LocationQuery): void {
  if (!LOGS_QUERY_KEYS.some((key) => key in query)) return;
  const tabBefore = activeTab.value;
  const requestBefore = requestQuerySignature();
  const operationBefore = operationQuerySignature();
  const gatewayBefore = gatewayQuerySignature();
  applyingRouteQuery = true;
  applyRouteQuery(routeQuerySearch("logs", query));
  void nextTick(() => {
    applyingRouteQuery = false;
  });
  const tabChanged = activeTab.value !== tabBefore;
  const requestChanged = requestQuerySignature() !== requestBefore;
  const operationChanged = operationQuerySignature() !== operationBefore;
  const gatewayChanged = gatewayQuerySignature() !== gatewayBefore;
  if (!tabChanged && !requestChanged && !operationChanged && !gatewayChanged) return;
  if (requestChanged) {
    requestPage.value = 1;
    expandedRequestKeys.value = [];
  }
  if (operationChanged) {
    operationPage.value = 1;
    expandedOperationIds.value = [];
  }
  if (gatewayChanged) gatewayPage.value = 1;
  if ((tabChanged && activeTab.value === "requests") || requestChanged) void loadRequestLogs();
  if ((tabChanged && activeTab.value === "operations") || operationChanged) void loadOperationLogs();
  if ((tabChanged && activeTab.value === "history") || gatewayChanged) void loadGatewayLogs();
}

onBeforeRouteUpdate((to) => {
  applyInboundQuery(to.query);
});

watch(() => route.query, (query) => {
  if (route.name !== "logs") return;
  applyInboundQuery(query);
});

const ACTIVATED_REFRESH_FRESHNESS_MS = 30_000;

async function loadGatewayLogs() {
  const error = await observabilityStore.loadGateway({
    limit: 200,
    requestId: requestIdFilter.value || null,
    level: gatewayLevelFilter.value || null,
    category: gatewayCategoryFilter.value.trim() || null,
  });
  if (error) message.error(t("加载历史混合日志失败：{error}", { error }));
}

async function loadRequestLogs() {
  const requestRange = resolveTimeRange(activePreset.value, timeRange.value);
  const error = await observabilityStore.loadRequests({
    limit: pageSize,
    offset: (requestPage.value - 1) * pageSize,
    status: blank(statusFilter.value),
    accountId: blank(accountFilter.value),
    model: blank(modelFilter.value),
    keyId: blank(keyFilter.value),
    providerId: blank(providerFilter.value),
    routeAccountId: blank(routeAccountFilter.value),
    credentialAccountId: blank(credentialAccountFilter.value),
    requestId: blank(requestIdFilter.value),
    startTime: requestRange ? toIsoString(requestRange[0]) : null,
    endTime: requestRange ? toIsoString(requestRange[1]) : null,
  });
  if (error) message.error(t("加载逻辑请求失败：{error}", { error }));
  if (error === null) {
    const present = new Set(requestLogs.value.map((row) => row.requestKey));
    expandedRequestKeys.value = expandedRequestKeys.value.filter((key) => present.has(key));
    await Promise.all(expandedRequestKeys.value.map((key) => observabilityStore.loadRequestAttempts(key, true)));
  }
}

async function loadOperationLogs() {
  const requestRange = resolveTimeRange(activePreset.value, timeRange.value);
  const error = await observabilityStore.loadOperations({
    limit: pageSize,
    offset: (operationPage.value - 1) * pageSize,
    action: blank(actionFilter.value),
    source: sourceFilter.value || null,
    outcome: outcomeFilter.value || null,
    subjectType: blank(subjectTypeFilter.value),
    subjectId: blank(subjectIdFilter.value),
    startTime: requestRange ? toIsoString(requestRange[0]) : null,
    endTime: requestRange ? toIsoString(requestRange[1]) : null,
  });
  if (error) message.error(t("加载用户操作失败：{error}", { error }));
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

async function refreshRequestLogs() {
  await Promise.all([loadRequestLogs(), loadForwardLogModels(), loadForwardLogKeys()]);
}

function changeRequestPage(page: number) {
  requestPage.value = page;
  expandedRequestKeys.value = [];
  void loadRequestLogs();
}

function changeOperationPage(page: number) {
  operationPage.value = page;
  expandedOperationIds.value = [];
  void loadOperationLogs();
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
  if (tab === "history") void loadGatewayLogs();
  if (tab === "requests") void loadRequestLogs();
  if (tab === "operations") void loadOperationLogs();
});

watch(
  [statusFilter, accountFilter, modelFilter, keyFilter, providerFilter, routeAccountFilter, credentialAccountFilter, timeRange, activePreset],
  () => {
    if (applyingRouteQuery) return;
    requestPage.value = 1;
    expandedRequestKeys.value = [];
    syncQueryState();
    void loadRequestLogs();
  },
);

watch([outcomeFilter, sourceFilter, timeRange, activePreset], () => {
  if (applyingRouteQuery) return;
  operationPage.value = 1;
  expandedOperationIds.value = [];
  syncQueryState();
  void loadOperationLogs();
});

let operationTextDebounce: ReturnType<typeof setTimeout> | null = null;
watch([actionFilter, subjectTypeFilter, subjectIdFilter], () => {
  if (applyingRouteQuery) return;
  if (operationTextDebounce !== null) clearTimeout(operationTextDebounce);
  operationPage.value = 1;
  expandedOperationIds.value = [];
  operationTextDebounce = setTimeout(() => {
    operationTextDebounce = null;
    syncQueryState();
    void loadOperationLogs();
  }, 300);
});

let requestIdDebounce: ReturnType<typeof setTimeout> | null = null;
watch(requestIdFilter, () => {
  if (applyingRouteQuery) return;
  if (requestIdDebounce !== null) clearTimeout(requestIdDebounce);
  requestPage.value = 1;
  gatewayPage.value = 1;
  expandedRequestKeys.value = [];
  requestIdDebounce = setTimeout(() => {
    requestIdDebounce = null;
    syncQueryState();
    void loadRequestLogs();
    void loadGatewayLogs();
  }, 300);
});

onUnmounted(() => {
  if (requestIdDebounce !== null) clearTimeout(requestIdDebounce);
  if (categoryDebounce !== null) clearTimeout(categoryDebounce);
  if (operationTextDebounce !== null) clearTimeout(operationTextDebounce);
});

let activatedOnce = false;
onActivated(() => {
  if (activatedOnce) {
    if (activeTab.value === "history" && !gatewayLoading.value && Date.now() - gatewayLoadedAt.value >= ACTIVATED_REFRESH_FRESHNESS_MS) {
      void loadGatewayLogs();
    }
    if (activeTab.value === "requests" && !requestLoading.value && Date.now() - requestLoadedAt.value >= ACTIVATED_REFRESH_FRESHNESS_MS) {
      void loadRequestLogs();
    }
    if (activeTab.value === "operations" && !operationLoading.value && Date.now() - operationLoadedAt.value >= ACTIVATED_REFRESH_FRESHNESS_MS) {
      void loadOperationLogs();
    }
  } else {
    activatedOnce = true;
  }
});

onMounted(() => {
  syncQueryState();
  if (activeTab.value === "history") void loadGatewayLogs();
  if (activeTab.value === "requests") void loadRequestLogs();
  if (activeTab.value === "operations") void loadOperationLogs();
  void loadAccounts();
  void loadForwardLogModels();
  void loadForwardLogKeys();
  void loadProviderCatalog();
});

onUnmounted(cleanup);
</script>

<style scoped>
.key-filter-note,
.log-limit-note {
  margin: 6px 0 10px;
  color: var(--ocg-subtle);
  font-size: var(--ocg-font-xs);
}
.logs-card {
  container-type: inline-size;
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
  grid-template-columns: repeat(auto-fit, minmax(140px, 1fr));
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
.filter-field.request-id-field { flex: 2 1 220px; max-width: 320px; }
.filter-field.time-range-field { flex: 1 1 200px; }
.filter-actions, .log-toolbar {
  display: flex;
  justify-content: flex-end;
  gap: var(--ocg-space-xs);
}
.filter-actions { flex: 0 0 auto; margin-left: auto; }
.log-toolbar { margin-bottom: var(--ocg-space-sm); }
.filter-label { font-size: var(--ocg-font-xs); color: var(--ocg-subtle); line-height: 1.2; }
.time-range-trigger { width: 100%; min-width: 120px; max-width: 240px; justify-content: flex-start; }
.time-range-trigger :deep(.n-button__content) { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
.time-range-panel { display: inline-flex; flex-direction: row; gap: var(--ocg-space-sm); max-width: calc(100vw - 48px); }
.preset-list { display: flex; flex-direction: column; gap: 2px; min-width: 100px; }
.preset-item { justify-content: flex-start; }
.preset-item :deep(.n-button__content) { white-space: nowrap; }
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
.custom-range-title { font-size: var(--ocg-font-sm); color: var(--ocg-muted); white-space: nowrap; }
.custom-time-picker { min-width: 0; }
.custom-time-picker :deep(.n-date-panel) { box-shadow: none; background: transparent; }
.custom-time-picker :deep(.n-date-panel-header),
.custom-time-picker :deep(.n-date-panel-calendar__picker-col),
.custom-time-picker :deep(.n-date-panel-actions) { background: transparent; }
.request-id-filter { width: min(360px, 100%); margin-right: auto; }
.gateway-level-filter { width: 130px; }
.gateway-category-filter { width: min(220px, 100%); }
:deep(.model-identity) { display: grid; gap: 2px; }
:deep(.model-identity-meta) { color: var(--ocg-subtle); font-size: var(--ocg-font-xs); }
:deep(.attempt-list) { display: grid; gap: var(--ocg-space-md); width: min(100%, calc(100cqw - 2 * var(--ocg-space-md))); }
:deep(.attempt-row) { display: grid; gap: var(--ocg-space-md); padding-top: var(--ocg-space-sm); border-top: 1px solid var(--ocg-border); }
.requests-table :deep(.n-data-table-expand-trigger),
.operations-table :deep(.n-data-table-expand-trigger) { display: none; }
.requests-table :deep(.n-data-table-td--expand),
.requests-table :deep(.n-data-table-th--expand),
.operations-table :deep(.n-data-table-td--expand),
.operations-table :deep(.n-data-table-th--expand) {
  width: 0;
  padding: 0;
  border-right-color: transparent;
}
@media (max-width: 650px) {
  .log-toolbar { flex-wrap: wrap; }
  .request-id-filter { width: 100%; }
  .gateway-category-filter { flex: 1 1 140px; }
}
:deep(.request-id-cell) { display: flex; align-items: center; gap: 6px; }
:deep(.request-id-cell code) {
  overflow: hidden;
  font: var(--ocg-font-xs)/1.4 "Cascadia Mono", Consolas, monospace;
  text-overflow: ellipsis;
  white-space: nowrap;
}
:deep(.status-tags) { display: flex; flex-wrap: wrap; gap: 3px; }
:deep(.diagnostic-detail) { display: grid; gap: var(--ocg-space-md); padding: var(--ocg-space-sm) 0; }
:deep(.diagnostic-detail h4) { margin: 0 0 5px; color: var(--ocg-muted); font-size: var(--ocg-font-sm); }
:deep(.diagnostic-meta) {
  display: grid;
  grid-template-columns: max-content minmax(120px, 1fr) max-content minmax(120px, 1fr);
  gap: 5px var(--ocg-space-md);
  margin: 0;
}
:deep(.diagnostic-meta dt) { color: var(--ocg-subtle); }
:deep(.diagnostic-meta dd) { margin: 0; font-family: "Cascadia Mono", Consolas, monospace; word-break: break-word; }
@container (max-width: 640px) {
  :deep(.diagnostic-meta) { grid-template-columns: max-content minmax(0, 1fr); }
}
:deep(.diagnostic-json), :deep(.error-text) {
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
:deep(.diagnostic-json) { max-height: 320px; overflow: auto; }
.advanced-filter-toggle { display: inline-flex; margin-bottom: var(--ocg-space-md); }
.filter-bar:not(.show-advanced) .advanced-filter { display: none; }
@media (max-width: 860px) {
  .time-range-panel { flex-direction: column; }
  .custom-range-wrapper.is-visible {
    width: auto;
    max-width: 100%;
    border-left: none;
    border-top: 1px solid var(--ocg-border);
    padding-left: 0;
    padding-top: var(--ocg-space-sm);
  }
  .custom-time-picker { overflow-x: auto; }
}
@media (max-width: 760px) {
  .stats-row { grid-template-columns: repeat(3, 1fr); gap: var(--ocg-space-sm); }
}
@media (max-width: 560px) {
  .stat-card { padding: 10px; }
  .stats-row { margin-bottom: var(--ocg-space-md); grid-template-columns: repeat(3, minmax(0, 1fr)); }
  .logs-card { padding: 2px var(--ocg-space-md) var(--ocg-space-md); }
  .filter-field { flex: 1 1 140px; }
  .filter-actions { margin-left: 0; }
}
</style>
