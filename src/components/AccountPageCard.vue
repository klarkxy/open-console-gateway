<template>
  <AccountCardFrame :route-id="card.cardId" :name="card.platform?.name ?? card.destination.name" :family="accountPageFamily(card)"
    :type-label="card.destination.brandFamily ?? card.destination.name" :subtitle="card.destination.baseUrl ?? ''"
    :order-handle-disabled="!!pending" :dragging="false" :tone="card.availability !== 'available' ? 'unavailable' : null"
    :class="{ 'account-card--collapsed': collapsed }" @order-keydown="emit('order-keydown', $event)"
    @order-drag-start="emit('arrange')">
    <template #tags>
      <n-tag v-if="cpaStatus" size="small" role="status" :type="cpaCardStatusTagType(cpaStatus)">{{ t(CPA_CARD_STATUS_KEYS[cpaStatus]) }}</n-tag>
      <n-tag v-if="availabilityKey" size="small" role="status">{{ t(availabilityKey) }}</n-tag>
      <span v-if="collapsed" class="destination-card-summary">{{ t('{count} 个 Key', { count: card.totalCredentials }) }}</span>
    </template>
    <template #actions>
      <n-button circle quaternary size="small" :aria-label="collapsed ? t('展开卡片') : t('收起卡片')" @click="emit('toggle-collapse')">
        <template #icon><n-icon :component="collapsed ? RightOutlined : DownOutlined" /></template>
      </n-button>
      <n-button v-if="addAction" circle quaternary size="small" :disabled="pending || !addAction.allowed"
        :aria-label="t('添加 Key')" @click="emit('action', 'add-key')"><template #icon><n-icon :component="PlusOutlined" /></template></n-button>
      <n-dropdown v-if="menuOptions.length" :options="menuOptions" trigger="click" @select="key => emit('action', String(key))">
        <n-button circle quaternary size="small" :aria-label="t('更多操作')"><template #icon><n-icon :component="MoreOutlined" /></template></n-button>
      </n-dropdown>
    </template>
    <div v-if="!collapsed" class="destination-card-body">
      <ApiPriceMeter v-if="walletCells.length" :cells="walletCells" :caption="walletCaption" />
      <n-button v-if="card.platform?.snapshot && (card.platform.snapshot.modelCount || card.platform.snapshot.groupCount)"
        text size="small" @click="emit('action', 'platform-models')">{{ t('{count} 个模型', { count: card.platform.snapshot.modelCount }) }}</n-button>
      <slot name="platform-models" />
      <div class="destination-rows"><slot /></div>
      <div v-if="offset > 0 || hasMore" class="account-page-pagination">
        <n-button size="small" :disabled="pending || loading || !offset" @click="emit('page', previousOffset ?? Math.max(0, offset - 5))">{{ t('上一页') }}</n-button>
        <span>{{ t('{count} 个 Key', { count: card.matchedCredentials }) }}</span>
        <n-button size="small" :disabled="pending || loading || !hasMore" @click="emit('page', offset + card.rows.length)">{{ t('下一页') }}</n-button>
      </div>
    </div>
  </AccountCardFrame>
</template>
<script setup lang="ts">
import { computed } from "vue";
import { NButton, NDropdown, NIcon, NTag } from "naive-ui";
import { DownOutlined, MoreOutlined, PlusOutlined, RightOutlined } from "@vicons/antd";
import type { AccountPageCard } from "../api/pages.ts";
import { ACCOUNT_PAGE_ACTION_KEYS, accountPageCardActionKey, accountPageCpaStatus, accountPageFamily, accountPageWallet, pageAction } from "../domain/account-page.ts";
import { CPA_CARD_STATUS_KEYS, cpaCardStatusTagType } from "../domain/cpa-runtime.ts";
import { CARD_QUOTA_AVAILABILITY_KEYS } from "../domain/quota-recovery.ts";
import { PAY_GO_METER_LABEL_KEYS, PAY_GO_METER_EMPTY, formatPayGoObservedAt } from "../domain/pay-go-meter.ts";
import { formatQuotaAmount } from "../domain/platform-accounts.ts";
import { locale, t } from "../i18n/index.ts";
import ApiPriceMeter from "./ApiPriceMeter.vue";
import AccountCardFrame from "./AccountCardFrame.vue";
const props = withDefaults(defineProps<{ card: AccountPageCard; collapsed: boolean; pending?: boolean; loading?: boolean; offset?: number; previousOffset?: number; hasMore?: boolean }>(), { offset: 0, hasMore: false });
const emit = defineEmits<{ action: [key: string]; "toggle-collapse": []; arrange: []; "order-keydown": [event: KeyboardEvent]; page: [offset: number] }>();
const addAction = computed(() => pageAction(props.card.actions, "add-key"));
const cpaStatus = computed(() => accountPageCpaStatus(props.card));
const availabilityKey = computed(() => (CARD_QUOTA_AVAILABILITY_KEYS as Record<string, import("../i18n/index.ts").MessageKey>)[props.card.availability] ?? null);
const wallet = computed(() => accountPageWallet(props.card));
const walletCaption = computed(() => wallet.value?.observedAt ? formatPayGoObservedAt(wallet.value.observedAt, locale.value) : "");
const walletCells = computed(() => {
  const meter = wallet.value;
  if (!meter) return [];
  const money = (value: number | null) => value == null ? PAY_GO_METER_EMPTY : formatQuotaAmount(value, meter.unit, locale.value);
  return [{ key: "remaining", label: t(PAY_GO_METER_LABEL_KEYS.remaining), value: meter.remainingUnlimited ? t("不限") : money(meter.remaining) },
    { key: "month", label: t("本月"), value: money(meter.monthUsed) }, { key: "history", label: t("历史"), value: money(meter.historyUsed) }];
});
const menuOptions = computed(() => (props.card.actions ?? []).filter(action => accountPageCardActionKey(action.key) !== "add-key")
  .map(action => ({ key: accountPageCardActionKey(action.key), label: ACCOUNT_PAGE_ACTION_KEYS[accountPageCardActionKey(action.key)]
    ? t(ACCOUNT_PAGE_ACTION_KEYS[accountPageCardActionKey(action.key)]) : action.key, disabled: props.pending || !action.allowed })));
</script>
<style scoped>
.destination-card-body { display: grid; gap: var(--ocg-space-md); min-width: 0; }
.destination-rows { display: grid; min-width: 0; }
.destination-rows > :deep(* + *) { border-top: 1px solid var(--ocg-border); }
.destination-card-summary { color: var(--ocg-muted); font-size: var(--ocg-font-sm); }
.account-page-pagination { display: flex; justify-content: flex-end; align-items: center; gap: var(--ocg-space-sm); color: var(--ocg-muted); font-size: var(--ocg-font-sm); }
</style>
