<template>
  <section class="billing-panel" :aria-label="t('用量')">
    <p v-if="mode === 'initial_loading'" role="status">{{ t("加载中…") }}</p>
    <p v-else-if="mode === 'initial_error'" class="billing-panel__error" role="alert">
      <span>{{ t(failureKey!) }}</span>
      <n-button text size="tiny" type="primary" :disabled="loading" @click="reload">
        {{ t("重试") }}
      </n-button>
    </p>
    <template v-else>
      <p v-if="overlayKey" class="billing-panel__error" role="alert">
        <span>{{ t(overlayKey) }}</span>
        <n-button text size="tiny" type="primary" :disabled="loading || mutating" @click="reload">
          {{ t("重试") }}
        </n-button>
      </p>
      <OfficialApiPanel
        v-if="kind === 'cash'"
        :provider-id="account.provider_id"
        :account-id="account.id"
        :account-version="account.updated_at"
        :account-status="status?.cash ?? null"
        :now="now"
        :mutating="mutating"
        @refresh-balance="onRefreshCash"
      />
      <AccountCreditBalance
        v-else-if="kind === 'cash_balances'"
        :credit-balances="creditBalances"
        :usage-load-error="null"
        :usage-loading="loading"
        @reload-usage="reload"
      />
      <ProviderQuotaSummary
        v-else-if="kind === 'quota' || kind === 'credits_usd_month'"
        :usage="presented"
        :now="now"
      />
      <n-button
        v-if="creditConfigureAvailable"
        size="tiny"
        quaternary
        :disabled="mutating"
        @click="creditSetupOpen = true"
      >
        {{ t("积分计费") }}
      </n-button>
      <CreditMeterPanel
        v-if="kind === 'credits_meter' || kind === 'credits_setup' || creditSetupOpen"
        :account-id="account.id"
        :binding="binding"
        :status="status!"
        :now="now"
        :setup-requested="creditSetupOpen"
        @setup-closed="creditSetupOpen = false"
      />
    </template>
  </section>
</template>

<script setup lang="ts">
import { computed, ref, watch } from "vue";
import { NButton } from "naive-ui";
import type { Account } from "../api/dashboard";
import type { Identity } from "../api/identities.ts";
import type { Connection } from "../api/connections.ts";
import {
  BILLING_ERROR_KEYS,
  billingBinding,
  billingPanelMode,
  billingPanelOverlayError,
  billingSurfaceKind,
  cashCreditConfigureAvailable,
  presentedUsageOf,
} from "../domain/billing.ts";
import { accountInferenceEndpointUrl } from "../domain/upstream-balance.ts";
import { t } from "../i18n/index.ts";
import { useBillingStore } from "../stores/billing.ts";
import AccountCreditBalance from "./AccountCreditBalance.vue";
import CreditMeterPanel from "./CreditMeterPanel.vue";
import OfficialApiPanel from "./OfficialApiPanel.vue";
import ProviderQuotaSummary from "./ProviderQuotaSummary.vue";

const props = withDefaults(
  defineProps<{
    account: Account;
    identity?: Identity | null;
    connections?: readonly Connection[] | null;
    now: number;
  }>(),
  {
    identity: null,
    connections: null,
  },
);

const store = useBillingStore();
const creditSetupOpen = ref(false);
const endpointUrl = computed(() => (
  accountInferenceEndpointUrl(props.account, props.identity, props.connections)
));
const binding = computed(() => billingBinding(props.account.updated_at, endpointUrl.value));
const slot = computed(() => store.byId[props.account.id]);
const status = computed(() => slot.value?.status ?? null);
const loading = computed(() => slot.value?.loading ?? false);
const mutating = computed(() => slot.value?.mutating ?? false);
const mode = computed(() => billingPanelMode({
  status: status.value,
  loaded: slot.value?.loaded ?? false,
  loading: loading.value,
  error: slot.value?.error ?? null,
}));
const failureKey = computed(() => {
  const code = slot.value?.error;
  return code ? BILLING_ERROR_KEYS[code] : null;
});
const overlayKey = computed(() => {
  const code = billingPanelOverlayError({
    status: status.value,
    error: slot.value?.error ?? null,
  });
  return code ? BILLING_ERROR_KEYS[code] : null;
});
const kind = computed(() => status.value ? billingSurfaceKind(status.value) : "empty");
const presented = computed(() => presentedUsageOf(status.value));
const creditBalances = computed(() => presented.value?.credit_balances ?? []);
const creditConfigureAvailable = computed(() => (
  status.value ? cashCreditConfigureAvailable(status.value) : false
));

function reload(): void {
  void store.load(props.account.id, binding.value);
}

async function onRefreshCash(): Promise<void> {
  try {
    await store.refreshCash(props.account.id, binding.value);
  } catch {
    // Store keeps the last snapshot.
  }
}

watch(
  () => [props.account.id, props.account.updated_at, endpointUrl.value] as const,
  () => {
    creditSetupOpen.value = false;
    reload();
  },
  { immediate: true },
);
watch(() => store.sessionEpoch, () => { creditSetupOpen.value = false; });
</script>

<style scoped>
.billing-panel {
  display: grid;
  gap: var(--ocg-space-sm);
  min-width: 0;
}

.billing-panel p {
  margin: 0;
  color: var(--ocg-muted);
  font-size: var(--ocg-font-sm);
}

.billing-panel__error {
  display: flex;
  flex-wrap: wrap;
  align-items: center;
  gap: var(--ocg-space-sm);
  color: var(--ocg-error);
}
</style>
