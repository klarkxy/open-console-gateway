<template>
  <div class="credential-row">
    <div class="credential-row__head">
      <span class="credential-row__name">{{ credential.name }}</span>
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
      @reload-usage="emit('reload-usage')"
      @open-wizard="emit('open-wizard')"
    />
  </div>
</template>

<script setup lang="ts">
import { NTag } from "naive-ui";
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
import CredentialActions from "./CredentialActions.vue";
import CredentialBody, { type CredentialFigure } from "./CredentialBody.vue";
import CredentialTags from "./CredentialTags.vue";

withDefaults(
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
    duplicateName?: boolean;
    showRefresh?: boolean;
    refreshing?: boolean;
  }>(),
  {
    account: null,
    identity: null,
    quotaLimitsFailed: false,
    accountNames: undefined,
    connections: null,
    extraTags: () => [],
    figure: null,
    duplicateName: false,
    showRefresh: undefined,
    refreshing: undefined,
  },
);

const emit = defineEmits<{
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
</script>

<style scoped>
.credential-row {
  display: grid;
  gap: var(--ocg-space-xs);
  padding: var(--ocg-space-sm);
  border: 1px solid var(--ocg-border);
  border-radius: var(--ocg-radius-md);
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
