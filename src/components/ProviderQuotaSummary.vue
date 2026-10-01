<template>
  <div class="provider-quota-summary">
    <div
      v-if="displayedWindows.length === 0"
      class="provider-quota-row provider-quota-row--empty"
      role="status"
    >
      <span class="provider-quota-row__label">{{ t("尚未刷新") }}</span>
      <span class="provider-quota-row__meter" />
      <strong class="provider-quota-row__used">{{ t("未知") }}</strong>
      <span class="provider-quota-row__reset" aria-hidden="true" />
    </div>
    <template v-else>
      <div v-for="window in displayedWindows" :key="window.window_kind" class="provider-quota-row">
        <span class="provider-quota-row__label">{{ windowLabel(window) }}</span>
        <n-progress
          v-if="quotaPercent(window) !== null"
          type="line"
          :percentage="quotaPercent(window) ?? 0"
          :status="(quotaPercent(window) ?? 0) >= 100 ? 'error' : 'default'"
          :show-indicator="false"
          :height="8"
          :border-radius="4"
        />
        <span v-else class="provider-quota-row__meter" />
        <strong class="provider-quota-row__used">{{ usedLabel(window) }}</strong>
        <time v-if="window.resets_at" class="provider-quota-row__reset">
          {{ t("{time}后重置", { time: formatCooldownRemainingText(cooldownRemainingUntil(window.resets_at, now)) }) }}
        </time>
        <span v-else class="provider-quota-row__reset" aria-hidden="true" />
      </div>
    </template>
  </div>
</template>

<script setup lang="ts">
import { NProgress } from "naive-ui";
import { computed } from "vue";
import type { ProviderQuotaWindow } from "../api/providers.ts";
import { cooldownRemainingUntil } from "../domain/account-display.ts";
import { formatCooldownRemainingText } from "../views/account-status-text.ts";
import { isMiniMaxVideoQuotaWindow, providerQuotaWindowLabel } from "../domain/accounts-usage.ts";
import { t } from "../i18n/index.ts";

const props = defineProps<{
  /** Quota windows only. Cash, credit, and sync fields are not read. */
  usage: { quota_windows: readonly ProviderQuotaWindow[] } | null;
  now: number;
}>();

const displayedWindows = computed(() => (
  props.usage?.quota_windows.filter((window) => !isMiniMaxVideoQuotaWindow(window)) ?? []
));

function windowLabel(window: ProviderQuotaWindow): string {
  return providerQuotaWindowLabel(window, {
    fiveHours: t("5 小时"),
    week: t("本周"),
    month: t("本月"),
    hours: (count) => `${count}${t("小时")}`,
  });
}

function quotaPercent(window: ProviderQuotaWindow): number | null {
  const limit = window.limit_value;
  if (typeof window.used !== "number" || !Number.isFinite(window.used)) return null;
  if (typeof limit !== "number" || !Number.isFinite(limit) || limit <= 0) return null;
  return Math.max(0, Math.min(100, (window.used / limit) * 100));
}

function usedLabel(window: ProviderQuotaWindow): string {
  const percent = quotaPercent(window);
  if (percent === null || window.limit_value === null) return t("未知");
  if (window.unit === "percent") {
    return `${percent.toLocaleString(undefined, { maximumFractionDigits: 1 })}%`;
  }
  const used = window.used.toLocaleString();
  const limit = window.limit_value.toLocaleString();
  if (window.used > window.limit_value) {
    const extra = (window.used - window.limit_value).toLocaleString();
    return `${used} / ${limit} · ${t("超出 {amount}", { amount: extra })}`;
  }
  return `${used} / ${limit}`;
}
</script>

<style scoped>
.provider-quota-summary {
  display: grid;
  gap: var(--ocg-space-sm);
}

.provider-quota-row {
  display: grid;
  grid-template-columns: minmax(4.5rem, auto) 1fr minmax(6.5rem, auto) minmax(6rem, auto);
  align-items: center;
  gap: var(--ocg-space-sm) var(--ocg-space-md);
  min-width: 0;
}

.provider-quota-row__meter {
  height: 8px;
  border-radius: 4px;
  background: var(--ocg-divider);
}

.provider-quota-row__label {
  overflow: hidden;
  color: var(--ocg-muted);
  font-size: var(--ocg-font-sm);
  text-overflow: ellipsis;
  white-space: nowrap;
}

.provider-quota-row__used {
  color: var(--ocg-ink);
  font-family: "Cascadia Mono", Consolas, monospace;
  font-size: var(--ocg-font-sm);
  font-variant-numeric: tabular-nums;
  font-weight: 600;
  justify-self: end;
  text-align: right;
}

.provider-quota-row__reset {
  color: var(--ocg-muted);
  font-size: var(--ocg-font-xs);
  font-variant-numeric: tabular-nums;
  justify-self: end;
  text-align: right;
}

@media (max-width: 640px) {
  .provider-quota-row {
    grid-template-columns: auto 1fr auto;
  }

  .provider-quota-row__reset {
    grid-column: 1 / -1;
  }
}
</style>
