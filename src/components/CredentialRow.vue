<template>
  <div
    class="credential-row"
    :data-credential-id="credential.id"
    :data-quota-status="quotaPresentation?.kind ?? ''"
    :class="{
      'credential-row--dragging': dragging,
      'credential-row--unavailable': unavailable,
      'credential-row--compact': sortMode,
    }"
  >
    <div class="credential-row__head">
      <n-button quaternary circle size="tiny" class="credential-order-handle"
        :disabled="orderDisabled" :aria-label="t('调整 Key {name} 的顺序', { name: account?.name ?? credential.name })"
        aria-describedby="account-order-instructions"
        @pointerdown="emit('order-drag-start', $event)" @keydown="emit('order-keydown', $event)" @click.prevent>
        <template #icon><n-icon :component="HolderOutlined" /></template>
      </n-button>
      <span class="credential-row__name">{{ account?.name ?? credential.name }}</span>
      <n-tag v-if="quotaLabel" size="small" role="status">{{ quotaLabel }}</n-tag>
      <n-tag v-if="cpaStatusLabel" size="small" role="status" :type="cpaStatusType">
        {{ cpaStatusLabel }}
      </n-tag>
      <template v-if="!sortMode">
        <n-button
          v-if="modelCount !== null"
          text
          size="small"
          class="credential-models-trigger"
          :aria-label="t('{count} 个模型', { count: modelCount })"
          @click="emit('open-models')"
        >
          <n-tag size="small" :bordered="false">{{ t("{count} 个模型", { count: modelCount }) }}</n-tag>
        </n-button>
        <n-button
          v-if="canRetryQuota"
          size="tiny"
          secondary
          :loading="quotaRetrying"
          :aria-label="t('重新尝试')"
          @click="emit('retry-quota')"
        >
          {{ t("重新尝试") }}
        </n-button>
        <CredentialTags
          v-if="account"
          :account="account"
          :identity="identity"
          :catalog="catalog"
          :limits="rowLimits"
          :now="now"
          :purchase-date-saving="purchaseDateSaving"
          :account-names="accountNames"
          :extra-tags="extraTags"
          :duplicate-name="duplicateName"
          :hide-model-restriction="hideModelCount"
          @update-purchase-date="emit('update-purchase-date', $event)"
        />
        <template v-else>
          <n-tag
            v-for="(tag, index) in extraTags"
            :key="`${tag}:${index}`"
            size="small"
            :bordered="false"
          >
            {{ tag }}
          </n-tag>
        </template>
        <div class="credential-row__actions">
          <CredentialActions
            v-if="account"
            compact
            :account="account"
            :identity="identity"
            :catalog="catalog"
            :usage="rowUsage"
            :limits="rowLimits"
            :edits="edits"
            :now="now"
            :usage-loading="rowUsageLoading"
            :usage-load-error="rowUsageLoadError"
            :usage-refresh-loading="rowUsageRefreshLoading"
            :refresh-state="refreshState"
            :menu-options="rowActions"
            :connections="connections"
            @toggle="!accountDeleting && emit('toggle')"
            @menu-select="selectAction"
            @refresh-usage="selectAction('refresh-usage')"
            @usage-editor-open="emit('usage-editor-open')"
            @usage-update-draft="(key, value) => emit('usage-update-draft', key, value)"
            @usage-update-resets-first="(key, value) => emit('usage-update-resets-first', key, value)"
            @usage-update-resets-second="(key, value) => emit('usage-update-resets-second', key, value)"
            @usage-save="(key) => emit('usage-save', key)"
          />
        </div>
      </template>
    </div>
    <CredentialBody
      v-if="account && !sortMode"
      :account="account"
      :identity="identity"
      :catalog="catalog"
      :provider-usage="rowProviderUsage"
      :now="now"
      :usage-loading="rowUsageLoading"
      :usage-load-error="rowUsageLoadError"
      :connections="connections"
      :figure="figure"
      :hide-model-count="hideModelCount"
      @reload-usage="emit('reload-usage')"
      @open-wizard="emit('open-wizard')"
    />
  </div>
</template>

<script setup lang="ts">
import { computed, unref, type MaybeRef } from "vue";
import { NTag, NButton, NIcon } from "naive-ui";
import { HolderOutlined } from "@vicons/antd";
import { t } from "../i18n/index.ts";
import type { Account, UsageWindow } from "../api/dashboard";
import type { Destination, DestinationCredential } from "../api/destinations.ts";
import type { Identity } from "../api/identities.ts";
import type {
  ProviderCatalogEntry,
  ProviderUsageResponse,
} from "../api/providers.ts";
import type { Connection } from "../api/connections.ts";
import type { UsageKey } from "../domain/accounts-usage.ts";
import type { AccountMenuOption } from "../domain/account-display.ts";
import type { AccountUsageEdits, UsageLimitView } from "../domain/useAccountUsage.ts";
import { accountCapabilities } from "../domain/account-capabilities.ts";
import { accountRowActions } from "../domain/account-row-actions.ts";
import type { AccountRefreshState } from "../domain/account-refresh-queue.ts";
import { billingBinding } from "../domain/billing.ts";
import { findPlanDefinition } from "../domain/plans.ts";
import { accountInferenceEndpointUrl, officialBalanceSupported } from "../domain/upstream-balance.ts";
import { usageCompanionCatalog } from "../domain/usage-refresh-catalog.ts";
import { useAccountRemoval } from "../domain/useAccountRemoval.ts";
import { useBillingStore } from "../stores/billing.ts";
import {
  credentialIsRouteAvailable,
  quotaRecoveryPresentation,
  quotaRetryRequestNeeded,
  withAccountEnablement,
} from "../domain/quota-recovery.ts";
import { cpaCardProcessDown, cpaCardStatusTagType, type CpaCardStatus } from "../domain/cpa-runtime.ts";
import { cpaCardStatusText, quotaRecoveryText } from "../views/account-status-text.ts";
import CredentialActions from "./CredentialActions.vue";
import CredentialBody, { type CredentialFigure } from "./CredentialBody.vue";
import CredentialTags from "./CredentialTags.vue";

const props = withDefaults(
  defineProps<{
    credential: DestinationCredential;
    destination: Destination;
    account?: Account | null;
    identity?: Identity | null;
    catalog: readonly ProviderCatalogEntry[] | null;
    usage: MaybeRef<UsageWindow>;
    providerUsage: MaybeRef<ProviderUsageResponse | null>;
    limits: MaybeRef<UsageLimitView[]>;
    edits: AccountUsageEdits | undefined;
    now: number;
    usageLoading: MaybeRef<boolean>;
    usageReadBlocked?: boolean;
    usageLoadError: MaybeRef<string | null>;
    usageRefreshLoading: MaybeRef<boolean>;
    refreshState?: AccountRefreshState;
    purchaseDateSaving: boolean;
    menuOptions: AccountMenuOption[];
    accountNames?: Readonly<Record<string, string>>;
    connections?: readonly Connection[] | null;
    extraTags?: string[];
    figure?: CredentialFigure | null;
    hideModelCount?: boolean;
    duplicateName?: boolean;
    orderDisabled?: boolean;
    dragging?: boolean;
    quotaRetrying?: boolean;
    cpaStatus?: CpaCardStatus | null;
    modelCount?: number | null;
    /** Compact single-line rendering for card sort mode. */
    sortMode?: boolean;
  }>(),
  {
    account: null,
    identity: null,
    accountNames: undefined,
    connections: null,
    extraTags: () => [],
    figure: null,
    hideModelCount: false,
    duplicateName: false,
    orderDisabled: true,
    dragging: false,
    quotaRetrying: false,
    cpaStatus: null,
    modelCount: null,
    sortMode: false,
  },
);

const emit = defineEmits<{
  toggle: [];
  "order-drag-start": [event: PointerEvent];
  "order-keydown": [event: KeyboardEvent];
  "update-purchase-date": [date: string];
  "reload-usage": [];
  "open-wizard": [];
  "menu-select": [key: string | number];
  "usage-editor-open": [];
  "usage-update-draft": [key: UsageKey, value: number | null];
  "usage-update-resets-first": [key: UsageKey, value: number | null];
  "usage-update-resets-second": [key: UsageKey, value: number | null];
  "usage-save": [key: UsageKey];
  "retry-quota": [];
  "open-models": [];
}>();

const billing = useBillingStore();
const { confirmDelete, deleting } = useAccountRemoval();
const rowUsage = computed(() => unref(props.usage));
const rowProviderUsage = computed(() => unref(props.providerUsage));
const rowLimits = computed(() => unref(props.limits));
const rowUsageLoading = computed(() => unref(props.usageLoading) || Boolean(props.usageReadBlocked));
const rowUsageLoadError = computed(() => unref(props.usageLoadError));
const rowUsageRefreshLoading = computed(() => unref(props.usageRefreshLoading));

const accountDeleting = computed(() => Boolean(props.account && deleting.value[props.account.id]));
const refreshSupported = computed(() => {
  const account = props.account;
  if (!account || account.setup_step !== "ready") return false;
  const caps = accountCapabilities(account, props.catalog, props.destination);
  if (caps.externalIntegration || caps.toggleWrite === "provider_settings") return false;
  if (props.destination.legacy.kind === "platform_parent") return true;
  const endpoint = accountInferenceEndpointUrl(account, props.identity, props.connections);
  const slot = billing.slotFor(account.id).value;
  const status = slot?.boundVersion === billingBinding(account.updated_at, endpoint) ? slot.status : null;
  const surface = findPlanDefinition(account.provider_id, props.catalog);
  const quota = status ? status.officialRefresh
    : surface?.usage_availability === "available" || officialBalanceSupported(endpoint, props.connections);
  const models = usageCompanionCatalog({
    providerId: account.provider_id, catalog: props.catalog, destination: props.destination,
  }).kind !== "none";
  return quota || models;
});
const rowActions = computed(() => accountRowActions(props.menuOptions, props.account, {
  platformLinked: props.destination.legacy.kind === "platform_parent",
  refreshSupported: refreshSupported.value,
  deleting: accountDeleting.value,
  refreshBusy: rowUsageLoading.value || rowUsageRefreshLoading.value,
}));

function selectAction(key: string | number): void {
  const action = rowActions.value.find(option => option.key === key);
  if (!action || action.disabled) return;
  if (key === "delete" && props.account) confirmDelete(props.account);
  else emit("menu-select", key);
}

const quotaPresentation = computed(() => (
  quotaRecoveryPresentation(props.credential.quota_recovery, props.now)
));
const presentedCredential = computed(() => withAccountEnablement(
  props.credential,
  props.account?.enabled,
));
const unavailable = computed(() => (
  cpaCardProcessDown(props.cpaStatus)
  || !credentialIsRouteAvailable(presentedCredential.value, props.destination, props.now)
));
const quotaLabel = computed(() => (
  quotaPresentation.value ? quotaRecoveryText(quotaPresentation.value) : ""
));
const cpaStatusLabel = computed(() => (
  props.cpaStatus ? cpaCardStatusText(props.cpaStatus) : ""
));
const cpaStatusType = computed(() => (
  props.cpaStatus ? cpaCardStatusTagType(props.cpaStatus) : "default"
));
const canRetryQuota = computed(() => quotaRetryRequestNeeded(props.credential.quota_recovery));
</script>

<style scoped>
.credential-order-handle { touch-action: none; cursor: grab; }
.credential-row--dragging { opacity: 0.65; }
.credential-row--unavailable {
  color: var(--ocg-muted);
}
.credential-row--unavailable .credential-row__name {
  color: var(--ocg-muted);
}
.credential-row {
  display: grid;
  gap: var(--ocg-space-xs);
  padding: var(--ocg-space-sm);
  transition: background-color var(--ocg-motion-fast) var(--ocg-ease);
}

.credential-row:hover {
  background: var(--ocg-surface-sunken);
}

.credential-row--compact {
  padding: var(--ocg-space-xs) var(--ocg-space-sm);
}

.credential-models-trigger {
  min-width: 0;
  padding: 0;
}
.credential-models-trigger :deep(.n-button__content) {
  min-width: 0;
}
.credential-row__head {
  display: flex;
  flex-wrap: wrap;
  align-items: center;
  gap: var(--ocg-space-sm);
}

.credential-row__name {
  font-weight: 600;
  font-size: var(--ocg-font-sm);
  color: var(--ocg-ink);
}

.credential-row__actions {
  display: flex;
  align-items: center;
  gap: var(--ocg-space-sm);
  margin-left: auto;
}
</style>
