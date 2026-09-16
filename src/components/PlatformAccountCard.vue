<template>
  <n-card
    :data-account-id="routeId"
    size="small"
    class="account-card platform-account-card"
    :class="{ 'account-card--dragging': dragging }"
  >
    <template #header>
      <div class="account-title">
        <n-tooltip trigger="hover">
          <template #trigger>
            <n-button
              circle
              quaternary
              size="small"
              class="account-order-handle"
              :class="{ 'account-order-handle--dragging': dragging }"
              :disabled="orderHandleDisabled"
              :aria-label="t('拖动调整账号 {name} 的优先级', { name: parent.name })"
              aria-describedby="account-order-instructions"
              @click.prevent
              @keydown="emit('order-keydown', $event)"
              @pointerdown="emit('order-drag-start', $event)"
            >
              <template #icon><n-icon :component="HolderOutlined" /></template>
            </n-button>
          </template>
          {{ orderHandleDisabled
            ? t("添加 Key 后可拖动此账号调整路由顺序")
            : t("拖动调整账号 {name} 的优先级", { name: parent.name }) }}
        </n-tooltip>
        <div class="account-heading">
          <div class="account-name-row">
            <span class="account-name">{{ parent.name }}</span>
            <n-tag size="small" :bordered="false">{{ kindLabel }}</n-tag>
            <n-tag size="small" :bordered="false">
              {{ t("{count} 个 Key", { count: keys.length }) }}
            </n-tag>
            <n-tag v-if="parentBalanceText" size="small" type="success" :bordered="false">
              {{ t("当前余额 {value}", { value: parentBalanceText }) }}
            </n-tag>
            <n-tag v-if="parent.snapshot?.stale" size="small" type="warning" :bordered="false">
              {{ t("快照已过期") }}
            </n-tag>
          </div>
          <span class="mono platform-parent-url">{{ parent.baseUrl }}</span>
        </div>
      </div>
    </template>
    <template #header-extra>
      <n-space size="small" @click.stop>
        <n-button
          size="tiny"
          quaternary
          :loading="refreshingParent"
          :disabled="mutating"
          @click="emit('refresh-parent')"
        >{{ t("刷新") }}</n-button>
        <n-button size="tiny" quaternary :disabled="mutating" @click="emit('edit')">
          {{ t("编辑") }}
        </n-button>
        <n-tooltip v-if="keys.length > 0" trigger="hover">
          <template #trigger>
            <span>
              <n-button size="tiny" quaternary disabled>{{ t("删除") }}</n-button>
            </span>
          </template>
          {{ t("已关联 {count} 个 Key，先取消关联后再删除", { count: keys.length }) }}
        </n-tooltip>
        <n-button
          v-else
          size="tiny"
          quaternary
          :disabled="mutating"
          @click="emit('delete')"
        >{{ t("删除") }}</n-button>
      </n-space>
    </template>

    <div class="platform-parent-body">
      <n-alert
        v-if="parent.snapshot && parent.snapshot.errors.length > 0"
        type="warning"
        :show-icon="false"
      >
        {{ t("刷新错误") }}: {{ parent.snapshot.errors.join(", ") }}
      </n-alert>
      <div v-if="parent.snapshot" class="platform-observed">
        {{ t("观测时间：{time}", { time: observedText(parent.snapshot) }) }}
      </div>
      <div v-else class="platform-observed">{{ t("尚无平台数据，点「刷新」获取。") }}</div>
      <p v-if="!parent.hasUserCredential" class="platform-hint">
        {{ t("未填写用户凭证，无法读取账号钱包。") }}
      </p>

      <template v-if="parent.snapshot">
        <div v-for="kind in parentQuotaKinds" :key="kind" class="platform-block">
          <div class="platform-block-title">{{ t(quotaKindKeys[kind] as MessageKey) }}</div>
          <div v-if="quotasOf(parent.snapshot, kind).length === 0" class="platform-unknown">{{ t("未知") }}</div>
          <div v-for="(quota, index) in quotasOf(parent.snapshot, kind)" :key="index" class="quota-row">
            <span class="quota-values">
              <template v-if="quota.unlimited">{{ t("不限") }}</template>
              <template v-else>
                {{ t("已用 {value}", { value: quotaAmount(quota.used, quota.unit) }) }}
                · {{ t("剩余 {value}", { value: quotaAmount(quota.remaining, quota.unit) }) }}
                · {{ t("限额 {value}", { value: quotaAmount(quota.limit, quota.unit) }) }}
              </template>
            </span>
          </div>
        </div>
        <PlatformPriceTable :snapshot="parent.snapshot" />
      </template>

      <div class="platform-block">
        <div class="platform-children-head">
          <span class="platform-block-title">{{ t("已关联 Key（{count}）", { count: keys.length }) }}</span>
          <n-space size="small">
            <n-button
              size="tiny"
              secondary
              :disabled="mutating || keys.length === 0"
              @click="emit('fetch-all-models')"
            >{{ t("获取全部模型") }}</n-button>
            <n-button size="tiny" secondary :disabled="mutating" @click="emit('add-key')">
              {{ t("添加 Key") }}
            </n-button>
            <n-button size="tiny" secondary :disabled="mutating" @click="emit('link-existing')">
              {{ t("关联已有 Key") }}
            </n-button>
          </n-space>
        </div>
        <p v-if="overlay.uniqueIds.length > 0" class="platform-hint">
          {{ overlay.sharedIds.length > 0
            ? t("共 {unique} 个模型，{shared} 个由多把 Key 提供。", { unique: overlay.uniqueIds.length, shared: overlay.sharedIds.length })
            : t("共 {unique} 个模型。", { unique: overlay.uniqueIds.length }) }}
        </p>
        <n-alert
          v-if="pendingLink && pendingLink.parentId === parent.id"
          type="warning"
          :show-icon="false"
        >
          <div class="platform-pending-link">
            <span>{{ t("Key 已创建，关联尚未完成。") }}</span>
            <n-button size="tiny" secondary :loading="mutating" :disabled="mutating" @click="emit('retry-pending-link')">
              {{ t("重试关联") }}
            </n-button>
          </div>
        </n-alert>
        <div v-if="keys.length === 0 && pendingLink?.parentId !== parent.id" class="platform-hint">
          {{ t("尚无关联 Key。") }}
        </div>
        <div v-for="(account, index) in keys" :key="account.id" class="platform-child">
          <div class="platform-child-head">
            <n-switch
              size="small"
              :value="account.enabled"
              :disabled="mutating || !account.plan_routable"
              :aria-label="account.enabled ? t('禁用账号 {name}', { name: account.name }) : t('启用账号 {name}', { name: account.name })"
              @update:value="emit('toggle-key', account.id)"
            />
            <span class="platform-child-name">{{ account.name }}</span>
            <n-tag v-if="duplicateNames.has(account.name.trim())" size="small" type="warning" :bordered="false">
              {{ t("名称重复") }}
            </n-tag>
            <n-tag v-if="keyGroup(account.id)" size="small" :bordered="false">
              {{ keyGroup(account.id) }}
            </n-tag>
            <n-tag v-if="keyTokenName(account.id)" size="small" :bordered="false">
              {{ keyTokenName(account.id) }}
            </n-tag>
            <n-tag v-if="keyBalanceText(account.id)" size="small" type="success" :bordered="false">
              {{ t("当前余额 {value}", { value: keyBalanceText(account.id) }) }}
            </n-tag>
            <n-tag size="small" :bordered="false">
              {{ t("{count} 个模型", { count: keySummary(account.id)?.total ?? account.model_capabilities.length }) }}
            </n-tag>
            <n-tag v-if="(keySummary(account.id)?.shared ?? 0) > 0" size="small" type="info" :bordered="false">
              {{ t("与其他 Key 共用 {count} 个", { count: keySummary(account.id)!.shared }) }}
            </n-tag>
            <n-tag v-if="(keySummary(account.id)?.exclusive ?? 0) > 0 && overlay.keys.length > 1" size="small" :bordered="false">
              {{ t("独有 {count} 个", { count: keySummary(account.id)!.exclusive }) }}
            </n-tag>
            <n-space size="small" class="platform-child-actions">
              <n-button
                size="tiny"
                quaternary
                :disabled="mutating || index === 0"
                :aria-label="t('上移')"
                @click="emit('move-key', account.id, -1)"
              >{{ t("上移") }}</n-button>
              <n-button
                size="tiny"
                quaternary
                :disabled="mutating || index === keys.length - 1"
                :aria-label="t('下移')"
                @click="emit('move-key', account.id, 1)"
              >{{ t("下移") }}</n-button>
              <n-button
                size="tiny"
                quaternary
                :loading="!!refreshing[`${parent.id}:${account.id}`]"
                :disabled="mutating"
                @click="emit('refresh-child', account.id)"
              >{{ t("刷新") }}</n-button>
              <n-button size="tiny" quaternary :disabled="mutating" @click="emit('fetch-models', account.id)">
                {{ t("获取模型") }}
              </n-button>
              <n-button size="tiny" quaternary :disabled="mutating" @click="emit('edit-key', account.id)">
                {{ t("编辑") }}
              </n-button>
              <n-button size="tiny" quaternary :disabled="mutating" @click="emit('unlink', account.id)">
                {{ t("取消关联") }}
              </n-button>
            </n-space>
          </div>
          <div v-if="linkOf(account.id)?.snapshot" class="platform-hint">
            {{ t("观测时间：{time}", { time: observedText(linkOf(account.id)!.snapshot!) }) }}
          </div>
          <div v-else class="platform-hint">{{ t("尚无快照，点「刷新」获取。") }}</div>
          <template v-if="linkOf(account.id)?.snapshot">
            <div class="platform-block">
              <div class="platform-block-title">{{ t(quotaKindKeys.key_limit as MessageKey) }}</div>
              <div v-if="quotasOf(linkOf(account.id)!.snapshot!, 'key_limit').length === 0" class="platform-unknown">
                {{ t("未知") }}
              </div>
              <div
                v-for="(quota, index) in quotasOf(linkOf(account.id)!.snapshot!, 'key_limit')"
                :key="index"
                class="quota-row"
              >
                <span class="quota-values">
                  <template v-if="quota.unlimited">{{ t("不限") }}</template>
                  <template v-else>
                    {{ t("已用 {value}", { value: quotaAmount(quota.used, quota.unit) }) }}
                    · {{ t("剩余 {value}", { value: quotaAmount(quota.remaining, quota.unit) }) }}
                    · {{ t("限额 {value}", { value: quotaAmount(quota.limit, quota.unit) }) }}
                  </template>
                </span>
              </div>
            </div>
          </template>
        </div>
      </div>
    </div>
  </n-card>
</template>

<script setup lang="ts">
import { computed } from "vue";
import {
  NAlert,
  NButton,
  NCard,
  NIcon,
  NSpace,
  NSwitch,
  NTag,
  NTooltip,
} from "naive-ui";
import { HolderOutlined } from "@vicons/antd";
import type { Account } from "../api/dashboard.ts";
import type {
  PlatformAccount,
  PlatformLink,
  PlatformQuotaKind,
  PlatformSnapshot,
} from "../api/platform-accounts.ts";
import {
  PLATFORM_KIND_LABELS,
  PLATFORM_QUOTA_KIND_KEYS,
  formatPlatformTime,
  formatQuotaAmount,
  platformKeyGroupLabel,
  platformKeyQuotaName,
  platformModelOverlay,
  platformRouteItemId,
  primaryQuota,
  quotasByKind,
} from "../domain/platform-accounts.ts";
import { locale, t, type MessageKey } from "../i18n/index.ts";
import PlatformPriceTable from "./PlatformPriceTable.vue";

const props = defineProps<{
  parent: PlatformAccount;
  keys: Account[];
  links: PlatformLink[];
  mutating: boolean;
  refreshing: Record<string, boolean>;
  pendingLink: { accountId: string; parentId: string } | null;
  orderHandleDisabled: boolean;
  dragging: boolean;
}>();

const emit = defineEmits<{
  "order-keydown": [event: KeyboardEvent];
  "order-drag-start": [event: PointerEvent];
  "refresh-parent": [];
  edit: [];
  delete: [];
  "add-key": [];
  "link-existing": [];
  "retry-pending-link": [];
  "toggle-key": [accountId: string];
  "move-key": [accountId: string, delta: number];
  "refresh-child": [accountId: string];
  "fetch-models": [accountId: string];
  "edit-key": [accountId: string];
  unlink: [accountId: string];
  "fetch-all-models": [];
}>();

const parentQuotaKinds: readonly PlatformQuotaKind[] = ["wallet", "subscription"];
const quotaKindKeys = PLATFORM_QUOTA_KIND_KEYS;
const kindLabel = computed(() => PLATFORM_KIND_LABELS[props.parent.kind]);
const routeId = computed(() => platformRouteItemId(props.parent.id));
const refreshingParent = computed(() => Boolean(props.refreshing[props.parent.id]));
const overlay = computed(() => platformModelOverlay(props.keys));
const duplicateNames = computed(() => {
  const counts = new Map<string, number>();
  for (const account of props.keys) {
    const name = account.name.trim();
    if (!name) continue;
    counts.set(name, (counts.get(name) ?? 0) + 1);
  }
  return new Set([...counts.entries()].filter(([, count]) => count > 1).map(([name]) => name));
});

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

function linkOf(accountId: string): PlatformLink | undefined {
  return props.links.find((link) => link.accountId === accountId);
}

function quotasOf(snapshot: PlatformSnapshot, kind: PlatformQuotaKind) {
  return quotasByKind(snapshot.quotas)[kind];
}

function quotaAmount(value: number | null, unit: string): string {
  return value === null ? t("未知") : formatQuotaAmount(value, unit, locale.value);
}

function remainingText(snapshot: PlatformSnapshot | null | undefined, kind: PlatformQuotaKind): string {
  const quota = primaryQuota(snapshot?.quotas ?? [], kind);
  if (!quota) return "";
  if (quota.unlimited) return t("不限");
  if (quota.remaining === null) return "";
  return formatQuotaAmount(quota.remaining, quota.unit, locale.value);
}

const parentBalanceText = computed(() => remainingText(props.parent.snapshot, "wallet"));

function keyBalanceText(accountId: string): string {
  return remainingText(linkOf(accountId)?.snapshot, "key_limit");
}

function observedText(snapshot: PlatformSnapshot): string {
  return formatPlatformTime(snapshot.observedAt, locale.value) || t("未知");
}
</script>

<style scoped>
.account-title {
  display: flex;
  align-items: flex-start;
  gap: var(--ocg-space-sm);
  min-width: 0;
}

.account-heading {
  display: grid;
  gap: 2px;
  min-width: 0;
}

.account-name-row {
  display: flex;
  flex-wrap: wrap;
  align-items: center;
  gap: var(--ocg-space-sm);
}

.account-name {
  font-weight: 600;
}

.platform-parent-url {
  font-size: var(--ocg-font-xs);
  color: var(--ocg-subtle);
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
  max-width: 48ch;
}

.platform-parent-body {
  display: grid;
  gap: var(--ocg-space-md);
}

.platform-block {
  display: grid;
  gap: 6px;
}

.platform-block-title {
  font-size: var(--ocg-font-xs);
  font-weight: 600;
  color: var(--ocg-muted);
}

.platform-observed,
.platform-hint {
  font-size: var(--ocg-font-xs);
  color: var(--ocg-subtle);
}

.platform-unknown {
  font-size: var(--ocg-font-sm);
  color: var(--ocg-subtle);
}

.quota-row {
  font-size: var(--ocg-font-sm);
}

.platform-children-head {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: var(--ocg-space-sm);
}

.platform-pending-link {
  display: flex;
  flex-wrap: wrap;
  align-items: center;
  justify-content: space-between;
  gap: var(--ocg-space-sm);
}

.platform-child {
  display: grid;
  gap: var(--ocg-space-xs);
  padding: var(--ocg-space-sm);
  border: 1px solid var(--ocg-border);
  border-radius: var(--ocg-radius-md);
}

.platform-child-head {
  display: flex;
  flex-wrap: wrap;
  align-items: center;
  gap: var(--ocg-space-sm);
}

.platform-child-name {
  font-weight: 600;
}

.platform-child-actions {
  margin-left: auto;
}

.account-order-handle--dragging {
  cursor: grabbing;
}
</style>
