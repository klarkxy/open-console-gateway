<template>
  <article class="cpa-key-row">
    <div class="cpa-account-main">
      <div v-if="runtimeKey.protected" class="cpa-account-title">
        <strong class="mono">{{ runtimeKey.hint }}</strong>
        <n-tag type="info" size="small">{{ t("OCG 路由 Key") }}</n-tag>
      </div>
      <strong v-else class="mono">{{ runtimeKey.hint }}</strong>
      <n-tooltip trigger="hover">
        <template #trigger>
          <span class="cpa-muted">{{ t("指纹") }} · {{ runtimeKey.fingerprint.slice(0, 12) }}…</span>
        </template>
        {{ runtimeKey.fingerprint }}
      </n-tooltip>
    </div>
    <n-space v-if="!runtimeKey.protected" wrap>
      <n-button
        size="small"
        secondary
        :disabled="!!keyAction"
        :loading="keyAction === `rotate:${runtimeKey.fingerprint}`"
        @click="emit('rotate', runtimeKey)"
      >{{ t("轮换 Key") }}</n-button>
      <n-button
        size="small"
        type="error"
        secondary
        :disabled="!!keyAction"
        :loading="keyAction === `delete:${runtimeKey.fingerprint}`"
        @click="emit('delete', runtimeKey)"
      >{{ t("删除") }}</n-button>
    </n-space>
    <n-button
      v-else
      size="small"
      secondary
      :disabled="!!keyAction"
      :loading="keyAction === `rotate:${runtimeKey.fingerprint}`"
      @click="emit('rotate', runtimeKey)"
    >{{ t("轮换 OCG 路由 Key") }}</n-button>
  </article>
</template>

<script setup lang="ts">
import { NButton, NSpace, NTag, NTooltip } from "naive-ui";
import type { CpaRuntimeKey } from "../api/generated/dashboard-v3.ts";
import { t } from "../i18n/index.ts";

defineProps<{
  runtimeKey: CpaRuntimeKey;
  keyAction: string;
}>();

const emit = defineEmits<{
  rotate: [key: CpaRuntimeKey];
  delete: [key: CpaRuntimeKey];
}>();
</script>

<style scoped>
.cpa-key-row { display: flex; align-items: center; justify-content: space-between; gap: var(--ocg-space-lg); padding: var(--ocg-space-md); border: 1px solid var(--ocg-divider); border-radius: var(--ocg-radius-md); }
.cpa-account-main { display: grid; gap: var(--ocg-space-xs); min-width: 0; }
.cpa-account-title { display: flex; flex-wrap: wrap; align-items: center; gap: 6px; color: var(--ocg-ink); }
.cpa-muted { overflow-wrap: anywhere; color: var(--ocg-muted); font-size: var(--ocg-font-sm); }
@media (max-width: 760px) {
  .cpa-key-row { align-items: stretch; flex-direction: column; }
}
</style>
