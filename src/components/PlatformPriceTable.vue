<template>
  <div v-if="rows.length > 0" class="platform-block">
    <div class="platform-block-title">{{ t("模型候选与价格") }}</div>
    <table class="platform-table">
      <thead>
        <tr>
          <th>{{ t("模型") }}</th>
          <th>{{ t("分组") }}</th>
          <th>{{ t("输入") }}</th>
          <th>{{ t("输出") }}</th>
          <th>{{ t("缓存读") }}</th>
          <th>{{ t("缓存写") }}</th>
          <th>{{ t("标记") }}</th>
          <th>{{ t("来源") }}</th>
        </tr>
      </thead>
      <tbody>
        <tr v-for="row in rows" :key="row.key">
          <td class="mono">{{ row.model }}</td>
          <td>{{ row.groupId ?? t("无分组") }}</td>
          <td v-for="(rate, rateIndex) in row.rates" :key="rateIndex" class="mono">
            <n-tooltip v-if="rate && rate.perMillion" trigger="hover">
              <template #trigger><span>{{ rate.label }}</span></template>
              {{ rate.perMillion }}
            </n-tooltip>
            <span v-else-if="rate">{{ rate.label }}</span>
            <span v-else class="platform-unknown">{{ t("未知") }}</span>
          </td>
          <td>
            <n-space size="small" wrap>
              <n-tag
                v-if="row.distinction === 'billed'"
                size="small"
                type="success"
                :bordered="false"
              >{{ t("实际计费价") }}</n-tag>
              <n-tag
                v-for="flag in row.flags"
                :key="flag"
                size="small"
                :type="flag === 'official_reference' ? 'info' : 'warning'"
                :bordered="false"
              >{{ flagLabel(flag, row.price) }}</n-tag>
            </n-space>
          </td>
          <td>{{ row.source }}</td>
        </tr>
      </tbody>
    </table>
  </div>
</template>

<script setup lang="ts">
import { computed } from "vue";
import { NSpace, NTag, NTooltip } from "naive-ui";
import type { PlatformPrice, PlatformSnapshot } from "../api/platform-accounts.ts";
import {
  formatPlatformRate,
  platformPriceRows,
  platformUnavailableReasonKey,
  type FormattedPlatformRate,
  type PlatformPriceFlag,
  type PlatformPriceRow,
} from "../domain/platform-accounts.ts";
import { locale, t, type MessageKey } from "../i18n/index.ts";

/**
 * Full model/price table for one platform snapshot. Shared by the platform
 * parent and each linked Key: user-credential parent refreshes intentionally
 * return no models/prices, so the Key snapshot is where prices usually live.
 * Rates stay in the original currency per token; rows carry localized flags,
 * expired/stale markers, and billed/official distinction.
 */
const props = defineProps<{ snapshot: PlatformSnapshot }>();

interface PriceRowDisplay extends PlatformPriceRow {
  rates: (FormattedPlatformRate | null)[];
}

const rows = computed<PriceRowDisplay[]>(() => (
  platformPriceRows(props.snapshot, Date.now() / 1000).map((row) => ({
    ...row,
    rates: row.price
      ? [
        formatPlatformRate(row.price.input, row.price.currency, locale.value),
        formatPlatformRate(row.price.output, row.price.currency, locale.value),
        formatPlatformRate(row.price.cacheRead, row.price.currency, locale.value),
        formatPlatformRate(row.price.cacheWrite, row.price.currency, locale.value),
      ]
      : [null, null, null, null],
  }))
));

function flagLabel(flag: PlatformPriceFlag, price: PlatformPrice | null): string {
  switch (flag) {
    case "official_reference": return t("官方参考价");
    case "unavailable": {
      const reason = price?.unavailableReason ?? "";
      const reasonKey = platformUnavailableReasonKey(reason);
      return t("不可用：{reason}", { reason: reasonKey ? t(reasonKey as MessageKey) : reason });
    }
    case "expired": return t("价格已过期");
    case "stale": return t("快照已过期");
  }
}
</script>

<style scoped>
.platform-block {
  display: grid;
  gap: 6px;
}

.platform-block-title {
  font-size: var(--ocg-font-xs);
  font-weight: 600;
  color: var(--ocg-muted);
}

.platform-unknown {
  font-size: var(--ocg-font-sm);
  color: var(--ocg-subtle);
}

.platform-table {
  width: 100%;
  border-collapse: collapse;
  font-size: var(--ocg-font-sm);
}

.platform-table th {
  text-align: left;
  font-size: var(--ocg-font-xs);
  font-weight: 600;
  color: var(--ocg-muted);
  padding: 4px 8px;
  border-bottom: 1px solid var(--ocg-border);
}

.platform-table td {
  padding: 4px 8px;
  border-bottom: 1px solid var(--ocg-divider);
  vertical-align: top;
}
</style>
