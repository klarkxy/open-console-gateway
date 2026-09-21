<template>
  <div v-if="usageLoadError" class="usage-load-error" role="alert">
    <span>{{ t("用量加载失败") }}</span>
    <n-button text size="tiny" type="primary" :loading="usageLoading" @click="emit('reload-usage')">
      {{ t("重试") }}
    </n-button>
  </div>
  <ApiPriceMeter v-else :cells="cells" />
</template>

<script setup lang="ts">
import { computed } from "vue";
import { NButton } from "naive-ui";
import type { ProviderCreditBalance } from "../api/providers.ts";
import { locale, t } from "../i18n/index.ts";
import { formatQuotaAmount } from "../domain/platform-accounts.ts";
import ApiPriceMeter, { type ApiPriceMeterCell } from "./ApiPriceMeter.vue";

const props = defineProps<{
  creditBalances: readonly ProviderCreditBalance[];
  usageLoadError: string | null;
  usageLoading: boolean;
}>();

const emit = defineEmits<{
  "reload-usage": [];
}>();

const cells = computed<ApiPriceMeterCell[]>(() => {
  if (props.creditBalances.length === 0) {
    return [{
      key: "remaining",
      label: t("余额"),
      value: "—",
      caption: t("尚未刷新"),
    }];
  }
  return props.creditBalances.map((row) => ({
    key: `${row.balance_kind}:${row.unit}`,
    label: t("余额"),
    value: formatQuotaAmount(row.amount, row.unit, locale.value),
    caption: formatObservedAt(row.observed_at, locale.value),
  }));
});

function formatObservedAt(value: string | null, localeName: string): string | undefined {
  if (!value) return undefined;
  const ms = Date.parse(value);
  if (!Number.isFinite(ms)) return undefined;
  return new Intl.DateTimeFormat(localeName, {
    month: "2-digit",
    day: "2-digit",
    hour: "2-digit",
    minute: "2-digit",
  }).format(new Date(ms));
}
</script>

<style scoped>
.usage-load-error {
  display: flex;
  align-items: center;
  gap: var(--ocg-space-sm);
  color: var(--ocg-error);
  font-size: var(--ocg-font-sm);
}
</style>
