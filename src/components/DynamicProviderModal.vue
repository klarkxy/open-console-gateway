<template>
  <FormSurface
    :show="show"
    :title="isEdit ? t('编辑供应商') : createTitle"
    :embedded="embedded"
    modal-class="dynamic-provider-modal"
    modal-style="width: 720px; max-width: calc(100vw - 32px)"
    :close-on-esc="!busy"
    @update:show="onSurfaceUpdateShow"
  >
    <n-form label-placement="top" @submit.prevent="save">
      <n-alert v-if="formError" type="error" class="form-error" role="alert">
        {{ formError }}
      </n-alert>
      <n-alert v-if="testSuccess" type="success" class="form-error" role="status">
        {{ testSuccess }}
      </n-alert>
      <n-alert v-if="conflictNotice" type="warning" class="form-error" role="alert">
        {{ conflictNotice }}
      </n-alert>
      <n-alert v-if="showAdvancedDetails" type="default" :show-icon="false" class="form-error">
        {{ t("填写连接信息即可保存；该供应商的价格和官方用量始终未知。") }}
      </n-alert>

      <div class="modal-grid">
        <dl v-if="fixedPreset" class="connection-summary full-width-field" :aria-label="t('连接信息')">
          <div class="connection-summary__row">
            <dt>{{ t("API 地址") }}</dt>
            <dd v-if="fixedPreset.endpointUrl"><code>{{ fixedPreset.endpointUrl }}</code></dd>
            <dd v-else class="connection-summary__pending">{{ t("需在下方填写") }}</dd>
          </div>
          <div class="connection-summary__row">
            <dt>{{ t("上游协议") }}</dt>
            <dd>{{ protocolDisplayName(fixedPreset.protocol) }}</dd>
          </div>
          <div class="connection-summary__row">
            <dt>{{ t("鉴权方式") }}</dt>
            <dd>{{ fixedPreset.authKind === "bearer" ? "Bearer" : "x-api-key" }}</dd>
          </div>
          <div v-if="fixedSeeded" class="connection-summary__row">
            <dt>{{ t("默认模型") }}</dt>
            <dd>{{ t("{count} 个", { count: fixedSeededModels.length }) }}</dd>
          </div>
        </dl>
        <n-form-item v-if="!isEdit && !presetSelectionLocked" :label="t('供应商预设')" class="full-width-field">
          <div class="preset-picker">
            <n-select
              :value="selectedPresetId"
              :options="presetOptions"
              filterable
              :disabled="busy"
              :placeholder="t('搜索预设')"
              :aria-label="t('供应商预设')"
              @update:value="onPresetChange"
            />
            <div v-if="selectedPreset" class="preset-details">
              <span class="preset-links">
                <a :href="selectedPreset.docsUrl" target="_blank" rel="noopener noreferrer">{{ t("官方文档") }}</a>
                <a :href="selectedPreset.websiteUrl" target="_blank" rel="noopener noreferrer">{{ t("控制台") }}</a>
              </span>
              <span class="field-hint">{{ presetNote }}</span>
            </div>
          </div>
        </n-form-item>
        <n-form-item v-if="!fixedPreset || settingsOpen" :label="t('名称')" path="name">
          <n-input
            v-model:value="draft.name"
            :input-props="{ 'aria-label': t('名称') }"
            :placeholder="t('例如：主号')"
          />
        </n-form-item>
        <n-form-item v-if="!fixedPreset" :label="t('鉴权方式')">
          <n-select
            v-model:value="draft.auth_kind"
            :options="authOptions"
            :disabled="busy"
            :aria-label="t('鉴权方式')"
          />
        </n-form-item>
        <n-form-item v-if="!fixedPreset || fixedEndpointRequired" :label="t('API 地址')" class="full-width-field">
          <n-input
            v-model:value="draft.endpoint_url"
            :disabled="busy"
            :input-props="{ 'aria-label': t('API 地址') }"
            :placeholder="endpointPlaceholder"
          />
        </n-form-item>
        <n-form-item v-if="!fixedPreset" :label="t('上游协议')">
          <n-select
            v-model:value="draft.upstream_protocol"
            :options="protocolOptions"
            :disabled="busy"
            :aria-label="t('上游协议')"
          />
        </n-form-item>
        <p v-if="fixedSeeded" class="fixed-models-summary">
          {{ t("默认模型：{models}", { models: fixedSeededModels.join(", ") }) }}
        </p>
        <n-form-item v-if="!isEdit && props.context === 'account' && (!fixedPreset || settingsOpen)" :label="t('第一个账号名称')">
          <n-input
            v-model:value="draft.account_name"
            :input-props="{ 'aria-label': t('第一个账号名称') }"
          />
        </n-form-item>
        <n-form-item
          v-if="showKeyField"
          :label="t('API Key')"
          class="full-width-field"
        >
          <n-input
            v-model:value="draft.key"
            type="password"
            show-password-on="click"
            :disabled="busy"
            :input-props="{ 'aria-label': t('API Key') }"
            :placeholder="keyPlaceholder"
          />
          <p v-if="keyIsTemporary" class="field-hint">
            {{ t("此 Key 仅临时用于获取模型和测试模型，保存不会更新它；更换已保存的 Key 请到账号页。") }}
          </p>
          <p v-else-if="optionalCreateKeyHint" class="field-hint">
            {{ t("可选。现在填写会同时创建第一个账号；留空则只保存连接，之后可在供应商详情或账号页补充 Key。") }}
          </p>
        </n-form-item>
        <n-form-item v-if="!isEdit && props.context === 'account' && (!fixedPreset || settingsOpen)" :label="t('备注')" class="full-width-field">
          <n-input
            v-model:value="draft.notes"
            type="textarea"
            :autosize="{ minRows: 2, maxRows: 6 }"
            :input-props="{ 'aria-label': t('备注') }"
          />
        </n-form-item>
        <n-form-item v-if="showAdvancedDetails" :label="t('模型映射')" class="full-width-field">
          <div class="capability-rows">
            <div class="capability-actions">
              <n-button
                attr-type="button"
                size="small"
                secondary
                :loading="discovering"
                :disabled="busy || discoveryUnavailable || probeKeyMissing"
                @click="discover"
              >
                {{ t("获取模型") }}
              </n-button>
              <n-button attr-type="button" size="small" secondary :disabled="busy" @click="addMapping">
                {{ t("添加映射") }}
              </n-button>
            </div>
            <p class="field-hint">{{ t("对外模型名不区分大小写且必须唯一；上游模型 ID 可复用。") }}</p>
            <p class="field-hint">
              {{ t("模型默认跟随供应商的协议与地址；仅当某个模型需要不同上游时才覆盖，鉴权始终使用供应商的 Key。") }}
            </p>
            <p v-if="probeKeyMissing" class="field-hint">
              {{ t("获取模型和测试模型需要 Key") }}
            </p>
            <p v-if="discoveryUnavailable" class="field-hint">
              {{ t("此预设未配置模型发现，请手动填写准确的模型 ID。") }}
            </p>
            <p v-if="isEdit && endpointPreset" class="field-hint">
              {{ t("当前 Endpoint 与预设“{preset}”匹配；模型发现与导入命名沿用该预设。", { preset: endpointPreset.name }) }}
            </p>
            <p v-else-if="isEdit && templatePreset" class="field-hint">
              {{ t("来源预设模板：“{preset}”；当前 Endpoint 已自定义。", { preset: templatePreset.name }) }}
            </p>
            <n-alert v-if="discoveryError" type="error" :show-icon="false">{{ discoveryError }}</n-alert>
            <p v-if="discoveryInfo" class="field-hint">{{ discoveryInfo }}</p>
            <div v-for="(row, index) in draft.models" :key="index" class="mapping-row">
              <div class="mapping-row-main">
                <n-input
                  v-model:value="row.public_model"
                  :disabled="busy"
                  :placeholder="t('对外模型名')"
                  :input-props="{ 'aria-label': t('对外模型名') }"
                />
                <n-input
                  v-model:value="row.upstream_model"
                  :disabled="busy"
                  :placeholder="t('上游模型 ID')"
                  :input-props="{ 'aria-label': t('上游模型 ID') }"
                />
                <n-button attr-type="button" quaternary :disabled="draft.models.length < 2 || busy" @click="removeMapping(index)">
                  {{ t("删除映射") }}
                </n-button>
              </div>
              <div v-if="!fixedPreset" class="mapping-row-route">
                <n-select
                  :value="row.upstream_override ? 'override' : 'inherit'"
                  :options="routeModeOptions"
                  :disabled="busy"
                  :aria-label="t('上游连接')"
                  @update:value="(mode) => setMappingRouteMode(row, String(mode))"
                />
                <template v-if="row.upstream_override">
                  <n-select
                    v-model:value="row.upstream_override.protocol"
                    :options="protocolOptions"
                    :disabled="busy"
                    :aria-label="t('覆盖的上游协议')"
                  />
                  <n-input
                    v-model:value="row.upstream_override.endpoint_url"
                    :disabled="busy"
                    :placeholder="t('覆盖的上游地址（必填）')"
                    :input-props="{ 'aria-label': t('覆盖的上游地址') }"
                  />
                </template>
              </div>
            </div>
            <div v-if="discoveredModels.length" class="discovery-import">
              <n-select
                v-model:value="selectedDiscovery"
                multiple
                :disabled="busy"
                :options="discoveredModels.map((model) => ({ label: model, value: model }))"
                :placeholder="t('选择要导入的模型')"
                :aria-label="t('选择要导入的模型')"
              />
              <n-button attr-type="button" size="small" :disabled="busy" @click="importDiscovered">{{ t("导入所选") }}</n-button>
            </div>
            <p v-if="discoveredModels.length && importPresetId" class="field-hint">
              {{ t("导入的对外模型名使用“预设 ID/模型 ID”格式；上游模型 ID 保持原样。") }}
            </p>
          </div>
        </n-form-item>
        <n-form-item v-if="showAdvancedDetails" :label="t('模型测试')" class="full-width-field">
          <div class="test-section">
            <n-select
              v-model:value="testTargetIndex"
              :options="testTargetOptions"
              :disabled="busy || testTargets.length === 0"
              :placeholder="t('选择要测试的模型')"
              :consistent-menu-width="false"
              :aria-label="t('选择要测试的模型')"
            />
            <p class="field-hint">
              {{ t("按所选模型的当前配置测试连接：模型覆盖优先，否则跟随供应商默认；测试只作观测，不会开启或改动路由。") }}
            </p>
          </div>
        </n-form-item>
        <div v-if="fixedPreset" class="fixed-settings-toggle">
          <n-button
            attr-type="button"
            text
            size="small"
            :aria-expanded="settingsOpen"
            @click="settingsOpen = !settingsOpen"
          >
            <template #icon>
              <n-icon :component="settingsOpen ? DownOutlined : RightOutlined" aria-hidden="true" />
            </template>
            {{ t("更多设置") }}
          </n-button>
        </div>
      </div>
    </n-form>
    <template #footer>
      <div class="modal-footer">
        <n-popconfirm
          v-if="testNeedsConfirm && showAdvancedDetails"
          :positive-text="t('测试模型')"
          :negative-text="t('取消')"
          @positive-click="runTest"
        >
          <template #trigger>
            <n-button attr-type="button" secondary :loading="testing" :disabled="busy || probeKeyMissing">
              {{ t("测试模型") }}
            </n-button>
          </template>
          {{ t(paidTestWarningKey) }}
        </n-popconfirm>
        <n-space>
          <n-button v-if="!embedded" attr-type="button" :disabled="busy" @click="$emit('update:show', false)">{{ t("取消") }}</n-button>
          <n-button type="primary" attr-type="submit" :loading="saving" :disabled="busy" @click="save">
            {{ saving ? t("正在保存…") : (isEdit ? t("保存供应商") : createTitle) }}
          </n-button>
        </n-space>
      </div>
    </template>
  </FormSurface>
</template>

<script setup lang="ts">
import { computed, onUnmounted, ref, watch } from "vue";
import {
  NAlert,
  NButton,
  NForm,
  NFormItem,
  NIcon,
  NInput,
  NPopconfirm,
  NSelect,
  NSpace,
} from "naive-ui";
import { DownOutlined, RightOutlined } from "@vicons/antd";
import { isRevisionConflict, providerApi, type ProviderDefinitionView } from "../api/providers.ts";
import { locale, t, type MessageKey } from "../i18n/index.ts";
import { dashboardErrorDetail } from "../utils/errors.ts";
import { protocolDisplayName } from "../domain/provider-contracts.ts";
import {
  PROVIDER_PRESETS,
  applyProviderPresetToDraft,
  groupProviderPresetsByOffering,
  providerPresetDefaultModels,
  providerPresetEndpointPlaceholder,
  providerPresetImportPublicName,
  providerPresetModelDiscoveryEnabled,
  providerPresetNote,
  resolveEditPreset,
} from "../domain/provider-presets.ts";
import {
  DYNAMIC_AUTH_KINDS,
  DYNAMIC_PAID_TEST_WARNING_KEY,
  DYNAMIC_PROTOCOLS,
  DYNAMIC_PROVIDER_DRAFT_ERROR_KEYS,
  buildProviderDefinitionCreateBody,
  buildProviderDefinitionUpdateBody,
  completeDynamicTestTargets,
  dynamicAuthRequiresKey,
  dynamicMappingOverrideError,
  dynamicProviderActionNeedsConfirm,
  emptyProviderDefinitionDraft,
  resolveDynamicMappingRoute,
  sanitizeProviderDefinitionDraft,
  validateProviderDefinitionDraft,
  type DynamicAuthKind,
  type ProviderDefinitionDraft,
  type ProviderDefinitionMapping,
  type DynamicUpstreamProtocol,
} from "../domain/dynamic-provider.ts";
import FormSurface from "./FormSurface.vue";

const props = defineProps<{
  show: boolean;
  provider: ProviderDefinitionView | null;
  /** Create mode only: preset applied on every open; null/unknown stays manual. */
  initialPresetId?: string | null;
  /** "account" titles the atomic create as adding an account, not a supplier. */
  context?: "provider" | "account";
  /** Inline rendering inside a host pane instead of a modal. */
  embedded?: boolean;
  /** Create mode only: the host rail owns preset choice, so hide the picker. */
  presetSelectionLocked?: boolean;
}>();

const emit = defineEmits<{
  (event: "update:show", value: boolean): void;
  (event: "saved", providerId: string): void;
  (event: "conflict"): void;
  /** Hosts embed the form and block dismissal while work is in flight. */
  (event: "busyChange", busy: boolean): void;
}>();

const draft = ref<ProviderDefinitionDraft>(emptyProviderDefinitionDraft());
const formError = ref("");
const conflictNotice = ref("");
const testSuccess = ref("");
const discoveryError = ref("");
const discoveryInfo = ref("");
const discoveredModels = ref<string[]>([]);
const selectedDiscovery = ref<string[]>([]);
const saving = ref(false);
const discovering = ref(false);
const testing = ref(false);
const testTargetIndex = ref(0);
/** Optional settings section for fixed-preset creates; collapsed on open/switch. */
const settingsOpen = ref(false);
const MANUAL_PRESET_ID = "manual";
const selectedPresetId = ref(MANUAL_PRESET_ID);
// Bumped on close/reopen and on every preset switch so a slow discovery or
// test response from a previous context can never land in the current form.
const requestGeneration = ref(0);

const isEdit = computed(() => Boolean(props.provider));
const createTitle = computed(() => (
  props.context === "account" ? t("新增账号") : t("新建供应商")
));
const busy = computed(() => saving.value || discovering.value || testing.value);
// Hosts embedding this form block switching/closing on this signal.
watch(busy, (value) => emit("busyChange", value));
onUnmounted(() => {
  // The busy watcher is already stopped at this point, so release the host's
  // lock with a direct emit, and invalidate in-flight discovery/test via the
  // generation counter so an abandoned response can never commit anywhere.
  requestGeneration.value += 1;
  saving.value = false;
  discovering.value = false;
  testing.value = false;
  emit("busyChange", false);
});
const testNeedsConfirm = dynamicProviderActionNeedsConfirm("test");
const paidTestWarningKey = DYNAMIC_PAID_TEST_WARNING_KEY;
const selectedPreset = computed(() => (
  isEdit.value ? null : PROVIDER_PRESETS.find((preset) => preset.id === selectedPresetId.value) ?? null
));
// Edit mode has no preset picker: persisted provenance (preset_id) wins, and
// legacy rows fall back to safe endpoint/prefix inference. Template metadata
// and the verified endpoint match stay separate so a modified custom URL is
// never presented as the official endpoint, and an endpoint-only match never
// prefixes previously unprefixed manual mappings.
const editPreset = computed(() => (
  isEdit.value
    ? resolveEditPreset(
      props.provider?.preset_id ?? draft.value.preset_id,
      draft.value.endpoint_url,
      draft.value.auth_kind,
      draft.value.models,
    )
    : null
));
const endpointPreset = computed(() => editPreset.value?.endpointMatch ?? null);
const templatePreset = computed(() => editPreset.value?.template ?? null);
const effectivePreset = computed(() => (
  isEdit.value ? editPreset.value?.discoveryPreset ?? null : selectedPreset.value
));
const importPresetId = computed(() => (
  isEdit.value ? editPreset.value?.importPresetId ?? null : selectedPreset.value?.id ?? null
));
const presetOptions = computed(() => {
  const manual = { label: t("手动（自定义）"), value: MANUAL_PRESET_ID };
  const offeringGroups = groupProviderPresetsByOffering(PROVIDER_PRESETS);
  const groups = ([
    ["plan", "Plan", offeringGroups.plan],
    ["api", "API", offeringGroups.api],
  ] as const)
    .map(([offering, label, presets]) => ({
      type: "group" as const,
      label,
      key: `preset-group-${offering}`,
      children: presets.map((preset) => ({ label: preset.name, value: preset.id })),
    }))
    .filter((group) => group.children.length > 0);
  return [manual, ...groups];
});
const presetNote = computed(() => (
  selectedPreset.value ? providerPresetNote(selectedPreset.value, locale.value) : ""
));
const discoveryUnavailable = computed(() => (
  effectivePreset.value ? !providerPresetModelDiscoveryEnabled(effectivePreset.value) : false
));
/**
 * Create mode with an explicit preset is a fixed connection: protocol, auth,
 * and a configured preset endpoint are pinned by the preset and never offered
 * as controls. Edit mode and manual creation keep every existing control.
 */
const fixedPreset = computed(() => (isEdit.value ? null : selectedPreset.value));
const fixedSeededModels = computed(() => (
  fixedPreset.value ? providerPresetDefaultModels(fixedPreset.value) : []
));
const fixedSeeded = computed(() => fixedSeededModels.value.length > 0);
/** Azure/Bedrock-style presets have no fixed endpoint; the address stays required. */
const fixedEndpointRequired = computed(() => Boolean(fixedPreset.value && !fixedPreset.value.endpointUrl));
/**
 * Seeded fixed rows keep the model editor and model test behind More
 * settings; without seeds the editor stays visible so save validation is
 * never a hidden blocker.
 */
const showAdvancedDetails = computed(() => !fixedSeeded.value || settingsOpen.value);
const testTargets = computed(() => completeDynamicTestTargets(draft.value.models));
const testTarget = computed(() => (
  testTargets.value[Math.min(testTargetIndex.value, Math.max(testTargets.value.length - 1, 0))] ?? null
));
const testTargetOptions = computed(() => testTargets.value.map((target, index) => ({
  value: index,
  label: `${target.public_model} → ${target.upstream_model}`,
})));
const showKeyField = computed(() => {
  if (!isEdit.value) {
    return dynamicAuthRequiresKey(draft.value.auth_kind);
  }
  return dynamicAuthRequiresKey(draft.value.auth_kind) || props.provider?.auth_kind === "none";
});
const optionalCreateKeyHint = computed(() => (
  !isEdit.value
  && props.context !== "account"
  && dynamicAuthRequiresKey(draft.value.auth_kind)
));
const probeKeyMissing = computed(() => (
  !isEdit.value
  && props.context !== "account"
  && dynamicAuthRequiresKey(draft.value.auth_kind)
  && !draft.value.key.trim()
));
// Update bodies only carry a Key when a none-auth Provider gains keyed auth;
// otherwise stored Keys belong to Accounts and this field only feeds
// discovery/test, so the save-time hint would be misleading.
const keySavedOnUpdate = computed(() => (
  isEdit.value && props.provider?.auth_kind === "none" && dynamicAuthRequiresKey(draft.value.auth_kind)
));
const keyIsTemporary = computed(() => isEdit.value && !keySavedOnUpdate.value);
const keyPlaceholder = computed(() => {
  if (!isEdit.value) return "sk-...";
  return keySavedOnUpdate.value ? t("Key 只会在保存或测试时发送，不会重新显示。") : t("已设置");
});
const endpointPlaceholder = computed(() => {
  const preset = selectedPreset.value;
  if (preset && !preset.endpointUrl) {
    const placeholder = providerPresetEndpointPlaceholder(preset);
    if (placeholder) return placeholder;
  }
  return t("推荐填写不带 /v1 的 API 根地址；OCG 会自动补全 /v1 和协议路径。已带 /v1 时不会重复添加。");
});
const protocolOptions = computed(() => DYNAMIC_PROTOCOLS.map((value) => ({
  value,
  label: protocolDisplayName(value),
})));
const routeModeOptions = computed(() => [
  { value: "inherit", label: t("跟随供应商默认") },
  { value: "override", label: t("覆盖协议与地址") },
]);

function setMappingRouteMode(row: ProviderDefinitionMapping, mode: string): void {
  if (busy.value) return;
  if (mode === "override") {
    if (row.upstream_override) return;
    row.upstream_override = {
      protocol: draft.value.upstream_protocol || "chat_completions",
      endpoint_url: "",
    };
    return;
  }
  row.upstream_override = null;
}
const authOptions = computed(() => DYNAMIC_AUTH_KINDS.map((value) => ({
  value,
  label: value === "none" ? t("无鉴权") : value === "bearer" ? "Bearer" : "x-api-key",
})));

watch(
  () => [props.show, props.provider, props.initialPresetId] as const,
  ([visible, provider]) => {
    // Any close/reopen invalidates in-flight discovery/test responses.
    requestGeneration.value += 1;
    testTargetIndex.value = 0;
    settingsOpen.value = false;
    if (!visible) return;
    formError.value = "";
    conflictNotice.value = "";
    testSuccess.value = "";
    discoveryError.value = "";
    discoveryInfo.value = "";
    discoveredModels.value = [];
    selectedDiscovery.value = [];
    selectedPresetId.value = MANUAL_PRESET_ID;
    if (provider) {
      draft.value = {
        name: provider.name,
        endpoint_url: provider.endpoint_url ?? "",
        upstream_protocol: provider.upstream_protocol ?? "",
        auth_kind: provider.auth_kind ?? "",
        // Edit roundtrip preserves each row's override exactly; absent/null
        // means the row inherits the supplier default.
        models: provider.models.map((model) => ({
          public_model: model.public_model,
          upstream_model: model.upstream_model,
          upstream_override: model.upstream_override ? { ...model.upstream_override } : null,
        })),
        account_name: "",
        notes: "",
        key: "",
        // Persisted provenance drives edit-mode hints; undefined omits the
        // field on PATCH so a legacy manual row is preserved, not cleared.
        preset_id: provider.preset_id ?? undefined,
      };
    } else {
      // Every create open starts from a clean draft (no Key or models carry
      // over), then the explicit preset from the chooser is applied on top.
      draft.value = emptyProviderDefinitionDraft();
      const preset = props.initialPresetId
        ? PROVIDER_PRESETS.find((entry) => entry.id === props.initialPresetId) ?? null
        : null;
      if (preset) {
        selectedPresetId.value = preset.id;
        draft.value = applyProviderPresetToDraft(draft.value, preset);
      }
    }
  },
  { immediate: true },
);

// A test result describes one exact draft context; any edit to the supplier
// connection, Key, or the selected mapping (including its override) is stale.
watch(
  () => [
    draft.value.endpoint_url,
    draft.value.upstream_protocol,
    draft.value.auth_kind,
    draft.value.key,
    testTarget.value?.public_model,
    testTarget.value?.upstream_model,
    testTarget.value?.upstream_override?.protocol,
    testTarget.value?.upstream_override?.endpoint_url,
  ] as const,
  () => {
    testSuccess.value = "";
  },
);

function onPresetChange(value: string): void {
  if (isEdit.value || busy.value || value === selectedPresetId.value) return;
  selectedPresetId.value = value;
  requestGeneration.value += 1;
  testTargetIndex.value = 0;
  settingsOpen.value = false;
  const preset = PROVIDER_PRESETS.find((entry) => entry.id === value) ?? null;
  draft.value = applyProviderPresetToDraft(draft.value, preset);
  formError.value = "";
  conflictNotice.value = "";
  testSuccess.value = "";
  discoveryError.value = "";
  discoveryInfo.value = "";
  discoveredModels.value = [];
  selectedDiscovery.value = [];
}

function addMapping(): void {
  if (busy.value) return;
  draft.value.models.push({ public_model: "", upstream_model: "", upstream_override: null });
}

function removeMapping(index: number): void {
  if (busy.value || draft.value.models.length < 2) return;
  draft.value.models.splice(index, 1);
}

function importDiscovered(): void {
  if (busy.value) return;
  const presetId = importPresetId.value;
  const existing = new Set(draft.value.models.map((row) => row.public_model.trim().toLocaleLowerCase()));
  for (const model of selectedDiscovery.value) {
    const publicName = providerPresetImportPublicName(presetId, model);
    if (existing.has(publicName.toLocaleLowerCase())) continue;
    if (draft.value.models.length === 1 && !draft.value.models[0]?.public_model && !draft.value.models[0]?.upstream_model) {
      draft.value.models[0] = { public_model: publicName, upstream_model: model, upstream_override: null };
    } else {
      draft.value.models.push({ public_model: publicName, upstream_model: model, upstream_override: null });
    }
    existing.add(publicName.toLocaleLowerCase());
  }
}

async function discover(): Promise<void> {
  if (busy.value || discoveryUnavailable.value || probeKeyMissing.value) return;
  discoveryError.value = "";
  discoveryInfo.value = "";
  discovering.value = true;
  const generation = requestGeneration.value;
  try {
    const result = await providerApi.discoverProviderDefinitionModels({
      endpoint_url: draft.value.endpoint_url,
      upstream_protocol: draft.value.upstream_protocol as DynamicUpstreamProtocol,
      auth_kind: draft.value.auth_kind as DynamicAuthKind,
      key: draft.value.key || undefined,
    });
    if (generation !== requestGeneration.value) return;
    discoveredModels.value = result.models;
    discoveryInfo.value = result.models.length === 0
      ? t("未获取到模型，请手动添加模型 ID")
      : result.truncated
        ? t("已获取 {count} 个模型（结果已截断）", { count: result.models.length })
        : t("已获取 {count} 个模型", { count: result.models.length });
  } catch (error) {
    if (generation !== requestGeneration.value) return;
    discoveryError.value = dashboardErrorDetail(error);
  } finally {
    discovering.value = false;
  }
}

async function runTest(): Promise<void> {
  if (busy.value || probeKeyMissing.value) return;
  const mapping = testTarget.value;
  if (!mapping) {
    formError.value = t("请至少添加一个完整模型映射");
    return;
  }
  // Validate the selected mapping's route before any request: a present but
  // unfinished override is an error, never a silent supplier-default test.
  const overrideError = dynamicMappingOverrideError(mapping);
  if (overrideError) {
    formError.value = t(DYNAMIC_PROVIDER_DRAFT_ERROR_KEYS[overrideError] as MessageKey);
    return;
  }
  testing.value = true;
  formError.value = "";
  testSuccess.value = "";
  const generation = requestGeneration.value;
  // Any explicit per-model override wins by presence; otherwise the supplier
  // default applies.
  const route = resolveDynamicMappingRoute(draft.value, mapping);
  const usesOverride = Boolean(mapping.upstream_override);
  try {
    const result = await providerApi.testProviderDefinition({
      endpoint_url: route.endpoint_url,
      upstream_protocol: route.upstream_protocol as DynamicUpstreamProtocol,
      auth_kind: draft.value.auth_kind as DynamicAuthKind,
      public_model: mapping.public_model,
      upstream_model: mapping.upstream_model,
      key: draft.value.key || undefined,
    });
    if (generation !== requestGeneration.value) return;
    if (result.ok) {
      testSuccess.value = t("测试成功：{model} · {protocol}（{source}）", {
        model: mapping.public_model,
        protocol: protocolDisplayName(route.upstream_protocol as DynamicUpstreamProtocol),
        source: usesOverride ? t("模型覆盖") : t("供应商默认"),
      });
    } else {
      formError.value = t("测试失败: {error}", { error: result.error || "" });
    }
  } catch (error) {
    if (generation !== requestGeneration.value) return;
    formError.value = t("测试失败: {error}", { error: dashboardErrorDetail(error) });
  } finally {
    testing.value = false;
  }
}

function onSurfaceUpdateShow(visible: boolean): void {
  // The standalone modal applies the same busy dismissal guard embedded
  // hosts enforce: in-flight save/test/discovery keeps the form open.
  if (!visible && busy.value) return;
  emit("update:show", visible);
}

async function save(): Promise<void> {
  // Re-entrant submits (Enter key, double click) must not duplicate the write.
  if (busy.value) return;
  const error = validateProviderDefinitionDraft(draft.value, {
    mode: isEdit.value ? "edit" : "create",
    previousAuthKind: props.provider?.auth_kind ?? "",
    requireKey: !isEdit.value && props.context === "account",
  });
  if (error) {
    formError.value = t(DYNAMIC_PROVIDER_DRAFT_ERROR_KEYS[error] as MessageKey);
    return;
  }
  saving.value = true;
  formError.value = "";
  conflictNotice.value = "";
  try {
    const saved = isEdit.value && props.provider
      ? await providerApi.updateProviderDefinition(
        props.provider.id,
        buildProviderDefinitionUpdateBody(draft.value, props.provider.auth_kind ?? ""),
      )
      : await providerApi.createProviderDefinition(buildProviderDefinitionCreateBody(draft.value));
    draft.value = sanitizeProviderDefinitionDraft(draft.value);
    emit("saved", saved.id);
    emit("update:show", false);
  } catch (cause) {
    if (isRevisionConflict(cause)) {
      conflictNotice.value = t("数据已更新，请检查后重新保存。不会自动重试。");
      emit("conflict");
    } else {
      formError.value = dashboardErrorDetail(cause);
    }
  } finally {
    saving.value = false;
  }
}
</script>

<style scoped>
.modal-grid {
  display: grid;
  grid-template-columns: repeat(2, minmax(0, 1fr));
  gap: 12px;
}
.form-error { margin-bottom: 12px; }
.full-width-field { grid-column: 1 / -1; }
.field-hint {
  margin: 6px 0 0;
  color: var(--ocg-muted);
  font-size: var(--ocg-font-xs);
}
.capability-rows, .mapping-row { display: grid; gap: 8px; }
.preset-picker { display: grid; gap: 8px; width: 100%; }
.preset-details {
  display: flex;
  align-items: baseline;
  gap: 12px;
  flex-wrap: wrap;
}
.preset-links { display: flex; gap: 12px; }
.capability-actions, .modal-footer, .discovery-import {
  display: flex;
  align-items: center;
  gap: 8px;
  justify-content: space-between;
}
.mapping-row-main {
  display: grid;
  grid-template-columns: minmax(0, 1fr) minmax(0, 1fr) auto;
  gap: 8px;
}
.mapping-row-route {
  display: grid;
  grid-template-columns: repeat(auto-fit, minmax(160px, 1fr));
  gap: 8px;
}
.discovery-import { grid-column: 1 / -1; }
.test-section { display: grid; gap: 8px; width: 100%; }
.fixed-models-summary {
  grid-column: 1 / -1;
  margin: 0;
  color: var(--ocg-muted);
  font-size: var(--ocg-font-xs);
}
.connection-summary {
  display: grid;
  grid-template-columns: repeat(auto-fit, minmax(160px, 1fr));
  gap: 8px 16px;
  margin: 0;
  padding: 10px 12px;
  border: 1px solid var(--ocg-border);
  border-radius: 10px;
  background: var(--ocg-canvas);
}
.connection-summary__row {
  display: grid;
  gap: 2px;
  min-width: 0;
}
.connection-summary dt {
  color: var(--ocg-muted);
  font-size: var(--ocg-font-xs);
}
.connection-summary dd {
  margin: 0;
  color: var(--ocg-ink);
  font-size: var(--ocg-font-sm);
  overflow-wrap: anywhere;
}
.connection-summary__pending {
  color: var(--ocg-muted);
}
.fixed-settings-toggle { grid-column: 1 / -1; }
</style>
