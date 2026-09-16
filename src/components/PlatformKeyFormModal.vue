<template>
  <FormSurface
    :show="show"
    :title="title"
    modal-class="platform-key-form-modal"
    modal-style="width: 520px; max-width: calc(100vw - 32px)"
    :close-on-esc="!busy"
    @update:show="setVisible"
  >
    <n-alert v-if="formError" type="error" class="form-error" role="alert">
      {{ formError }}
    </n-alert>
    <n-form label-placement="top" @submit.prevent="submit">
      <p class="field-hint">
        {{ t("New API 与 Sub2API 自带协议转换。这里只保存名称与 Key，保存时按该 Key 拉取可用模型。同一模型出现在多把 Key 上会按 Key 顺序叠加路由，各用各的倍率。") }}
      </p>
      <p v-if="parentName" class="field-hint">
        {{ t("站点 {name} 托管 Endpoint；协议路径在请求时按客户端格式选择。", { name: parentName }) }}
      </p>
      <n-form-item :label="t('名称')" required>
        <n-input
          v-model:value="draft.name"
          :disabled="fieldsLocked"
          :placeholder="t('例如：Codex 稳定 / Codex Pro')"
          :input-props="{ 'aria-label': t('名称') }"
        />
      </n-form-item>
      <n-form-item :label="t('API Key')" required>
        <n-input
          v-model:value="draft.key"
          type="password"
          show-password-on="click"
          autofocus
          :disabled="fieldsLocked"
          :placeholder="isEdit ? t('留空则保持已保存的 Key') : t('sk-...')"
          :input-props="{ 'aria-label': t('API Key'), autocomplete: 'off' }"
        />
      </n-form-item>
      <n-form-item :label="t('备注')">
        <n-input
          v-model:value="draft.notes"
          type="textarea"
          :disabled="fieldsLocked"
          :autosize="{ minRows: 3, maxRows: 8 }"
          :maxlength="4000"
          show-count
          :placeholder="t('可填写任意备注')"
          :input-props="{ 'aria-label': t('备注') }"
        />
      </n-form-item>
    </n-form>
    <template #footer>
      <n-space justify="end">
        <n-button :disabled="busy" @click="setVisible(false)">{{ t("取消") }}</n-button>
        <n-button type="primary" :loading="busy" :disabled="busy || !canSubmit" @click="submit">
          {{ t("保存") }}
        </n-button>
      </n-space>
    </template>
  </FormSurface>
</template>

<script setup lang="ts">
import { computed, ref, watch } from "vue";
import { NAlert, NButton, NForm, NFormItem, NInput, NSpace } from "naive-ui";
import { t } from "../i18n/index.ts";
import FormSurface from "./FormSurface.vue";

export interface PlatformKeyFormPayload {
  name: string;
  key: string;
  notes: string;
}

const props = withDefaults(defineProps<{
  show: boolean;
  busy?: boolean;
  parentName?: string;
  title?: string;
  editing?: { name: string; notes: string } | null;
  externalError?: string;
}>(), {
  busy: false,
  parentName: "",
  title: "",
  editing: null,
  externalError: "",
});

const emit = defineEmits<{
  "update:show": [show: boolean];
  save: [payload: PlatformKeyFormPayload];
}>();

const draft = ref({ name: "", key: "", notes: "" });
const localError = ref("");

const isEdit = computed(() => Boolean(props.editing));
const title = computed(() => props.title || (isEdit.value ? t("编辑 Key") : t("添加 Key")));
const formError = computed(() => props.externalError || localError.value);
const fieldsLocked = computed(() => props.busy);
const canSubmit = computed(() => {
  if (!draft.value.name.trim()) return false;
  if (!isEdit.value && !draft.value.key.trim()) return false;
  return true;
});

watch(() => [props.show, props.editing?.name, props.editing?.notes] as const, ([show]) => {
  if (!show) return;
  draft.value = {
    name: props.editing?.name ?? "",
    key: "",
    notes: props.editing?.notes ?? "",
  };
  localError.value = "";
});

function setVisible(show: boolean): void {
  if (!show && props.busy) return;
  emit("update:show", show);
}

function submit(): void {
  if (props.busy || !canSubmit.value) return;
  localError.value = "";
  emit("save", {
    name: draft.value.name.trim(),
    key: draft.value.key.trim(),
    notes: draft.value.notes.trim(),
  });
}
</script>

<style scoped>
.form-error {
  margin-bottom: 12px;
}

.field-hint {
  margin: 0 0 12px;
  font-size: var(--ocg-font-xs);
  color: var(--ocg-subtle);
}
</style>
