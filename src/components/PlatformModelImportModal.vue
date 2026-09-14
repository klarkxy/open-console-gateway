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
      {{ t("仅导入勾选的候选；价格刷新不会自动添加模型。候选不代表该 Key 拥有调用权限。") }}
    </n-alert>
    <n-empty
      v-if="candidates.length === 0"
      :description="t('该 Key 暂无候选模型，请先刷新平台快照。')"
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
          <span v-if="candidate.price" class="platform-import-price mono">
            <template v-if="candidate.price.unavailableReason">
              {{ t("不可用：{reason}", { reason: reasonText(candidate.price.unavailableReason) }) }}
            </template>
            <template v-else>
              {{ priceSummary(candidate.price) }}
            </template>
          </span>
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
import type { PlatformLink, PlatformPrice } from "../api/platform-accounts.ts";
import {
  formatPlatformRate,
  platformModelCandidates,
  platformUnavailableReasonKey,
} from "../domain/platform-accounts.ts";
import { locale, t, type MessageKey } from "../i18n/index.ts";
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

function reasonText(reason: string): string {
  const key = platformUnavailableReasonKey(reason);
  return key ? t(key as MessageKey) : reason;
}

function priceSummary(price: PlatformPrice): string {
  const input = formatPlatformRate(price.input, price.currency, locale.value);
  const output = formatPlatformRate(price.output, price.currency, locale.value);
  const parts = [
    input ? `${t("输入")} ${input.label}` : null,
    output ? `${t("输出")} ${output.label}` : null,
  ].filter((part) => part !== null);
  return parts.length ? parts.join(" / ") : t("未知");
}

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
  gap: 8px;
  max-height: 50vh;
  overflow-y: auto;
}

.platform-import-row {
  display: flex;
  flex-wrap: wrap;
  align-items: center;
  gap: 8px;
  padding: 6px 8px;
  border: 1px solid var(--ocg-border);
  border-radius: 6px;
}

.platform-import-row.is-mapped {
  opacity: 0.65;
}

.platform-import-price {
  margin-left: auto;
  font-size: var(--ocg-font-xs);
  color: var(--ocg-muted);
}
</style>
