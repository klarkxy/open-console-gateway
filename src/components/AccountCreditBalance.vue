<template>
  <div class="account-credit-balance">
    <div v-if="usageLoadError" class="usage-load-error" role="alert">
      <span>{{ t("用量加载失败") }}</span>
      <n-button text size="tiny" type="primary" :loading="usageLoading" @click="emit('reload-usage')">
        {{ t("重试") }}
      </n-button>
    </div>
    <AccountFigure
      v-else-if="creditBalances.length === 0"
      :label="t('余额')"
      value="—"
      :caption="t('尚未刷新')"
    />
    <template v-else>
      <AccountFigure
        v-for="row in creditBalances"
        :key="row.balance_kind"
        :label="t('余额')"
        :value="formatQuotaAmount(row.amount, row.unit, locale)"
      />
    </template>
  </div>
</template>

<script setup lang="ts">
import { NButton } from "naive-ui";
import type { ProviderCreditBalance } from "../api/providers.ts";
import { locale, t } from "../i18n/index.ts";
import { formatQuotaAmount } from "../domain/platform-accounts.ts";
import AccountFigure from "./AccountFigure.vue";

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
  justify-items: start;
  gap: var(--ocg-space-xs);
}

.usage-load-error {
  display: flex;
  align-items: center;
  gap: var(--ocg-space-sm);
  color: var(--ocg-error);
  font-size: var(--ocg-font-sm);
}
</style>
