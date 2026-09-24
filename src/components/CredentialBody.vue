<template>
  <AccountFigure
    v-if="figure"
    compact
    :value="figure.value"
    :label="figure.label"
    :caption="figure.caption"
  />
  <div v-if="!accountIsReady(account)" class="managed-pending">
    <div>
      <strong>{{ t(MANAGED_STEP_LABEL_KEYS[account.setup_step]) }}</strong>
      <p>{{ t("注册进度已保存，继续使用该账号的独立浏览器 Profile。") }}</p>
    </div>
    <n-button type="primary" secondary @click="emit('open-wizard')">
      {{ t("继续注册") }}
    </n-button>
  </div>
  <div v-else-if="isDraft" class="provider-unconfigured" role="status">
    <p>{{ draftDescription }}</p>
  </div>
  <div v-else-if="capabilities.billingTierRequired && ollamaNeedsBilling" class="provider-unconfigured" role="status">
    <p>{{ t("配置 Ollama 计费档位以显示本月额度") }}</p>
  </div>
  <BillingPanel
    v-else-if="showsBilling"
    :account="account"
    :identity="identity"
    :connections="connections"
    :now="now"
  />
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
  <div v-else-if="showsModelCount || showsOfficialBalance" class="account-meta-row">
    <AccountCreditBalance
      v-if="showsOfficialBalance"
      :credit-balances="creditBalances"
      :usage-load-error="usageLoadError"
      :usage-loading="usageLoading"
      @reload-usage="emit('reload-usage')"
    />
    <span v-if="showsModelCount" class="account-meta-row__models">
      {{ t("{count} 个模型", { count: uniquePublicModelCount(account) }) }}
    </span>
  </div>
</template>

<script setup lang="ts">
import { useDestinationsStore } from "../stores/destinations.ts";
import { computed } from "vue";
import { NButton } from "naive-ui";
import type { Account } from "../api/dashboard";
import type { Identity } from "../api/identities.ts";
import type {
  ProviderCatalogEntry,
  ProviderUsageResponse,
} from "../api/providers.ts";
import type { Connection } from "../api/connections.ts";
import {
  MANAGED_STEP_LABEL_KEYS,
  ROUTING_DRAFT_DESCRIPTION_KEYS,
  accountIsReady,
  accountRoutingDraftState,
} from "../domain/account-display.ts";
import { accountCapabilities } from "../domain/account-capabilities.ts";
import { findPlanDefinition } from "../domain/plans.ts";
import { t } from "../i18n/index.ts";
import { uniquePublicModelCount } from "../domain/platform-accounts.ts";
import { accountInferenceEndpointUrl, officialBalanceSupported } from "../domain/upstream-balance.ts";
import AccountCreditBalance from "./AccountCreditBalance.vue";
import AccountFigure from "./AccountFigure.vue";
import BillingPanel from "./BillingPanel.vue";
import ProviderQuotaSummary from "./ProviderQuotaSummary.vue";

export type CredentialFigure = {
  value: string;
  label?: string;
  caption?: string;
};

const props = withDefaults(
  defineProps<{
    account: Account;
    identity?: Identity | null;
    catalog: readonly ProviderCatalogEntry[] | null;
    providerUsage: ProviderUsageResponse | null;
    now: number;
    usageLoading: boolean;
    usageLoadError: string | null;
    connections?: readonly Connection[] | null;
    figure?: CredentialFigure | null;
    hideModelCount?: boolean;
  }>(),
  {
    identity: null,
    connections: null,
    figure: null,
    hideModelCount: false,
  },
);

const emit = defineEmits<{
  "reload-usage": [];
  "open-wizard": [];
}>();

const destinations = useDestinationsStore();
const destination = computed(() => destinations.destinationForAccount(props.account.id));
const capabilities = computed(() => accountCapabilities(props.account, props.catalog, destination.value));
const showsModelCount = computed(() => (
  capabilities.value.endpointOnAccount && !props.hideModelCount
));
const ollamaNeedsBilling = computed(() => !props.account.ollama_billing_tier);
const plan = computed(() => findPlanDefinition(props.account.provider_id, props.catalog));
const manualUsageCalibration = computed(() => (
  plan.value?.manual_usage_calibration ?? false
));
const usageRefreshAvailable = computed(() => plan.value?.usage_availability === "available");
const inferenceEndpointUrl = computed(() => (
  accountInferenceEndpointUrl(props.account, props.identity, props.connections)
));
const balanceRefreshAvailable = computed(() => officialBalanceSupported(inferenceEndpointUrl.value, props.connections));
const creditBalances = computed(() => props.providerUsage?.credit_balances ?? []);
const showsOfficialBalance = computed(() => (
  balanceRefreshAvailable.value || creditBalances.value.length > 0
));
const usageDisplayAvailable = computed(() => (
  usageRefreshAvailable.value || manualUsageCalibration.value
));
const showsBilling = computed(() => {
  if (props.hideModelCount) return false;
  if (plan.value?.model_source === "official_api_preset") return true;
  if (usageDisplayAvailable.value || balanceRefreshAvailable.value) return true;
  return plan.value?.kind === "custom" || plan.value?.dynamic === true;
});
const isDraft = computed(() => (
  accountIsReady(props.account)
  && !props.account.plan_routable
));
const draftDescription = computed(() => {
  const state = accountRoutingDraftState(props.account);
  return state ? t(ROUTING_DRAFT_DESCRIPTION_KEYS[state]) : "";
});
</script>

<style scoped>
.provider-unconfigured {
  color: var(--ocg-warning);
  font-size: var(--ocg-font-sm);
}

.provider-unconfigured > p {
  margin: 0;
}

.official-plan-usage {
  margin-top: var(--ocg-space-sm);
}

.account-meta-row {
  display: flex;
  flex-wrap: wrap;
  align-items: baseline;
  gap: var(--ocg-space-xs) var(--ocg-space-lg);
  min-width: 0;
}

.account-meta-row__models {
  color: var(--ocg-muted);
  font-size: var(--ocg-font-sm);
}

.managed-pending {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: var(--ocg-space-lg);
  padding-top: var(--ocg-space-sm);
}

.managed-pending strong {
  color: var(--ocg-ink);
  font-size: var(--ocg-font-md);
}

.managed-pending p {
  margin: var(--ocg-space-xs) 0 0;
  color: var(--ocg-muted);
  font-size: var(--ocg-font-sm);
}

.usage-load-error {
  display: flex;
  align-items: center;
  justify-content: center;
  gap: var(--ocg-space-sm);
  min-height: 42px;
  color: var(--ocg-error);
  font-size: var(--ocg-font-sm);
}

@media (max-width: 640px) {
  .managed-pending {
    align-items: stretch;
    flex-direction: column;
  }
}
</style>
