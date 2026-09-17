<template>
  <section class="official-api-panel" :aria-label="t('官网 API 账务参考')">
    <div class="official-api-head">
      <strong>{{ accountId ? t("官网余额与本月估算") : t("官网价格参考") }}</strong>
      <n-button v-if="accountId && account?.balanceAvailable" size="small" secondary :loading="busy" :disabled="!data || loading" @click="refresh('balance')">{{ t("刷新余额") }}</n-button>
      <n-button v-else-if="!accountId" size="small" secondary :loading="busy" :disabled="!data || loading" @click="refresh('prices')">{{ t("刷新价格表") }}</n-button>
    </div>
    <p v-if="loading" role="status">{{ t("加载中…") }}</p>
    <p v-if="error" class="official-api-error" role="alert">{{ error }} <n-button text size="tiny" :disabled="loading || busy" @click="load">{{ t("重试") }}</n-button>
    </p>
    <template v-if="data">
      <template v-if="account">
        <p v-if="!account.balanceAvailable">{{ t("此供应商未接入公开余额 API") }}</p>
        <p v-else-if="account.balances.length === 0">{{ t("尚未查询官网余额") }}</p>
        <dl v-else class="official-api-balances">
          <div v-for="balance in account.balances" :key="balance.currency">
            <dt>{{ t("总余额") }} · {{ balance.currency }}</dt>
            <dd class="official-api-number">{{ amount(balance.total) }}</dd>
            <dd>{{ t("赠送余额") }} {{ amount(balance.granted) }} · {{ t("充值余额") }} {{ amount(balance.toppedUp) }}</dd>
            <dd>{{ t("查询时间") }} {{ timestamp(balance.observedAt) }}</dd>
          </div>
        </dl>
        <div class="official-api-spend">
          <span>{{ t("本月本地费用估算（UTC）") }}</span>
          <span v-if="account.monthSpend.length === 0">{{ t("暂无已定价请求") }}</span>
          <span v-for="spend in account.monthSpend" :key="spend.currency" class="official-api-number">{{ spend.currency }} {{ amount(spend.amount) }}</span>
          <span v-if="account.unpricedRequests">{{ t("另有 {count} 条请求费用未知", { count: account.unpricedRequests }) }}</span>
        </div>
        <p>{{ t("余额是查询时快照，赠送与充值不重复计入总额。估算不是账单，不影响路由。") }}</p>
      </template>
      <template v-else>
        <p>{{ t("每百万 Token，按原币估算；未知模型、复杂分档或额外收费请求保持未知。") }}</p>
        <div class="official-api-table">
          <n-table size="small" :single-line="false">
            <thead><tr><th>{{ t("模型") }}</th><th>{{ t("价格时段") }}</th><th>{{ t("币种") }}</th><th>{{ t("输入") }}</th><th>{{ t("输出") }}</th><th>{{ t("缓存命中") }}</th></tr></thead>
            <tbody><tr v-for="row in data.prices.rows" :key="`${row.model}/${row.period}`">
              <td><code>{{ row.model }}</code></td><td>{{ period(row.period) }}</td><td>{{ row.currency }}</td>
              <td>{{ amount(row.inputPerMillion) }}</td><td>{{ amount(row.outputPerMillion) }}</td><td>{{ row.cacheReadPerMillion === null ? t("未知") : amount(row.cacheReadPerMillion) }}</td>
            </tr></tbody>
          </n-table>
        </div>
        <p v-if="data.prices.kind === 'deepseek'">{{ t("高峰为周一至周五 UTC 01:00–04:00、06:00–10:00，其余为低峰。") }}</p>
      </template>
      <p class="official-api-source">
        <a :href="data.prices.sourceUrl" target="_blank" rel="noopener noreferrer">{{ t("官方来源") }}</a>
        · {{ t("价格核验时间") }} {{ timestamp(data.prices.observedAt) }}
        <span v-if="stale" class="official-api-error"> · {{ t("价格参考已过期，刷新前新请求费用保持未知") }}</span>
      </p>
    </template>
  </section>
</template>

<script setup lang="ts">
import { computed, ref, watch, onBeforeUnmount } from "vue";
import { NButton, NTable } from "naive-ui";
import { officialApi } from "../api/official-api.ts";
import type { OfficialApiStatus, OfficialApiPrices } from "../api/generated/dashboard-v4.ts";
import { t } from "../i18n/index.ts";
const props = defineProps<{ providerId: string; accountId?: string; accountVersion?: string; now?: number }>();
const data = ref<OfficialApiStatus | OfficialApiPrices | null>(null);
const account = computed(() => data.value && "balances" in data.value ? data.value : null);
const loading = ref(false); const busy = ref(false); const error = ref("");
let generation = 0;
const stale = computed(() => data.value ? Date.parse(data.value.prices.validUntil) <= (props.now ?? Date.now()) : false);
const amount = (value: number) => new Intl.NumberFormat(undefined, { maximumFractionDigits: 6 }).format(value);
const timestamp = (value: string) => new Date(value).toLocaleString();
const period = (value: string) => value === "peak" ? t("高峰") : value === "off_peak" ? t("低峰") : t("全时段");
async function load() {
  const current = ++generation; loading.value = true; busy.value = false; error.value = ""; data.value = null;
  try {
    const result = props.accountId ? await officialApi.status(props.accountId) : await officialApi.prices(props.providerId);
    if (current === generation) data.value = result;
  } catch (reason) { if (current === generation) error.value = reason instanceof Error ? reason.message : String(reason); }
  finally { if (current === generation) loading.value = false; }
}
async function refresh(kind: "balance" | "prices") {
  const snapshot = data.value; if (!snapshot || busy.value || loading.value) return;
  const current = generation; const id = props.accountId; busy.value = true; error.value = "";
  const tokens = { expectedRevision: snapshot.revision, processGeneration: snapshot.processGeneration };
  try {
    const result = kind === "balance" && id
      ? await officialApi.refreshBalance(id, tokens)
      : await officialApi.refreshPrices(props.providerId, tokens);
    if (current === generation) data.value = result;
  } catch (reason) { if (current === generation) error.value = reason instanceof Error ? reason.message : String(reason); }
  finally { if (current === generation) busy.value = false; }
}
watch(() => [props.providerId, props.accountId, props.accountVersion], load, { immediate: true });
onBeforeUnmount(() => { generation++; });
</script>

<style scoped>
.official-api-panel { display: grid; gap: 8px; min-width: 0; font-size: var(--ocg-font-sm); }
.official-api-head { display: flex; justify-content: space-between; align-items: center; gap: 8px; flex-wrap: wrap; }
.official-api-panel p { margin: 0; color: var(--ocg-muted); overflow-wrap: anywhere; }
.official-api-balances { display: flex; flex-wrap: wrap; gap: 16px; margin: 0; }
.official-api-balances dd { margin: 0; }
.official-api-number { font-family: "Cascadia Mono", Consolas, monospace; font-variant-numeric: tabular-nums; }
.official-api-spend { display: flex; gap: 8px; flex-wrap: wrap; }
.official-api-table { overflow-x: auto; }
.official-api-error { color: var(--ocg-error) !important; }
.official-api-source a { color: inherit; text-decoration: underline; }
</style>
