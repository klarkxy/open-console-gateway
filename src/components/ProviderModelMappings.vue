<template>
  <div class="model-mappings">
    <div class="model-mappings-head">
      <p class="model-mappings-hint">{{ t("模型默认跟随供应商的协议与地址；仅当某个模型需要不同上游时才覆盖，鉴权始终使用供应商的 Key。") }}</p>
      <div class="model-mappings-actions">
        <n-button v-if="refreshable" type="primary" size="small" :loading="refreshing" :disabled="disabled" @click="$emit('refresh')">
          {{ refreshing ? t("正在刷新模型目录…") : t("刷新模型目录") }}
        </n-button>
        <n-button v-if="editable" secondary size="small" :disabled="disabled" @click="$emit('edit')">
          {{ t("编辑") }}
        </n-button>
      </div>
    </div>
    <n-alert v-if="refreshError" type="error" :title="t('刷新模型目录失败：{error}', { error: refreshError })" />
    <div class="model-mappings-table-wrap">
      <table class="model-mappings-table">
        <thead>
          <tr>
            <th>{{ t("对外模型名") }}</th>
            <th>{{ t("上游模型 ID") }}</th>
            <th>{{ t("上游连接") }}</th>
          </tr>
        </thead>
        <tbody>
          <tr v-for="model in models" :key="model.public_model">
            <td>
              <code>{{ model.public_model }}</code>
              <n-tag v-if="disabledModels.has(model.public_model.toLowerCase())" class="model-state" size="small" :bordered="false">{{ t("已停用") }}</n-tag>
            </td>
            <td><code>{{ model.upstream_model }}</code></td>
            <td>
              <template v-if="model.upstream_override">
                {{ protocolDisplayName(model.upstream_override.protocol) }} · <code>{{ model.upstream_override.endpoint_url }}</code>
              </template>
              <span v-else class="model-mappings-inherit">{{ t("跟随供应商默认") }}</span>
            </td>
          </tr>
        </tbody>
      </table>
    </div>
  </div>
</template>

<script setup lang="ts">
import { NAlert, NButton, NTag } from "naive-ui";
import type { ProviderDefinitionModelView } from "../api/providers.ts";
import { t } from "../i18n/index.ts";
import { protocolDisplayName } from "../domain/provider-contracts.ts";

defineProps<{
  models: ProviderDefinitionModelView[];
  editable: boolean;
  disabledModels: ReadonlySet<string>;
  refreshable?: boolean;
  refreshing?: boolean;
  disabled?: boolean;
  refreshError?: string;
}>();

defineEmits<{
  (event: "edit"): void;
  (event: "refresh"): void;
}>();
</script>

<style scoped>
.model-mappings {
  min-width: 0;
}
.model-mappings-head {
  display: flex;
  align-items: flex-start;
  justify-content: space-between;
  gap: var(--ocg-space-md);
  margin-bottom: var(--ocg-space-md);
}
.model-mappings-hint {
  margin: 0;
  color: var(--ocg-muted);
  font-size: var(--ocg-font-xs);
}
.model-mappings-actions {
  display: flex;
  flex-wrap: wrap;
  justify-content: flex-end;
  gap: var(--ocg-space-sm);
  flex-shrink: 0;
}
@media (max-width: 640px) {
  .model-mappings-head { flex-direction: column; }
}
.model-mappings-table-wrap {
  overflow-x: auto;
}
.model-mappings-table {
  width: 100%;
  min-width: 560px;
  border-collapse: collapse;
  font-size: var(--ocg-font-sm);
}
.model-mappings-table th,
.model-mappings-table td {
  padding: 10px var(--ocg-space-md);
  border-bottom: 1px solid var(--ocg-border);
  text-align: left;
  vertical-align: middle;
}
.model-mappings-table th {
  color: var(--ocg-muted);
  font-size: var(--ocg-font-xs);
  font-weight: 600;
}
.model-mappings-table td code {
  overflow-wrap: anywhere;
}
.model-state {
  margin-inline-start: var(--ocg-space-sm);
}
.model-mappings-inherit {
  color: var(--ocg-muted);
}
</style>
