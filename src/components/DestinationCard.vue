<template>
  <AccountCardFrame
    :route-id="group.id"
    :name="group.destination.name"
    :family="family"
    :type-label="typeLabel"
    :subtitle="group.destination.base_url ?? ''"
    :order-handle-disabled="orderHandleDisabled"
    :order-handle-hint="orderHandleHint"
    :dragging="dragging"
    @order-keydown="emit('order-keydown', $event)"
    @order-drag-start="emit('order-drag-start', $event)"
  >
    <template v-if="parent" #actions>
      <div class="account-action account-action--secondary">
        <n-tooltip trigger="hover">
          <template #trigger>
            <n-button
              circle
              quaternary
              size="small"
              :aria-label="t('添加 Key')"
              :disabled="mutating"
              @click="emit('add-key')"
            >
              <template #icon><n-icon :component="PlusOutlined" /></template>
            </n-button>
          </template>
          {{ t("添加 Key") }}
        </n-tooltip>
      </div>
      <div class="account-action account-action--tertiary">
        <n-tooltip trigger="hover">
          <template #trigger>
            <n-button
              circle
              quaternary
              size="small"
              :loading="refreshingParent"
              :disabled="mutating"
              :aria-label="t('刷新')"
              @click="emit('refresh-parent')"
            >
              <template #icon><n-icon :component="ReloadOutlined" /></template>
            </n-button>
          </template>
          {{ t("刷新") }}
        </n-tooltip>
      </div>
      <div class="account-action account-action--menu">
        <n-dropdown
          :options="parentMenuOptions"
          trigger="click"
          placement="bottom-end"
          @select="handleParentMenuSelect"
        >
          <n-tooltip trigger="hover">
            <template #trigger>
              <n-button circle quaternary size="small" :aria-label="t('更多操作')">
                <template #icon><n-icon :component="MoreOutlined" /></template>
              </n-button>
            </template>
            {{ t("更多操作") }}
          </n-tooltip>
        </n-dropdown>
      </div>
    </template>

    <div class="destination-card-body">
      <AccountFigure
        v-if="parentBalanceText"
        :label="t('余额')"
        :value="parentBalanceText"
        :caption="parentUsedText"
      />
      <n-alert
        v-if="parent?.snapshot && parent.snapshot.errors.length > 0"
        type="warning"
        :show-icon="false"
      >
        {{ t("刷新错误") }}: {{ parent.snapshot.errors.join(", ") }}
      </n-alert>
      <PlatformPriceTable v-if="parent?.snapshot" :snapshot="parent.snapshot" />

      <n-alert
        v-if="parent && pendingLink && pendingLink.parentId === parent.id"
        type="warning"
        :show-icon="false"
      >
        <div class="destination-pending-link">
          <span>{{ t("Key 已创建，关联尚未完成。") }}</span>
          <n-button size="tiny" secondary :loading="mutating" :disabled="mutating" @click="emit('retry-pending-link')">
            {{ t("重试关联") }}
          </n-button>
        </div>
      </n-alert>
      <div
        v-if="parent && group.accounts.length === 0 && pendingLink?.parentId !== parent.id"
        class="destination-hint"
      >
        {{ t("尚无关联 Key。") }}
      </div>
      <div v-if="group.accounts.length > 0" class="destination-rows">
        <template v-for="(account, index) in group.accounts" :key="account.id">
          <slot
            name="row"
            :account="account"
            :index="index"
            :extra-tags="extraTagsFor(account)"
            :figure="figureFor(account)"
            :duplicate-name="duplicateNames.has(account.name.trim())"
          />
        </template>
      </div>
    </div>
  </AccountCardFrame>
</template>

<script setup lang="ts">
import { computed } from "vue";
import {
  NAlert,
  NButton,
  NDropdown,
  NIcon,
  NTooltip,
} from "naive-ui";
import { MoreOutlined, PlusOutlined, ReloadOutlined } from "@vicons/antd";
import type { Account } from "../api/dashboard.ts";
import type { ProviderCatalogEntry } from "../api/providers.ts";
import type {
  PlatformAccount,
  PlatformLink,
  PlatformQuotaKind,
} from "../api/platform-accounts.ts";
import { destinationBrandFamily, platformBrandFamily } from "../domain/account-brand.ts";
import { destinationTypeLabel } from "../domain/account-display.ts";
import type { DestinationGroup } from "../domain/destination-groups.ts";
import {
  PLATFORM_KIND_LABELS,
  formatQuotaAmount,
  platformKeyGroupLabel,
  platformKeyQuotaName,
  platformModelOverlay,
  primaryQuota,
} from "../domain/platform-accounts.ts";
import { locale, t } from "../i18n/index.ts";
import { accountTypeLabelText } from "../views/account-status-text.ts";
import AccountCardFrame from "./AccountCardFrame.vue";
import AccountFigure from "./AccountFigure.vue";
import type { CredentialFigure } from "./CredentialBody.vue";
import PlatformPriceTable from "./PlatformPriceTable.vue";

const props = defineProps<{
  group: DestinationGroup;
  parent: PlatformAccount | null;
  catalog: readonly ProviderCatalogEntry[] | null;
  links: PlatformLink[];
  mutating: boolean;
  importing?: boolean;
  refreshing: Record<string, boolean>;
  pendingLink: { accountId: string; parentId: string } | null;
  orderHandleDisabled: boolean;
  orderHandleHint?: string;
  dragging: boolean;
  now: number;
}>();

const emit = defineEmits<{
  "order-keydown": [event: KeyboardEvent];
  "order-drag-start": [event: PointerEvent];
  "refresh-parent": [];
  edit: [];
  delete: [];
  "add-key": [];
  "import-keys": [];
  "link-existing": [];
  "retry-pending-link": [];
  "fetch-all-models": [];
}>();

const firstAccount = computed(() => props.group.accounts[0] ?? null);
const family = computed(() => {
  if (props.parent) return platformBrandFamily(props.parent.kind);
  return destinationBrandFamily(
    props.group.destination,
    firstAccount.value,
    props.catalog,
  );
});
const typeLabel = computed(() => {
  if (props.parent) return PLATFORM_KIND_LABELS[props.parent.kind];
  return accountTypeLabelText(destinationTypeLabel(props.group.destination));
});
const refreshingParent = computed(() => (
  props.parent ? Boolean(props.refreshing[props.parent.id]) : false
));
const overlay = computed(() => platformModelOverlay(props.group.accounts));
const duplicateNames = computed(() => {
  const counts = new Map<string, number>();
  for (const account of props.group.accounts) {
    const name = account.name.trim();
    if (!name) continue;
    counts.set(name, (counts.get(name) ?? 0) + 1);
  }
  return new Set([...counts.entries()].filter(([, count]) => count > 1).map(([name]) => name));
});

const parentMenuOptions = computed(() => {
  if (!props.parent) return [];
  return [
    { label: t("获取全部模型"), key: "fetch-all-models", disabled: props.mutating || props.group.accounts.length === 0 },
    ...(props.parent.kind === "new_api"
      ? [{
          label: props.parent.hasUserCredential ? t("从站点导入 Key") : t("填写用户 ID 和系统访问令牌后可导入"),
          key: "import-keys",
          disabled: props.mutating || !props.parent.hasUserCredential || Boolean(props.importing),
        }]
      : []),
    { label: t("关联已有 Key"), key: "link-existing", disabled: props.mutating },
    { label: t("编辑"), key: "edit", disabled: props.mutating },
    {
      label: props.group.accounts.length > 0
        ? t("已关联 {count} 个 Key，先取消关联后再删除", { count: props.group.accounts.length })
        : t("删除"),
      key: "delete",
      disabled: props.mutating || props.group.accounts.length > 0,
    },
  ];
});

function handleParentMenuSelect(key: string | number) {
  if (key === "fetch-all-models") emit("fetch-all-models");
  else if (key === "import-keys") emit("import-keys");
  else if (key === "link-existing") emit("link-existing");
  else if (key === "edit") emit("edit");
  else if (key === "delete") emit("delete");
}

function keySummary(accountId: string) {
  return overlay.value.keys.find((row) => row.accountId === accountId);
}

function keyGroup(accountId: string): string {
  const link = linkOf(accountId);
  return platformKeyGroupLabel(link, link?.snapshot);
}

function keyTokenName(accountId: string): string {
  return platformKeyQuotaName(linkOf(accountId)?.snapshot) ?? "";
}

function showsTokenName(account: Account): boolean {
  const tokenName = keyTokenName(account.id);
  return tokenName !== "" && tokenName !== account.name.trim();
}

function linkOf(accountId: string): PlatformLink | undefined {
  return props.links.find((link) => link.accountId === accountId);
}

function remainingText(kind: PlatformQuotaKind): string {
  if (!props.parent) return "";
  const quota = primaryQuota(props.parent.snapshot?.quotas ?? [], kind);
  if (!quota) return "";
  if (quota.unlimited) return t("不限");
  if (quota.remaining === null) return "";
  return formatQuotaAmount(quota.remaining, quota.unit, locale.value);
}

const parentBalanceText = computed(() => remainingText("wallet"));
const parentUsedText = computed(() => {
  if (!props.parent) return "";
  const quota = primaryQuota(props.parent.snapshot?.quotas ?? [], "wallet");
  if (!quota || quota.unlimited || quota.used === null) return "";
  return t("已用 {value}", { value: formatQuotaAmount(quota.used, quota.unit, locale.value) });
});

function extraTagsFor(account: Account): string[] {
  if (!props.parent) return [];
  const tags: string[] = [];
  const group = keyGroup(account.id);
  if (group) tags.push(group);
  if (showsTokenName(account)) tags.push(keyTokenName(account.id));
  tags.push(t("{count} 个模型", {
    count: keySummary(account.id)?.total ?? account.model_capabilities.length,
  }));
  return tags;
}

function figureFor(account: Account): CredentialFigure | null {
  if (!props.parent) return null;
  const quota = primaryQuota(linkOf(account.id)?.snapshot?.quotas ?? [], "key_limit");
  if (!quota || quota.unlimited || quota.remaining === null) return null;
  return { value: formatQuotaAmount(quota.remaining, quota.unit, locale.value) };
}
</script>

<style scoped>
.destination-card-body {
  display: grid;
  gap: var(--ocg-space-md);
}

.destination-rows {
  display: grid;
  gap: var(--ocg-space-xs);
}

.destination-hint {
  font-size: var(--ocg-font-xs);
  color: var(--ocg-subtle);
}

.destination-pending-link {
  display: flex;
  flex-wrap: wrap;
  align-items: center;
  justify-content: space-between;
  gap: var(--ocg-space-sm);
}
</style>
