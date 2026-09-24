<template>
  <div v-if="slot?.error" role="alert">
    {{ t(BILLING_ERROR_KEYS[slot.error]) }}
    <n-button text :disabled="disabled" @click="reload">{{ t("重试") }}</n-button>
  </div>
  <p v-else-if="!slot?.loaded" role="status">{{ t("加载中…") }}</p>
  <CreditSetupFields v-if="status?.configurableCredits && !status.credits" :key="account.id" :presets="status.presets" :disabled="disabled" @change="emit('change', $event)" />
</template>
<script setup lang="ts">
import { computed, watch } from "vue";
import { NButton } from "naive-ui";
import type { Account } from "../api/dashboard.ts";
import { useBillingStore } from "../stores/billing.ts";
import { useProvidersStore } from "../stores/providers.ts";
import { useIdentitiesStore } from "../stores/identities.ts";
import { accountInferenceEndpointUrl } from "../domain/upstream-balance.ts";
import { BILLING_ERROR_KEYS, billingBinding } from "../domain/billing.ts";
import type { buildCreditSetup } from "../domain/credit-setup.ts";
import { t } from "../i18n/index.ts";
import CreditSetupFields from "./CreditSetupFields.vue";
const props = defineProps<{ account: Account; disabled?: boolean }>();
const emit = defineEmits<{ change: [result: ReturnType<typeof buildCreditSetup>] }>();
const store = useBillingStore();
const providers = useProvidersStore();
const identities = useIdentitiesStore();
const binding = computed(() => billingBinding(props.account.updated_at, accountInferenceEndpointUrl(props.account, identities.byAccountId.get(props.account.id), providers.connections)));
const slot = computed(() => store.byId[props.account.id]);
const status = computed(() => slot.value?.boundVersion === binding.value ? slot.value.status : null);
function reload(): void { void store.load(props.account.id, binding.value); }
watch(binding, reload, { immediate: true });
watch(() => [status.value?.configurableCredits, Boolean(status.value?.credits), slot.value?.error] as const, () => {
  if (!status.value || slot.value?.error) emit("change", { input: null, valid: false });
  else if (!status.value.configurableCredits || status.value.credits) emit("change", { input: null, valid: true });
}, { immediate: true });
</script>
