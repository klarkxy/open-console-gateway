<template>
  <div
    class="usage-editor-popover"
    :data-usage-editor-account-id="account.id"
  >
    <p class="usage-editor-caption">
      {{ t("手动上报用量百分比，仅用于校准额度显示") }}
    </p>
    <template v-for="limit in limits" :key="limit.key">
      <div
        v-if="!isUsageLimitReached(account, limit.key, now)"
        class="usage-editor-row"
      >
        <div class="usage-editor-label">
          <span>{{ limit.label }}</span>
          <span class="usage-editor-value">
            {{ formatCost(usage[limit.key]) }} /
            {{ formatCost(limit.limit) }} ·
            {{ edits[limit.key].draft }}%
          </span>
        </div>
        <div class="usage-editor">
          <n-input-number
            :value="edits[limit.key].draft"
            :min="0"
            :max="100"
            :step="0.1"
            :precision="1"
            size="tiny"
            :show-button="false"
            :disabled="loading || edits[limit.key].saving"
            :status="edits[limit.key].error ? 'error' : undefined"
            :input-props="{
              'aria-label': t('{name} {period} 当前用量百分比', {
                name: account.name,
                period: limit.label,
              }),
            }"
            @update:value="emit('update-draft', limit.key, $event)"
            @blur="emit('save', limit.key)"
            @keydown.enter.prevent="emit('save', limit.key)"
          >
            <template #suffix>%</template>
          </n-input-number>
          <n-slider
            v-usage-slider-label="t('{name} {period} 当前用量百分比', {
              name: account.name,
              period: limit.label,
            })"
            :value="edits[limit.key].draft"
            :min="0"
            :max="100"
            :step="0.1"
            :disabled="loading || edits[limit.key].saving"
            @update:value="emit('update-draft', limit.key, $event)"
            @dragend="emit('save', limit.key)"
            @focusout="emit('save', limit.key)"
          />
        </div>
        <div class="usage-resets-row">
          <template v-if="WINDOW_FULL_MINUTES[limit.key] !== null">
            <span class="usage-resets-hint">{{ t("距上游重置还剩") }}</span>
            <n-input-number
              :value="resetsFirstFieldValue(edits[limit.key], limit.key, now)"
              :min="0"
              :max="resetsFirstFieldMax(limit.key)"
              :step="1"
              size="tiny"
              :show-button="false"
              :disabled="loading || edits[limit.key].saving"
              :input-props="{
                'aria-label': t('{name} {period} 距上游重置还剩{unit}', {
                  name: account.name,
                  period: limit.label,
                  unit: resetsFirstLabel(limit.key),
                }),
              }"
              @update:value="emit('update-resets-first', limit.key, $event)"
              @blur="emit('save', limit.key)"
              @keydown.enter.prevent="emit('save', limit.key)"
            >
              <template #suffix>{{ resetsFirstLabel(limit.key) }}</template>
            </n-input-number>
            <n-input-number
              :value="resetsSecondFieldValue(edits[limit.key], limit.key, now)"
              :min="0"
              :max="resetsSecondFieldMax(limit.key)"
              :step="1"
              size="tiny"
              :show-button="false"
              :disabled="loading || edits[limit.key].saving"
              :input-props="{
                'aria-label': t('{name} {period} 距上游重置还剩{unit}', {
                  name: account.name,
                  period: limit.label,
                  unit: resetsSecondLabel(limit.key),
                }),
              }"
              @update:value="emit('update-resets-second', limit.key, $event)"
              @blur="emit('save', limit.key)"
              @keydown.enter.prevent="emit('save', limit.key)"
            >
              <template #suffix>{{ resetsSecondLabel(limit.key) }}</template>
            </n-input-number>
          </template>
          <span v-else class="usage-resets-hint">
            {{ t("到期于 {date}", { date: account.expires_on }) }}
          </span>
        </div>
        <span
          v-if="edits[limit.key].error"
          class="usage-save-error"
          role="alert"
        >
          {{ t("用量保存失败：{error}", {
            error: edits[limit.key].error || "",
          }) }}
        </span>
      </div>
    </template>
  </div>
</template>

<script setup lang="ts">
import { NInputNumber, NSlider } from "naive-ui";
import type { Account, UsageWindow } from "../api/dashboard";
import {
  isUsageLimitReached,
  resetsFirstFieldMax,
  resetsFirstFieldValue,
  resetsSecondFieldMax,
  resetsSecondFieldValue,
  WINDOW_FULL_MINUTES,
} from "../domain/accounts-usage.ts";
import type { UsageKey } from "../domain/accounts-usage.ts";
import type { AccountUsageEdits, UsageLimitView } from "../domain/useAccountUsage.ts";
import { t } from "../i18n/index.ts";
import { formatCost } from "../utils/format.ts";

defineProps<{
  account: Account;
  usage: UsageWindow;
  limits: UsageLimitView[];
  edits: AccountUsageEdits;
  loading: boolean;
  now: number;
}>();

const emit = defineEmits<{
  "update-draft": [key: UsageKey, value: number | null];
  "update-resets-first": [key: UsageKey, value: number | null];
  "update-resets-second": [key: UsageKey, value: number | null];
  save: [key: UsageKey];
}>();

function setUsageSliderLabel(el: HTMLElement, label: string) {
  el.querySelector<HTMLElement>("[role='slider']")?.setAttribute("aria-label", label);
}

const vUsageSliderLabel = {
  mounted: (el: HTMLElement, { value }: { value: string }) => setUsageSliderLabel(el, value),
  updated: (el: HTMLElement, { value }: { value: string }) => setUsageSliderLabel(el, value),
};

function resetsFirstLabel(key: UsageKey): string {
  return key === "window_5h" ? t("小时") : t("天");
}

function resetsSecondLabel(key: UsageKey): string {
  return key === "window_5h" ? t("分钟") : t("小时");
}
</script>

<style scoped>
.usage-editor-popover {
  display: grid;
  width: 100%;
  gap: var(--ocg-space-md);
}

.usage-editor-caption {
  margin: 0;
  color: var(--ocg-muted);
  font-size: var(--ocg-font-sm);
  line-height: 1.5;
}

.usage-editor-row {
  display: grid;
  gap: var(--ocg-space-sm);
}

.usage-editor-row + .usage-editor-row {
  padding-top: var(--ocg-space-md);
  border-top: 1px solid var(--ocg-divider);
}

.usage-editor-label {
  display: flex;
  align-items: baseline;
  justify-content: space-between;
  gap: var(--ocg-space-md);
  color: var(--ocg-ink);
  font-size: var(--ocg-font-sm);
  font-weight: 600;
}

.usage-editor-value {
  color: var(--ocg-muted);
  font-family: "Cascadia Mono", Consolas, monospace;
  font-size: var(--ocg-font-xs);
  font-weight: 500;
  white-space: nowrap;
}

.usage-editor {
  display: grid;
  grid-template-columns: 78px minmax(0, 1fr);
  align-items: center;
  gap: 10px;
}

.usage-editor :deep(.n-input-number) {
  width: 78px;
}

.usage-resets-row {
  display: flex;
  flex-wrap: wrap;
  align-items: center;
  gap: 6px;
  font-size: var(--ocg-font-xs);
  color: var(--ocg-muted);
}

.usage-resets-row :deep(.n-input-number) {
  width: 72px;
}

.usage-resets-hint {
  white-space: nowrap;
}

.usage-save-error {
  color: var(--ocg-error);
  font-size: var(--ocg-font-xs);
}
</style>
