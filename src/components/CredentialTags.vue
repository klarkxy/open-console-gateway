<template>
  <n-tooltip v-if="statusTooltip">
    <template #trigger>
      <n-tag :type="statusTagType" size="small">
        {{ statusLabel }}
      </n-tag>
    </template>
    {{ statusTooltip }}
  </n-tooltip>
  <n-tag v-else :type="statusTagType" size="small">
    {{ statusLabel }}
  </n-tag>
  <n-popover
    v-if="hasValidityPeriod"
    :show="purchaseDatePopoverShown"
    trigger="click"
    placement="bottom-start"
    :show-arrow="false"
    @update:show="handlePurchaseDatePopover"
  >
    <template #trigger>
      <n-button
        text
        size="small"
        class="account-expiry-trigger"
        :disabled="purchaseDateSaving"
        :aria-label="`${expiryText}；${t('到期于 {date}', { date: account.expires_on })}；${t('修改购买日期')}`"
      >
        <n-tag
          :type="accountExpiryTagType(account, now)"
          size="small"
          :bordered="false"
        >
          {{ expiryText }}
        </n-tag>
      </n-button>
    </template>
    <div class="purchase-date-popover">
      <strong>{{ t("购买日期") }}</strong>
      <n-date-picker
        v-model:formatted-value="purchaseDateDraft"
        type="date"
        value-format="yyyy-MM-dd"
        format="yyyy-MM-dd"
        :to="false"
        :clearable="false"
        :disabled="purchaseDateSaving"
        :is-date-disabled="isPurchaseDateDisabled"
        :aria-label="t('购买日期')"
      />
      <div class="purchase-date-popover__actions">
        <n-button
          size="small"
          :disabled="purchaseDateSaving || account.purchase_date === today"
          @click="commitPurchaseDate(today)"
        >
          {{ t("更新到今天") }}
        </n-button>
        <n-button
          type="primary"
          size="small"
          :loading="purchaseDateSaving"
          :disabled="!canSavePurchaseDate"
          @click="commitPurchaseDate(purchaseDateDraft)"
        >
          {{ t("保存") }}
        </n-button>
      </div>
    </div>
  </n-popover>
  <n-tag v-if="expiryDisplay === 'unknown'" size="small" :bordered="false">
    {{ t("未提供") }}
  </n-tag>
  <n-tag v-if="isManagedOnboardingAccount(account)" size="small" :bordered="false">
    {{ t("托管注册") }}
  </n-tag>
  <n-tag v-if="credentialCountLabel" size="small" :bordered="false">
    {{ credentialCountLabel }}
  </n-tag>
  <n-tag v-if="bindingDisabled" size="small" :bordered="false">
    {{ t("绑定已禁用") }}
  </n-tag>
  <n-tag v-if="modelRestrictionLabel && !hideModelRestriction" size="small" :bordered="false">
    {{ modelRestrictionLabel }}
  </n-tag>
  <n-tag v-if="quotaShareLabel" size="small" :bordered="false">
    {{ quotaShareLabel }}
  </n-tag>
  <n-tag v-if="duplicateName" size="small" type="warning" :bordered="false">
    {{ t("名称重复") }}
  </n-tag>
  <n-tag
    v-for="(tag, index) in extraTags"
    :key="`${tag}:${index}`"
    size="small"
    :bordered="false"
  >
    {{ tag }}
  </n-tag>
</template>

<script setup lang="ts">
import { computed, ref, watch } from "vue";
import {
  NButton,
  NDatePicker,
  NPopover,
  NTag,
  NTooltip,
} from "naive-ui";
import type { Account } from "../api/dashboard";
import type { Identity } from "../api/identities.ts";
import type { ProviderCatalogEntry } from "../api/providers.ts";
import { isCooling } from "../domain/accounts-usage.ts";
import {
  accountExpiry,
  accountExpiryTagType,
  cooldownDetails,
} from "../domain/account-display.ts";
import {
  accountCredentialCount,
  accountExpiryDisplay,
  inferenceLastError,
  presentedAccountStatus,
  presentedAccountStatusTagType,
  selectedBindingDisabled,
  selectedModelRestriction,
  selectedQuotaShare,
} from "../domain/account-identity.ts";
import { isManagedOnboardingAccount } from "../domain/account-capabilities.ts";
import { localDateString } from "../domain/account-lifecycle.ts";
import type { UsageLimitView } from "../domain/useAccountUsage.ts";
import {
  accountExpiryText,
  accountStatusText,
  cooldownDetailsText,
  credentialCountText,
  modelRestrictionText,
  quotaShareText,
} from "../views/account-status-text.ts";
import { t } from "../i18n/index.ts";

const props = withDefaults(
  defineProps<{
    account: Account;
    identity?: Identity | null;
    catalog: readonly ProviderCatalogEntry[] | null;
    limits: UsageLimitView[];
    now: number;
    purchaseDateSaving: boolean;
    accountNames?: Readonly<Record<string, string>>;
    extraTags?: string[];
    duplicateName?: boolean;
    hideModelRestriction?: boolean;
  }>(),
  {
    identity: null,
    accountNames: undefined,
    extraTags: () => [],
    duplicateName: false,
    hideModelRestriction: false,
  },
);

const emit = defineEmits<{
  "update-purchase-date": [date: string];
}>();

const overlayIdentity = computed(() => props.identity ?? null);
const credentialCountLabel = computed(() => {
  const count = accountCredentialCount(overlayIdentity.value);
  return count === null ? null : credentialCountText(count);
});
const statusLabel = computed(() => (
  accountStatusText(presentedAccountStatus(
    props.account,
    overlayIdentity.value,
    props.now,
    props.catalog,
  ))
));
const statusTagType = computed(() => (
  presentedAccountStatusTagType(
    props.account,
    overlayIdentity.value,
    props.now,
    props.catalog,
  )
));
const statusTooltip = computed(() => {
  if (props.account.auth_error) return props.account.auth_error;
  const overlayError = inferenceLastError(overlayIdentity.value, props.account.id);
  if (overlayError) return overlayError;
  if (isCooling(props.account, props.now)) {
    return cooldownDetailsText(cooldownDetails(props.account, props.now, props.limits));
  }
  return "";
});
const bindingDisabled = computed(() => (
  selectedBindingDisabled(overlayIdentity.value, props.account.id)
));
const modelRestrictionLabel = computed(() => {
  const restriction = selectedModelRestriction(overlayIdentity.value, props.account.id);
  return restriction ? modelRestrictionText(restriction) : null;
});
const quotaShareLabel = computed(() => {
  const share = selectedQuotaShare(
    overlayIdentity.value,
    props.account.id,
    (id) => props.accountNames?.[id] ?? null,
  );
  return share ? quotaShareText(share) : null;
});
const expiryDisplay = computed(() => (
  accountExpiryDisplay(props.account, overlayIdentity.value, props.catalog)
));
const hasValidityPeriod = computed(() => expiryDisplay.value === "v3");
const purchaseDatePopoverShown = ref(false);
const purchaseDateDraft = ref<string | null>(props.account.purchase_date || null);
const today = computed(() => localDateString(props.now));
const canSavePurchaseDate = computed(() => (
  !props.purchaseDateSaving
  && !!purchaseDateDraft.value
  && purchaseDateDraft.value <= today.value
  && purchaseDateDraft.value !== props.account.purchase_date
));
const expiryText = computed(() => accountExpiryText(accountExpiry(props.account, props.now)));

function handlePurchaseDatePopover(show: boolean): void {
  purchaseDatePopoverShown.value = show;
  if (show) purchaseDateDraft.value = props.account.purchase_date || today.value;
}

function isPurchaseDateDisabled(timestamp: number): boolean {
  return localDateString(timestamp) > today.value;
}

function commitPurchaseDate(date: string | null): void {
  if (!date || date > today.value || date === props.account.purchase_date) {
    purchaseDatePopoverShown.value = false;
    return;
  }
  emit("update-purchase-date", date);
  purchaseDatePopoverShown.value = false;
}

watch(() => props.account.purchase_date, (value) => {
  if (!purchaseDatePopoverShown.value) purchaseDateDraft.value = value || null;
});
</script>

<style scoped>
.account-expiry-trigger {
  min-width: 0;
}

.account-expiry-trigger :deep(.n-button__content) {
  min-width: 0;
}

.purchase-date-popover {
  display: grid;
  gap: var(--ocg-space-sm);
  width: min(280px, calc(100vw - 64px));
}

.purchase-date-popover__actions {
  display: flex;
  justify-content: flex-end;
  gap: var(--ocg-space-sm);
}
</style>
