<template>
  <AccountCardFrame
    :route-id="group.id"
    :name="group.destination.name"
    :family="family"
    :type-label="typeLabel"
    :subtitle="group.destination.base_url ?? ''"
    :tone="cardTone"
    :order-handle-disabled="orderHandleDisabled"
    :order-handle-hint="orderHandleHint"
    :dragging="dragging"
    @order-keydown="emit('order-keydown', $event)"
    @order-drag-start="emit('order-drag-start', $event)"
  >
    <template v-if="cpaStatusLabel || cardAvailabilityLabel" #tags>
      <n-tag v-if="cpaStatusLabel" size="small" role="status" :type="cpaStatusType">
        {{ cpaStatusLabel }}
      </n-tag>
      <n-tag v-if="cardAvailabilityLabel" size="small" role="status">{{ cardAvailabilityLabel }}</n-tag>
    </template>
    <template #actions>
      <div v-if="parent || group.destination.max_credentials !== 1" class="account-action account-action--secondary">
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
      <div v-if="parent" class="account-action account-action--tertiary">
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
      <ApiPriceMeter
        v-if="walletMeterCells.length > 0"
        :cells="walletMeterCells"
        :caption="walletMeterCaption"
      />
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
        v-if="cardAvailability === 'no_keys' && (!parent || pendingLink?.parentId !== parent.id)"
        class="destination-hint"
      >
        {{ t(CARD_QUOTA_AVAILABILITY_KEYS.no_keys) }}
      </div>
      <div v-if="group.credentials.length > 0" class="destination-rows">
        <template v-for="(credential, index) in group.credentials" :key="credential.id">
          <slot
            name="row"
            :credential="credential"
            :index="index"
            :extra-tags="extraTagsFor(credential)"
            :figure="figureFor(credential)"
            :duplicate-name="duplicateNames.has(credential.name.trim())"
            :hide-model-count="Boolean(parent)"
            :model-count="modelCountFor(credential)"
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
  NTag,
  NTooltip,
} from "naive-ui";
import { MoreOutlined, PlusOutlined, ReloadOutlined } from "@vicons/antd";
import type { Account } from "../api/dashboard.ts";
import type { DestinationCredential } from "../api/destinations.ts";
import type { ProviderCatalogEntry } from "../api/providers.ts";
import type {
  PlatformAccount,
  PlatformLink,
} from "../api/platform-accounts.ts";
import { destinationBrandFamily, platformBrandFamily } from "../domain/account-brand.ts";
import { destinationTypeLabel } from "../domain/account-display.ts";
import {
  isSingleAccountGroup,
  overlayAccountForCredential,
  type DestinationGroup,
} from "../domain/destination-groups.ts";
import { cpaCardProcessDown, cpaCardStatusTagType, type CpaCardStatus } from "../domain/cpa-runtime.ts";
import {
  CARD_QUOTA_AVAILABILITY_KEYS,
  cardQuotaAvailability,
  withAccountEnablement,
} from "../domain/quota-recovery.ts";
import {
  PLATFORM_KIND_LABELS,
  formatQuotaAmount,
  platformCredentialTags,
  platformKeyGroupLabel,
  platformKeyQuotaName,
  platformModelOverlay,
  platformWalletMeter,
  primaryQuota,
  uniquePublicModelCount,
} from "../domain/platform-accounts.ts";
import {
  PAY_GO_METER_EMPTY,
  PAY_GO_METER_LABEL_KEYS,
  formatPayGoObservedAt,
} from "../domain/pay-go-meter.ts";
import { locale, t } from "../i18n/index.ts";
import {
  accountTypeLabelText,
  cardQuotaAvailabilityText,
  cpaCardStatusText,
  platformCredentialTagText,
} from "../views/account-status-text.ts";
import AccountCardFrame, { type AccountCardTone } from "./AccountCardFrame.vue";
import ApiPriceMeter, { type ApiPriceMeterCell } from "./ApiPriceMeter.vue";
import type { CredentialFigure } from "./CredentialBody.vue";
import PlatformPriceTable from "./PlatformPriceTable.vue";

const props = defineProps<{
  group: DestinationGroup;
  /** Saved Keys on this card, unfiltered. */
  membership: readonly DestinationCredential[];
  parent: PlatformAccount | null;
  accountsById: ReadonlyMap<string, Account>;
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
  arrangingDisabled?: boolean;
  canRemoveEmptyCard?: boolean;
  cpaStatus?: CpaCardStatus | null;
}>();

const emit = defineEmits<{
  "order-keydown": [event: KeyboardEvent];
  "order-drag-start": [event: PointerEvent];
  "refresh-parent": [];
  edit: [];
  delete: [];
  "add-key": [];
  "add-card": [];
  "remove-empty-card": [];
  "import-keys": [];
  "link-existing": [];
  "retry-pending-link": [];
  "fetch-all-models": [];
}>();

const overlayAccounts = computed(() => (
  props.group.credentials
    .map((credential) => overlayAccountForCredential(credential, props.accountsById))
    .filter((account): account is Account => Boolean(account))
));
const firstAccount = computed(() => overlayAccounts.value[0] ?? null);
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
const membershipGroup = computed((): DestinationGroup => ({
  ...props.group,
  credentials: [...props.membership],
}));
const availabilityMembership = computed(() => (
  props.membership.map((credential) => withAccountEnablement(
    credential,
    overlayAccountForCredential(credential, props.accountsById)?.enabled,
  ))
));
const cardAvailability = computed(() => (
  cardQuotaAvailability(availabilityMembership.value, props.group.destination, props.now)
));
const cardTone = computed<AccountCardTone>(() => {
  if (cpaCardProcessDown(props.cpaStatus)) return "unavailable";
  return cardAvailability.value === "available" ? null : "unavailable";
});
const cardAvailabilityLabel = computed(() => {
  const kind = cardAvailability.value;
  if (kind === "available" || kind === "no_keys") return "";
  if (isSingleAccountGroup(membershipGroup.value)) return "";
  return cardQuotaAvailabilityText(kind);
});
const cpaStatusLabel = computed(() => (
  props.cpaStatus ? cpaCardStatusText(props.cpaStatus) : ""
));
const cpaStatusType = computed(() => (
  props.cpaStatus ? cpaCardStatusTagType(props.cpaStatus) : "default"
));
const refreshingParent = computed(() => (
  props.parent ? Boolean(props.refreshing[props.parent.id]) : false
));
const overlay = computed(() => platformModelOverlay(overlayAccounts.value));
const duplicateNames = computed(() => {
  const counts = new Map<string, number>();
  for (const credential of props.group.credentials) {
    const name = credential.name.trim();
    if (!name) continue;
    counts.set(name, (counts.get(name) ?? 0) + 1);
  }
  return new Set([...counts.entries()].filter(([, count]) => count > 1).map(([name]) => name));
});

const parentMenuOptions = computed(() => {
  const arrangement = [
    { label: t("再建一张卡片"), key: "add-card", disabled: props.arrangingDisabled },
    ...(props.canRemoveEmptyCard ? [{ label: t("删除空卡片"), key: "remove-empty-card", disabled: props.arrangingDisabled }] : []),
  ];
  if (!props.parent) return arrangement;
  return [
    ...arrangement,
    { label: t("获取全部模型"), key: "fetch-all-models", disabled: props.mutating || props.group.credentials.length === 0 },
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
      label: props.links.length > 0
        ? t("已关联 {count} 个 Key，先取消关联后再删除", { count: props.links.length })
        : t("删除"),
      key: "delete",
      disabled: props.mutating || props.links.length > 0,
    },
  ];
});

function handleParentMenuSelect(key: string | number) {
  if (key === "add-card" || key === "remove-empty-card") {
    if (!props.arrangingDisabled) {
      if (key === "add-card") emit("add-card");
      else emit("remove-empty-card");
    }
    return;
  }
  if (key === "fetch-all-models") emit("fetch-all-models");
  else if (key === "import-keys") emit("import-keys");
  else if (key === "link-existing") emit("link-existing");
  else if (key === "edit") emit("edit");
  else if (key === "delete") emit("delete");
}

function overlayId(credential: DestinationCredential): string {
  return credential.legacy_account_id || credential.id;
}

function keySummary(credential: DestinationCredential) {
  const accountId = overlayId(credential);
  return overlay.value.keys.find((row) => row.accountId === accountId);
}

function keyGroup(credential: DestinationCredential): string {
  const link = linkOf(credential);
  return platformKeyGroupLabel(link, link?.snapshot);
}

function keyTokenName(credential: DestinationCredential): string {
  return platformKeyQuotaName(linkOf(credential)?.snapshot) ?? "";
}

function linkOf(credential: DestinationCredential): PlatformLink | undefined {
  const accountId = overlayId(credential);
  return props.links.find((link) => (
    link.accountId === accountId || link.accountId === credential.id
  ));
}

const walletMeter = computed(() => platformWalletMeter(props.parent?.snapshot));
const walletMeterCaption = computed(() => {
  const observedAt = walletMeter.value?.observedAt;
  return observedAt ? formatPayGoObservedAt(observedAt, locale.value) : "";
});
const walletMeterCells = computed<ApiPriceMeterCell[]>(() => {
  const meter = walletMeter.value;
  if (!meter) return [];
  const money = (value: number) => formatQuotaAmount(value, meter.unit, locale.value);
  return [
    {
      key: "remaining",
      label: t(PAY_GO_METER_LABEL_KEYS.remaining),
      value: meter.remainingUnlimited
        ? t("不限")
        : meter.remaining == null ? PAY_GO_METER_EMPTY : money(meter.remaining),
    },
    {
      key: "month",
      label: t(PAY_GO_METER_LABEL_KEYS.month),
      value: meter.monthUsed == null ? PAY_GO_METER_EMPTY : money(meter.monthUsed),
    },
    {
      key: "history",
      label: t(PAY_GO_METER_LABEL_KEYS.history),
      value: meter.historyUsed == null ? PAY_GO_METER_EMPTY : money(meter.historyUsed),
    },
  ];
});

function extraTagsFor(credential: DestinationCredential): string[] {
  if (!props.parent) return [];
  const overlayAccount = overlayAccountForCredential(credential, props.accountsById);
  return platformCredentialTags({
    group: keyGroup(credential),
    tokenName: keyTokenName(credential),
    accountName: overlayAccount?.name.trim() || credential.name.trim(),
    modelCount: modelCountFor(credential) ?? 0,
  }).filter((tag) => tag.kind !== "models").map(platformCredentialTagText);
}

function modelCountFor(credential: DestinationCredential): number | null {
  if (!props.parent) return null;
  const overlayAccount = overlayAccountForCredential(credential, props.accountsById);
  return keySummary(credential)?.total ?? uniquePublicModelCount(overlayAccount);
}

function figureFor(credential: DestinationCredential): CredentialFigure | null {
  if (!props.parent) return null;
  const quota = primaryQuota(linkOf(credential)?.snapshot?.quotas ?? [], "key_limit");
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
