<template>
  <FormSurface
    :show="show"
    :title="t('添加 Key')"
    modal-class="identity-credential-create-modal"
    modal-style="width: 520px; max-width: calc(100vw - 32px)"
    :close-on-esc="!busy"
    @update:show="setVisible"
  >
    <n-alert v-if="unsupportedReason" type="warning" :show-icon="false" class="form-error" role="status">
      {{ t(unsupportedReason) }}
    </n-alert>
    <n-alert v-else-if="formError" type="error" class="form-error" role="alert">
      {{ formError }}
    </n-alert>

    <n-form
      v-if="!unsupportedReason"
      label-placement="top"
      @submit.prevent="submit"
    >
      <p class="field-hint">
        {{ t("不会根据同一身份自动共享额度，需明确选择。") }}
      </p>
      <n-form-item :label="t('账号名称（可选）')">
        <n-input
          v-model:value="draft.accountLabel"
          :disabled="fieldsLocked"
          :placeholder="t('账号名称（可选）')"
          :input-props="{ 'aria-label': t('账号名称（可选）') }"
        />
      </n-form-item>
      <n-form-item :label="t('API Key')" required>
        <n-input
          ref="secretInputRef"
          v-model:value="draft.secret"
          type="password"
          show-password-on="click"
          autofocus
          :disabled="fieldsLocked"
          :placeholder="t('填写新 Key')"
          :input-props="{ 'aria-label': t('API Key'), autocomplete: 'off' }"
        />
      </n-form-item>
      <n-form-item v-if="connectionOptions.length > 1" :label="t('连接')" required>
        <n-select
          v-model:value="draft.connectionId"
          :options="connectionOptions"
          :disabled="fieldsLocked"
          :aria-label="t('连接')"
        />
      </n-form-item>
      <n-form-item :label="t('独立额度')">
        <n-radio-group
          :value="draft.sharingKind"
          size="small"
          type="button"
          :disabled="fieldsLocked"
          :aria-label="t('独立额度')"
          @update:value="setSharingKind"
        >
          <n-radio-button value="independent">{{ t("独立额度") }}</n-radio-button>
          <n-radio-button value="shared">{{ t("与已有 Key 共享额度") }}</n-radio-button>
        </n-radio-group>
      </n-form-item>
      <n-form-item
        v-if="draft.sharingKind === 'shared'"
        :label="t('选择要共享额度的 Key')"
        required
      >
        <n-select
          v-model:value="draft.shareCredentialId"
          :options="shareOptions"
          :disabled="fieldsLocked"
          :placeholder="t('选择同一身份下要共享额度的 Key')"
          :aria-label="t('选择要共享额度的 Key')"
        />
      </n-form-item>
    </n-form>

    <template #footer>
      <n-space justify="end">
        <n-button :disabled="busy" @click="setVisible(false)">
          {{ busy ? t("加载中…") : t("取消") }}
        </n-button>
        <n-button
          type="primary"
          :loading="busy"
          :disabled="busy || !!unsupportedReason || !canSubmit"
          @click="submit"
        >
          {{ lastFailure === "uncertain" ? t("重试") : t("添加 Key") }}
        </n-button>
      </n-space>
    </template>
  </FormSurface>
</template>

<script setup lang="ts">
import { computed, nextTick, ref, watch } from "vue";
import {
  NAlert,
  NButton,
  NForm,
  NFormItem,
  NInput,
  NRadioButton,
  NRadioGroup,
  NSelect,
  NSpace,
  type InputInst,
} from "naive-ui";
import type { Connection } from "../api/connections.ts";
import type { IdentityCredentialCreateInput } from "../api/identities.ts";
import {
  buildCreatePayload,
  connectionAllowsIdentityCredentialCreate,
  createPayloadSignature,
  credentialEditorIssueKey,
  emptyCreateDraft,
  isUncertainCreateFailure,
  nextCreateOperationId,
  type CredentialCreateDraft,
  type CredentialCreateFailureKind,
} from "../domain/account-credential.ts";
import { t, type MessageKey } from "../i18n/index.ts";
import FormSurface from "./FormSurface.vue";

const props = defineProps<{
  show: boolean;
  unsupportedReason: MessageKey | null;
  busy: boolean;
  defaultConnectionId: string;
  connections: readonly Connection[];
  shareTargets: readonly { id: string; label: string }[];
}>();

const emit = defineEmits<{
  "update:show": [show: boolean];
  create: [payload: IdentityCredentialCreateInput];
}>();

const draft = ref<CredentialCreateDraft>(emptyCreateDraft());
const formError = ref("");
const secretInputRef = ref<InputInst | null>(null);
const operationId = ref<string | null>(null);
const lastSignature = ref<string | null>(null);
const lastFailure = ref<CredentialCreateFailureKind>("none");

const supportedConnections = computed(() => (
  props.connections.filter(connectionAllowsIdentityCredentialCreate)
));

const connectionOptions = computed(() => {
  const rows = supportedConnections.value;
  const options = rows.map((connection) => ({
    value: connection.id,
    label: connection.name,
  }));
  if (
    props.defaultConnectionId
    && !options.some((option) => option.value === props.defaultConnectionId)
  ) {
    options.unshift({
      value: props.defaultConnectionId,
      label: props.defaultConnectionId,
    });
  }
  return options;
});

const shareOptions = computed(() => (
  props.shareTargets.map((target) => ({ value: target.id, label: target.label }))
));

const draftLocked = computed(() => lastFailure.value === "uncertain");
const fieldsLocked = computed(() => props.busy || draftLocked.value);

const canSubmit = computed(() => {
  if (!draft.value.secret.trim() || !draft.value.connectionId.trim()) return false;
  if (draft.value.sharingKind === "shared") return draft.value.shareCredentialId.trim().length > 0;
  return true;
});

function hydrate(): void {
  formError.value = "";
  draft.value = emptyCreateDraft(props.defaultConnectionId);
  operationId.value = null;
  lastSignature.value = null;
  lastFailure.value = "none";
}

function setVisible(show: boolean): void {
  if (!show && props.busy) return;
  emit("update:show", show);
}

function setSharingKind(value: string | number): void {
  if (fieldsLocked.value) return;
  draft.value.sharingKind = value === "shared" ? "shared" : "independent";
  if (draft.value.sharingKind === "independent") draft.value.shareCredentialId = "";
}

function submit(): void {
  if (props.busy || props.unsupportedReason) return;
  formError.value = "";
  try {
    const payload = buildCreatePayload(
      draft.value,
      props.shareTargets.map((target) => target.id),
    );
    const signature = createPayloadSignature(payload);
    const nextId = nextCreateOperationId({
      previousId: operationId.value,
      previousSignature: lastSignature.value,
      nextSignature: signature,
      lastFailure: lastFailure.value,
    });
    operationId.value = nextId;
    lastSignature.value = signature;
    emit("create", { ...payload, operationId: nextId });
  } catch (error) {
    formError.value = t(credentialEditorIssueKey(error));
  }
}

function noteFailure(error: unknown): void {
  lastFailure.value = isUncertainCreateFailure(error) ? "uncertain" : "definitive";
  if (lastFailure.value === "uncertain") {
    formError.value = t("创建结果未知，Key 可能已添加。用相同内容重试，勿修改后提交。");
  }
}

defineExpose({ noteFailure });

watch(() => props.show, (show) => {
  if (!show) {
    draft.value = emptyCreateDraft();
    formError.value = "";
    return;
  }
  hydrate();
  void nextTick(() => secretInputRef.value?.focus());
});
</script>

<style scoped>
.form-error {
  margin-bottom: var(--ocg-space-md);
}

.field-hint {
  margin: 0 0 var(--ocg-space-md);
  color: var(--ocg-muted);
  font-size: var(--ocg-font-xs);
}
</style>
