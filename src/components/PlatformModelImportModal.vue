<template>
  <n-modal
    :show="show"
    preset="card"
    :title="t('导入模型到 Key')"
    class="platform-model-import-modal"
    style="width: 640px; max-width: calc(100vw - 32px)"
    :mask-closable="false"
    :close-on-esc="!busy"
    @update:show="setVisible"
  >
    <n-alert type="info" :show-icon="false" style="margin-bottom: 12px">
      {{ t("分组归属不代表该 Key 拥有对应模型的调用权限。") }}
    </n-alert>
    <n-empty
      v-if="candidates.length === 0"
      :description="t('该 Key 暂无候选模型，需先刷新平台快照。')"
    />
    <n-checkbox-group v-else v-model:value="selectedIds">
      <div class="platform-import-list">
        <div
          v-for="candidate in candidates"
          :key="candidate.id"
          class="platform-import-row"
          :class="{ 'is-mapped': candidate.alreadyMapped }"
        >
          <n-checkbox
            :value="candidate.id"
            :disabled="busy || candidate.alreadyMapped"
            :label="candidate.id"
            class="platform-import-check mono"
          />
          <n-tag v-if="candidate.platform" size="small" :bordered="false">{{ candidate.platform }}</n-tag>
          <n-tag v-if="candidate.groupId" size="small" :bordered="false">{{ candidate.groupId }}</n-tag>
          <n-tag v-if="candidate.alreadyMapped" size="small" type="success" :bordered="false">
            {{ t("已存在") }}
          </n-tag>
        </div>
      </div>
    </n-checkbox-group>
    <template #footer>
      <n-space justify="end">
        <n-button :disabled="busy" @click="setVisible(false)">{{ t("取消") }}</n-button>
        <n-button
          type="primary"
          :loading="busy"
          :disabled="selectedIds.length === 0"
          @click="submit"
        >{{ t("确认导入") }}</n-button>
      </n-space>
    </template>
  </n-modal>
</template>

<script setup lang="ts">
import { computed, ref, watch } from "vue";
import {
  NAlert,
  NButton,
  NCheckbox,
  NCheckboxGroup,
  NEmpty,
  NModal,
  NSpace,
  NTag,
} from "naive-ui";
import type { Account } from "../api/dashboard.ts";
import type { PlatformLink } from "../api/platform-accounts.ts";
import { platformModelCandidates } from "../domain/platform-accounts.ts";
import { t } from "../i18n/index.ts";
import { useLocalizedModalCloseLabel } from "../utils/modal-close-label.ts";

const props = defineProps<{
  show: boolean;
  account: Account | null;
  link: PlatformLink | null;
  busy: boolean;
}>();

const emit = defineEmits<{
  "update:show": [show: boolean];
  submit: [modelIds: string[]];
}>();

useLocalizedModalCloseLabel(computed(() => props.show), "platform-model-import-modal");

const selectedIds = ref<string[]>([]);

const candidates = computed(() => platformModelCandidates(
  props.link?.snapshot ?? null,
  props.account?.model_capabilities ?? [],
));

watch(() => props.show, (show) => {
  if (show) selectedIds.value = [];
});

function setVisible(show: boolean): void {
  if (!show && props.busy) return;
  emit("update:show", show);
}

function submit(): void {
  if (selectedIds.value.length === 0 || props.busy) return;
  emit("submit", [...selectedIds.value]);
}
</script>

<style scoped>
.platform-import-list {
  display: grid;
  gap: var(--ocg-space-sm);
  max-height: 50vh;
  overflow-y: auto;
}

.platform-import-row {
  display: flex;
  flex-wrap: wrap;
  align-items: center;
  gap: var(--ocg-space-sm);
  padding: 6px var(--ocg-space-sm);
  border: 1px solid var(--ocg-border);
  border-radius: var(--ocg-radius-sm);
}

.platform-import-row.is-mapped {
  opacity: 0.65;
}
</style>
