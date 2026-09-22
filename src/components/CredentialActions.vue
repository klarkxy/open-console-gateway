<template>
  <div v-if="accountIsReady(account)" :class="actionClass('enabled')">
    <n-tooltip trigger="hover">
      <template #trigger>
        <n-switch
          :value="account.enabled"
          :disabled="!!toggleBlockedReason"
          :aria-label="account.enabled ? t('禁用账号 {name}', { name: account.name }) : t('启用账号 {name}', { name: account.name })"
          @update:value="emit('toggle')"
        />
      </template>
      {{ toggleBlockedReason || (account.enabled
        ? t("禁用账号 {name}", { name: account.name })
        : t("启用账号 {name}", { name: account.name })) }}
    </n-tooltip>
  </div>

  <div v-if="refreshVisible" :class="actionClass('secondary')">
    <n-tooltip trigger="hover">
      <template #trigger>
        <n-button
          circle
          quaternary
          size="small"
          :aria-label="t('刷新')"
          :loading="refreshLoading"
          :disabled="usageLoading || !!usageLoadError"
          @click="emit('refresh-usage')"
        >
          <template #icon><n-icon :component="ReloadOutlined" /></template>
        </n-button>
      </template>
      {{ t("刷新") }}
    </n-tooltip>
  </div>

  <div
    v-if="(hasCreditMeter || (manualUsageCalibration && edits)) && accountIsReady(account)"
    :class="actionClass('secondary')"
  >
    <n-popover
      trigger="click"
      placement="bottom-end"
      :show-arrow="false"
      :width="320"
      :show="calibrationOpen"
      style="max-width: calc(100vw - 64px)"
      @update:show="setCalibrationOpen"
    >
      <template #trigger>
        <n-tooltip trigger="hover">
          <template #trigger>
            <n-button
              circle
              quaternary
              size="small"
              :aria-label="t('校准用量')"
              :disabled="hasCreditMeter ? creditCalibrationDisabled : !usageEditorAvailable"
            >
              <template #icon><n-icon :component="EditOutlined" /></template>
            </n-button>
          </template>
          {{ t("校准用量") }}
        </n-tooltip>
      </template>

      <CreditCalibrationEditor
        v-if="hasCreditMeter && calibrationOpen"
        :account-id="account.id"
        :binding="billing.byId[account.id]!.boundVersion"
        :status="billingStatus!"
        :now="now"
        @saved="calibrationOpen = false"
      />
      <AccountUsageEditor
        v-else-if="!hasCreditMeter"
        :account="account"
        :usage="usage"
        :limits="limits"
        :edits="edits!"
        :loading="usageLoading"
        :now="now"
        @update-draft="(key, value) => emit('usage-update-draft', key, value)"
        @update-resets-first="(key, value) => emit('usage-update-resets-first', key, value)"
        @update-resets-second="(key, value) => emit('usage-update-resets-second', key, value)"
        @save="(key) => emit('usage-save', key)"
      />
    </n-popover>
  </div>

  <div v-if="capabilities.testable" :class="actionClass('tertiary')">
    <n-tooltip trigger="hover">
      <template #trigger>
        <n-button
          circle
          quaternary
          size="small"
          :disabled="!accountIsReady(account)"
          :aria-label="t('测试账号 {name} 的连接', { name: account.name })"
          @click="emit('test-connection')"
        >
          <template #icon><n-icon :component="ApiOutlined" /></template>
        </n-button>
      </template>
      {{ accountIsReady(account) ? t("测试连接") : t("完成注册后可测试连接") }}
    </n-tooltip>
  </div>

  <div v-if="menuOptions.length > 0" :class="actionClass('menu')">
    <n-dropdown
      :options="renderedMenuOptions"
      trigger="click"
      placement="bottom-end"
      @select="(key: string | number) => emit('menu-select', key)"
    >
      <n-tooltip trigger="hover">
        <template #trigger>
          <n-button
            circle
            quaternary
            size="small"
            :aria-label="t('更多操作')"
          >
            <template #icon><n-icon :component="MoreOutlined" /></template>
          </n-button>
        </template>
        {{ t("更多操作") }}
      </n-tooltip>
    </n-dropdown>
  </div>
</template>

<script setup lang="ts">
import { computed, ref, watch } from "vue";
import {
  NButton,
  NDropdown,
  NIcon,
  NPopover,
  NSwitch,
  NTooltip,
} from "naive-ui";
import {
  ApiOutlined,
  EditOutlined,
  MoreOutlined,
  ReloadOutlined,
} from "@vicons/antd";
import type { Account, UsageWindow } from "../api/dashboard";
import type { Identity } from "../api/identities.ts";
import type { ProviderCatalogEntry } from "../api/providers.ts";
import { isUsageLimitReached } from "../domain/accounts-usage.ts";
import type { UsageKey } from "../domain/accounts-usage.ts";
import {
  accountIsReady,
  type AccountMenuOption,
} from "../domain/account-display.ts";
import { accountMenuLabelKey } from "../views/account-status-text.ts";
import { accountCapabilities } from "../domain/account-capabilities.ts";
import { findPlanDefinition } from "../domain/plans.ts";
import type { AccountUsageEdits, UsageLimitView } from "../domain/useAccountUsage.ts";
import { billingManualCalibration, creditCalibrationBlock, partitionCreditBuckets } from "../domain/billing.ts";
import { t } from "../i18n/index.ts";
import { useBillingStore } from "../stores/billing.ts";
import { accountInferenceEndpointUrl, officialBalanceSupported } from "../domain/upstream-balance.ts";
import type { Connection } from "../api/connections.ts";
import AccountUsageEditor from "./AccountUsageEditor.vue";
import CreditCalibrationEditor from "./CreditCalibrationEditor.vue";

const props = withDefaults(
  defineProps<{
    account: Account;
    identity?: Identity | null;
    catalog: readonly ProviderCatalogEntry[] | null;
    usage: UsageWindow;
    limits: UsageLimitView[];
    edits: AccountUsageEdits | undefined;
    now: number;
    usageLoading: boolean;
    usageLoadError: string | null;
    usageRefreshLoading: boolean;
    menuOptions: AccountMenuOption[];
    connections?: readonly Connection[] | null;
    compact?: boolean;
    showRefresh?: boolean;
    refreshing?: boolean;
  }>(),
  {
    identity: null,
    connections: null,
    compact: false,
    showRefresh: undefined,
    refreshing: undefined,
  },
);

const emit = defineEmits<{
  toggle: [];
  "test-connection": [];
  "refresh-usage": [];
  "menu-select": [key: string | number];
  "usage-editor-open": [];
  "usage-update-draft": [key: UsageKey, value: number | null];
  "usage-update-resets-first": [key: UsageKey, value: number | null];
  "usage-update-resets-second": [key: UsageKey, value: number | null];
  "usage-save": [key: UsageKey];
}>();

const billing = useBillingStore();
const capabilities = computed(() => accountCapabilities(props.account, props.catalog));
const plan = computed(() => findPlanDefinition(props.account.provider_id, props.catalog));
const billingStatus = computed(() => billing.byId[props.account.id]?.status ?? null);
const calibrationOpen = ref(false);
const hasCreditMeter = computed(() => Boolean(billingStatus.value?.credits));
const creditCalibrationDisabled = computed(() => Boolean(billing.byId[props.account.id]?.mutating)
  || Boolean(creditCalibrationBlock(billingStatus.value?.credits))
  || partitionCreditBuckets(billingStatus.value?.credits?.buckets ?? [], props.now).active.length === 0);
function setCalibrationOpen(show: boolean): void {
  calibrationOpen.value = show;
  if (show && !hasCreditMeter.value) emit("usage-editor-open");
}
watch(() => [props.account.id, props.account.updated_at, billing.sessionEpoch], () => { calibrationOpen.value = false; });
const manualUsageCalibration = computed(() => {
  if (billingStatus.value) return billingManualCalibration(billingStatus.value);
  return plan.value?.manual_usage_calibration ?? false;
});
const usageRefreshAvailable = computed(() => (
  billingStatus.value ? billingStatus.value.officialRefresh : plan.value?.usage_availability === "available"
));
const balanceRefreshAvailable = computed(() => officialBalanceSupported(
  accountInferenceEndpointUrl(props.account, props.identity, props.connections),
));
const canRefreshUsage = computed(() => usageRefreshAvailable.value || (
  billingStatus.value ? false : balanceRefreshAvailable.value
));
const refreshVisible = computed(() => {
  if (props.showRefresh === true) return true;
  if (props.showRefresh === false) return false;
  if (billingStatus.value?.cash) return false;
  if (!billingStatus.value && plan.value?.model_source === "official_api_preset") return false;
  return canRefreshUsage.value && accountIsReady(props.account);
});
const refreshLoading = computed(() => props.refreshing ?? props.usageRefreshLoading);
const toggleBlockedReason = computed(() => {
  if (!props.account.plan_routable) return t("该方案暂不可路由");
  return "";
});
const renderedMenuOptions = computed(() => props.menuOptions.map((option) => {
  const labelKey = accountMenuLabelKey(option.key);
  return {
    ...option,
    label: option.label ?? (labelKey ? t(labelKey) : String(option.key)),
  };
}));
const usageEditorAvailable = computed(() => {
  if (props.usageLoading || props.usageLoadError) return false;
  return props.limits.some(({ key }) => !isUsageLimitReached(props.account, key, props.now));
});

function actionClass(slot: "enabled" | "secondary" | "tertiary" | "menu"): string {
  return props.compact ? "credential-action" : `account-action account-action--${slot}`;
}
</script>

<style scoped>
.credential-action {
  display: flex;
  align-items: center;
}
</style>
