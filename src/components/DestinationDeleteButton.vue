<template>
  <n-popconfirm
    v-if="deletable"
    :positive-text="t('删除')"
    :negative-text="t('取消')"
    :disabled="disabled || deleting"
    @positive-click="onConfirm"
  >
    <template #trigger>
      <n-button type="error" secondary :size="size" :disabled="disabled || deleting" :loading="deleting">
        {{ t("删除连接") }}
      </n-button>
    </template>
    {{ t("删除后无法恢复，确认删除该连接？") }}
  </n-popconfirm>
  <n-tooltip v-else :disabled="disabled">
    <template #trigger>
      <n-button type="error" secondary :size="size" disabled>
        {{ t("删除连接") }}
      </n-button>
    </template>
    {{ t("仍有 Key 使用此连接，无法删除") }}
  </n-tooltip>
</template>

<script setup lang="ts">
import { computed, ref } from "vue";
import { NButton, NPopconfirm, NTooltip, useMessage } from "naive-ui";
import type { Destination } from "../api/destinations.ts";
import { useDestinationsStore } from "../stores/destinations.ts";
import { isDestinationDeletable } from "../domain/destination-edit.ts";
import { t } from "../i18n/index.ts";
import { dashboardErrorDetail } from "../utils/errors.ts";

const props = defineProps<{
  destination: Destination;
  /** Host-level action lock (unrelated mutations in flight). */
  disabled?: boolean;
  size?: "tiny" | "small" | "medium" | "large";
}>();

const emit = defineEmits<{
  (event: "deleted", destinationId: string): void;
}>();

const message = useMessage();
const destinationsStore = useDestinationsStore();
const deleting = ref(false);

/** Editable destinations only render this control; Keys block the delete. */
const deletable = computed(() => (
  isDestinationDeletable(props.destination, destinationsStore.credentials)
));

async function onConfirm(): Promise<void> {
  if (deleting.value || !deletable.value) return;
  deleting.value = true;
  try {
    await destinationsStore.deleteDestination(props.destination.id);
    message.success(t("连接已删除"));
    emit("deleted", props.destination.id);
  } catch (error) {
    message.error(t("删除失败：{error}", { error: dashboardErrorDetail(error) }));
  } finally {
    deleting.value = false;
  }
}
</script>
