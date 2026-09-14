<template>
  <n-modal
    :show="show"
    preset="card"
    :title="t('关联 Key（手动关联）')"
    class="platform-link-modal"
    style="width: 480px; max-width: calc(100vw - 32px)"
    :mask-closable="false"
    :close-on-esc="!busy"
    @update:show="setVisible"
  >
    <n-alert
      v-if="candidates.length === 0"
      type="info"
      :show-icon="false"
      style="margin-bottom: 12px"
    >
      <div class="platform-link-empty">
        <span>{{ t("没有可关联的 Custom API 账号。") }}</span>
        <n-button size="small" secondary :disabled="busy" @click="emit('addKey')">
          {{ t("添加 Key") }}
        </n-button>
      </div>
    </n-alert>
    <n-form v-else label-placement="top" @submit.prevent="submit">
      <n-form-item :label="t('Custom API 账号')" required>
        <n-select
          v-model:value="selectedAccountId"
          :options="accountOptions"
          :disabled="busy"
          :consistent-menu-width="false"
          :placeholder="t('选择要关联的账号')"
        />
      </n-form-item>
      <n-form-item :label="t('分组')">
        <n-select
          v-model:value="selectedGroupIndex"
          :options="groupOptions"
          :disabled="busy"
          :consistent-menu-width="false"
        />
        <template v-if="!parent?.snapshot" #feedback>
          {{ t("暂无平台快照；可不选分组直接关联，或先刷新平台账号。") }}
        </template>
      </n-form-item>
      <template v-if="selectedGroupIndex === MANUAL_GROUP">
        <n-form-item :label="t('分组 ID')">
          <n-input
            v-model:value="manualGroupId"
            :disabled="busy"
            class="mono"
            maxlength="200"
            show-count
            :placeholder="t('留空表示未知分组')"
            :input-props="{ 'aria-label': t('分组 ID') }"
          />
          <template #feedback>
            {{ t("仅在知晓准确分组 ID 时手动填写；不会从名称、地址或 Key 推断。") }}
          </template>
        </n-form-item>
        <n-form-item :label="t('分组平台（可选）')">
          <n-input
            v-model:value="manualPlatform"
            :disabled="busy"
            maxlength="64"
            show-count
            :input-props="{ 'aria-label': t('分组平台（可选）') }"
          />
        </n-form-item>
      </template>
      <n-alert type="warning" :show-icon="false">
        {{ t("仅从本地已有账号中选择，不会从平台获取任何 Key；分组归属不代表该 Key 拥有对应模型的调用权限。") }}
      </n-alert>
    </n-form>
    <template #footer>
      <n-space justify="end">
        <n-button :disabled="busy" @click="setVisible(false)">{{ t("取消") }}</n-button>
        <n-button
          type="primary"
          :loading="busy"
          :disabled="!selectedAccountId"
          @click="submit"
        >{{ t("关联") }}</n-button>
      </n-space>
    </template>
  </n-modal>
</template>

<script setup lang="ts">
import { computed, ref, watch } from "vue";
import { NAlert, NButton, NForm, NFormItem, NInput, NModal, NSelect, NSpace } from "naive-ui";
import type { Account } from "../api/dashboard.ts";
import type { PlatformAccount } from "../api/platform-accounts.ts";
import { platformGroupLabel, platformManualGroup } from "../domain/platform-accounts.ts";
import { t } from "../i18n/index.ts";
import { useLocalizedModalCloseLabel } from "../utils/modal-close-label.ts";


const props = defineProps<{
  show: boolean;
  parent: PlatformAccount | null;
  /** Unlinked Custom API accounts available for manual association. */
  candidates: Account[];
  busy: boolean;
}>();

const emit = defineEmits<{
  "update:show": [show: boolean];
  submit: [selection: { accountId: string; group: { id: string | null; platform: string | null } }];
  /** Empty-state escape hatch: create a Key for this parent directly. */
  addKey: [];
}>();

useLocalizedModalCloseLabel(computed(() => props.show), "platform-link-modal");

const NO_GROUP = -1;
const MANUAL_GROUP = -2;
const selectedAccountId = ref<string | null>(null);
const selectedGroupIndex = ref<number>(NO_GROUP);
const manualGroupId = ref("");
const manualPlatform = ref("");

const accountOptions = computed(() => props.candidates.map((account) => ({
  value: account.id,
  label: account.custom_config?.endpoint_url
    ? `${account.name} · ${account.custom_config.endpoint_url}`
    : account.name,
})));

const availableGroups = computed(() => props.parent?.snapshot?.groups ?? []);

const groupOptions = computed(() => [
  { value: NO_GROUP, label: t("无分组") },
  ...availableGroups.value.map((group, index) => ({
    value: index,
    label: platformGroupLabel(group) || t("无分组"),
  })),
  { value: MANUAL_GROUP, label: t("手动填写分组") },
]);

watch(() => props.show, (show) => {
  if (!show) return;
  selectedAccountId.value = null;
  selectedGroupIndex.value = NO_GROUP;
  manualGroupId.value = "";
  manualPlatform.value = "";
});

function setVisible(show: boolean): void {
  if (!show && props.busy) return;
  emit("update:show", show);
}

function submit(): void {
  if (!selectedAccountId.value || props.busy) return;
  let picked: { id: string | null; platform: string | null };
  if (selectedGroupIndex.value === MANUAL_GROUP) {
    try {
      picked = platformManualGroup(manualGroupId.value, manualPlatform.value);
    } catch {
      return; // maxlength bounds make this unreachable; never submit out-of-range input
    }
  } else if (selectedGroupIndex.value === NO_GROUP) {
    picked = { id: null, platform: null };
  } else {
    picked = {
      id: availableGroups.value[selectedGroupIndex.value]?.id ?? null,
      platform: availableGroups.value[selectedGroupIndex.value]?.platform ?? null,
    };
  }
  emit("submit", { accountId: selectedAccountId.value, group: picked });
}
</script>

<style scoped>
.platform-link-empty {
  display: flex;
  flex-wrap: wrap;
  align-items: center;
  justify-content: space-between;
  gap: 8px;
}
</style>
