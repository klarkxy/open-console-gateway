<template>
  <div class="provider-settings">
    <n-spin v-if="definitionLoading" size="small" />
    <template v-else>
      <dl class="provider-settings-facts">
        <div>
          <dt>{{ t("服务类型") }}</dt>
          <dd>{{ entry.offering === "plan" ? "Plan" : "API" }}</dd>
        </div>
        <div v-if="definition?.endpoint_url">
          <dt>{{ t("API 地址") }}</dt>
          <dd><code>{{ definition.endpoint_url }}</code></dd>
        </div>
        <div v-if="definition?.upstream_protocol">
          <dt>{{ t("上游协议") }}</dt>
          <dd>{{ protocolDisplayName(definition.upstream_protocol) }}</dd>
        </div>
        <div v-if="definition?.auth_kind">
          <dt>{{ t("鉴权方式") }}</dt>
          <dd>{{ authDisplayName(definition.auth_kind) }}</dd>
        </div>
        <div v-if="definition?.preset_id">
          <dt>{{ t("来源预设") }}</dt>
          <dd><code>{{ definition.preset_id }}</code></dd>
        </div>
        <template v-if="definition && entry.origin !== 'builtin'">
          <div>
            <dt>{{ t("创建时间") }}</dt>
            <dd>{{ formatDateTime(definition.created_at) }}</dd>
          </div>
          <div>
            <dt>{{ t("更新时间") }}</dt>
            <dd>{{ formatDateTime(definition.updated_at) }}</dd>
          </div>
        </template>
      </dl>

      <p v-if="entry.origin === 'builtin' && entry.provider_id !== 'custom'" class="provider-settings-note">
        {{ t("供应商连接由官方适配器提供。") }}
      </p>
      <p v-if="entry.origin !== 'builtin'" class="provider-settings-note">
        {{ t("该供应商没有价格或官方用量。") }}
      </p>

      <template v-if="entry.provider_id === 'custom' && entry.origin === 'builtin'">
        <p class="provider-settings-note">
          {{ t("模型与 Endpoint 按账号配置；每个 Custom API 账号独立管理自己的连接与映射。") }}
        </p>
        <n-button secondary size="small" @click="$emit('openAccounts')">
          {{ t("打开账号页") }}
        </n-button>
      </template>

      <OpenCodeInviteUrlField v-if="entry.managed_registration && entry.origin === 'builtin'" />

      <div v-if="entry.origin !== 'builtin' && (entry.editable || entry.deletable)" class="provider-settings-actions">
        <n-button v-if="entry.editable" secondary :disabled="actionLocked" @click="$emit('edit')">
          {{ t("编辑供应商") }}
        </n-button>
        <n-popconfirm
          v-if="entry.deletable"
          :positive-text="t('删除')"
          :negative-text="t('取消')"
          @positive-click="$emit('delete')"
        >
          <template #trigger>
            <n-button type="error" secondary :disabled="actionLocked">{{ t("删除供应商") }}</n-button>
          </template>
          {{ t("先删除引用该供应商的账号，再删除供应商；不会级联删除账号。") }}
        </n-popconfirm>
      </div>
    </template>
  </div>
</template>

<script setup lang="ts">
import { NButton, NPopconfirm, NSpin } from "naive-ui";
import type { ProviderCatalogEntry, ProviderDefinitionView } from "../api/providers.ts";
import { t } from "../i18n/index.ts";
import { formatDateTime } from "../utils/format.ts";
import { protocolDisplayName } from "../domain/provider-contracts.ts";
import OpenCodeInviteUrlField from "./OpenCodeInviteUrlField.vue";

defineProps<{
  entry: ProviderCatalogEntry;
  definition: ProviderDefinitionView | null;
  definitionLoading?: boolean;
  actionLocked?: boolean;
}>();

defineEmits<{
  (event: "edit"): void;
  (event: "delete"): void;
  (event: "openAccounts"): void;
}>();

function authDisplayName(kind: string): string {
  if (kind === "none") return t("无鉴权");
  if (kind === "bearer") return "Bearer";
  if (kind === "x-api-key") return "x-api-key";
  if (kind === "api-key") return "api-key";
  return kind;
}
</script>

<style scoped>
.provider-settings {
  display: grid;
  gap: var(--ocg-space-md);
  min-width: 0;
  justify-items: start;
}
.provider-settings-facts {
  display: grid;
  gap: var(--ocg-space-sm) var(--ocg-space-lg);
  margin: 0;
  width: 100%;
  grid-template-columns: repeat(auto-fit, minmax(180px, 1fr));
}
.provider-settings-facts dt {
  color: var(--ocg-muted);
  font-size: var(--ocg-font-xs);
}
.provider-settings-facts dd {
  margin: 0;
}
.provider-settings-facts dd code {
  overflow-wrap: anywhere;
}
.provider-settings-note {
  margin: 0;
  color: var(--ocg-muted);
  font-size: var(--ocg-font-sm);
}
.provider-settings-actions {
  display: flex;
  flex-wrap: wrap;
  gap: var(--ocg-space-sm);
}
.provider-settings :deep(.invite-section) {
  width: 100%;
}
</style>
