<template>
  <div ref="rowElement" class="credential-row" :data-credential-id="row.credential.id"
    :class="{ 'credential-row--unavailable': !row.routeAvailable }">
    <div class="credential-row__head">
      <n-button quaternary circle size="tiny" class="credential-order-handle"
        :aria-label="t('调整 Key {name} 的顺序', { name })" aria-describedby="account-order-instructions"
        :disabled="pending || !moveAction?.allowed" @keydown="emit('order-keydown', $event)" @click="emit('action', 'move-to-card')">
        <template #icon><n-icon :component="HolderOutlined" /></template>
      </n-button>
      <span class="credential-row__name">{{ name }}</span>
      <n-tag v-if="row.tags?.credentialCount" size="small" :bordered="false">{{ t('{count} 个凭据', { count: row.tags.credentialCount }) }}</n-tag>
      <n-tag v-if="row.tags?.bindingDisabled" size="small" :bordered="false">{{ t('绑定已禁用') }}</n-tag>
      <n-tag v-if="row.tags?.quotaShareName" size="small" :bordered="false">{{ t('与 {name} 共享额度', { name: row.tags.quotaShareName }) }}</n-tag>
      <n-tag v-else-if="row.tags?.quotaShareCount" size="small" :bordered="false">{{ t('与 {count} 个 Key 共享额度', { count: row.tags.quotaShareCount }) }}</n-tag>
      <n-tag v-if="row.tags?.duplicateName" size="small" type="warning" :bordered="false">{{ t('名称重复') }}</n-tag>
      <n-tag v-if="platformGroup" size="small" :bordered="false">{{ platformGroup }}</n-tag>
      <n-tag v-if="platformTokenName && platformTokenName !== name" size="small" :bordered="false">{{ platformTokenName }}</n-tag>
      <n-tag size="small" role="status" :type="accountPageTagType(row.status)" :title="row.account?.authError ?? ''">{{ statusText }}</n-tag>
      <n-tag v-if="cpaStatus" size="small" role="status" :type="cpaCardStatusTagType(cpaStatus)">{{ t(CPA_CARD_STATUS_KEYS[cpaStatus]) }}</n-tag>
      <n-button text size="small" class="credential-models-trigger" :disabled="pending || !modelsAction?.allowed"
        :aria-label="t('{count} 个模型', { count: row.modelCount })" @click="emit('action', 'models')">
        <n-tag size="small" :bordered="false">{{ t('{count} 个模型', { count: row.modelCount }) }}</n-tag>
      </n-button>
      <n-popover v-if="purchaseAction" trigger="click" :show="purchaseOpen" @update:show="openPurchase">
        <template #trigger><n-button text size="small" :disabled="pending || !purchaseAction.allowed"
          class="account-expiry-trigger" :aria-label="t('购买日期')">{{ expiryText }}</n-button></template>
        <div class="purchase-date-popover">
          <LazyDatePicker v-model:formatted-value="purchaseDraft" value-format="yyyy-MM-dd" type="date" clearable
            :is-date-disabled="isDateDisabled" :aria-label="t('购买日期')" />
          <div class="purchase-date-popover__actions">
            <n-button size="small" :disabled="pending || row.account?.purchaseDate === localDateString(now)"
              @click="commitPurchaseToday">{{ t('更新到今天') }}</n-button>
            <n-button type="primary" size="small" :disabled="!canSavePurchase"
            :loading="pending" @click="commitPurchase">{{ t('保存') }}</n-button></div>
        </div>
      </n-popover>
      <n-tag v-if="row.credential.scope.kind !== 'all'" size="small" :bordered="false">
        {{ row.credential.scope.singleModel ? t('仅 {model}', { model: row.credential.scope.singleModel }) : t('仅 {count} 个模型', { count: row.credential.scope.modelCount }) }}
      </n-tag>
      <n-tag v-if="row.credential.quotaRecovery && !accountPageStatus(row.status).startsWith('quota-')" size="small" role="status">{{ quotaText }}</n-tag>
      <n-button v-if="retryAction" size="tiny" secondary :disabled="pending || !retryAction.allowed"
        @click="emit('action', 'retry-quota')">{{ t('重新尝试') }}</n-button>
      <div class="credential-row__actions">
        <n-switch v-if="toggleAction" :value="row.account?.enabled ?? row.credential.enabled"
          :disabled="pending || !toggleAction.allowed" :aria-label="row.account?.enabled
            ? t('禁用账号 {name}', { name }) : t('启用账号 {name}', { name })"
          @update:value="emit('action', 'toggle')" />
        <n-button v-if="refreshAction" circle quaternary size="small" class="credential-refresh-trigger" :loading="!!refreshState"
          :disabled="pending || !refreshAction.allowed || !!refreshState" :aria-label="t('刷新')"
          @click="emit('action', 'refresh-usage')"><template #icon><n-icon :component="ReloadOutlined" /></template></n-button>
        <n-popover v-if="calibrationAction" trigger="click" placement="bottom-end" :show="calibrationOpen"
          :show-arrow="false" :width="320" @update:show="openCalibration">
          <template #trigger><n-button circle quaternary size="small" :disabled="pending || !calibrationAction.allowed"
            :aria-label="t('校准用量')"><template #icon><n-icon :component="EditOutlined" /></template></n-button></template>
          <slot name="calibration"><n-spin size="small" /></slot>
        </n-popover>
        <n-dropdown v-if="menuOptions.length" :options="menuOptions" trigger="click" placement="bottom-end"
          @select="key => emit('action', String(key))">
          <n-button circle quaternary size="small" :aria-label="t('更多操作')"><template #icon><n-icon :component="MoreOutlined" /></template></n-button>
        </n-dropdown>
      </div>
    </div>
    <AccountFigure v-if="platformFigure" compact :value="platformFigure" />
    <div v-if="row.account && row.account.setupStep !== 'ready'" class="managed-pending">
      <div><strong>{{ t(MANAGED_STEP_LABEL_KEYS[row.account.setupStep]) }}</strong>
        <p>{{ t('注册进度已保存，继续使用该账号的独立浏览器 Profile。') }}</p></div>
      <n-button type="primary" secondary :disabled="pending" @click="emit('action', 'continue-setup')">{{ t('继续注册') }}</n-button>
    </div>
    <section v-else-if="row.billing || manualReceipt" class="billing-panel" :aria-label="t('用量')">
      <OfficialApiPanel v-if="billingKind === 'cash'" :provider-id="row.account?.providerId ?? ''"
        :account-id="accountPageRowId(row)" :account-version="row.account?.updatedAt" :account-status="row.billing?.cash ?? null"
        :now="now" :mutating="pending || !!refreshState" @refresh-balance="emit('action', 'refresh-usage')" />
      <AccountCreditBalance v-else-if="billingKind === 'cash_balances'" :credit-balances="presented?.credit_balances ?? []"
        :usage-load-error="null" :usage-loading="false" @reload-usage="emit('action', 'reload-usage')" />
      <ProviderQuotaSummary v-else-if="manualReceipt || billingKind === 'quota' || billingKind === 'credits_usd_month'" :usage="manualReceipt ?? presented ?? null" :now="now" />
      <CreditMeterPanel v-if="billingKind === 'credits_meter' && row.billing" :account-id="accountPageRowId(row)"
        :binding="binding" :status="row.billing" :now="now" />
    </section>
  </div>
</template>

<script setup lang="ts">
import { computed, defineAsyncComponent, onActivated, onDeactivated, onMounted, onUnmounted, ref, watch } from "vue";
import { NButton, NDropdown, NIcon, NPopover, NSpin, NSwitch, NTag } from "naive-ui";
import { EditOutlined, HolderOutlined, MoreOutlined, ReloadOutlined } from "@vicons/antd";
import type { AccountPageRow } from "../api/pages.ts";
import { ACCOUNT_PAGE_ACTION_KEYS, ACCOUNT_PAGE_STATUS_KEYS, accountPageActionKey, accountPageCooldown,
  accountPageRowId, accountPageStatus, accountPageTagType, pageAction } from "../domain/account-page.ts";
import { MANAGED_STEP_LABEL_KEYS, accountExpiry, cooldownRemainingUntil } from "../domain/account-display.ts";
import { localDateString } from "../domain/account-lifecycle.ts";
import { accountExpiryText, formatCooldownRemainingText, quotaRecoveryText } from "../views/account-status-text.ts";
import { billingBinding, billingSurfaceKind, presentedUsageOf, type QuotaWindowsView } from "../domain/billing.ts";
import { t } from "../i18n/index.ts";
import OfficialApiPanel from "./OfficialApiPanel.vue";
import AccountCreditBalance from "./AccountCreditBalance.vue";
import CreditMeterPanel from "./CreditMeterPanel.vue";
import ProviderQuotaSummary from "./ProviderQuotaSummary.vue";
import AccountFigure from "./AccountFigure.vue";
import { formatQuotaAmount, platformGroupLabel } from "../domain/platform-accounts.ts";
import { locale } from "../i18n/index.ts";
import { CPA_CARD_STATUS_KEYS, cpaCardStatusTagType, type CpaCardStatus } from "../domain/cpa-runtime.ts";
const LazyDatePicker = defineAsyncComponent(() => import("./LazyDatePicker.vue"));

const props = defineProps<{ row: AccountPageRow; now: number; pending?: boolean; refreshState?: "queued" | "running"; manualReceipt?: QuotaWindowsView | null; cpaStatus?: CpaCardStatus | null }>();
const emit = defineEmits<{ action: [key: string]; "update-purchase-date": [date: string];
  "prepare-calibration": []; "order-keydown": [event: KeyboardEvent]; visible: [id: string, visible: boolean] }>();
const name = computed(() => props.row.account?.name ?? props.row.credential.name);
const platformFigure = computed(() => {
  const value = props.row.platformLink?.snapshot?.keyRemaining;
  return value ? formatQuotaAmount(value.amount, value.unit, locale.value) : null;
});
const platformGroup = computed(() => props.row.platformLink ? platformGroupLabel(props.row.platformLink.group) : "");
const platformTokenName = computed(() => props.row.platformLink?.snapshot?.keyName ?? "");
const statusText = computed(() => accountPageStatus(props.row.status).startsWith("quota-") && props.row.credential.quotaRecovery ? quotaText.value : accountPageStatus(props.row.status) === "cooling"
  ? t('冷却中·剩 {time}', { time: formatCooldownRemainingText(accountPageCooldown(props.row, props.now)) })
  : t(ACCOUNT_PAGE_STATUS_KEYS[accountPageStatus(props.row.status)]));
const toggleAction = computed(() => pageAction(props.row.actions, "toggle"));
const moveAction = computed(() => pageAction(props.row.actions, "move-to-card"));
const modelsAction = computed(() => pageAction(props.row.actions, "models"));
const refreshAction = computed(() => pageAction(props.row.actions, "refresh-usage"));
const calibrationAction = computed(() => pageAction(props.row.actions, "calibrate"));
const purchaseAction = computed(() => pageAction(props.row.actions, "purchase-date"));
const retryAction = computed(() => pageAction(props.row.actions, "retry-quota"));
const quotaText = computed(() => {
  const recovery = props.row.credential.quotaRecovery;
  if (!recovery) return "";
  return quotaRecoveryText(recovery.status === "waiting"
    ? { kind: "waiting", reason: recovery.reason, window: recovery.window, wait: cooldownRemainingUntil(recovery.nextRetryAt, props.now) }
    : { kind: recovery.status, reason: recovery.reason });
});
const menuOptions = computed(() => props.row.actions.filter(action => !["toggle", "refresh-usage", "calibrate", "purchase-date", "models"].includes(accountPageActionKey(action.key)))
  .map(action => ({ key: accountPageActionKey(action.key), label: ACCOUNT_PAGE_ACTION_KEYS[accountPageActionKey(action.key)]
    ? t(ACCOUNT_PAGE_ACTION_KEYS[accountPageActionKey(action.key)]) : action.key, disabled: props.pending || !action.allowed })));
const billingKind = computed(() => props.row.billing ? billingSurfaceKind(props.row.billing) : "empty");
const presented = computed(() => presentedUsageOf(props.row.billing));
const binding = computed(() => billingBinding(props.row.account?.updatedAt ?? "", props.row.inferenceEndpointUrl));
const calibrationOpen = ref(false);
function openCalibration(open: boolean): void { calibrationOpen.value = open; if (open) emit("prepare-calibration"); }
const purchaseOpen = ref(false);
const purchaseDraft = ref<string | null>(null);
const expiryText = computed(() => accountExpiryText(accountExpiry({ expires_on: props.row.account?.expiresOn ?? "" }, props.now)));
const canSavePurchase = computed(() => !!purchaseDraft.value && purchaseDraft.value <= localDateString(props.now)
  && purchaseDraft.value !== props.row.account?.purchaseDate && !props.pending);
function openPurchase(open: boolean): void { purchaseOpen.value = open; if (open) purchaseDraft.value = props.row.account?.purchaseDate || localDateString(props.now); }
function isDateDisabled(at: number): boolean { return localDateString(at) > localDateString(props.now); }
function commitPurchase(): void { if (canSavePurchase.value && purchaseDraft.value) { emit("update-purchase-date", purchaseDraft.value); purchaseOpen.value = false; } }
function commitPurchaseToday(): void { if (!props.pending) { emit("update-purchase-date", localDateString(props.now)); purchaseOpen.value = false; } }

const rowElement = ref<HTMLElement | null>(null);
let observer: IntersectionObserver | null = null;
function observe(): void {
  if (observer || !rowElement.value || typeof IntersectionObserver === "undefined") return;
  observer = new IntersectionObserver(entries => {
    for (const entry of entries) emit("visible", props.row.credential.id, entry.isIntersecting);
  }, { rootMargin: "0px", threshold: 0.01 });
  observer.observe(rowElement.value);
}
function stop(): void { observer?.disconnect(); observer = null; emit("visible", props.row.credential.id, false); }
onMounted(observe); onActivated(observe); onDeactivated(stop); onUnmounted(stop);
watch(() => props.row.credential.id, (_id, previous) => { emit("visible", previous, false); stop(); observe(); });
</script>

<style scoped>
.credential-row { display: grid; gap: var(--ocg-space-xs); padding: var(--ocg-space-sm); transition: background-color var(--ocg-motion-fast) var(--ocg-ease); min-width: 0; }
.credential-row:hover { background: var(--ocg-surface-sunken); }
.credential-row--unavailable, .credential-row--unavailable .credential-row__name { color: var(--ocg-muted); }
.credential-row__head { display: flex; align-items: center; flex-wrap: wrap; gap: var(--ocg-space-xs) var(--ocg-space-sm); }
.credential-row__name { font-weight: 600; font-size: var(--ocg-font-sm); color: var(--ocg-ink); }
.credential-row__actions { margin-left: auto; display: flex; align-items: center; gap: var(--ocg-space-xs); }
.billing-panel { display: grid; gap: var(--ocg-space-sm); min-width: 0; }
.purchase-date-popover { display: grid; gap: var(--ocg-space-sm); width: min(280px, calc(100vw - 64px)); }
.purchase-date-popover__actions { display: flex; justify-content: flex-end; gap: var(--ocg-space-sm); }
.managed-pending { display: flex; align-items: center; justify-content: space-between; gap: var(--ocg-space-md); }
.managed-pending p { font-size: var(--ocg-font-sm); color: var(--ocg-muted); margin: var(--ocg-space-xs) 0 0; }
</style>
