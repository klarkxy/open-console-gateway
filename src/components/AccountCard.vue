<template>
  <AccountCardFrame
    :route-id="account.id"
    :name="account.name"
    :family="brandFamily"
    :type-label="typeLabel"
    :subtitle="capabilities.endpointOnAccount ? account.custom_config?.endpoint_url ?? '' : ''"
    :tone="tone"
    :order-handle-disabled="orderHandleDisabled"
    :dragging="dragging"
    @order-keydown="emit('order-keydown', $event)"
    @order-drag-start="emit('order-drag-start', $event)"
  >
    <template #tags>
      <CredentialTags
        :account="account"
        :identity="identity"
        :catalog="catalog"
        :limits="limits"
        :now="now"
        :purchase-date-saving="purchaseDateSaving"
        :account-names="accountNames"
        @update-purchase-date="emit('update-purchase-date', $event)"
      />
    </template>

    <template #actions>
      <CredentialActions
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
    </template>

    <CredentialBody
      :account="account"
      :identity="identity"
      :catalog="catalog"
      :provider-usage="providerUsage"
      :now="now"
      :usage-loading="usageLoading"
      :usage-load-error="usageLoadError"
      :connections="connections"
      @reload-usage="emit('reload-usage')"
      @open-wizard="emit('open-wizard')"
    />
  </AccountCardFrame>
</template>

<script setup lang="ts">
import { useDestinationsStore } from "../stores/destinations.ts";
import { computed } from "vue";
import type { Account, UsageWindow } from "../api/dashboard";
import type { Identity } from "../api/identities.ts";
import type {
  ProviderCatalogEntry,
  ProviderUsageResponse,
} from "../api/providers.ts";
import { isCooling } from "../domain/accounts-usage.ts";
import type { UsageKey } from "../domain/accounts-usage.ts";
import {
  accountIsReady,
  accountTypeLabel,
  type AccountMenuOption,
} from "../domain/account-display.ts";
import { accountTypeLabelText } from "../views/account-status-text.ts";
import { accountBrandFamily } from "../domain/account-brand.ts";
import { accountCapabilities } from "../domain/account-capabilities.ts";
import type { AccountUsageEdits, UsageLimitView } from "../domain/useAccountUsage.ts";
import type { Connection } from "../api/connections.ts";
import AccountCardFrame, { type AccountCardTone } from "./AccountCardFrame.vue";
import CredentialActions from "./CredentialActions.vue";
import CredentialBody from "./CredentialBody.vue";
import CredentialTags from "./CredentialTags.vue";

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
  connections?: readonly Connection[] | null;
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

const destinations = useDestinationsStore();
const destination = computed(() => destinations.destinationForAccount(props.account.id));
const capabilities = computed(() => accountCapabilities(props.account, props.catalog, destination.value));
const brandFamily = computed(() => accountBrandFamily(props.account, props.catalog));
const typeLabel = computed(() => (
  accountTypeLabelText(accountTypeLabel(props.account, props.catalog, destination.value))
));
const isDraft = computed(() => (
  accountIsReady(props.account)
  && !props.account.plan_routable
));
const tone = computed<AccountCardTone>(() => {
  if (isCooling(props.account, props.now)) return "cooling";
  if (!accountIsReady(props.account)) return "pending";
  if (isDraft.value) return "draft";
  return null;
});
</script>
