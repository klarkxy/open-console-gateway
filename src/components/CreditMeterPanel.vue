<template>
  <section class="credit-meter" :aria-label="t('点数')">
    <div v-if="setupOpen" class="credit-meter__setup">
      <n-select
        v-if="status.presets.length > 0"
        :value="presetId"
        :options="presetOptions"
        :placeholder="t('档位')"
        :aria-label="t('档位')"
        :disabled="mutating"
        @update:value="onPreset"
      />
      <p v-if="draftIssue" class="credit-meter__hint" role="alert">{{ t(draftIssue) }}</p>
      <n-form label-placement="top" @submit.prevent="submitSetup">
        <n-form-item :label="t('当前剩余')" required>
          <div class="credit-meter__amount">
            <n-input-number
              v-model:value="remainingDraft"
              :show-button="false"
              :min="0"
              :precision="4"
              :disabled="mutating"
              :aria-label="t('当前剩余')"
            />
            <span v-if="unitLabel" class="mono">{{ unitLabel }}</span>
          </div>
        </n-form-item>
        <n-form-item :label="t('重置日期')" required>
          <input
            v-model="resetDraft"
            type="datetime-local"
            class="credit-meter__datetime mono"
            :disabled="mutating"
            :aria-label="t('重置日期')"
          >
        </n-form-item>
      </n-form>
      <n-button
        type="primary"
        size="small"
        :loading="mutating"
        :disabled="!canSubmitSetup"
        @click="submitSetup"
      >
        {{ t("保存") }}
      </n-button>
    </div>

    <template v-else-if="meter">
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
                :aria-label="calibrationReason"
                :disabled="mutating || activeBuckets.length === 0 || Boolean(calibrationBlock)"
                @click="openCalibrate"
              >
                <template #icon><n-icon :component="EditOutlined" /></template>
              </n-button>
            </template>
            {{ calibrationReason }}
          </n-tooltip>
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

    <div v-else class="credit-meter__setup">
      <n-button size="small" secondary :disabled="mutating" @click="openGenericSetup">
        {{ t("配置额度") }}
      </n-button>
    </div>
  </section>

  <FormSurface
    :show="calibrateOpen"
    :title="t('校准用量')"
    modal-style="width: 440px; max-width: calc(100vw - 32px)"
    :close-on-esc="!mutating"
    @update:show="setCalibrate"
  >
    <n-form label-placement="top" @submit.prevent="submitCalibrate">
      <n-form-item
        v-for="row in calibDrafts"
        :key="row.bucketId"
        :label="row.label"
      >
        <div class="credit-meter__amount">
          <n-input-number
            :value="row.remainingScaled"
            :show-button="false"
            :min="0"
            :precision="4"
            :disabled="mutating"
            :aria-label="row.label"
            @update:value="(value) => row.remainingScaled = value"
          />
          <span v-if="unitLabel" class="mono">{{ unitLabel }}</span>
        </div>
      </n-form-item>
      <p v-if="draftIssue" class="credit-meter__hint" role="alert">{{ t(draftIssue) }}</p>
    </n-form>
    <template #footer>
      <n-space justify="end">
        <n-button :disabled="mutating" @click="setCalibrate(false)">{{ t("取消") }}</n-button>
        <n-button type="primary" :loading="mutating" :disabled="Boolean(calibrationBlock)" @click="submitCalibrate">{{ t("保存") }}</n-button>
      </n-space>
    </template>
  </FormSurface>

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
      <n-form-item v-if="!meter" :label="t('名称')" required>
        <n-input v-model:value="genericName" :disabled="mutating" :aria-label="t('名称')" />
      </n-form-item>
      <n-form-item v-if="!meter" :label="t('币种')">
        <n-input v-model:value="genericCurrency" :disabled="mutating" :aria-label="t('币种')" />
      </n-form-item>
      <n-form-item v-if="!meter" :label="t('点数每货币')">
        <n-input-number
          v-model:value="genericFactor"
          :show-button="false"
          :min="0"
          :disabled="mutating"
          :aria-label="t('点数每货币')"
        />
      </n-form-item>
      <p v-if="draftIssue" class="credit-meter__hint" role="alert">{{ t(draftIssue) }}</p>
      <p class="credit-meter__hint">{{ t("每百万 Token，按原币估算；未知模型、复杂分档或额外收费请求保持未知。") }}</p>
      <n-form-item
        v-for="(rate, index) in rateDrafts"
        :key="index"
        :label="t('费率')"
      >
        <div class="credit-meter__rate">
          <label class="credit-meter__rate-field credit-meter__model">
          <span>{{ t("上游模型") }}</span>
          <n-input
            v-model:value="rate.model"
            :disabled="mutating"
            :placeholder="t('上游模型')"
            :aria-label="t('上游模型')"
            :input-props="{ 'aria-label': t('上游模型') }"
          />
          </label>
          <n-button class="credit-meter__remove" size="tiny" quaternary :disabled="mutating" :aria-label="t('删除')" @click="removeRate(index)">
            {{ t("删除") }}
          </n-button>
          <label class="credit-meter__rate-field">
          <span>{{ t("输入") }}</span>
          <n-input-number
            v-model:value="rate.inputPerMillion"
            :show-button="false"
            :min="0"
            :disabled="mutating"
            :aria-label="t('输入')"
            :input-props="{ 'aria-label': t('输入') }"
          />
          </label>
          <label class="credit-meter__rate-field">
          <span>{{ t("输出") }}</span>
          <n-input-number
            v-model:value="rate.outputPerMillion"
            :show-button="false"
            :min="0"
            :disabled="mutating"
            :aria-label="t('输出')"
            :input-props="{ 'aria-label': t('输出') }"
          />
          </label>
          <label class="credit-meter__rate-field">
          <span>{{ t("缓存读") }}</span>
          <n-input-number
            v-model:value="rate.cacheReadPerMillion"
            :show-button="false"
            :min="0"
            :disabled="mutating"
            :placeholder="t('未知')"
            :aria-label="t('缓存读')"
            :input-props="{ 'aria-label': t('缓存读') }"
          />
          </label>
          <label class="credit-meter__rate-field">
          <span>{{ t("缓存写") }}</span>
          <n-input-number
            v-model:value="rate.cacheWritePerMillion"
            :show-button="false"
            :min="0"
            :disabled="mutating"
            :placeholder="t('未知')"
            :aria-label="t('缓存写')"
            :input-props="{ 'aria-label': t('缓存写') }"
          />
          </label>
        </div>
      </n-form-item>
      <n-button size="tiny" quaternary :disabled="mutating" @click="addRate">
        {{ t("添加") }}
      </n-button>
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
      <n-form-item v-if="!meter" :label="t('当前剩余')" required>
        <div class="credit-meter__amount">
          <n-input-number
            v-model:value="remainingDraft"
            :show-button="false"
            :min="0"
            :precision="4"
            :disabled="mutating"
            :aria-label="t('当前剩余')"
          />
          <span v-if="unitLabel" class="mono">{{ unitLabel }}</span>
        </div>
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
  NInput,
  NInputNumber,
  NSelect,
  NSpace,
  NTooltip,
} from "naive-ui";
import { EditOutlined, PlusOutlined, ReloadOutlined, SettingOutlined } from "@vicons/antd";
import type { BillingStatus, CreditRate } from "../api/billing.ts";
import {
  BILLING_SOURCE_KEYS,
  CHINA_OFFSET_MINUTES,
  TOPUP_QUICK_AMOUNTS,
  buildCreditSettingsConfiguration,
  buildInitialCreditConfigure,
  calibrationBalances,
  configurationFromPreset,
  creditCalibrationBlock,
  creditDisplayFactor,
  creditsToScaled,
  formatOffsetDateTime,
  formatScaledCredits,
  CREDIT_AMOUNT_ISSUE_KEYS,
  CREDIT_DATE_ISSUE_KEY,
  CREDIT_SETUP_ISSUE_KEYS,
  fromDatetimeLocalValue,
  initialMonthlyBucket,
  meterNextResetAt,
  meterOffsetMinutes,
  nextCalendarMonthStart,
  offsetMinutesOrDefault,
  parseCreditAmount,
  partitionCreditBuckets,
  presetById,
  toDatetimeLocalValue,
  topupExpiryIso,
} from "../domain/billing.ts";
import { formatPayGoObservedAt } from "../domain/pay-go-meter.ts";
import { locale, t, type MessageKey } from "../i18n/index.ts";
import { useBillingStore } from "../stores/billing.ts";
import type { ApiPriceMeterCell } from "./ApiPriceMeter.vue";
import ApiPriceMeter from "./ApiPriceMeter.vue";
import FormSurface from "./FormSurface.vue";

const props = withDefaults(defineProps<{
  accountId: string;
  binding: string;
  status: BillingStatus;
  now: number;
  setupRequested?: boolean;
}>(), {
  setupRequested: false,
});
const emit = defineEmits<{ "setup-closed": [] }>();

const store = useBillingStore();
const unitFactor = computed(() => creditDisplayFactor(props.status.presets.length));
const unitLabel = computed(() => props.status.presets.length > 0 ? "M" : "");
const presetId = ref<string | null>(null);
const remainingDraft = ref<number | null>(null);
const monthlyEnabled = ref(false);
const monthlyAmountDraft = ref<number | null>(null);
const resetDraft = ref("");
const calibrateOpen = ref(false);
const topupOpen = ref(false);
const settingsOpen = ref(false);
const genericOpen = ref(false);
const topupAmount = ref<number | null>(null);
const topupExpiry = ref("");
const topupThirtyDays = ref(false);
const genericName = ref("");
const genericCurrency = ref("CNY");
const genericFactor = ref<number | null>(1);
const rateDrafts = reactive<CreditRate[]>([]);
const calibDrafts = reactive<Array<{ bucketId: string; label: string; remainingScaled: number | null }>>([]);
const draftIssue = ref<MessageKey | null>(null);

const slot = computed(() => store.byId[props.accountId]);
const mutating = computed(() => slot.value?.mutating ?? false);
const loading = computed(() => slot.value?.loading ?? false);
const meter = computed(() => props.status.credits);
const offsetMinutes = computed(() => meterOffsetMinutes(meter.value));
const setupOpen = computed(() => (
  !meter.value
  && props.status.presets.length > 0
  && (props.status.configurableCredits || props.setupRequested)
));
const partitioned = computed(() => partitionCreditBuckets(meter.value?.buckets ?? [], props.now));
const activeBuckets = computed(() => partitioned.value.active);
const expiredBuckets = computed(() => partitioned.value.expired);
const calibrationBlock = computed(() => creditCalibrationBlock(meter.value));
const calibrationReason = computed(() => {
  const view = meter.value;
  if (calibrationBlock.value === "pending" && view) {
    return t("{count} 条待结算", { count: view.pendingRequests });
  }
  return t("校准用量");
});
const presetOptions = computed(() => props.status.presets.map((preset) => ({
  label: preset.configuration.name,
  value: preset.id,
})));
const canSubmitSetup = computed(() => (
  !("issue" in parseCreditAmount(remainingDraft.value, unitFactor.value))
  && Boolean(fromDatetimeLocalValue(resetDraft.value, CHINA_OFFSET_MINUTES))
  && (props.status.presets.length === 0 || Boolean(presetId.value))
));

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
  presetId.value = props.status.presets[0]?.id ?? null;
  remainingDraft.value = props.status.presets[0]
    ? creditsToScaled(props.status.presets[0].initialGrant, unitFactor.value)
    : null;
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
  calibDrafts.splice(0, calibDrafts.length);
  calibrateOpen.value = false;
  topupOpen.value = false;
  settingsOpen.value = false;
  genericOpen.value = false;
}

function onPreset(id: string | number | null): void {
  if (id == null) return;
  const selected = String(id);
  presetId.value = selected;
  const preset = presetById(props.status.presets, selected);
  if (!preset) return;
  remainingDraft.value = creditsToScaled(preset.initialGrant, unitFactor.value);
  monthlyEnabled.value = true;
  monthlyAmountDraft.value = creditsToScaled(
    preset.configuration.monthly?.amount ?? preset.initialGrant,
    unitFactor.value,
  );
  const next = preset.configuration.monthly?.nextResetAt
    ?? nextCalendarMonthStart(props.now, CHINA_OFFSET_MINUTES).toISOString();
  resetDraft.value = toDatetimeLocalValue(
    next,
    offsetMinutesOrDefault(preset.configuration.monthly?.timezoneOffsetMinutes),
  );
}

async function submitSetup(): Promise<void> {
  if (mutating.value) return;
  const parsed = parseCreditAmount(remainingDraft.value, unitFactor.value);
  const resetAt = fromDatetimeLocalValue(resetDraft.value, CHINA_OFFSET_MINUTES);
  if ("issue" in parsed) {
    draftIssue.value = CREDIT_AMOUNT_ISSUE_KEYS[parsed.issue];
    return;
  }
  if (!resetAt) {
    draftIssue.value = CREDIT_DATE_ISSUE_KEY;
    return;
  }
  draftIssue.value = null;
  const remaining = parsed.amount;
  const preset = presetId.value ? presetById(props.status.presets, presetId.value) : undefined;
  const configuration = preset
    ? configurationFromPreset(preset, resetAt)
    : {
      name: genericName.value.trim() || "credits",
      currency: genericCurrency.value.trim() || "CNY",
      creditsPerCurrency: genericFactor.value && genericFactor.value > 0 ? genericFactor.value : 1,
      rates: rateDrafts.filter((rate) => rate.model.trim()),
      monthly: {
        amount: remaining,
        nextResetAt: resetAt,
        timezoneOffsetMinutes: CHINA_OFFSET_MINUTES,
        renewalEndsAt: null,
      },
      sourceUrl: null,
    };
  try {
    await store.configureCredits(props.accountId, props.binding, {
      configuration,
      initialBuckets: [initialMonthlyBucket(configuration, remaining, new Date(props.now).toISOString())],
    });
  } catch {
    // Store keeps the last snapshot.
  }
}

function openCalibrate(): void {
  if (creditCalibrationBlock(meter.value)) return;
  calibDrafts.splice(0, calibDrafts.length, ...activeBuckets.value.map((bucket) => ({
    bucketId: bucket.id,
    label: bucket.label,
    remainingScaled: creditsToScaled(bucket.remaining, unitFactor.value),
  })));
  calibrateOpen.value = true;
}

function setCalibrate(open: boolean): void {
  calibrateOpen.value = open;
  if (!open) calibDrafts.splice(0, calibDrafts.length);
}

async function submitCalibrate(): Promise<void> {
  if (mutating.value) return;
  if (creditCalibrationBlock(meter.value)) return;
  const balances = calibrationBalances(calibDrafts, unitFactor.value);
  if (!balances) {
    draftIssue.value = CREDIT_AMOUNT_ISSUE_KEYS.invalid;
    return;
  }
  draftIssue.value = null;
  try {
    await store.calibrateCredits(props.accountId, props.binding, balances);
    setCalibrate(false);
  } catch {
    // Keep the dialog for another attempt.
  }
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

function addRate(): void {
  rateDrafts.push({
    model: "",
    inputPerMillion: 0,
    outputPerMillion: 0,
    cacheReadPerMillion: null,
    cacheWritePerMillion: null,
  });
}

function removeRate(index: number): void {
  rateDrafts.splice(index, 1);
}

function reload(): void {
  void store.load(props.accountId, props.binding);
}

function openGenericSetup(): void {
  genericOpen.value = true;
  settingsOpen.value = true;
  genericName.value = "";
  genericCurrency.value = "CNY";
  genericFactor.value = 1;
  remainingDraft.value = null;
  monthlyEnabled.value = false;
  monthlyAmountDraft.value = null;
  resetDraft.value = defaultResetLocal();
  rateDrafts.splice(0, rateDrafts.length, {
    model: "",
    inputPerMillion: 0,
    outputPerMillion: 0,
    cacheReadPerMillion: null,
    cacheWritePerMillion: null,
  });
}

function openSettings(): void {
  genericOpen.value = false;
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
  if (!open) {
    genericOpen.value = false;
    if (props.setupRequested) emit("setup-closed");
  }
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
    if (!view) {
      const remaining = parseCreditAmount(remainingDraft.value, unitFactor.value);
      if ("issue" in remaining) {
        draftIssue.value = CREDIT_AMOUNT_ISSUE_KEYS[remaining.issue];
        return;
      }
      const built = buildInitialCreditConfigure({
        name: genericName.value.trim() || "credits",
        currency: genericCurrency.value.trim() || "CNY",
        creditsPerCurrency: genericFactor.value && genericFactor.value > 0 ? genericFactor.value : 1,
        rates,
        remaining: remaining.amount,
        monthlyEnabled: monthlyEnabled.value,
        monthlyAmount: grant.amount,
        nextResetAt: resetAt,
        timezoneOffsetMinutes: CHINA_OFFSET_MINUTES,
        sourceUrl: null,
        startsAt: new Date(props.now).toISOString(),
      });
      if ("issue" in built) {
        draftIssue.value = CREDIT_SETUP_ISSUE_KEYS[built.issue];
        return;
      }
      draftIssue.value = null;
      await store.configureCredits(props.accountId, props.binding, built);
    } else {
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

watch(() => props.setupRequested, (requested) => {
    if (!requested || meter.value) return;
    if (props.status.presets.length === 0) openGenericSetup();
}, { immediate: true });

watch(monthlyEnabled, (enabled) => {
  if (enabled && !resetDraft.value) resetDraft.value = defaultResetLocal();
});

onBeforeUnmount(() => { clearDrafts(); });
</script>

<style scoped>
.credit-meter {
  display: grid;
  gap: var(--ocg-space-sm);
  min-width: 0;
}

.credit-meter__head,
.credit-meter__actions,
.credit-meter__quick {
  display: flex;
  flex-wrap: wrap;
  align-items: center;
  gap: var(--ocg-space-xs);
}

.credit-meter__head {
  justify-content: space-between;
}

.credit-meter__source,
.credit-meter__hint,
.credit-meter p {
  margin: 0;
  color: var(--ocg-muted);
  font-size: var(--ocg-font-xs);
}

.credit-meter__source {
  display: flex;
  flex-wrap: wrap;
  gap: var(--ocg-space-sm);
}

.credit-meter__source a {
  color: inherit;
  text-decoration: underline;
}

.credit-meter__datetime {
  width: 100%;
  min-height: 34px;
  padding: 0 var(--ocg-space-sm);
  border: 1px solid var(--ocg-border);
  border-radius: var(--ocg-radius-sm);
  background: var(--ocg-surface);
  color: var(--ocg-ink);
}

.credit-meter__expired {
  margin: 0;
  padding-left: var(--ocg-space-lg);
  color: var(--ocg-muted);
  font-size: var(--ocg-font-xs);
}

.credit-meter__rate {
  display: grid;
  grid-template-columns: repeat(4, minmax(0, 1fr));
  gap: var(--ocg-space-xs);
  align-items: center;
}

.credit-meter__rate-field {
  display: grid;
  gap: var(--ocg-space-xs);
  min-width: 0;
  font-size: var(--ocg-font-xs);
}

.credit-meter__model { grid-column: 1 / 4; }
.credit-meter__remove { grid-column: 4; align-self: end; }

.credit-meter__amount {
  display: flex;
  align-items: center;
  gap: var(--ocg-space-sm);
  min-width: 0;
}

.credit-meter__amount :deep(.n-input-number) {
  flex: 1 1 auto;
  min-width: 0;
}

.credit-meter__setup {
  display: grid;
  gap: var(--ocg-space-sm);
}

@media (max-width: 640px) {
  .credit-meter__rate {
    grid-template-columns: repeat(2, minmax(0, 1fr));
  }
  .credit-meter__model { grid-column: 1; }
  .credit-meter__remove { grid-column: 2; }
}
</style>
