<template>
  <section class="credit-meter" :aria-label="t('点数')">
    <template v-if="meter">
      <div class="credit-meter__head">
        <p class="credit-meter__source">
          <a
            v-if="meter.configuration.sourceUrl"
            :href="meter.configuration.sourceUrl"
            target="_blank"
            rel="noopener noreferrer"
          >{{ t("官方来源") }}</a>
        </p>
        <div class="credit-meter__actions">
          <n-tooltip trigger="hover">
            <template #trigger>
              <n-button
                circle
                quaternary
                size="small"
                :aria-label="t('充值')"
                :disabled="mutating"
                @click="openTopup"
              >
                <template #icon><n-icon :component="PlusOutlined" /></template>
              </n-button>
            </template>
            {{ t("充值") }}
          </n-tooltip>
          <n-tooltip trigger="hover">
            <template #trigger>
              <n-button
                circle
                quaternary
                size="small"
                :aria-label="t('费率')"
                :disabled="mutating"
                @click="openSettings"
              >
                <template #icon><n-icon :component="SettingOutlined" /></template>
              </n-button>
            </template>
            {{ t("费率") }}
          </n-tooltip>
          <n-tooltip trigger="hover">
            <template #trigger>
              <n-button
                circle
                quaternary
                size="small"
                :aria-label="t('刷新')"
                :loading="loading"
                :disabled="mutating"
                @click="reload"
              >
                <template #icon><n-icon :component="ReloadOutlined" /></template>
              </n-button>
            </template>
            {{ t("刷新") }}
          </n-tooltip>
        </div>
      </div>
      <ApiPriceMeter :cells="cells" :caption="caption" />
      <ul v-if="expiredBuckets.length > 0" class="credit-meter__expired">
        <li v-for="row in expiredBuckets" :key="row.id">
          {{ row.label }} · {{ formatScaledCredits(row.remaining, locale, unitFactor) }} {{ unitLabel }}
          <time v-if="row.expiresAt">{{ formatExpiry(row.expiresAt) }}</time>
        </li>
      </ul>
    </template>

  </section>

  <FormSurface
    :show="topupOpen"
    :title="t('充值')"
    modal-style="width: 440px; max-width: calc(100vw - 32px)"
    :close-on-esc="!mutating"
    @update:show="setTopup"
  >
    <n-form label-placement="top" @submit.prevent="submitTopup">
      <n-space v-if="status.presets.length > 0" class="credit-meter__quick">
        <n-button
          v-for="amount in TOPUP_QUICK_AMOUNTS"
          :key="amount"
          size="tiny"
          secondary
          @click="fillTopup(amount)"
        >
          {{ formatScaledCredits(amount, locale, unitFactor) }} {{ unitLabel }}
        </n-button>
      </n-space>
      <n-form-item :label="t('充值')" required>
        <div class="credit-meter__amount">
          <n-input-number
            v-model:value="topupAmount"
            :show-button="false"
            :min="0"
            :precision="4"
            :disabled="mutating"
            :aria-label="t('充值')"
          />
          <span v-if="unitLabel" class="mono">{{ unitLabel }}</span>
        </div>
      </n-form-item>
      <n-form-item :label="t('重置日期')">
        <input
          v-model="topupExpiry"
          type="datetime-local"
          class="credit-meter__datetime mono"
          :disabled="mutating"
          :aria-label="t('重置日期')"
        >
      </n-form-item>
      <n-checkbox v-model:checked="topupThirtyDays" :disabled="mutating">
        {{ t("30 天") }}
      </n-checkbox>
      <p v-if="draftIssue" class="credit-meter__hint" role="alert">{{ t(draftIssue) }}</p>
    </n-form>
    <template #footer>
      <n-space justify="end">
        <n-button :disabled="mutating" @click="setTopup(false)">{{ t("取消") }}</n-button>
        <n-button type="primary" :loading="mutating" :disabled="!topupAmount" @click="submitTopup">
          {{ t("保存") }}
        </n-button>
      </n-space>
    </template>
  </FormSurface>

  <FormSurface
    :show="settingsOpen"
    :title="t('费率')"
    modal-style="width: 520px; max-width: calc(100vw - 32px)"
    :close-on-esc="!mutating"
    @update:show="setSettings"
  >
    <n-form label-placement="top" @submit.prevent="submitSettings">
      <p v-if="draftIssue" class="credit-meter__hint" role="alert">{{ t(draftIssue) }}</p>
      <CreditRateFields :model-value="rateDrafts" :disabled="mutating" @update:model-value="rates => rateDrafts.splice(0, rateDrafts.length, ...rates)" />
      <n-checkbox v-model:checked="monthlyEnabled" :disabled="mutating">
        {{ t("月度额度") }}
      </n-checkbox>
      <n-form-item v-if="monthlyEnabled" :label="t('月度额度')">
        <div class="credit-meter__amount">
          <n-input-number
            v-model:value="monthlyAmountDraft"
            :show-button="false"
            :min="0"
            :precision="4"
            :disabled="mutating"
            :aria-label="t('月度额度')"
          />
          <span v-if="unitLabel" class="mono">{{ unitLabel }}</span>
        </div>
      </n-form-item>
      <n-form-item v-if="monthlyEnabled" :label="t('重置日期')" :required="monthlyEnabled">
        <input
          v-model="resetDraft"
          type="datetime-local"
          class="credit-meter__datetime mono"
          :disabled="mutating"
          :aria-label="t('重置日期')"
        >
      </n-form-item>
    </n-form>
    <template #footer>
      <n-space justify="space-between">
        <n-button v-if="meter" quaternary type="error" :disabled="mutating" @click="submitDisable">
          {{ t("停用额度") }}
        </n-button>
        <span v-else />
        <n-space>
          <n-button :disabled="mutating" @click="setSettings(false)">{{ t("取消") }}</n-button>
          <n-button type="primary" :loading="mutating" @click="submitSettings">{{ t("保存") }}</n-button>
        </n-space>
      </n-space>
    </template>
  </FormSurface>
</template>

<script setup lang="ts">
import { computed, onBeforeUnmount, reactive, ref, watch } from "vue";
import {
  NButton,
  NCheckbox,
  NForm,
  NFormItem,
  NIcon,
  NInputNumber,
  NSpace,
  NTooltip,
} from "naive-ui";
import { PlusOutlined, ReloadOutlined, SettingOutlined } from "@vicons/antd";
import type { BillingStatus, CreditRate } from "../api/billing.ts";
import {
  BILLING_SOURCE_KEYS,
  CHINA_OFFSET_MINUTES,
  TOPUP_QUICK_AMOUNTS,
  buildCreditSettingsConfiguration,
  creditDisplayFactor,
  creditsToScaled,
  formatOffsetDateTime,
  formatScaledCredits,
  CREDIT_AMOUNT_ISSUE_KEYS,
  CREDIT_DATE_ISSUE_KEY,
  CREDIT_SETUP_ISSUE_KEYS,
  fromDatetimeLocalValue,
  meterNextResetAt,
  meterOffsetMinutes,
  nextCalendarMonthStart,
  offsetMinutesOrDefault,
  parseCreditAmount,
  partitionCreditBuckets,
  toDatetimeLocalValue,
  topupExpiryIso,
} from "../domain/billing.ts";
import { formatPayGoObservedAt } from "../domain/pay-go-meter.ts";
import { locale, t, type MessageKey } from "../i18n/index.ts";
import { useBillingStore } from "../stores/billing.ts";
import type { ApiPriceMeterCell } from "./ApiPriceMeter.vue";
import ApiPriceMeter from "./ApiPriceMeter.vue";
import FormSurface from "./FormSurface.vue";
import CreditRateFields from "./CreditRateFields.vue";

const props = defineProps<{
  accountId: string;
  binding: string;
  status: BillingStatus;
  now: number;
}>();

const store = useBillingStore();
const unitFactor = computed(() => creditDisplayFactor(props.status.presets.length));
const unitLabel = computed(() => props.status.presets.length > 0 ? "M" : "");
const monthlyEnabled = ref(false);
const monthlyAmountDraft = ref<number | null>(null);
const resetDraft = ref("");
const topupOpen = ref(false);
const settingsOpen = ref(false);
const topupAmount = ref<number | null>(null);
const topupExpiry = ref("");
const topupThirtyDays = ref(false);
const genericName = ref("");
const genericCurrency = ref("CNY");
const genericFactor = ref<number | null>(1);
const rateDrafts = reactive<CreditRate[]>([]);
const draftIssue = ref<MessageKey | null>(null);

const slot = computed(() => store.byId[props.accountId]);
const mutating = computed(() => slot.value?.mutating ?? false);
const loading = computed(() => slot.value?.loading ?? false);
const meter = computed(() => props.status.credits);
const offsetMinutes = computed(() => meterOffsetMinutes(meter.value));
const partitioned = computed(() => partitionCreditBuckets(meter.value?.buckets ?? [], props.now));
const expiredBuckets = computed(() => partitioned.value.expired);
const cells = computed<ApiPriceMeterCell[]>(() => {
  const view = meter.value;
  if (!view) return [];
  const rows: ApiPriceMeterCell[] = [{
    key: "remaining",
    label: t("剩余"),
    value: `${formatScaledCredits(view.remaining, locale.value, unitFactor.value)} / ${formatScaledCredits(view.activeGranted, locale.value, unitFactor.value)} ${unitLabel.value}`,
  }];
  if (view.lastCalibrationAt) {
    rows.push({
      key: "spent",
      label: t("校准后已消耗"),
      value: `${formatScaledCredits(view.spentSinceCalibration, locale.value, unitFactor.value)} ${unitLabel.value}`,
    });
  }
  if (view.overdrawn > 0) {
    rows.push({
      key: "overdrawn",
      label: t("透支"),
      value: `${formatScaledCredits(view.overdrawn, locale.value, unitFactor.value)} ${unitLabel.value}`,
    });
  }
  return rows;
});

const caption = computed(() => {
  const view = meter.value;
  if (!view) return "";
  const parts = [t(BILLING_SOURCE_KEYS[props.status.source])];
  const observed = formatPayGoObservedAt(view.estimatedAt, locale.value);
  if (observed) parts.push(observed);
  const reset = meterNextResetAt(view);
  const resetLabel = reset
    ? formatOffsetDateTime(reset, offsetMinutesOrDefault(view.configuration.monthly?.timezoneOffsetMinutes))
    : null;
  if (resetLabel) parts.push(resetLabel);
  if (view.unpricedRequests > 0) {
    parts.push(t("另有 {count} 条未知", { count: view.unpricedRequests }));
  }
  if (view.pendingRequests > 0) {
    parts.push(t("{count} 条待结算", { count: view.pendingRequests }));
  }
  return parts.join(" · ");
});

function formatExpiry(iso: string): string {
  return formatOffsetDateTime(iso, offsetMinutes.value) ?? iso;
}

function defaultResetLocal(): string {
  return toDatetimeLocalValue(
    nextCalendarMonthStart(props.now, CHINA_OFFSET_MINUTES).toISOString(),
    CHINA_OFFSET_MINUTES,
  );
}

function clearDrafts(): void {
  monthlyEnabled.value = props.status.presets.length > 0;
  monthlyAmountDraft.value = props.status.presets[0]
    ? creditsToScaled(props.status.presets[0].configuration.monthly?.amount ?? props.status.presets[0].initialGrant, unitFactor.value)
    : null;
  resetDraft.value = defaultResetLocal();
  draftIssue.value = null;
  topupAmount.value = null;
  topupExpiry.value = "";
  topupThirtyDays.value = false;
  genericName.value = "";
  genericCurrency.value = "CNY";
  genericFactor.value = 1;
  rateDrafts.splice(0, rateDrafts.length);
  topupOpen.value = false;
  settingsOpen.value = false;
}

function fillTopup(amount: number): void {
  topupAmount.value = creditsToScaled(amount, unitFactor.value);
  if (topupThirtyDays.value || !topupExpiry.value) {
    topupThirtyDays.value = true;
    topupExpiry.value = toDatetimeLocalValue(topupExpiryIso(props.now), offsetMinutes.value);
  }
}

function openTopup(): void {
  topupAmount.value = null;
  topupThirtyDays.value = false;
  topupExpiry.value = "";
  topupOpen.value = true;
}

function setTopup(open: boolean): void {
  topupOpen.value = open;
}

watch(topupThirtyDays, (checked) => {
  if (checked) {
    topupExpiry.value = toDatetimeLocalValue(topupExpiryIso(props.now), offsetMinutes.value);
  }
});

async function submitTopup(): Promise<void> {
  if (mutating.value) return;
  const parsed = parseCreditAmount(topupAmount.value, unitFactor.value);
  if ("issue" in parsed) {
    draftIssue.value = CREDIT_AMOUNT_ISSUE_KEYS[parsed.issue];
    return;
  }
  let expiresAt: string | null = null;
  if (topupExpiry.value) {
    expiresAt = fromDatetimeLocalValue(topupExpiry.value, offsetMinutes.value);
    if (!expiresAt) {
      draftIssue.value = CREDIT_DATE_ISSUE_KEY;
      return;
    }
  }
  draftIssue.value = null;
  try {
    await store.grantCredits(props.accountId, props.binding, {
      label: t("充值"),
      amount: parsed.amount,
      expiresAt,
    });
    setTopup(false);
  } catch {
    // Keep the dialog.
  }
}

function reload(): void {
  void store.load(props.accountId, props.binding);
}

function openSettings(): void {
  const view = meter.value;
  if (view) {
    genericName.value = view.configuration.name;
    genericCurrency.value = view.configuration.currency;
    genericFactor.value = view.configuration.creditsPerCurrency;
    rateDrafts.splice(0, rateDrafts.length, ...view.configuration.rates.map((rate) => ({ ...rate })));
    monthlyEnabled.value = Boolean(view.configuration.monthly);
    monthlyAmountDraft.value = view.configuration.monthly
      ? creditsToScaled(view.configuration.monthly.amount, unitFactor.value)
      : null;
    resetDraft.value = view.configuration.monthly
      ? toDatetimeLocalValue(
        view.configuration.monthly.nextResetAt,
        offsetMinutesOrDefault(view.configuration.monthly.timezoneOffsetMinutes),
      )
      : defaultResetLocal();
  }
  settingsOpen.value = true;
}

function setSettings(open: boolean): void {
  settingsOpen.value = open;
}

function parsedMonthlyGrant(): { amount: number | null } | { issue: "missing" | "invalid" } {
  if (!monthlyEnabled.value) return { amount: null };
  return parseCreditAmount(monthlyAmountDraft.value, unitFactor.value);
}

function parsedMonthlyReset(): string | null {
  if (!monthlyEnabled.value) return null;
  if (!resetDraft.value) return null;
  return fromDatetimeLocalValue(
    resetDraft.value,
    offsetMinutesOrDefault(meter.value?.configuration.monthly?.timezoneOffsetMinutes),
  );
}

async function submitSettings(): Promise<void> {
  if (mutating.value) return;
  const view = meter.value;
  const rates = rateDrafts.filter((rate) => rate.model.trim()).map((rate) => ({
    model: rate.model,
    inputPerMillion: rate.inputPerMillion,
    outputPerMillion: rate.outputPerMillion,
    cacheReadPerMillion: rate.cacheReadPerMillion,
    cacheWritePerMillion: rate.cacheWritePerMillion,
  }));
  const grant = parsedMonthlyGrant();
  if ("issue" in grant) {
    draftIssue.value = CREDIT_AMOUNT_ISSUE_KEYS[grant.issue];
    return;
  }
  const resetAt = parsedMonthlyReset();
  if (monthlyEnabled.value && (resetDraft.value === "" || !resetAt)) {
    draftIssue.value = CREDIT_DATE_ISSUE_KEY;
    return;
  }
  try {
    if (view) {
      const configuration = buildCreditSettingsConfiguration(view.configuration, {
        name: genericName.value.trim() || view.configuration.name,
        currency: genericCurrency.value.trim() || view.configuration.currency,
        creditsPerCurrency: genericFactor.value && genericFactor.value > 0
          ? genericFactor.value
          : view.configuration.creditsPerCurrency,
        rates: rates.length > 0 ? rates : view.configuration.rates,
        monthlyEnabled: monthlyEnabled.value,
        monthlyAmount: grant.amount,
        nextResetAt: resetAt,
        timezoneOffsetMinutes: offsetMinutesOrDefault(view.configuration.monthly?.timezoneOffsetMinutes),
      });
      if ("issue" in configuration) {
        draftIssue.value = CREDIT_SETUP_ISSUE_KEYS[configuration.issue];
        return;
      }
      draftIssue.value = null;
      await store.configureCredits(props.accountId, props.binding, { configuration });
    }
    setSettings(false);
  } catch {
    // Keep the dialog.
  }
}

async function submitDisable(): Promise<void> {
  if (mutating.value) return;
  try {
    await store.disableCredits(props.accountId, props.binding);
    setSettings(false);
  } catch {
    // Keep the dialog.
  }
}

watch(
  () => [props.accountId, props.binding, store.sessionEpoch] as const,
  () => { clearDrafts(); },
  { immediate: true },
);

watch(monthlyEnabled, (enabled) => {
  if (enabled && !resetDraft.value) resetDraft.value = defaultResetLocal();
});

onBeforeUnmount(() => { clearDrafts(); });
</script>

<style scoped>
.credit-meter { display: grid; gap: var(--ocg-space-sm); min-width: 0; }
.credit-meter__head, .credit-meter__actions, .credit-meter__amount { display: flex; align-items: center; gap: var(--ocg-space-sm); }
.credit-meter__head { justify-content: space-between; }
.credit-meter__source, .credit-meter__hint, .credit-meter__expired { margin: 0; color: var(--ocg-muted); font-size: var(--ocg-font-xs); }
.credit-meter__datetime { width: 100%; padding: var(--ocg-space-sm); color: var(--ocg-ink); background: var(--ocg-surface); border: 1px solid var(--ocg-border); border-radius: var(--ocg-radius-sm); }
.credit-meter__quick { margin-bottom: var(--ocg-space-md); }
</style>
