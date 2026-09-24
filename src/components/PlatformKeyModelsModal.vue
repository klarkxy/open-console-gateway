<template>
  <n-modal
    :show="show"
    preset="card"
    :title="account?.name ?? t('模型')"
    class="platform-key-models-modal"
    style="width: 640px; max-width: calc(100vw - 32px)"
    :mask-closable="!busy"
    :close-on-esc="!busy"
    @update:show="emit('update:show', $event)"
  >
    <n-empty v-if="rows.length === 0" :description="t('该 Key 暂无候选模型，需先刷新平台快照。')" />
    <div v-else class="platform-key-models-table-wrap">
      <table class="platform-key-models-table">
        <thead>
          <tr>
            <th>{{ t("对外模型名") }}</th>
            <th>{{ t("上游模型 ID") }}</th>
          </tr>
        </thead>
        <tbody>
          <tr v-for="row in rows" :key="row.public_model">
            <td><code>{{ row.public_model }}</code></td>
            <td><code>{{ row.upstream_model }}</code></td>
          </tr>
        </tbody>
      </table>
    </div>
    <template #footer>
      <n-space justify="end">
        <n-button :disabled="busy" @click="emit('update:show', false)">{{ t("取消") }}</n-button>
        <n-button type="primary" :loading="busy" :disabled="!account" @click="emit('fetch')">
          {{ t("获取模型") }}
        </n-button>
      </n-space>
    </template>
  </n-modal>
</template>

<script setup lang="ts">
import { computed } from "vue";
import { NButton, NEmpty, NModal, NSpace } from "naive-ui";
import type { Account } from "../api/dashboard.ts";
import { platformKeyModelRows } from "../domain/platform-accounts.ts";
import { t } from "../i18n/index.ts";

const props = defineProps<{
  show: boolean;
  account: Account | null;
  busy: boolean;
}>();

const emit = defineEmits<{
  "update:show": [show: boolean];
  fetch: [];
}>();

const rows = computed(() => platformKeyModelRows(props.account));
</script>

<style scoped>
.platform-key-models-table-wrap {
  overflow-x: auto;
}
.platform-key-models-table {
  width: 100%;
  min-width: 420px;
  border-collapse: collapse;
  font-size: var(--ocg-font-sm);
}
.platform-key-models-table th,
.platform-key-models-table td {
  padding: 8px 10px;
  text-align: left;
  border-bottom: 1px solid var(--ocg-border);
  vertical-align: top;
}
.platform-key-models-table th {
  color: var(--ocg-muted);
  font-weight: 600;
}
</style>
