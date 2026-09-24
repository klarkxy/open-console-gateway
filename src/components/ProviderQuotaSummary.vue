<template>
  <div class="provider-quota-summary">
    <div
      v-if="displayedWindows.length === 0"
      class="provider-quota-row provider-quota-row--empty"
      role="status"
    >
      <div class="provider-quota-row__heading">
        <span>{{ t("尚未刷新") }}</span>
        <strong>—</strong>
      </div>
      <n-progress
        type="line"
        :percentage="0"
        status="default"
        :show-indicator="false"
        :height="8"
        :border-radius="4"
      />
    </div>
    <template v-else>
      <div v-for="window in displayedWindows" :key="window.window_kind" class="provider-quota-row">
        <div class="provider-quota-row__heading">
          <span>{{ windowLabel(window) }}</span>
          <strong>{{ usedLabel(window) }}</strong>
        </div>
        <n-progress
          type="line"
          :percentage="usedPercent(window)"
          :status="usedPercent(window) >= 100 ? 'error' : 'default'"
          :show-indicator="false"
          :height="8"
          :border-radius="4"
        />
        <time v-if="window.resets_at" class="provider-quota-row__reset">
          {{ t("{time}后重置", { time: formatCooldownRemainingText(cooldownRemainingUntil(window.resets_at, now)) }) }}
        </time>
      </div>
    </template>
  </div>
</template>

<script setup lang="ts">
import { NProgress } from "naive-ui";
import { computed } from "vue";
import type { ProviderQuotaWindow, ProviderUsageResponse } from "../api/providers.ts";
import { cooldownRemainingUntil } from "../domain/account-display.ts";
import { formatCooldownRemainingText } from "../views/account-status-text.ts";
import { isMiniMaxVideoQuotaWindow, providerQuotaWindowLabel } from "../domain/accounts-usage.ts";
import { t } from "../i18n/index.ts";

const props = defineProps<{ usage: ProviderUsageResponse | null; now: number }>();

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

function usedPercent(window: ProviderQuotaWindow): number {
  if (window.limit_value === null || window.limit_value <= 0) return 0;
  return Math.max(0, Math.min(100, (window.used / window.limit_value) * 100));
}

function usedLabel(window: ProviderQuotaWindow): string {
  if (window.limit_value === null) return "∞";
  if (window.unit === "percent") {
    return `${usedPercent(window).toLocaleString(undefined, { maximumFractionDigits: 1 })}%`;
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
  grid-template-columns: repeat(auto-fit, minmax(180px, 1fr));
  gap: var(--ocg-space-md);
}

.provider-quota-row {
  display: grid;
  gap: 6px;
  min-width: 0;
}

.provider-quota-row__heading {
  display: flex;
  justify-content: space-between;
  gap: var(--ocg-space-md);
  color: var(--ocg-muted);
  font-size: var(--ocg-font-sm);
}

.provider-quota-row__heading strong {
  color: var(--ocg-ink);
  font-family: "Cascadia Mono", Consolas, monospace;
  font-size: var(--ocg-font-md);
  font-variant-numeric: tabular-nums;
  font-weight: 600;
}

.provider-quota-row__reset {
  color: var(--ocg-muted);
  font-size: var(--ocg-font-xs);
  font-variant-numeric: tabular-nums;
}

@media (max-width: 640px) {
  .provider-quota-summary {
    grid-template-columns: 1fr;
  }
}
</style>
