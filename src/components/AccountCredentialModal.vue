<template>
  <FormSurface
    :show="show"
    :title="title"
    modal-class="account-credential-modal"
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
      <template v-if="mode === 'rotate'">
        <p class="field-hint">{{ t("仅替换本机后续请求使用的 Key。供应商侧原凭据不会被撤销，仍由你自行管理。") }}</p>
        <n-form-item :label="t('API Key')" required>
          <n-input
            ref="secretInputRef"
            v-model:value="rotateDraft.secret"
            type="password"
            show-password-on="click"
            autofocus
            :disabled="busy"
            :placeholder="t('填写新 Key')"
            :input-props="{ 'aria-label': t('API Key'), autocomplete: 'off' }"
          />
        </n-form-item>
      </template>

      <template v-else>
        <n-form-item :label="t('绑定启用')">
          <n-switch
            v-model:value="bindingDraft.enabled"
            :disabled="busy"
            :aria-label="t('绑定启用')"
          />
        </n-form-item>
        <n-form-item :label="t('模型范围')" required>
          <n-radio-group
            :value="bindingDraft.scopeKind"
            size="small"
            type="button"
            :disabled="busy"
            :aria-label="t('模型范围')"
            @update:value="setScopeKind"
          >
            <n-radio-button value="all">{{ t("全部模型") }}</n-radio-button>
            <n-radio-button value="only">{{ t("仅指定模型") }}</n-radio-button>
          </n-radio-group>
          <template #feedback>{{ t("填写准确的模型名称。客户端请求名和供应商模型 ID 均可，须完全一致，不会改写或补全。") }}</template>
        </n-form-item>
        <n-form-item v-if="bindingDraft.scopeKind === 'only'" :label="t('模型名称')" required>
          <div class="model-rows">
            <div
              v-for="(_, index) in bindingDraft.models"
              :key="index"
              class="model-row"
            >
              <n-input
                :value="bindingDraft.models[index]"
                :disabled="busy"
                class="mono"
                :placeholder="t('准确的模型名称')"
                :input-props="{ 'aria-label': t('模型名称') }"
                @update:value="(value: string) => updateModel(index, value)"
              />
              <n-button
                attr-type="button"
                quaternary
                :disabled="busy || bindingDraft.models.length < 2"
                :aria-label="t('删除模型')"
                @click="removeModel(index)"
              >
                {{ t("删除模型") }}
              </n-button>
            </div>
            <n-button attr-type="button" size="small" secondary :disabled="busy" @click="addModel">
              {{ t("添加模型") }}
            </n-button>
          </div>
        </n-form-item>
        <n-form-item v-if="destinationOptions.length > 0" :label="t('允许此 Key 发往')">
          <div class="destination-rows" role="group" :aria-label="t('允许此 Key 发往')">
            <n-checkbox
              v-for="destination in destinationOptions"
              :key="destination.id"
              :checked="bindingDraft.selectedEndpointIds.includes(destination.id)"
              :disabled="busy"
              :aria-label="destinationConsentLabel(destination)"
              @update:checked="(checked: boolean) => toggleDestination(destination.id, checked)"
            >
              <span class="destination-label">
                <span class="mono">{{ destination.protocol }}</span>
                <span v-if="destination.url" class="mono destination-url">{{ destination.url }}</span>
                <span v-else class="destination-locked">{{ t("该目标已锁定。官方地址不会作为 Origin 填写。") }}</span>
              </span>
            </n-checkbox>
          </div>
        </n-form-item>
        <p v-if="staleOrigins.length > 0 || staleEndpointIds.length > 0" class="field-hint" role="status">
          {{ t("已保存的目标（供应商地址可能已更改）") }}
          <span class="mono">{{ [...staleOrigins, ...staleEndpointIds].join(" · ") }}</span>
        </p>
      </template>
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
          {{ mode === "rotate" ? t("轮换 Key") : t("保存") }}
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
  NCheckbox,
  NForm,
  NFormItem,
  NInput,
  NRadioButton,
  NRadioGroup,
  NSpace,
  NSwitch,
  type InputInst,
} from "naive-ui";
import type { Connection } from "../api/connections.ts";
import type { BindingPatchInput, IdentityBinding } from "../api/identities.ts";
import {
  bindingDestinationOptions,
  bindingDraftFrom,
  buildBindingPayload,
  buildRotatePayload,
  credentialEditorIssueKey,
  emptyRotateDraft,
  staleSavedEndpointIds,
  staleSavedOrigins,
  type BindingDestinationOption,
  type CredentialBindingDraft,
  type CredentialEditorMode,
  type CredentialRotateDraft,
} from "../domain/account-credential.ts";
import { t, type MessageKey } from "../i18n/index.ts";
import FormSurface from "./FormSurface.vue";

const props = defineProps<{
  show: boolean;
  mode: CredentialEditorMode;
  binding: IdentityBinding | null;
  connection: Connection | null;
  unsupportedReason: MessageKey | null;
  busy: boolean;
}>();

const emit = defineEmits<{
  "update:show": [show: boolean];
  rotate: [payload: { secretInput: string }];
  saveBinding: [payload: BindingPatchInput];
}>();

const rotateDraft = ref<CredentialRotateDraft>(emptyRotateDraft());
const bindingDraft = ref<CredentialBindingDraft>(bindingDraftFrom(null));
const formError = ref("");
const secretInputRef = ref<InputInst | null>(null);

const title = computed(() => (
  props.mode === "rotate" ? t("轮换 Key") : t("编辑绑定")
));

const endpoints = computed(() => props.connection?.endpoints ?? []);
const destinationOptions = computed(() => bindingDestinationOptions(endpoints.value));
const staleOrigins = computed(() => staleSavedOrigins(props.binding, endpoints.value));
const staleEndpointIds = computed(() => staleSavedEndpointIds(props.binding, endpoints.value));

const canSubmit = computed(() => {
  if (props.mode === "rotate") return rotateDraft.value.secret.trim().length > 0;
  if (bindingDraft.value.scopeKind === "all") return true;
  return bindingDraft.value.models.some((model) => model.trim().length > 0);
});

function destinationConsentLabel(destination: BindingDestinationOption): string {
  if (destination.url) return t("允许此 Key 发往 {protocol} {url}", {
    protocol: destination.protocol,
    url: destination.url,
  });
  return t("允许此 Key 发往 {protocol}（官方目标）", { protocol: destination.protocol });
}

function clearSecrets(): void {
  rotateDraft.value = emptyRotateDraft();
}

function hydrate(): void {
  formError.value = "";
  rotateDraft.value = emptyRotateDraft();
  bindingDraft.value = bindingDraftFrom(props.binding, endpoints.value);
}

function setVisible(show: boolean): void {
  if (!show && props.busy) return;
  emit("update:show", show);
}

function setScopeKind(value: string | number): void {
  if (props.busy) return;
  const kind = value === "only" ? "only" : "all";
  bindingDraft.value.scopeKind = kind;
  if (kind === "only" && bindingDraft.value.models.length === 0) {
    bindingDraft.value.models = [""];
  }
}

function updateModel(index: number, value: string): void {
  if (props.busy) return;
  bindingDraft.value.models[index] = value;
}

function addModel(): void {
  if (props.busy) return;
  bindingDraft.value.models.push("");
}

function removeModel(index: number): void {
  if (props.busy || bindingDraft.value.models.length < 2) return;
  bindingDraft.value.models.splice(index, 1);
}

function toggleDestination(id: string, checked: boolean): void {
  if (props.busy) return;
  bindingDraft.value.destinationsTouched = true;
  const selected = bindingDraft.value.selectedEndpointIds;
  const index = selected.indexOf(id);
  if (checked && index < 0) selected.push(id);
  if (!checked && index >= 0) selected.splice(index, 1);
}

function submit(): void {
  if (props.busy || props.unsupportedReason) return;
  formError.value = "";
  try {
    if (props.mode === "rotate") {
      emit("rotate", buildRotatePayload(rotateDraft.value));
      return;
    }
    emit("saveBinding", buildBindingPayload(bindingDraft.value, endpoints.value));
  } catch (error) {
    formError.value = t(credentialEditorIssueKey(error));
  }
}

watch(() => [props.show, props.mode] as const, ([show]) => {
  if (!show) {
    clearSecrets();
    formError.value = "";
    return;
  }
  hydrate();
  void nextTick(() => {
    if (props.mode === "rotate") secretInputRef.value?.focus();
  });
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

.model-rows,
.destination-rows {
  display: grid;
  gap: var(--ocg-space-sm);
}

.model-row {
  display: grid;
  grid-template-columns: minmax(0, 1fr) auto;
  gap: var(--ocg-space-sm);
  align-items: center;
}

.destination-label {
  display: grid;
  gap: 2px;
}

.destination-url,
.destination-locked {
  color: var(--ocg-muted);
  font-size: var(--ocg-font-xs);
}
</style>
