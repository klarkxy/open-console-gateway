<template>
  <section
    class="official-api-panel"
    :class="{ 'official-api-panel--account': Boolean(accountId) }"
    :aria-label="t('官网 API 账务参考')"
  >
    <div v-if="!accountId" class="official-api-head">
      <strong>{{ t("官网价格参考") }}</strong>
      <n-button
        size="small"
        secondary
        :loading="mutating"
        :disabled="!prices || loading"
        @click="refresh('prices')"
      >
        {{ t("刷新价格表") }}
      </n-button>
    </div>
    <p v-if="initialLoading" role="status">{{ t("加载中…") }}</p>
    <p v-if="errorKey" class="official-api-error" role="alert">
      {{ t(errorKey) }}
      <n-button text size="tiny" :disabled="loading || mutating" @click="load">{{ t("重试") }}</n-button>
    </p>
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
              :disabled="mutating || loading"
              @click="refresh('balance')"
            >
              <template #icon><n-icon :component="ReloadOutlined" /></template>
            </n-button>
          </template>
          {{ t("刷新余额") }}
        </n-tooltip>
      </template>
    </ApiPriceMeter>
    <template v-else-if="prices && !account">
      <div class="official-api-table">
        <n-table size="small" :single-line="false">
          <thead>
            <tr>
              <th>{{ t("模型") }}</th>
              <th>{{ t("价格时段") }}</th>
              <th>{{ t("币种") }}</th>
              <th>{{ t("输入") }}</th>
              <th>{{ t("输出") }}</th>
              <th>{{ t("缓存命中") }}</th>
            </tr>
          </thead>
          <tbody>
            <tr v-for="row in prices!.prices.rows" :key="`${row.model}/${row.period}`">
              <td><code>{{ row.model }}</code></td>
              <td>{{ period(row.period) }}</td>
              <td>{{ row.currency }}</td>
              <td>{{ amount(row.inputPerMillion) }}</td>
              <td>{{ amount(row.outputPerMillion) }}</td>
              <td>{{ row.cacheReadPerMillion === null ? t("未知") : amount(row.cacheReadPerMillion) }}</td>
            </tr>
          </tbody>
        </n-table>
      </div>
      <p class="official-api-source">
        <a :href="prices!.prices.sourceUrl" target="_blank" rel="noopener noreferrer">{{ t("官方来源") }}</a>
        · {{ timestamp(prices!.prices.observedAt) }}
        <span v-if="stale" class="official-api-error"> · {{ t("价格已过期") }}</span>
      </p>
    </template>
  </section>
</template>

<script setup lang="ts">
import { computed, watch } from "vue";
import { NButton, NIcon, NTable, NTooltip } from "naive-ui";
import { ReloadOutlined } from "@vicons/antd";
import type { OfficialApiStatus } from "../api/generated/dashboard-v4.ts";
import { BILLING_ERROR_KEYS } from "../domain/billing.ts";
import {
  joinOfficialApiSpend,
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
import { useBillingStore } from "../stores/billing.ts";
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
const store = useBillingStore();
const priceSlot = computed(() => store.pricesById[props.providerId]);
const prices = computed(() => priceSlot.value?.prices ?? null);
const account = computed(() => props.accountId ? (props.accountStatus ?? null) : null);
const loading = computed(() => priceSlot.value?.loading ?? false);
const mutating = computed(() => props.accountId ? props.mutating : (priceSlot.value?.mutating ?? false));
const initialLoading = computed(() => !props.accountId && loading.value && !prices.value);
const errorKey = computed(() => {
  if (props.accountId) return null;
  const code = priceSlot.value?.error;
  return code ? BILLING_ERROR_KEYS[code] : null;
});
const stale = computed(() => {
  const sheet = account.value?.prices ?? prices.value?.prices;
  return sheet ? Date.parse(sheet.validUntil) <= (props.now ?? Date.now()) : false;
});

function amount(value: number): string {
  return new Intl.NumberFormat(undefined, { maximumFractionDigits: 6 }).format(value);
}

function money(value: number, currency: string): string {
  return formatQuotaAmount(value, currency, locale.value);
}

function timestamp(value: string): string {
  return formatPayGoObservedAt(value, locale.value);
}

function period(value: string): string {
  return value === "peak" ? t("高峰") : value === "off_peak" ? t("低峰") : t("全时段");
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
  const month = joinOfficialApiSpend(meter.monthSpend, money);
  cells.push({
    key: "month",
    label: t(PAY_GO_METER_LABEL_KEYS.month),
    value: month ?? PAY_GO_METER_EMPTY,
    caption: meter.unpriced > 0 ? t("另有 {count} 条未知", { count: meter.unpriced }) : undefined,
    hint: t("本地估算，不是账单"),
  });
  const history = joinOfficialApiSpend(meter.lifetimeSpend, money);
  cells.push({
    key: "history",
    label: t(PAY_GO_METER_LABEL_KEYS.history),
    value: history ?? PAY_GO_METER_EMPTY,
    hint: t("本地估算，不是账单"),
  });
  return cells;
});

function load(): void {
  if (props.accountId) return;
  void store.loadPrices(props.providerId);
}

async function refresh(kind: "balance" | "prices") {
  if (kind === "balance" && props.accountId) {
    emit("refresh-balance");
    return;
  }
  if (!prices.value || mutating.value || loading.value) return;
  try {
    await store.refreshPrices(props.providerId);
  } catch {
    // Store keeps the last good sheet.
  }
}

watch(() => props.providerId, load, { immediate: true });
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

.official-api-head {
  display: flex;
  justify-content: space-between;
  align-items: center;
  gap: var(--ocg-space-sm);
  flex-wrap: wrap;
}

.official-api-panel p {
  margin: 0;
  color: var(--ocg-muted);
  overflow-wrap: anywhere;
}

.official-api-table {
  overflow-x: auto;
}

.official-api-error {
  color: var(--ocg-error) !important;
}

.official-api-source a {
  color: inherit;
  text-decoration: underline;
}
</style>
