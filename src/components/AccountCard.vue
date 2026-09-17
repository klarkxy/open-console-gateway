<template>
  <n-card
    :data-account-id="account.id"
    size="small"
    class="account-card"
    :class="{
      'account-card--cooling': isCooling(account, now),
      'account-card--pending': !accountIsReady(account),
      'account-card--draft': isDraft,
      'account-card--dragging': dragging,
    }"
  >
    <template #header>
      <div class="account-title">
        <n-tooltip trigger="hover">
          <template #trigger>
            <n-button
              circle
              quaternary
              size="small"
              class="account-order-handle"
              :class="{ 'account-order-handle--dragging': dragging }"
              :disabled="orderHandleDisabled"
              :aria-label="t('拖动调整账号 {name} 的优先级', { name: account.name })"
              aria-describedby="account-order-instructions"
              @click.prevent
              @keydown="emit('order-keydown', $event)"
              @pointerdown="emit('order-drag-start', $event)"
            >
              <template #icon><n-icon :component="HolderOutlined" /></template>
            </n-button>
          </template>
          {{ t("拖动调整账号 {name} 的优先级", { name: account.name }) }}
        </n-tooltip>
        <div class="account-heading">
          <div class="account-name-row">
            <span class="account-name">{{ account.name }}</span>
            <n-tag v-if="isCpa" type="info" size="small" :bordered="false">
              {{ t("CPA 订阅池") }}
            </n-tag>
            <n-tag v-else-if="account.account_type === 'managed'" type="info" size="small" :bordered="false">
              {{ t("托管注册") }}
            </n-tag>
            <n-tag v-else-if="isZen" type="info" size="small" :bordered="false">
              {{ t("免费通道") }}
            </n-tag>
            <n-tag
              v-else
              :type="isDraft ? 'warning' : 'default'"
              size="small"
              :bordered="false"
            >
              {{ planLabel(account, catalog) }}
            </n-tag>
            <n-tag
              v-if="showsDeclaredRelation"
              size="small"
              :bordered="false"
            >
              {{ t("声明关系") }}
            </n-tag>
            <n-tag
              v-if="credentialCountLabel"
              size="small"
              :bordered="false"
            >
              {{ credentialCountLabel }}
            </n-tag>
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
            <n-tag
              v-if="bindingDisabled"
              size="small"
              :bordered="false"
            >
              {{ t("绑定已禁用") }}
            </n-tag>
            <n-tag
              v-if="modelRestrictionLabel"
              size="small"
              :bordered="false"
            >
              {{ modelRestrictionLabel }}
            </n-tag>
            <n-tag
              v-if="quotaShareLabel"
              size="small"
              :bordered="false"
            >
              {{ quotaShareLabel }}
            </n-tag>
            <n-tag
              v-if="expiryDisplay === 'unknown'"
              size="small"
              :bordered="false"
            >
              {{ t("未提供") }}
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
                  :aria-label="`${accountExpiryLabel(account, now)}；${t('到期于 {date}', { date: account.expires_on })}；${t('修改购买日期')}`"
                >
                  <n-tag
                    :type="accountExpiryTagType(account, now)"
                    size="small"
                    :bordered="false"
                  >
                    {{ accountExpiryLabel(account, now) }}
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
          </div>
        </div>
      </div>
    </template>

    <template #header-extra>
      <div class="account-actions" :class="{ 'account-actions--calibratable': usageRefreshAvailable && manualUsageCalibration }">
        <div v-if="accountIsReady(account)" class="account-action account-action--enabled">
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

        <div v-if="usageRefreshAvailable && accountIsReady(account)" class="account-action account-action--secondary">
          <n-tooltip trigger="hover">
            <template #trigger>
              <n-button
                circle
                quaternary
                size="small"
                :aria-label="t('刷新额度')"
                :loading="usageRefreshLoading"
                :disabled="usageLoading || !!usageLoadError"
                @click="emit('refresh-usage')"
              >
                <template #icon><n-icon :component="ReloadOutlined" /></template>
              </n-button>
            </template>
            {{ t("刷新额度") }}
          </n-tooltip>
        </div>

        <div
          v-if="manualUsageCalibration && accountIsReady(account) && edits"
          class="account-action account-action--secondary account-action--calibration"
        >
          <n-popover
            trigger="click"
            placement="bottom-end"
            :show-arrow="false"
            :width="320"
            style="max-width: calc(100vw - 64px)"
            @update:show="(show: boolean) => show && emit('usage-editor-open')"
          >
          <template #trigger>
            <n-tooltip trigger="hover">
              <template #trigger>
                <n-button
                  circle
                  quaternary
                  size="small"
                  :aria-label="t('校准用量')"
                  :disabled="!usageEditorAvailable"
                >
                  <template #icon><n-icon :component="EditOutlined" /></template>
                </n-button>
              </template>
              {{ t("校准用量") }}
            </n-tooltip>
          </template>

            <AccountUsageEditor
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

        <div v-if="!isCpa" class="account-action account-action--test">
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

        <div v-if="menuOptions.length > 0" class="account-action account-action--menu">
          <n-dropdown
            :options="menuOptions"
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
      </div>
    </template>

    <div v-if="!accountIsReady(account)" class="managed-pending">
      <div>
        <strong>{{ managedStepLabel(account.setup_step) }}</strong>
        <p>{{ t("注册进度已保存。继续后仍会使用该账号自己的浏览器 Profile。") }}</p>
      </div>
      <n-button type="primary" secondary @click="emit('open-wizard')">
        {{ t("继续注册") }}
      </n-button>
    </div>
    <div v-else-if="isDraft" class="provider-unconfigured" role="status">
      <p>{{ draftDescription }}</p>
    </div>
    <div v-else-if="isOllamaCloud && ollamaNeedsBilling" class="provider-unconfigured" role="status">
      <p>{{ t("请配置 Ollama 计费档位以显示本月额度") }}</p>
    </div>
    <div v-else-if="usageDisplayAvailable" class="official-plan-usage">
      <div v-if="usageLoadError" class="usage-load-error" role="alert">
        <span>{{ t("用量加载失败") }}</span>
        <n-button
          text
          size="tiny"
          type="primary"
          :loading="usageLoading"
          @click="emit('reload-usage')"
        >
          {{ t("重试") }}
        </n-button>
      </div>
      <ProviderQuotaSummary v-else :usage="providerUsage" :now="now" />
    </div>
    <div v-else-if="isCustom" class="custom-endpoint">
      <div class="custom-endpoint__meta">
        <span v-if="account.custom_config?.endpoint_url" class="custom-endpoint__url">
          {{ account.custom_config.endpoint_url }}
        </span>
        <span class="custom-endpoint__models">
          {{ t("{count} 个模型", { count: account.model_capabilities.length }) }}
        </span>
      </div>
    </div>
  </n-card>
</template>

<script setup lang="ts">
import { computed, ref, watch } from "vue";
import {
  NButton,
  NCard,
  NDatePicker,
  NDropdown,
  NIcon,
  NPopover,
  NSwitch,
  NTag,
  NTooltip,
} from "naive-ui";
import {
  ApiOutlined,
  EditOutlined,
  HolderOutlined,
  MoreOutlined,
  ReloadOutlined,
} from "@vicons/antd";
import type { Account, UsageWindow } from "../api/dashboard";
import type { Identity } from "../api/identities.ts";
import type {
  ProviderCatalogEntry,
  ProviderUsageResponse,
} from "../api/providers.ts";
import { isCooling, isUsageLimitReached } from "../domain/accounts-usage.ts";
import type { UsageKey } from "../domain/accounts-usage.ts";
import {
  accountExpiryLabel,
  accountExpiryTagType,
  accountIsReady,
  accountRoutingDraftDescription,
  cooldownDetails,
  managedStepLabel,
} from "../domain/account-display.ts";
import {
  accountCredentialCountLabel,
  accountExpiryDisplay,
  accountShowsDeclaredRelation,
  inferenceLastError,
  presentedAccountStatusLabel,
  presentedAccountStatusTagType,
  selectedBindingDisabled,
  selectedModelRestrictionLabel,
  selectedQuotaShareLabel,
} from "../domain/account-identity.ts";
import type { AccountMenuOption } from "../domain/account-display.ts";
import {
  isCpaIntegrationAccount,
  isOllamaCloudAccount,
  isZenFreeAccount,
} from "../domain/account-providers.ts";
import { isCustomApiAccount } from "../domain/custom-account.ts";
import { localDateString } from "../domain/account-lifecycle.ts";
import { findPlanDefinition, planLabel } from "../domain/plans.ts";
import type { AccountUsageEdits, UsageLimitView } from "../domain/useAccountUsage.ts";
import { t } from "../i18n/index.ts";
import AccountUsageEditor from "./AccountUsageEditor.vue";
import ProviderQuotaSummary from "./ProviderQuotaSummary.vue";

const props = defineProps<{
  account: Account;
  identity?: Identity | null;
  catalog: readonly ProviderCatalogEntry[] | null;
  usage: UsageWindow;
  providerUsage: ProviderUsageResponse | null;
  limits: UsageLimitView[];
  edits: AccountUsageEdits | undefined;
  now: number;
  orderHandleDisabled: boolean;
  dragging: boolean;
  usageLoading: boolean;
  usageLoadError: string | null;
  usageRefreshLoading: boolean;
  purchaseDateSaving: boolean;
  quotaLimitsFailed: boolean;
  menuOptions: AccountMenuOption[];
  accountNames?: Readonly<Record<string, string>>;
}>();

const emit = defineEmits<{
  "order-keydown": [event: KeyboardEvent];
  "order-drag-start": [event: PointerEvent];
  toggle: [];
  "test-connection": [];
  "refresh-usage": [];
  "update-purchase-date": [date: string];
  "reload-usage": [];
  "open-wizard": [];
  "menu-select": [key: string | number];
  "usage-editor-open": [];
  "usage-update-draft": [key: UsageKey, value: number | null];
  "usage-update-resets-first": [key: UsageKey, value: number | null];
  "usage-update-resets-second": [key: UsageKey, value: number | null];
  "usage-save": [key: UsageKey];
}>();

const isZen = computed(() => isZenFreeAccount(props.account));
const isCpa = computed(() => isCpaIntegrationAccount(props.account));
const isCustom = computed(() => isCustomApiAccount(props.account));
const isOllamaCloud = computed(() => isOllamaCloudAccount(props.account));
const ollamaNeedsBilling = computed(() => !props.account.ollama_billing_tier);
const overlayIdentity = computed(() => props.identity ?? null);
const showsDeclaredRelation = computed(() => accountShowsDeclaredRelation(overlayIdentity.value));
const credentialCountLabel = computed(() => accountCredentialCountLabel(overlayIdentity.value));
const statusLabel = computed(() => (
  presentedAccountStatusLabel(props.account, overlayIdentity.value, props.now)
));
const statusTagType = computed(() => (
  presentedAccountStatusTagType(props.account, overlayIdentity.value, props.now)
));
const statusTooltip = computed(() => {
  if (props.account.auth_error) return props.account.auth_error;
  const overlayError = inferenceLastError(overlayIdentity.value, props.account.id);
  if (overlayError) return overlayError;
  if (isCooling(props.account, props.now)) return cooldownDetails(props.account, props.now, props.limits);
  return "";
});
const bindingDisabled = computed(() => (
  selectedBindingDisabled(overlayIdentity.value, props.account.id)
));
const modelRestrictionLabel = computed(() => (
  selectedModelRestrictionLabel(overlayIdentity.value, props.account.id)
));
const quotaShareLabel = computed(() => (
  selectedQuotaShareLabel(
    overlayIdentity.value,
    props.account.id,
    (id) => props.accountNames?.[id] ?? null,
  )
));
const expiryDisplay = computed(() => (
  accountExpiryDisplay(props.account, overlayIdentity.value, props.catalog)
));
// Purchase/expiry UI is only for built-in billed families: Custom API, Zen
// Free, and user-defined (dynamic) Providers model no billing cadence, so
// their (possibly synthetic or blanked) dates stay hidden unless V3 already
// stored a real purchase date on a built-in card.
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
const plan = computed(() => findPlanDefinition(props.account.provider_id, props.catalog));
// Manual calibration display is catalog-driven: no hardcoded per-plan meters.
const manualUsageCalibration = computed(() => (
  plan.value?.manual_usage_calibration ?? false
));
const usageRefreshAvailable = computed(() => plan.value?.usage_availability === "available");
const usageDisplayAvailable = computed(() => (
  usageRefreshAvailable.value || manualUsageCalibration.value
));
const toggleBlockedReason = computed(() => {
  if (!props.account.plan_routable) return t("该方案暂不可路由");
  return "";
});
const isDraft = computed(() => (
  accountIsReady(props.account)
  && !props.account.plan_routable
));

const draftDescription = computed(() => {
  const key = accountRoutingDraftDescription(props.account);
  return key ? t(key) : "";
});

const usageEditorAvailable = computed(() => {
  if (props.usageLoading || props.usageLoadError) return false;
  return props.limits.some(({ key }) => !isUsageLimitReached(props.account, key, props.now));
});

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
.account-card {
  border-radius: 14px;
  box-shadow: var(--ocg-shadow-sm);
  transition: border-color 0.16s ease, box-shadow 0.16s ease, opacity 0.16s ease;
}

.account-card--cooling {
  border-color: color-mix(in srgb, var(--ocg-error) 45%, transparent);
}

.account-card--pending,
.account-card--draft {
  border-color: color-mix(in srgb, var(--ocg-primary) 32%, var(--ocg-divider));
}

.account-card--dragging {
  border-color: var(--ocg-primary);
  box-shadow: 0 10px 28px color-mix(in srgb, var(--ocg-primary) 18%, transparent);
  opacity: 0.72;
}

.provider-unconfigured {
  color: var(--ocg-warning);
  font-size: var(--ocg-font-sm);
}

.provider-unconfigured > p {
  margin: 0;
}

.manual-usage-block {
  display: grid;
  gap: 8px;
  margin-top: 10px;
}

.official-plan-usage {
  margin-top: 10px;
}

.account-actions {
  display: grid;
  grid-template-columns: repeat(4, 40px);
  align-items: center;
  justify-content: end;
  column-gap: 8px;
}

.account-action {
  display: flex;
  align-items: center;
  justify-content: center;
  min-width: 0;
}

.account-action--enabled {
  grid-column: 1;
}

.account-action--secondary {
  grid-column: 2;
}

.account-action--test {
  grid-column: 3;
}

.account-action--menu {
  grid-column: 4;
}

.custom-endpoint {
  display: grid;
  justify-items: start;
  gap: 8px;
}

.custom-endpoint__meta {
  display: flex;
  flex-wrap: wrap;
  align-items: center;
  gap: 4px 12px;
  min-width: 0;
}

.custom-endpoint__url {
  overflow-wrap: anywhere;
  color: var(--ocg-ink);
  font-size: var(--ocg-font-sm);
}

.custom-endpoint__models {
  color: var(--ocg-muted);
  font-size: var(--ocg-font-sm);
}

.account-title {
  display: flex;
  align-items: center;
  gap: 8px;
  min-width: 0;
  width: 100%;
}

.account-order-handle {
  flex: 0 0 auto;
  cursor: grab;
  touch-action: none;
  user-select: none;
}

.account-order-handle--dragging {
  cursor: grabbing;
}

.account-heading {
  display: flex;
  align-items: center;
  flex: 1 1 auto;
  min-width: 0;
}

.account-name-row {
  display: flex;
  flex-wrap: wrap;
  align-items: center;
  gap: 4px 6px;
  min-width: 0;
}

.account-name {
  overflow: hidden;
  color: var(--ocg-ink);
  font-size: var(--ocg-font-md);
  font-weight: 600;
  text-overflow: ellipsis;
  white-space: nowrap;
}

.account-name-row :deep(.n-tag) {
  flex: 0 0 auto;
}

.account-expiry-trigger {
  min-width: 0;
}

.account-expiry-trigger :deep(.n-button__content) {
  min-width: 0;
}

.purchase-date-popover {
  display: grid;
  gap: 10px;
  width: min(280px, calc(100vw - 64px));
}

.purchase-date-popover__actions {
  display: flex;
  justify-content: flex-end;
  gap: 8px;
}

.managed-pending {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 16px;
  padding: 10px 2px 2px;
}

.managed-pending strong {
  color: var(--ocg-ink);
  font-size: var(--ocg-font-md);
}

.managed-pending p {
  margin: 4px 0 0;
  color: var(--ocg-muted);
  font-size: var(--ocg-font-sm);
}

.usage-sync-meta {
  margin: 8px 0 0;
  color: var(--ocg-text-3);
  font-size: var(--ocg-font-size-12);
  line-height: 1.4;
}

.usage-load-error {
  display: flex;
  align-items: center;
  justify-content: center;
  gap: 8px;
  min-height: 42px;
  color: var(--ocg-error);
  font-size: var(--ocg-font-sm);
}

@media (max-width: 900px) {
  .account-card :deep(.n-card-header) {
    align-items: flex-start;
  }

  .account-card :deep(.n-card-header__extra) {
    margin-left: 8px;
  }
}

@media (max-width: 640px) {
  .managed-pending {
    align-items: stretch;
    flex-direction: column;
  }

  .account-card :deep(.n-card-header) {
    flex-wrap: wrap;
    gap: 8px;
  }

  .account-card :deep(.n-card-header__main),
  .account-card :deep(.n-card-header__extra) {
    width: 100%;
  }

  .account-card :deep(.n-card-header__extra) {
    display: flex;
    justify-content: flex-end;
    margin-left: 0;
  }
}
.account-actions--calibratable {
  grid-template-columns: repeat(5, 40px);
}
.account-actions--calibratable .account-action--calibration { grid-column: 3; }
.account-actions--calibratable .account-action--test { grid-column: 4; }
.account-actions--calibratable .account-action--menu { grid-column: 5; }
</style>
