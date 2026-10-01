<template>
  <div v-if="rows.length > 0" class="platform-block">
    <div class="platform-block-title">{{ t("模型") }}</div>
    <p class="platform-note">{{ t("分组归属不代表该 Key 拥有对应模型的调用权限。") }}</p>
    <table class="platform-table">
      <thead>
        <tr>
          <th>{{ t("模型") }}</th>
          <th>{{ t("分组") }}</th>
          <th>{{ t("来源") }}</th>
        </tr>
      </thead>
      <tbody>
        <tr v-for="row in rows" :key="row.key">
          <td class="mono">{{ row.model }}</td>
          <td>{{ row.groupId ?? t("无分组") }}</td>
          <td>{{ row.source }}</td>
        </tr>
      </tbody>
    </table>
  </div>
</template>

<script setup lang="ts">
import { computed } from "vue";
import type { PlatformSnapshot } from "../api/platform-accounts.ts";
import { t } from "../i18n/index.ts";

const props = defineProps<{ snapshot: PlatformSnapshot }>();

const rows = computed(() => {
  if (props.snapshot.models.length > 0) {
    return props.snapshot.models.map((model) => ({
      key: `model:${model.id}:${model.groupId ?? ""}`,
      model: model.id,
      groupId: model.groupId,
      source: model.source,
    }));
  }
  return props.snapshot.groups.map((group, index) => ({
    key: `group:${group.id ?? index}:${group.platform ?? ""}`,
    model: "—",
    groupId: group.id,
    source: group.platform ?? "",
  }));
});
</script>

<style scoped>
.platform-block {
  display: grid;
  gap: 6px;
}

.platform-block-title {
  font-size: var(--ocg-font-xs);
  font-weight: 600;
  color: var(--ocg-muted);
}

.platform-note {
  margin: 0;
  font-size: var(--ocg-font-xs);
  color: var(--ocg-subtle);
}

.platform-table {
  width: 100%;
  border-collapse: collapse;
  font-size: var(--ocg-font-sm);
}

.platform-table th {
  text-align: left;
  font-size: var(--ocg-font-xs);
  font-weight: 600;
  color: var(--ocg-muted);
  padding: var(--ocg-space-xs) var(--ocg-space-sm);
  border-bottom: 1px solid var(--ocg-border);
}

.platform-table td {
  padding: var(--ocg-space-xs) var(--ocg-space-sm);
  border-bottom: 1px solid var(--ocg-divider);
  vertical-align: top;
}
</style>
