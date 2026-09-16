<template>
  <div class="account-credit-balance">
    <div v-if="usageLoadError" class="usage-load-error" role="alert">
      <span>{{ t("用量加载失败") }}</span>
      <n-button text size="tiny" type="primary" :loading="usageLoading" @click="emit('reload-usage')">
        {{ t("重试") }}
      </n-button>
    </div>
    <div v-else-if="creditBalances.length === 0" class="account-credit-balance__empty">
      {{ t("尚未刷新") }}
    </div>
    <template v-else>
      <div v-for="row in creditBalances" :key="row.balance_kind" class="account-credit-balance__row">
        {{ t("当前余额 {value}", { value: formatQuotaAmount(row.amount, row.unit, locale) }) }}
      </div>
    </template>
  </div>
</template>

<script setup lang="ts">
import { NButton } from "naive-ui";
import type { ProviderCreditBalance } from "../api/providers.ts";
import { locale, t } from "../i18n/index.ts";
import { formatQuotaAmount } from "../domain/platform-accounts.ts";

defineProps<{
  creditBalances: readonly ProviderCreditBalance[];
  usageLoadError: string | null;
  usageLoading: boolean;
}>();

const emit = defineEmits<{
  "reload-usage": [];
}>();
</script>

<style scoped>
.account-credit-balance {
  display: grid;
  gap: var(--ocg-space-xs);
  margin-top: var(--ocg-space-sm);
  font-size: var(--ocg-font-sm);
}

.account-credit-balance__empty {
  color: var(--ocg-subtle);
}

.account-credit-balance__row {
  font-variant-numeric: tabular-nums;
}

.usage-load-error {
  display: flex;
  align-items: center;
  justify-content: center;
  gap: var(--ocg-space-sm);
  min-height: 42px;
  color: var(--ocg-error);
  font-size: var(--ocg-font-sm);
}
</style>
