<template>
  <div class="goat-pricing-reference">
    <dl class="pricing-ledger">
      <div class="pricing-ledger__revision">
        <dt>{{ t("修订版本") }}</dt>
        <dd><code>{{ snapshot?.revision ?? "—" }}</code></dd>
      </div>
      <div>
        <dt>{{ t("启用时间") }}</dt>
        <dd>{{ snapshot ? formatTimestamp(snapshot.activated_at) : "—" }}</dd>
      </div>
      <div>
        <dt>{{ t("文档更新时间") }}</dt>
        <dd>{{ snapshot ? documentUpdatedAt : "—" }}</dd>
      </div>
    </dl>

    <p class="pricing-note">
      USD / 1M tokens · {{ t("未知价格不会参与费用估算") }}
    </p>

    <n-data-table
      :columns="columns"
      :data="rows"
      :pagination="false"
      :row-key="rowKey"
      :scroll-x="870"
      size="small"
    />
  </div>
</template>

<script setup lang="ts">
import { computed, h, ref, watch } from "vue";
import { NButton, NDataTable, NIcon, NInputNumber, NTooltip } from "naive-ui";
import type { DataTableColumns } from "naive-ui";
import { CheckOutlined, CloseOutlined } from "@vicons/antd";
import { locale, t } from "../i18n/index.ts";
import type { ProviderNeutralPricingSnapshot } from "../api/providers.ts";
import { formatPricingRate } from "../domain/pricing-view.ts";

type OfficialRate = number | "free" | null;

interface GoatPricingRow {
  modelId: string;
  model: string;
  input: OfficialRate;
  output: OfficialRate;
  cacheRead: OfficialRate;
  cacheWrite: OfficialRate;
  quotaMultiplier: number | null;
}

const props = defineProps<{
  snapshot?: ProviderNeutralPricingSnapshot | null;
  savingModelId?: string | null;
  disabled?: boolean;
  editable?: boolean;
}>();
const emit = defineEmits<{
  "save-multiplier": [modelId: string, multiplier: number];
}>();
const multiplierDrafts = ref<Partial<Record<string, number | null>>>({});

watch(() => props.snapshot?.revision, () => {
  multiplierDrafts.value = {};
});

const rows = computed<GoatPricingRow[]>(() => {
  if (!props.snapshot) return [];
  return props.snapshot.values.map((row) => {
    const free = row.input_per_million === null
      && row.output_per_million === null
      && row.cache_read_per_million === null;
    return {
      modelId: row.model_id,
      model: row.display_name,
      input: free ? "free" : row.input_per_million,
      output: free ? "free" : row.output_per_million,
      cacheRead: free ? "free" : row.cache_read_per_million,
      cacheWrite: row.cache_write_per_million,
      quotaMultiplier: free ? null : row.quota_multiplier,
    };
  });
});

const documentUpdatedAt = computed(() => {
  const value = props.snapshot?.document_updated_at;
  return value ? formatTimestamp(value) : "—";
});

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

function renderOfficialRate(value: OfficialRate) {
  if (value === "free") return t("免费");
  const formatted = formatPricingRate(value, locale.value);
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

function hasDraft(modelId: string): boolean {
  return multiplierDrafts.value[modelId] !== undefined;
}

function multiplierValue(row: GoatPricingRow): number | null {
  return hasDraft(row.modelId)
    ? multiplierDrafts.value[row.modelId] ?? null
    : row.quotaMultiplier;
}

function validMultiplier(value: number | null): value is number {
  return typeof value === "number" && Number.isFinite(value) && value > 0 && value <= 1000;
}

function updateDraft(row: GoatPricingRow, value: number | null) {
  if (validMultiplier(value) && value === row.quotaMultiplier) {
    delete multiplierDrafts.value[row.modelId];
  } else {
    multiplierDrafts.value[row.modelId] = value;
  }
}

function discardDraft(modelId: string) {
  delete multiplierDrafts.value[modelId];
}

function renderMultiplierAction(
  row: GoatPricingRow,
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
      loading: primary && props.savingModelId === row.modelId,
      disabled: disabled || props.disabled || Boolean(props.savingModelId),
      "aria-label": `${label}: ${row.model}`,
      onClick: action,
    }, { icon: () => h(NIcon, { component: icon }) }),
    default: () => label,
  });
}

function renderQuotaMultiplier(row: GoatPricingRow) {
  if (row.quotaMultiplier === null || !props.snapshot) return "—";
  if (!props.editable) return `×${row.quotaMultiplier.toLocaleString()}`;
  const value = multiplierValue(row);
  const dirty = hasDraft(row.modelId);
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
      disabled: props.disabled || Boolean(props.savingModelId),
      inputProps: { "aria-label": `${row.model} ${t("官方倍率")}` },
      onUpdateValue: (next: number | null) => updateDraft(row, next),
      onKeydown: (event: KeyboardEvent) => {
        if (event.key === "Enter" && dirty && valid) {
          event.preventDefault();
          emit("save-multiplier", row.modelId, value);
        } else if (event.key === "Escape" && dirty) {
          event.preventDefault();
          discardDraft(row.modelId);
        }
      },
    }, { prefix: () => "×" }),
    dirty
      ? h("div", { class: "multiplier-editor__actions" }, [
        renderMultiplierAction(
          row,
          t("保存倍率"),
          CheckOutlined,
          () => valid && emit("save-multiplier", row.modelId, value),
          true,
          !valid,
        ),
        renderMultiplierAction(row, t("放弃修改"), CloseOutlined, () => discardDraft(row.modelId)),
      ])
      : null,
  ]);
}

const columns = computed<DataTableColumns<GoatPricingRow>>(() => [
  {
    title: t("模型"),
    key: "model",
    width: 190,
    fixed: "left",
    ellipsis: { tooltip: true },
  },
  { title: t("输入"), key: "input", width: 112, align: "right", render: (row) => renderOfficialRate(row.input) },
  { title: t("输出"), key: "output", width: 112, align: "right", render: (row) => renderOfficialRate(row.output) },
  { title: t("缓存读"), key: "cacheRead", width: 112, align: "right", render: (row) => renderOfficialRate(row.cacheRead) },
  { title: t("缓存写"), key: "cacheWrite", width: 112, align: "right", render: (row) => renderOfficialRate(row.cacheWrite) },
  { title: t("官方倍率"), key: "quotaMultiplier", width: 238, render: renderQuotaMultiplier },
]);

function rowKey(row: GoatPricingRow): string {
  return row.modelId;
}
</script>

<style scoped>
.goat-pricing-reference {
  min-width: 0;
  width: 100%;
}

.pricing-ledger {
  display: grid;
  grid-template-columns: minmax(180px, 1.4fr) repeat(2, minmax(112px, 1fr));
  gap: 1px;
  margin: 0 0 14px;
  overflow: hidden;
  border: 1px solid var(--ocg-border);
  border-radius: 10px;
  background: var(--ocg-border);
  font-variant-numeric: tabular-nums;
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
  margin: 0 0 10px;
  color: var(--ocg-subtle);
  font-size: var(--ocg-font-sm);
}

:deep(.n-data-table-td) {
  font-variant-numeric: tabular-nums;
}

:deep(.tiny-rate) {
  border-bottom: 1px dotted currentColor;
  cursor: help;
}

:deep(.multiplier-editor) {
  display: flex;
  align-items: center;
  gap: 6px;
}

:deep(.multiplier-editor .n-input-number) {
  width: 118px;
}

:deep(.multiplier-editor__actions) {
  display: flex;
  align-items: center;
  gap: 2px;
}

@media (max-width: 900px) {
  .pricing-ledger {
    grid-template-columns: repeat(3, minmax(0, 1fr));
  }
}

@media (max-width: 640px) {
  .pricing-ledger {
    grid-template-columns: repeat(2, minmax(0, 1fr));
  }
}
</style>
