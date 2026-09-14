<template>
  <div class="pricing-catalog">
    <div
      v-if="catalogLoading"
      class="catalog-state"
      role="status"
      aria-live="polite"
      :aria-label="t('加载中…')"
    >
      <n-spin size="small" />
    </div>
    <n-alert
      v-else-if="catalogError"
      type="error"
      :title="t('加载服务商目录失败: {error}', { error: catalogError })"
    >
      <n-button size="small" secondary @click="loadProviderCatalog">{{ t("重试") }}</n-button>
    </n-alert>
    <template v-if="!catalogLoading">
      <n-tabs
        v-model:value="activePlanId"
        type="line"
        display-directive="if"
        class="pricing-plan-tabs"
      >
        <n-tab-pane
          v-for="group in planGroups"
          :key="group.plan.id"
          :name="group.plan.id"
          :tab="group.label"
        >
          <section
            class="pricing-card"
            :aria-labelledby="`pricing-title-${group.plan.id}`"
          >
        <div class="pricing-head">
          <div>
            <h2 :id="`pricing-title-${group.plan.id}`">
              {{ group.label }}
              <n-tag
                v-if="group.plan.kind === 'free'"
                type="success"
                size="small"
                :bordered="false"
              >{{ t("免费 · 出口 IP 共享") }}</n-tag>
            </h2>
            <n-alert
              v-if="pricingDisplay(group).state === 'error'"
              type="error"
              :title="t(pricingDisplay(group).messageKey, { error: pricingDisplay(group).error ?? '' })"
            >
              <n-button size="small" secondary @click="retryGroupPricing(group)">{{ t("重试") }}</n-button>
            </n-alert>
            <p v-else>{{ t(pricingDisplay(group).messageKey) }}</p>
          </div>
          <div v-if="group.pricingAvailability === 'available'" class="pricing-actions">
            <n-button
              v-if="pricingSourceUrl(group)"
              tag="a"
              text
              :href="pricingSourceUrl(group)"
              target="_blank"
              rel="noopener noreferrer"
            >{{ t("官方来源") }}</n-button>
            <n-button
              v-if="canRefresh(group)"
              type="primary"
              :loading="refreshing"
              :disabled="loading || refreshing || savingModelId !== null || confirmationOpen || !providerRefreshRevision"
              @click="requestPricingRefresh"
            >{{ refreshing ? t("正在刷新…") : t("刷新价格表") }}</n-button>
          </div>
        </div>

        <template v-if="group.content.kind === 'models'">
            <n-alert v-if="refreshError" type="warning" :title="t('刷新额度价格表失败: {error}', { error: refreshError })" />

            <n-spin :show="loading">
              <template v-if="pricingDisplay(group).state === 'available-table'">
                <dl class="pricing-ledger pricing-ledger--compact">
                  <div class="pricing-ledger__revision">
                    <dt>{{ t("修订版本") }}</dt>
                    <dd><code>{{ group.content.snapshot.revision }}</code></dd>
                  </div>
                  <div>
                    <dt>{{ t("启用时间") }}</dt>
                    <dd>{{ formatTimestamp(group.content.snapshot.activated_at) }}</dd>
                  </div>
                  <div>
                    <dt>{{ t("文档更新时间") }}</dt>
                    <dd>{{ group.content.snapshot.document_updated_at ? formatTimestamp(group.content.snapshot.document_updated_at) : "—" }}</dd>
                  </div>
                </dl>

                <p class="pricing-note">
                  {{ t("未知价格不会参与费用估算") }}
                </p>
                <n-data-table
                  :columns="columns"
                  :data="tableRows"
                  :pagination="false"
                  :row-key="rowKey"
                  :expanded-row-keys="expandedRowKeys"
                  :scroll-x="1310"
                  size="small"
                  @update:expanded-row-keys="updateExpandedRowKeys"
                />
              </template>
            </n-spin>
        </template>

        <ProviderPricingReference
          v-else-if="group.content.kind === 'values'"
          :snapshot="group.content.snapshot"
          :saving-model-id="savingModelId"
          :disabled="refreshing"
          :editable="pricingMultiplierEditable(group)"
          @save-multiplier="(modelId, multiplier) => saveProviderMultiplier(group, modelId, multiplier)"
        />
        <n-alert
          v-if="group.content.kind === 'values' && refreshError"
          type="warning"
          :title="t('刷新额度价格表失败: {error}', { error: refreshError })"
        />

        <template v-else-if="group.content.kind === 'opaque' && pricingDisplay(group).state === 'available-table'">
            <dl class="pricing-ledger pricing-ledger--compact">
              <div>
                <dt>{{ t("修订版本") }}</dt>
                <dd><code>{{ group.content.snapshot.revision }}</code></dd>
              </div>
              <div>
                <dt>{{ t("启用时间") }}</dt>
                <dd>{{ formatTimestamp(group.content.snapshot.activated_at) }}</dd>
              </div>
              <div>
                <dt>{{ t("文档更新时间") }}</dt>
                <dd>{{ group.content.snapshot.document_updated_at ? formatTimestamp(group.content.snapshot.document_updated_at) : "—" }}</dd>
              </div>
            </dl>
            <div class="pricing-actions">
              <n-button
                v-if="group.content.snapshot.source_url"
                tag="a"
                text
                :href="group.content.snapshot.source_url"
                target="_blank"
                rel="noopener noreferrer"
              >{{ t("官方来源") }}</n-button>
            </div>
        </template>
          </section>
        </n-tab-pane>
      </n-tabs>
    </template>
  </div>
</template>

<script setup lang="ts">
import { computed, h, onMounted, ref, watch } from "vue";
import {
  NAlert,
  NButton,
  NDataTable,
  NIcon,
  NInputNumber,
  NSpin,
  NTabPane,
  NTag,
  NTabs,
  NTooltip,
  useDialog,
  useMessage,
} from "naive-ui";
import type { DataTableColumns, DataTableRowKey } from "naive-ui";
import { CheckOutlined, CloseOutlined, RightOutlined } from "@vicons/antd";
import { DashboardRequestError, dashboardApi } from "../api/dashboard";
import { dashboardV4 } from "../api/dashboard-v4.ts";
import type {
  PricingMultiplierChange,
  PricingSnapshot,
} from "../api/dashboard";
import { providerApi } from "../api/providers.ts";
import type { ProviderCatalogEntry } from "../api/providers.ts";
import type { ProviderTemplate } from "../api/generated/dashboard-v4.ts";
import ProviderPricingReference from "./ProviderPricingReference.vue";
import { locale, t } from "../i18n/index.ts";
import {
  buildPricingTableRows,
  formatPricingMultiplier,
  formatPricingRate,
} from "../domain/pricing-view.ts";
import type { PricingTableRow } from "../domain/pricing-view.ts";
import { providerSurfaces } from "../domain/plans.ts";
import {
  buildScopedPlanPricingGroups,
  resolvePlanPricingDisplay,
} from "../domain/pricing-plans.ts";
import type { PlanPricingGroup, ProviderSnapshots } from "../domain/pricing-plans.ts";

const props = defineProps<{
  providerId: string;
}>();

const message = useMessage();
const dialog = useDialog();
const snapshot = ref<PricingSnapshot | null>(null);
const catalog = ref<ProviderCatalogEntry[] | null>(null);
const catalogLoading = ref(false);
const catalogError = ref("");
const loading = ref(false);
const refreshing = ref(false);
const confirmationOpen = ref(false);
const savingModelId = ref<string | null>(null);
const multiplierDrafts = ref<Partial<Record<string, number | null>>>({});
const expandedRowKeys = ref<DataTableRowKey[]>([]);
const loadError = ref("");
const refreshError = ref("");
const providerSnapshots = ref<ProviderSnapshots>({});
const providerSnapshotErrors = ref<Partial<Record<string, string>>>({});
const providerTemplates = ref<ProviderTemplate[]>([]);
const activePlanId = ref<string>("");

const planGroups = computed<PlanPricingGroup[]>(() => (
  buildScopedPlanPricingGroups(props.providerId, catalog.value, snapshot.value, providerSnapshots.value)
));

const activeGroup = computed(() => planGroups.value.find(({ plan }) => (
  plan.provider_id === activePlanId.value
)) ?? planGroups.value[0] ?? null);

const tableRows = computed(() => buildPricingTableRows(
  activeGroup.value?.content.kind === "models" ? activeGroup.value.content.snapshot.models : [],
  {
  highspeed: t("高速别名"),
  minimaxM3Upper: t("> 512K 输入"),
  priorityService: t("优先服务"),
  minimaxM3UpperPriority: t("> 512K 输入 + 优先服务"),
  },
));

const providerRefreshRevision = computed<string | undefined>(() => {
  const group = activeGroup.value;
  if (!group) return undefined;
  return providerSnapshots.value[group.plan.provider_id]?.provider_pricing_revision
    ?? (group.plan.provider_id === "opencode" ? snapshot.value?.revision : undefined);
});

function pricingError(group: PlanPricingGroup): string | null {
  return providerSnapshotErrors.value[group.plan.provider_id]
    ?? (catalog.value == null && group.plan.provider_id === "opencode" && !snapshot.value
      ? loadError.value || null
      : null);
}

function pricingDisplay(group: PlanPricingGroup) {
  return resolvePlanPricingDisplay(group, pricingError(group));
}

function pricingSourceUrl(group: PlanPricingGroup): string {
  return group.content.kind === "none" ? "" : group.content.snapshot.source_url;
}

function retryGroupPricing(group: PlanPricingGroup) {
  if (catalog.value == null && group.plan.provider_id === "opencode") {
    void loadPricing();
  } else if (catalog.value) {
    void loadProviderSnapshots(catalog.value);
  } else {
    void loadProviderCatalog();
  }
}

// The catalog is fetched once on mount (or via the error-state retry); there
// is deliberately no polling, matching the manual-refresh pricing policy.
async function loadProviderCatalog() {
  if (catalogLoading.value) return;
  catalogLoading.value = true;
  catalogError.value = "";
  try {
    catalog.value = await providerApi.getProviderCatalog();
  } catch (error) {
    catalog.value = null;
    catalogError.value = error instanceof Error ? error.message : String(error);
    if (props.providerId === "opencode" || !props.providerId) await loadPricing();
  } finally {
    catalogLoading.value = false;
  }
}

// Provider-scoped pricing snapshots are loaded opportunistically for plans the
// catalog marks as pricing-available. Failures are swallowed so a missing
// endpoint or an in-flight backend change never breaks the page.
async function loadProviderSnapshots(catalogValue: ProviderCatalogEntry[]) {
  const availablePlans = providerSurfaces(catalogValue).filter((plan) => (
    plan.pricing_availability === "available"
    && (!props.providerId || plan.provider_id === props.providerId)
  ));

  const results = await Promise.allSettled(
    availablePlans.map((plan) => providerApi.getProviderPricing(plan.provider_id)),
  );
  const next: ProviderSnapshots = {};
  const nextErrors: Partial<Record<string, string>> = {};
  results.forEach((result, index) => {
    const plan = availablePlans[index]!;
    if (result.status === "fulfilled" && plan) {
      next[plan.provider_id] = result.value;
    } else if (result.status === "rejected" && plan) {
      nextErrors[plan.provider_id] = result.reason instanceof Error
        ? result.reason.message
        : String(result.reason);
    }
  });
  providerSnapshots.value = next;
  providerSnapshotErrors.value = nextErrors;
}

watch(catalog, (catalogValue) => {
  if (catalogValue !== null) {
    void loadProviderSnapshots(catalogValue);
  }
}, { immediate: true });

async function loadProviderTemplates(): Promise<void> {
  try {
    providerTemplates.value = (await dashboardV4.getTemplates()).templates;
  } catch {
    // V4 absence is an intentional read-only downgrade.
    providerTemplates.value = [];
  }
}

function pricingMultiplierEditable(group: PlanPricingGroup): boolean {
  const template = providerTemplates.value.find((item) => item.id === group.plan.provider_id);
  return template?.pricingMultiplierEditable === true;
}

function canRefresh(group: PlanPricingGroup): boolean {
  return group.pricingAvailability === "available";
}

function formatRate(value: number | null) {
  return formatPricingRate(value, locale.value);
}

function renderRate(value: number | null | undefined) {
  if (value === undefined) return "";
  const formatted = formatRate(value);
  if (!formatted.exact) return formatted.label;
  const exactLabel = t("精确值：{value} / 百万 tokens", { value: formatted.exact });
  return h(NTooltip, { trigger: "focus" }, {
    trigger: () => h("span", {
      class: "tiny-rate",
      tabindex: 0,
      title: exactLabel,
      "aria-label": `${formatted.label}, ${exactLabel}`,
    }, formatted.label),
    default: () => exactLabel,
  });
}

function formatTimestamp(value: string): string {
  const date = new Date(value);
  if (Number.isNaN(date.getTime())) return value;
  return new Intl.DateTimeFormat(locale.value, {
    year: "numeric",
    month: "2-digit",
    day: "2-digit",
    hour: "2-digit",
    minute: "2-digit",
  }).format(date);
}

function rowKey(row: PricingTableRow): string {
  return row.row_key;
}

function updateExpandedRowKeys(keys: DataTableRowKey[]) {
  expandedRowKeys.value = keys;
}

function rowExpanded(row: PricingTableRow): boolean {
  return expandedRowKeys.value.includes(row.row_key);
}

function toggleRow(row: PricingTableRow) {
  expandedRowKeys.value = rowExpanded(row)
    ? expandedRowKeys.value.filter((key) => key !== row.row_key)
    : [...expandedRowKeys.value, row.row_key];
}

function renderModel(row: PricingTableRow) {
  if (row.kind !== "group") {
    return h("span", {
      "aria-label": row.kind === "variant" ? `${row.model_id} ${row.display_name}` : undefined,
    }, row.display_name || row.model_id);
  }
  const count = (row.children?.length ?? 0) + 1;
  const tierCount = count === 1 ? t("1 个价格档位") : t("{count} 个价格档位", { count });
  return h(NButton, {
    text: true,
    class: "pricing-tree-toggle",
    "aria-expanded": rowExpanded(row),
    "aria-label": `${row.display_name}, ${tierCount}`,
    onClick: () => toggleRow(row),
  }, {
    icon: () => h(NIcon, {
      component: RightOutlined,
      class: ["pricing-tree-chevron", rowExpanded(row) && "pricing-tree-chevron--expanded"],
    }),
    default: () => row.display_name || row.model_id,
  });
}

function snapshotMultiplier(modelId: string): number {
  const group = activeGroup.value;
  return group?.content.kind === "models"
    ? group.content.snapshot.models.find(({ model_id }) => model_id === modelId)?.quota_multiplier ?? 1
    : 1;
}

function hasMultiplierDraft(modelId: string): boolean {
  return multiplierDrafts.value[modelId] !== undefined;
}

function multiplierValue(modelId: string): number | null {
  return hasMultiplierDraft(modelId)
    ? multiplierDrafts.value[modelId] ?? null
    : snapshotMultiplier(modelId);
}

function validMultiplier(value: number | null | undefined): value is number {
  return typeof value === "number" && Number.isFinite(value) && value > 0;
}

function updateMultiplierDraft(modelId: string, value: number | null) {
  const current = snapshotMultiplier(modelId);
  if (validMultiplier(value) && Math.abs(value - current) < Number.EPSILON) {
    delete multiplierDrafts.value[modelId];
    return;
  }
  multiplierDrafts.value[modelId] = value;
}

function discardMultiplierDraft(modelId: string) {
  delete multiplierDrafts.value[modelId];
}

async function reloadPricingAfterRevisionChange(): Promise<string | null> {
  try {
    if (catalog.value) await loadProviderSnapshots(catalog.value);
    else await loadPricing();
    message.warning(t("价格表已在其他位置更新，已重新加载"));
    return null;
  } catch (error) {
    return error instanceof Error ? error.message : String(error);
  }
}

async function saveMultiplier(modelId: string) {
  const group = activeGroup.value;
  const active = group?.content.kind === "models" ? group.content.snapshot : null;
  const multiplier = multiplierValue(modelId);
  if (!group || !active || !pricingMultiplierEditable(group)
    || !hasMultiplierDraft(modelId) || !validMultiplier(multiplier) || savingModelId.value) return;
  savingModelId.value = modelId;
  try {
    const result = await providerApi.updateProviderPricingMultipliers(
      group.plan.provider_id,
      active.revision,
      [{ model_id: modelId, multiplier }],
    );
    providerSnapshots.value = {
      ...providerSnapshots.value,
      [group.plan.provider_id]: result,
    };
    discardMultiplierDraft(modelId);
    message.success(t("官方倍率已保存"));
  } catch (error) {
    const detail = error instanceof Error ? error.message : String(error);
    if (
      error instanceof DashboardRequestError
      && error.status === 409
      && detail.includes("pricing revision changed")
    ) {
      const reloadError = await reloadPricingAfterRevisionChange();
      if (reloadError) {
        message.error(t("保存官方倍率失败: {error}", { error: reloadError }));
      }
    } else {
      message.error(t("保存官方倍率失败: {error}", { error: detail }));
    }
  } finally {
    savingModelId.value = null;
  }
}

async function saveProviderMultiplier(
  group: PlanPricingGroup,
  modelId: string,
  multiplier: number,
) {
  if (group.content.kind !== "values" || !pricingMultiplierEditable(group) || savingModelId.value) return;
  savingModelId.value = modelId;
  try {
    const result = await providerApi.updateProviderPricingMultipliers(
      group.plan.provider_id,
      group.content.snapshot.revision,
      [{ model_id: modelId, multiplier }],
    );
    providerSnapshots.value = { ...providerSnapshots.value, [group.plan.provider_id]: result };
    message.success(t("官方倍率已保存"));
  } catch (error) {
    const detail = error instanceof Error ? error.message : String(error);
    if (
      error instanceof DashboardRequestError
      && error.status === 409
      && detail.includes("provider pricing revision changed")
    ) {
      const reloadError = await reloadPricingAfterRevisionChange();
      if (reloadError) message.error(t("保存官方倍率失败: {error}", { error: reloadError }));
    } else {
      message.error(t("保存官方倍率失败: {error}", { error: detail }));
    }
  } finally {
    savingModelId.value = null;
  }
}

function renderMultiplierAction(
  row: PricingTableRow,
  label: string,
  icon: typeof CheckOutlined,
  action: () => void,
  primary = false,
  disabled = false,
) {
  return h(NTooltip, { trigger: "hover" }, {
    trigger: () => h(NButton, {
      circle: true,
      quaternary: true,
      size: "tiny",
      type: primary ? "primary" : "default",
      loading: primary && savingModelId.value === row.model_id,
      disabled: disabled || savingModelId.value !== null || refreshing.value,
      "aria-label": `${label}: ${row.display_name}`,
      onClick: action,
    }, { icon: () => h(NIcon, { component: icon }) }),
    default: () => label,
  });
}

function renderMultiplierEditor(row: PricingTableRow) {
  if (!row.editable_multiplier) return "";
  if (!activeGroup.value || !pricingMultiplierEditable(activeGroup.value)) {
    return formatPricingMultiplier(row.quota_multiplier);
  }
  const value = multiplierValue(row.model_id);
  const dirty = hasMultiplierDraft(row.model_id);
  const valid = validMultiplier(value);
  return h("div", { class: ["multiplier-editor", dirty && "multiplier-editor--dirty"] }, [
    h(NInputNumber, {
      value,
      min: 0.0001,
      max: 1000,
      step: 0.1,
      showButton: false,
      updateValueOnInput: true,
      size: "small",
      status: dirty && !valid ? "error" : undefined,
      disabled: savingModelId.value !== null || refreshing.value,
      inputProps: { "aria-label": `${row.display_name} ${t("官方倍率")}` },
      onUpdateValue: (nextValue: number | null) => updateMultiplierDraft(row.model_id, nextValue),
      onKeydown: (event: KeyboardEvent) => {
        if (event.key === "Enter" && dirty && valid) {
          event.preventDefault();
          void saveMultiplier(row.model_id);
        } else if (event.key === "Escape" && dirty) {
          event.preventDefault();
          discardMultiplierDraft(row.model_id);
        }
      },
    }, { prefix: () => "×" }),
    dirty
      ? h("div", { class: "multiplier-editor__actions" }, [
        renderMultiplierAction(
          row,
          t("保存倍率"),
          CheckOutlined,
          () => void saveMultiplier(row.model_id),
          true,
          !valid,
        ),
        renderMultiplierAction(row, t("放弃修改"), CloseOutlined, () => discardMultiplierDraft(row.model_id)),
      ])
      : null,
  ]);
}

const columns = computed<DataTableColumns<PricingTableRow>>(() => [
  {
    title: t("模型"),
    key: "model_id",
    width: 190,
    fixed: "left",
    ellipsis: { tooltip: true },
    render: renderModel,
  },
  { title: t("输入"), key: "input", width: 112, align: "right", render: (row) => renderRate(row.input) },
  { title: t("输出"), key: "output", width: 112, align: "right", render: (row) => renderRate(row.output) },
  { title: t("缓存读"), key: "cache_read", width: 112, align: "right", render: (row) => renderRate(row.cache_read) },
  { title: t("缓存写"), key: "cache_write", width: 112, align: "right", render: (row) => renderRate(row.cache_write) },
  { title: "Usage", key: "usage", width: 100, align: "right", render: (row) => renderRate(row.usage) },
  {
    title: t("官方倍率"),
    key: "quota_multiplier",
    width: 238,
    render: renderMultiplierEditor,
  },
]);

async function loadPricing() {
  if (loading.value) return;
  loading.value = true;
  loadError.value = "";
  try {
    snapshot.value = await dashboardApi.getPricing();
  } catch (error) {
    loadError.value = error instanceof Error ? error.message : String(error);
  } finally {
    loading.value = false;
  }
}

type MultiplierPolicy = "keep_current" | "use_official";

function multiplierDisplayName(modelId: string): string {
  const group = activeGroup.value;
  return group?.content.kind === "models"
    ? group.content.snapshot.models.find(({ model_id }) => model_id === modelId)?.display_name || modelId
    : modelId;
}

function renderMultiplierChanges(changes: readonly PricingMultiplierChange[]) {
  return h("div", { class: "pricing-refresh-comparison" }, [
    h("p", t("刷新到的官方倍率与当前设置不同。选择是否覆盖当前倍率后，才会启用新的价格与模型列表。")),
    h("div", { class: "pricing-refresh-comparison__scroll" }, [
      h("table", [
        h("thead", [h("tr", [
          h("th", { scope: "col" }, t("模型")),
          h("th", { scope: "col" }, t("当前倍率")),
          h("th", { scope: "col" }, t("最新官方倍率")),
        ])]),
        h("tbody", changes.map((change) => h("tr", { key: change.model_id }, [
          h("th", { scope: "row" }, multiplierDisplayName(change.model_id)),
          h("td", formatPricingMultiplier(change.current_multiplier)),
          h("td", formatPricingMultiplier(change.official_multiplier)),
        ]))),
      ]),
    ]),
  ]);
}

function showRefreshConfirmation(
  changes: PricingMultiplierChange[],
  expectedRevision: string | undefined,
  officialContentHash: string,
) {
  confirmationOpen.value = true;
  let instance: { destroy: () => void } | null = null;
  const close = () => {
    instance?.destroy();
    confirmationOpen.value = false;
  };
  const apply = (policy: MultiplierPolicy) => {
    close();
    void performPricingRefresh(policy, expectedRevision, officialContentHash);
  };
  instance = dialog.warning({
    title: t("价格表与当前倍率不同"),
    style: "width: min(640px, calc(100vw - 64px))",
    content: () => renderMultiplierChanges(changes),
    closable: false,
    closeOnEsc: false,
    maskClosable: false,
    action: () => h("div", { class: "pricing-refresh-actions" }, [
      h(NButton, { onClick: close }, { default: () => t("取消刷新") }),
      h(NButton, { secondary: true, onClick: () => apply("keep_current") }, {
        default: () => t("保留当前倍率"),
      }),
      h(NButton, { type: "primary", onClick: () => apply("use_official") }, {
        default: () => t("使用最新官方倍率"),
      }),
    ]),
  });
}

function requestPricingRefresh() {
  if (Object.keys(multiplierDrafts.value).length) {
    message.warning(t("请先保存或放弃倍率修改"));
    return;
  }
  void performPricingRefresh();
}

async function performPricingRefresh(
  policy?: MultiplierPolicy,
  expectedRevision = providerRefreshRevision.value,
  expectedOfficialContentHash?: string,
) {
  if (refreshing.value) return;
  const group = activeGroup.value;
  if (!group || !canRefresh(group)) return;
  refreshing.value = true;
  refreshError.value = "";
  try {
    const result = await dashboardApi.refreshProviderPricing(group.plan.provider_id, {
      policy,
      expected_provider_revision: expectedRevision,
      expected_official_content_hash: expectedOfficialContentHash,
    });
    if (result.refresh_status === "needs_confirmation") {
      if (!result.official_content_hash) {
        refreshError.value = t("刷新确认缺少官方内容哈希");
        return;
      }
      showRefreshConfirmation(
        result.multiplier_changes ?? [],
        expectedRevision,
        result.official_content_hash,
      );
    } else if (result.refresh_status === "success") {
      if (catalog.value) await loadProviderSnapshots(catalog.value);
      else if (group.plan.provider_id === "opencode") await loadPricing();
      message.success(policy === "keep_current"
        ? t("价格表已更新，已保留当前倍率")
        : policy === "use_official"
          ? t("价格表已更新，已采用最新官方倍率")
          : t("价格表已更新"));
    } else if (result.refresh_status === "unchanged") {
      if (catalog.value) await loadProviderSnapshots(catalog.value);
      else if (group.plan.provider_id === "opencode") await loadPricing();
      message.info(t("价格表没有变化"));
    } else {
      refreshError.value = result.error || t("价格表刷新失败，详见页面提示");
      message.warning(t("价格表刷新失败，详见页面提示"));
    }
  } catch (error) {
    const detail = error instanceof Error ? error.message : String(error);
    if (
      error instanceof DashboardRequestError
      && error.status === 409
      && detail.includes("pricing revision changed")
    ) {
      refreshError.value = await reloadPricingAfterRevisionChange() ?? "";
    } else if (!policy && error instanceof DashboardRequestError && error.status === 409) {
      message.warning(t("已有价格表刷新正在进行"));
    } else {
      refreshError.value = detail;
    }
  } finally {
    refreshing.value = false;
  }
}

watch(planGroups, (groups) => {
  if (groups[0] && !groups.some((group) => group.plan.id === activePlanId.value)) {
    activePlanId.value = groups[0].plan.id;
  }
});

watch(() => props.providerId, (id) => {
  if (catalog.value) void loadProviderSnapshots(catalog.value);
  else if ((id === "opencode" || !id) && !snapshot.value && !loading.value) void loadPricing();
});

onMounted(() => void loadProviderTemplates());
onMounted(() => void loadProviderCatalog());
</script>

<style scoped>
.pricing-catalog {
  display: grid;
  gap: 16px;
}
.pricing-plan-tabs {
  min-width: 0;
}
.catalog-state {
  min-height: 42px;
  display: grid;
  place-items: center;
}
.pricing-card h2 :deep(.n-tag) {
  margin-left: 8px;
  vertical-align: middle;
}
.pricing-card {
  grid-column: 1 / -1;
  padding: 22px;
  border: 1px solid var(--ocg-border);
  border-radius: 14px;
  background: var(--ocg-surface);
  box-shadow: var(--ocg-shadow-sm);
}
.pricing-head {
  display: flex;
  align-items: flex-start;
  justify-content: space-between;
  gap: 16px;
  margin-bottom: 18px;
}
.pricing-head h2 {
  margin: 0;
  color: var(--ocg-ink);
  font: 700 var(--ocg-font-xl)/1.3 "Bahnschrift", "Segoe UI Variable Display", sans-serif;
}
.pricing-head p,
.pricing-note {
  margin: 4px 0 0;
  color: var(--ocg-subtle);
  font-size: var(--ocg-font-sm);
}
.pricing-actions {
  display: flex;
  flex: 0 0 auto;
  align-items: center;
  gap: 12px;
}
.pricing-ledger {
  display: grid;
  grid-template-columns: minmax(180px, 1.4fr) repeat(5, minmax(112px, 1fr));
  gap: 1px;
  margin: 0 0 14px;
  overflow: hidden;
  border: 1px solid var(--ocg-border);
  border-radius: 10px;
  background: var(--ocg-border);
}
.pricing-ledger--compact {
  grid-template-columns: repeat(3, minmax(0, 1fr));
}
.pricing-ledger > div {
  min-width: 0;
  padding: 10px 12px;
  background: var(--ocg-canvas);
}
.pricing-ledger dt {
  margin-bottom: 4px;
  color: var(--ocg-subtle);
  font-size: var(--ocg-font-xs);
}
.pricing-ledger dd {
  overflow: hidden;
  margin: 0;
  color: var(--ocg-ink);
  font-weight: 600;
  text-overflow: ellipsis;
  white-space: nowrap;
}
.pricing-ledger code {
  font-family: "Cascadia Mono", Consolas, monospace;
}
.pricing-note {
  margin-bottom: 10px;
}
:deep(.n-data-table-expand-trigger) {
  display: none;
}
:deep(.pricing-tree-toggle) {
  min-width: 0;
}
:deep(.pricing-tree-toggle .n-button__content) {
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}
:deep(.pricing-tree-chevron) {
  transition: transform 160ms ease;
}
:deep(.pricing-tree-chevron--expanded) {
  transform: rotate(90deg);
}
:deep(.multiplier-editor) {
  display: flex;
  align-items: center;
  gap: 6px;
}
:deep(.multiplier-editor .n-input-number) {
  width: 112px;
}
:deep(.multiplier-editor--dirty .n-input-number) {
  box-shadow: 0 0 0 1px color-mix(in srgb, var(--ocg-primary) 62%, transparent);
}
:deep(.multiplier-editor__actions) {
  display: inline-flex;
  align-items: center;
  gap: 2px;
}
:deep(.tiny-rate) {
  border-bottom: 1px dotted currentColor;
  cursor: help;
}
:global(.pricing-refresh-comparison) {
  max-width: 100%;
}
:global(.pricing-refresh-comparison > p) {
  margin: 0 0 12px;
  color: var(--ocg-subtle);
}
:global(.pricing-refresh-comparison__scroll) {
  max-height: 280px;
  overflow: auto;
  border: 1px solid var(--ocg-border);
  border-radius: 8px;
}
:global(.pricing-refresh-comparison table) {
  width: 100%;
  border-collapse: collapse;
}
:global(.pricing-refresh-comparison th),
:global(.pricing-refresh-comparison td) {
  padding: 8px 10px;
  border-bottom: 1px solid var(--ocg-border);
  text-align: left;
}
:global(.pricing-refresh-comparison thead th) {
  position: sticky;
  top: 0;
  background: var(--ocg-canvas);
  color: var(--ocg-subtle);
  font-size: var(--ocg-font-xs);
}
:global(.pricing-refresh-comparison td) {
  font-family: "Cascadia Mono", Consolas, monospace;
}
:global(.pricing-refresh-actions) {
  display: flex;
  flex-wrap: wrap;
  justify-content: flex-end;
  gap: 8px;
}
@media (prefers-reduced-motion: reduce) {
  :deep(.pricing-tree-chevron) {
    transition: none;
  }
}
@media (max-width: 900px) {
  .pricing-ledger {
    grid-template-columns: repeat(3, minmax(0, 1fr));
  }
  .pricing-ledger--compact {
    grid-template-columns: repeat(2, minmax(0, 1fr));
  }
}
@media (max-width: 640px) {
  .pricing-head {
    align-items: stretch;
    flex-direction: column;
  }
  .pricing-actions {
    justify-content: space-between;
  }
  .pricing-ledger {
    grid-template-columns: repeat(2, minmax(0, 1fr));
  }
  .pricing-ledger--compact {
    grid-template-columns: repeat(1, minmax(0, 1fr));
  }
  :global(.pricing-refresh-actions) {
    align-items: stretch;
    flex-direction: column;
  }
}
</style>
