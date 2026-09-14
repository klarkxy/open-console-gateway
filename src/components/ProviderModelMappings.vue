<template>
  <div class="model-mappings">
    <div class="model-mappings-head">
      <p class="model-mappings-hint">{{ t("模型默认跟随供应商的协议与地址；仅当某个模型需要不同上游时才覆盖，鉴权始终使用供应商的 Key。") }}</p>
      <n-button v-if="editable" secondary size="small" @click="$emit('edit')">
        {{ t("编辑") }}
      </n-button>
    </div>
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
            <td><code>{{ model.public_model }}</code></td>
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
import { NButton } from "naive-ui";
import type { ProviderDefinitionModelView } from "../api/providers.ts";
import { t } from "../i18n/index.ts";
import { protocolDisplayName } from "../domain/provider-contracts.ts";

defineProps<{
  models: ProviderDefinitionModelView[];
  editable: boolean;
}>();

defineEmits<{
  (event: "edit"): void;
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
  gap: 12px;
  margin-bottom: 12px;
}
.model-mappings-hint {
  margin: 0;
  color: var(--ocg-muted);
  font-size: var(--ocg-font-xs);
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
  padding: 10px 12px;
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
.model-mappings-inherit {
  color: var(--ocg-muted);
}
</style>
