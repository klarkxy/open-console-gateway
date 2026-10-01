<template>
  <section
    v-if="accountId"
    class="official-api-panel official-api-panel--account"
    :aria-label="t('官网 API 账务参考')"
  >
    <ApiPriceMeter
      v-if="account && meterCells.length > 0"
      :cells="meterCells"
      :caption="meterCaption"
    >
      <template v-if="account.balanceAvailable" #refresh>
        <n-tooltip trigger="hover">
          <template #trigger>
            <n-button
              circle
              quaternary
              size="small"
              :aria-label="t('刷新余额')"
              :loading="mutating"
              :disabled="mutating"
              @click="emit('refresh-balance')"
            >
              <template #icon><n-icon :component="ReloadOutlined" /></template>
            </n-button>
          </template>
          {{ t("刷新余额") }}
        </n-tooltip>
      </template>
    </ApiPriceMeter>
  </section>
</template>

<script setup lang="ts">
import { computed } from "vue";
import { NButton, NIcon, NTooltip } from "naive-ui";
import { ReloadOutlined } from "@vicons/antd";
import type { OfficialApiStatus } from "../api/generated/dashboard-v4.ts";
import {
  officialApiAccountMeter,
  OFFICIAL_API_METER_EMPTY_KEYS,
} from "../domain/official-api-meter.ts";
import {
  PAY_GO_METER_EMPTY,
  PAY_GO_METER_LABEL_KEYS,
  formatPayGoObservedAt,
} from "../domain/pay-go-meter.ts";
import { formatQuotaAmount } from "../domain/platform-accounts.ts";
import { locale, t } from "../i18n/index.ts";
import type { ApiPriceMeterCell } from "./ApiPriceMeter.vue";
import ApiPriceMeter from "./ApiPriceMeter.vue";

const props = withDefaults(defineProps<{
  providerId: string;
  accountId?: string;
  accountVersion?: string;
  now?: number;
  accountStatus?: OfficialApiStatus | null;
  mutating?: boolean;
}>(), {
  mutating: false,
});
const emit = defineEmits<{ "refresh-balance": [] }>();
const account = computed(() => props.accountId ? (props.accountStatus ?? null) : null);

function money(value: number, currency: string): string {
  return formatQuotaAmount(value, currency, locale.value);
}

function timestamp(value: string): string {
  return formatPayGoObservedAt(value, locale.value);
}

const meterCaption = computed(() => {
  const observedAt = account.value?.balances[0]?.observedAt;
  return observedAt ? timestamp(observedAt) : "";
});

const meterCells = computed<ApiPriceMeterCell[]>(() => {
  const snapshot = account.value;
  if (!snapshot) return [];
  const meter = officialApiAccountMeter(snapshot);
  const cells: ApiPriceMeterCell[] = [];
  if (meter.remainingEmpty) {
    cells.push({
      key: "remaining",
      label: t(PAY_GO_METER_LABEL_KEYS.remaining),
      value: PAY_GO_METER_EMPTY,
      caption: t(OFFICIAL_API_METER_EMPTY_KEYS[meter.remainingEmpty]),
    });
  } else {
    for (const row of meter.remaining) {
      cells.push({
        key: `remaining:${row.currency}`,
        label: t(PAY_GO_METER_LABEL_KEYS.remaining),
        value: money(row.total, row.currency),
        caption: row.gift === null ? undefined : t("赠送 {amount}", {
          amount: money(row.gift, row.currency),
        }),
      });
    }
  }
  return cells;
});
</script>

<style scoped>
.official-api-panel {
  display: grid;
  gap: var(--ocg-space-sm);
  min-width: 0;
  font-size: var(--ocg-font-sm);
}

.official-api-panel--account {
  font-size: inherit;
}
</style>
