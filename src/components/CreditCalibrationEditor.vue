<template>
  <n-form label-placement="top" @submit.prevent="save">
    <n-form-item v-for="row in drafts" :key="row.bucketId" :label="row.label">
      <div class="credit-calibration-amount">
        <n-input-number v-model:value="row.remainingScaled" :show-button="false" :min="0" :precision="4" :disabled="blocked || mutating" :aria-label="row.label" :input-props="{ 'aria-label': row.label }" />
        <span v-if="status.presets.length" class="mono">M</span>
      </div>
    </n-form-item>
    <p v-if="errorKey" role="alert">{{ t(errorKey) }}</p>
    <p v-if="blocked" role="status">{{ t("{count} 条待结算", { count: status.credits?.pendingRequests ?? 0 }) }}</p>
    <n-button type="primary" size="small" :loading="mutating" :disabled="blocked || drafts.length === 0" @click="save">{{ t("保存") }}</n-button>
  </n-form>
</template>
<script setup lang="ts">
import { computed, ref, watch } from "vue";
import { NButton, NForm, NFormItem, NInputNumber } from "naive-ui";
import type { BillingStatus } from "../api/billing.ts";
import { BILLING_ERROR_KEYS, CREDIT_AMOUNT_ISSUE_KEYS, calibrationBalances, creditCalibrationBlock, creditDisplayFactor, creditsToScaled, partitionCreditBuckets } from "../domain/billing.ts";
import { t, type MessageKey } from "../i18n/index.ts";
import { useBillingStore } from "../stores/billing.ts";
const props = defineProps<{ accountId: string; binding: string; status: BillingStatus; now: number }>();
const emit = defineEmits<{ saved: [] }>();
const store = useBillingStore();
const factor = computed(() => creditDisplayFactor(props.status.presets.length));
const blocked = computed(() => Boolean(creditCalibrationBlock(props.status.credits)));
const mutating = computed(() => store.byId[props.accountId]?.mutating ?? false);
const drafts = ref<Array<{ bucketId: string; label: string; remainingScaled: number | null }>>([]);
const validationError = ref<MessageKey | null>(null);
const errorKey = computed(() => validationError.value ?? (store.byId[props.accountId]?.error ? BILLING_ERROR_KEYS[store.byId[props.accountId]!.error!] : null));
watch(() => [props.accountId, props.binding, store.sessionEpoch], () => {
  validationError.value = null;
  drafts.value = partitionCreditBuckets(props.status.credits?.buckets ?? [], props.now).active.map(bucket => ({ bucketId: bucket.id, label: bucket.label, remainingScaled: creditsToScaled(bucket.remaining, factor.value) }));
}, { immediate: true });
async function save(): Promise<void> {
  if (blocked.value || mutating.value) return;
  const balances = calibrationBalances(drafts.value, factor.value);
  if (!balances || !balances.length) { validationError.value = CREDIT_AMOUNT_ISSUE_KEYS.invalid; return; }
  const epoch = store.sessionEpoch;
  try {
    await store.calibrateCredits(props.accountId, props.binding, balances);
    if (epoch === store.sessionEpoch) emit("saved");
  } catch { /* Keep the draft and show the store error. */ }
}
</script>
<style scoped>
.credit-calibration-amount { display: flex; gap: var(--ocg-space-sm); align-items: center; }
</style>
