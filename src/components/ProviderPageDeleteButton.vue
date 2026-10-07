<template>
  <n-popconfirm v-if="deletable" :positive-text="t('删除')" :negative-text="t('取消')"
    :disabled="disabled || deleting" @positive-click="remove">
    <template #trigger><n-button type="error" secondary :size="size" :disabled="disabled || deleting" :loading="deleting">{{ t('删除连接') }}</n-button></template>
    {{ t('删除后无法恢复，确认删除该连接？') }}
  </n-popconfirm>
  <n-tooltip v-else>
    <template #trigger><n-button type="error" secondary :size="size" disabled>{{ t('删除连接') }}</n-button></template>
    {{ t('仍有 Key 使用此连接，无法删除') }}
  </n-tooltip>
</template>
<script setup lang="ts">
import { ref } from "vue";
import { NButton, NPopconfirm, NTooltip } from "naive-ui";
import { t } from "../i18n/index.ts";
const props = defineProps<{ deletable: boolean; disabled?: boolean; size?: "tiny" | "small" | "medium" | "large"; remove: () => Promise<void> }>();
const deleting = ref(false);
async function remove(): Promise<void> { if (deleting.value || props.disabled || !props.deletable) return; deleting.value = true; try { await props.remove(); } finally { deleting.value = false; } }
</script>
