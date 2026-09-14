<template>
  <FormSurface
    :show="show"
    :title="editing ? t('编辑平台账号') : t('添加平台账号')"
    :embedded="embedded"
    modal-class="platform-account-form-modal"
    modal-style="width: 520px; max-width: calc(100vw - 32px)"
    :close-on-esc="!busy"
    @update:show="setVisible"
  >
    <n-form label-placement="top" @submit.prevent="submit">
      <n-form-item v-if="!embedded" :label="t('平台类型')" required>
        <n-select
          v-model:value="form.kind"
          :options="kindOptions"
          :disabled="busy || !!editing"
          :consistent-menu-width="false"
        />
        <template v-if="editing" #feedback>{{ t("平台类型与地址创建后不可修改") }}</template>
        <template v-else #feedback>
          {{ t("New API 与 Sub2API 是平台软件类型；同一类型可添加多个独立命名的站点实例。") }}
        </template>
      </n-form-item>
      <n-form-item :label="t('名称')" required>
        <n-input
          v-model:value="form.name"
          autofocus
          :disabled="busy"
          maxlength="200"
          :placeholder="t('例如：我的 New API')"
          :input-props="{ 'aria-label': t('名称') }"
        />
      </n-form-item>
      <n-form-item
        :label="t('平台地址')"
        required
        :validation-status="baseUrlIssue ? 'error' : undefined"
        :feedback="baseUrlFeedback"
      >
        <n-input
          v-model:value="form.baseUrl"
          :disabled="busy || !!editing"
          class="mono"
          :placeholder="customApiUrlPlaceholder()"
          :input-props="{ 'aria-label': t('平台地址') }"
        />
      </n-form-item>
      <n-form-item :label="t('管理凭证（可选）')">
        <n-input
          v-model:value="form.userCredential"
          type="password"
          show-password-on="click"
          :disabled="busy || clearCredential"
          :placeholder="credentialPlaceholder"
          :input-props="{ 'aria-label': t('管理凭证（可选）'), autocomplete: 'off' }"
        />
        <template #feedback>
          {{ editing && editing.hasUserCredential
            ? t("已保存凭证；留空保持不变")
            : t("仅用于刷新余额、分组与价格等平台数据，不会用作推理 Key") }}
        </template>
      </n-form-item>
      <n-form-item v-if="editing && editing.hasUserCredential" :show-label="false">
        <n-checkbox v-model:checked="clearCredential" :disabled="busy">
          {{ t("清除已保存的凭证") }}
        </n-checkbox>
      </n-form-item>
    </n-form>
    <template #footer>
      <n-space justify="end">
        <n-button v-if="!embedded" :disabled="busy" @click="setVisible(false)">{{ t("取消") }}</n-button>
        <n-button type="primary" :loading="busy" :disabled="!canSubmit" @click="submit">
          {{ editing ? t("保存") : t("创建") }}
        </n-button>
      </n-space>
    </template>
  </FormSurface>
</template>

<script setup lang="ts">
import { computed, ref, watch } from "vue";
import {
  NButton,
  NCheckbox,
  NForm,
  NFormItem,
  NInput,
  NSelect,
  NSpace,
} from "naive-ui";
import type { PlatformAccount, PlatformKind } from "../api/platform-accounts.ts";
import { PLATFORM_KIND_LABELS } from "../domain/platform-accounts.ts";
import {
  CUSTOM_ENDPOINT_URL_ISSUE_KEYS,
  customApiUrlPlaceholder,
  customEndpointUrlIssue,
} from "../domain/custom-account.ts";
import { t, type MessageKey } from "../i18n/index.ts";
import FormSurface from "./FormSurface.vue";

export interface PlatformAccountFormPayload {
  kind: PlatformKind;
  name: string;
  baseUrl: string;
  /** undefined preserves the saved credential; "" clears it. */
  userCredential?: string;
}

const props = defineProps<{
  show: boolean;
  editing: PlatformAccount | null;
  presetKind: PlatformKind;
  busy: boolean;
  /** Inline rendering inside the Add Account chooser instead of a modal. */
  embedded?: boolean;
}>();

const emit = defineEmits<{
  "update:show": [show: boolean];
  save: [payload: PlatformAccountFormPayload];
}>();

const form = ref({ kind: props.presetKind as PlatformKind, name: "", baseUrl: "", userCredential: "" });
const clearCredential = ref(false);
const baseUrlTouched = ref(false);

const kindOptions = computed(() => (
  (Object.entries(PLATFORM_KIND_LABELS) as [PlatformKind, string][]).map(([value, label]) => ({ value, label }))
));

const baseUrlIssue = computed(() => {
  if (props.editing || (!baseUrlTouched.value && !form.value.baseUrl)) return null;
  return customEndpointUrlIssue(form.value.baseUrl);
});
const baseUrlFeedback = computed(() => {
  const issue = baseUrlIssue.value;
  if (props.editing) return t("平台类型与地址创建后不可修改");
  return issue ? t(CUSTOM_ENDPOINT_URL_ISSUE_KEYS[issue] as MessageKey) : "";
});
const credentialPlaceholder = computed(() => (
  props.editing?.hasUserCredential ? t("已保存凭证；留空保持不变") : ""
));

const canSubmit = computed(() => {
  if (!form.value.name.trim() || form.value.name.trim().length > 200) return false;
  if (!props.editing && customEndpointUrlIssue(form.value.baseUrl) !== null) return false;
  return true;
});

// `presetKind` joins the trigger so an embedded chooser kind switch resets the
// draft (name/URL/credential never leak across platform kinds); in modal use
// the kind is always settled before `show` opens, so behavior is unchanged.
watch([() => props.show, () => props.presetKind], ([show]) => {
  if (!show) return;
  form.value = {
    kind: props.editing?.kind ?? props.presetKind,
    name: props.editing?.name ?? "",
    baseUrl: props.editing?.baseUrl ?? "",
    userCredential: "",
  };
  clearCredential.value = false;
  baseUrlTouched.value = false;
});

watch(() => form.value.baseUrl, () => {
  baseUrlTouched.value = true;
});

function setVisible(show: boolean): void {
  if (!show && props.busy) return;
  emit("update:show", show);
}

function submit(): void {
  if (!canSubmit.value || props.busy) return;
  const userCredential = clearCredential.value
    ? ""
    : form.value.userCredential.trim() || undefined;
  emit("save", {
    kind: form.value.kind,
    name: form.value.name.trim(),
    baseUrl: form.value.baseUrl.trim(),
    userCredential,
  });
}
</script>
