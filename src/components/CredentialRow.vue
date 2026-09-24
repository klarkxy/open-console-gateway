<template>
  <div
    class="credential-row"
    :data-credential-id="credential.id"
    :data-quota-status="quotaPresentation?.kind ?? ''"
    :class="{
      'credential-row--dragging': dragging,
      'credential-row--unavailable': unavailable,
    }"
  >
    <div class="credential-row__head">
      <n-button quaternary circle size="tiny" class="credential-order-handle"
        :disabled="orderDisabled" :aria-label="t('调整 Key {name} 的顺序', { name: credential.name })"
        aria-describedby="account-order-instructions"
        @pointerdown="emit('order-drag-start', $event)" @keydown="emit('order-keydown', $event)" @click.prevent>
        <template #icon><n-icon :component="HolderOutlined" /></template>
      </n-button>
      <span class="credential-row__name">{{ credential.name }}</span>
      <n-tag v-if="quotaLabel" size="small" role="status">{{ quotaLabel }}</n-tag>
      <n-tag v-if="cpaStatusLabel" size="small" role="status" :type="cpaStatusType">
        {{ cpaStatusLabel }}
      </n-tag>
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
        :limits="limits"
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
          :usage="usage"
          :limits="limits"
          :edits="edits"
          :now="now"
          :usage-loading="usageLoading"
          :usage-load-error="usageLoadError"
          :usage-refresh-loading="usageRefreshLoading"
          :menu-options="menuOptions"
          :connections="connections"
          :show-refresh="showRefresh"
          :refreshing="refreshing"
          @toggle="emit('toggle')"
          @test-connection="emit('test-connection')"
          @refresh-usage="emit('refresh-usage')"
          @menu-select="emit('menu-select', $event)"
          @usage-editor-open="emit('usage-editor-open')"
          @usage-update-draft="(key, value) => emit('usage-update-draft', key, value)"
          @usage-update-resets-first="(key, value) => emit('usage-update-resets-first', key, value)"
          @usage-update-resets-second="(key, value) => emit('usage-update-resets-second', key, value)"
          @usage-save="(key) => emit('usage-save', key)"
        />
      </div>
    </div>
    <CredentialBody
      v-if="account"
      :account="account"
      :identity="identity"
      :catalog="catalog"
      :provider-usage="providerUsage"
      :now="now"
      :usage-loading="usageLoading"
      :usage-load-error="usageLoadError"
      :connections="connections"
      :figure="figure"
      :hide-model-count="hideModelCount"
      @reload-usage="emit('reload-usage')"
      @open-wizard="emit('open-wizard')"
    />
  </div>
</template>

<script setup lang="ts">
import { computed } from "vue";
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
    usage: UsageWindow;
    providerUsage: ProviderUsageResponse | null;
    limits: UsageLimitView[];
    edits: AccountUsageEdits | undefined;
    now: number;
    usageLoading: boolean;
    usageLoadError: string | null;
    usageRefreshLoading: boolean;
    purchaseDateSaving: boolean;
    quotaLimitsFailed?: boolean;
    menuOptions: AccountMenuOption[];
    accountNames?: Readonly<Record<string, string>>;
    connections?: readonly Connection[] | null;
    extraTags?: string[];
    figure?: CredentialFigure | null;
    hideModelCount?: boolean;
    duplicateName?: boolean;
    showRefresh?: boolean;
    refreshing?: boolean;
    orderDisabled?: boolean;
    dragging?: boolean;
    quotaRetrying?: boolean;
    cpaStatus?: CpaCardStatus | null;
    modelCount?: number | null;
  }>(),
  {
    account: null,
    identity: null,
    quotaLimitsFailed: false,
    accountNames: undefined,
    connections: null,
    extraTags: () => [],
    figure: null,
    hideModelCount: false,
    duplicateName: false,
    showRefresh: undefined,
    refreshing: undefined,
    orderDisabled: true,
    dragging: false,
    quotaRetrying: false,
    cpaStatus: null,
    modelCount: null,
  },
);

const emit = defineEmits<{
  toggle: [];
  "order-drag-start": [event: PointerEvent];
  "order-keydown": [event: KeyboardEvent];
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
  "retry-quota": [];
  "open-models": [];
}>();

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
.credential-row--dragging { opacity: 0.65; border-color: var(--ocg-primary); }
.credential-row--unavailable {
  background: color-mix(in srgb, var(--ocg-muted) 12%, var(--ocg-surface));
  border-color: var(--ocg-border);
}
.credential-row--unavailable .credential-row__name {
  color: var(--ocg-muted);
}
.credential-row {
  display: grid;
  gap: var(--ocg-space-xs);
  padding: var(--ocg-space-sm);
  border: 1px solid var(--ocg-border);
  border-radius: var(--ocg-radius-md);
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
