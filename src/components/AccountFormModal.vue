<template>
  <FormSurface
    :show="show"
    :title="title"
    :embedded="embedded"
    modal-class="account-modal"
    @update:show="$emit('update:show', $event)"
  >
    <div ref="formElement">
    <n-form
      ref="formRef"
      :model="form"
      :rules="rules"
      label-placement="top"
    >
      <n-alert v-if="formError" type="error" class="form-error" role="alert">
        {{ formError }}
      </n-alert>
      <n-alert v-if="externalError" type="error" class="form-error" role="alert">
        {{ externalError }}
      </n-alert>
      <p v-if="!isEdit" class="field-hint">
        {{ t("创建后默认启用，可立即参与路由。测试连接不会改变开关。") }}
      </p>
      <p v-if="platformParent && !isDynamicPlan" class="connection-summary__note form-error">
        {{ t("Endpoint 与协议路径由平台账号 {name} 托管；账号只保存名称、Key 与模型映射。", { name: platformParent.name }) }}
      </p>
      <dl
        v-if="isDynamicPlan"
        class="connection-summary form-error"
        :aria-label="t('连接信息')"
      >
        <template v-if="dynamicDetail">
          <div class="connection-summary__row">
            <dt>{{ t("目标供应商") }}</dt>
            <dd>{{ dynamicDetail.name }}</dd>
          </div>
          <div class="connection-summary__row">
            <dt>{{ t("API 地址") }}</dt>
            <dd><code>{{ dynamicDetail.endpoint_url }}</code></dd>
          </div>
          <div class="connection-summary__row">
            <dt>{{ t("上游协议") }}</dt>
            <dd>{{ dynamicDetail.upstream_protocol ? protocolDisplayName(dynamicDetail.upstream_protocol) : t("供应商预设") }}</dd>
          </div>
          <div class="connection-summary__row">
            <dt>{{ t("模型映射") }}</dt>
            <dd>{{ t("{count} 个", { count: dynamicDetail.models.length }) }}</dd>
          </div>
          <p class="connection-summary__note">
            {{ t("账号只保存名称与 Key；连接始终使用以上供应商配置。") }}
          </p>
        </template>
        <p v-else class="connection-summary__note">
          {{ dynamicDetailLoading
            ? t("正在加载连接信息…")
            : t("连接信息由该供应商统一管理；账号只保存名称与 Key。") }}
        </p>
      </dl>
      <div class="modal-grid">
        <n-form-item path="name" :label="t('名称')">
          <n-input
            :value="form.name"
            :input-props="{ 'aria-label': t('名称') }"
            :placeholder="t('例如：主号')"
            @update:value="handleNameUpdate"
          />
        </n-form-item>

        <n-form-item
          v-if="hasField('username')"
          path="username"
          :label="t('账号')"
        >
          <n-input
            :value="form.username"
            :input-props="{ 'aria-label': t('登录账号') }"
            :placeholder="t('OpenCode-Go 账号')"
            @update:value="form.username = $event"
          />
        </n-form-item>

        <n-form-item
          v-if="hasField('purchase_date')"
          path="purchaseDate"
          :label="t('购买日期')"
        >
          <div class="purchase-date-control">
            <n-date-picker
              v-model:value="form.purchaseDate"
              type="date"
              format="yyyy-MM-dd"
              :clearable="!purchaseDateRequired"
              :is-date-disabled="isPurchaseDateDisabled"
              :input-props="{ 'aria-label': t('购买日期') }"
            />
            <n-button
              v-if="isEdit"
              secondary
              :disabled="isPurchaseDateToday"
              @click="setPurchaseDateToday"
            >
              {{ t("今日") }}
            </n-button>
          </div>
        </n-form-item>

        <n-form-item
          v-if="hasField('key')"
          path="key"
          :label="t('API Key')"
          class="full-width-field"
        >
          <n-input
            v-model:value="form.key"
            :input-props="{ 'aria-label': t('API Key') }"
            type="password"
            show-password-on="click"
            :placeholder="keyPlaceholder"
          />
        </n-form-item>

        <n-form-item
          v-if="hasField('ollama_billing_tier')"
          path="ollamaBillingTier"
          :label="t('计费档位')"
        >
          <div class="billing-field">
            <n-select
              v-model:value="form.ollamaBillingTier"
              :options="ollamaBillingOptions"
              :placeholder="t('选择计费档位')"
              :aria-label="t('计费档位')"
            />
            <p class="field-hint">{{ t("新建须选择 Pro / Max / Team 并填写购买日期；未配置的既有账号仍可路由。") }}</p>
          </div>
        </n-form-item>

        <n-form-item
          v-if="isCustomPlan"
          path="endpointUrl"
          :label="t('API 地址')"
          class="full-width-field"
        >
          <div class="endpoint-field">
            <n-input
              v-model:value="form.endpointUrl"
              :disabled="endpointLocked || !!platformParent"
              :input-props="{ 'aria-label': t('API 地址') }"
              :placeholder="endpointPlaceholder"
            />
            <p v-if="platformParent" class="field-hint">
              {{ t("Endpoint 由平台账号 {name} 托管，随上游协议自动推导。", { name: platformParent.name }) }}
            </p>
            <p v-else-if="endpointLocked" class="field-hint">{{ endpointLockHint }}</p>
            <p v-else class="field-hint">
              {{ t("推荐填写不带 /v1 的 API 根地址；OCG 会自动补全 /v1 和协议路径。已带 /v1 时不会重复添加。") }}
            </p>
          </div>
        </n-form-item>

        <n-form-item
          v-if="isCustomPlan"
          path="upstreamProtocol"
          :label="t('上游协议')"
        >
          <div class="protocol-field">
            <n-select
              v-model:value="form.upstreamProtocol"
              :options="upstreamProtocolOptions"
              :placeholder="t('上游协议')"
              :aria-label="t('上游协议')"
            />
            <p class="field-hint">{{ platformParent
              ? t("协议仅用于从平台账号推导 Endpoint，对账号下全部模型统一生效。")
              : t("所选协议对该账号下全部模型统一生效。") }}</p>
          </div>
        </n-form-item>

        <n-form-item
          v-if="isCustomPlan"
          path="modelCapabilities"
          :label="t('模型映射')"
          class="full-width-field"
        >
          <div class="capability-rows">
            <div class="capability-actions">
              <n-button
                size="small"
                secondary
                :loading="discoveringModels"
                :disabled="!canDiscoverModels"
                @click="discoverModels"
              >
                {{ t("获取模型") }}
              </n-button>
              <n-button size="small" secondary @click="addModelMapping">
                {{ t("添加映射") }}
              </n-button>
              <span v-if="discoverySuccess" class="field-hint">{{ discoverySuccess }}</span>
            </div>
            <p v-if="showManualModelHint" class="field-hint">
              {{ t("非标准完整 Endpoint 无法自动推导 /models；请手动添加模型映射。") }}
            </p>
            <n-alert v-if="discoveryError" type="error" :show-icon="false">
              {{ discoveryError }}
            </n-alert>
            <div v-if="discoveryOptions.length > 0" class="discovery-import">
              <n-select
                v-model:value="selectedDiscoveredModels"
                :options="discoveryOptions"
                multiple
                filterable
                :placeholder="t('选择要导入的模型')"
                :aria-label="t('选择要导入的模型')"
                max-tag-count="responsive"
              />
              <n-button
                size="small"
                secondary
                :disabled="selectedDiscoveredModels.length === 0"
                @click="importSelectedModels"
              >{{ t("导入所选") }}</n-button>
            </div>
            <div class="mapping-rows" role="list" :aria-label="t('模型映射')">
              <div
                v-for="(mapping, index) in form.modelCapabilities"
                :key="mapping.row_id"
                class="mapping-row"
                role="listitem"
              >
                <n-input
                  v-model:value="mapping.public_model"
                  :placeholder="t('对外模型名')"
                  :aria-label="t('对外模型名')"
                />
                <n-input
                  v-model:value="mapping.upstream_model"
                  :placeholder="t('上游模型 ID')"
                  :aria-label="t('上游模型 ID')"
                  class="mono"
                />
                <n-button
                  size="small"
                  tertiary
                  :aria-label="t('删除映射')"
                  @click="removeModelMapping(index)"
                >{{ t("删除") }}</n-button>
              </div>
            </div>
            <p class="field-hint capability-count">
              {{ t("{count} 个模型", { count: form.modelCapabilities.length }) }} ·
              {{ t("对外模型名不区分大小写且必须唯一；上游模型 ID 可复用。") }}
            </p>
          </div>
        </n-form-item>

        <n-form-item
          v-if="hasField('notes')"
          path="notes"
          :label="t('备注')"
          class="full-width-field"
        >
          <n-input
            v-model:value="form.notes"
            type="textarea"
            :autosize="{ minRows: 4, maxRows: 10 }"
            :maxlength="4000"
            show-count
            :placeholder="t('可填写任意备注')"
            :input-props="{ 'aria-label': t('备注') }"
          />
        </n-form-item>
      </div>
    </n-form>
    </div>
    <template #footer>
      <div class="modal-footer" :class="{ 'modal-footer--embedded': embedded }">
        <n-button
          v-if="isEdit && isCooling"
          text
          size="small"
          type="warning"
          @click="$emit('resetCooldown')"
        >
          {{ t("重置冷却") }}
        </n-button>
        <n-space>
          <n-button v-if="!embedded" @click="$emit('update:show', false)">{{ t("取消") }}</n-button>
          <n-button type="primary" :loading="busy" @click="handleSave">{{ t("保存") }}</n-button>
        </n-space>
      </div>
    </template>
  </FormSurface>
</template>

<script setup lang="ts">
import { computed, nextTick, ref, watch } from "vue";
import type { FormInst, FormRules } from "naive-ui";
import {
  NAlert,
  NButton,
  NDatePicker,
  NForm,
  NFormItem,
  NInput,
  NSelect,
  NSpace,
} from "naive-ui";
import { dashboardApi, type Account, type AccountInput, type AccountProtocol } from "../api/dashboard";
import { providerApi, type ProviderDefinitionView } from "../api/providers.ts";
import type { ProviderCatalogEntry, ProviderCatalogFormField } from "../api/providers.ts";
import { t } from "../i18n/index.ts";
import { localDateString } from "../domain/account-lifecycle.ts";
import { findCatalogEntry, planFamilyLabel, planForAccount } from "../domain/plans.ts";
import type { PlanDefinition } from "../domain/plans.ts";
import { platformInferenceEndpoint } from "../domain/platform-accounts.ts";
import { resolveAccountFormFields } from "../domain/account-form-fields.ts";
import {
  accountCreatePayloadErrorKey,
  buildCreateAccountPayload,
  type AccountCreateCapability,
  type AccountCreateFormValues,
} from "../domain/account-create-payload.ts";
import {
  CUSTOM_ENDPOINT_URL_ISSUE_KEYS,
  CUSTOM_PROTOCOLS,
  customEndpointUrlIssue,
  customApiUrlPlaceholder,
  customApiUrlNeedsManualModels,
  customApiUrlSupportsModelDiscovery,
} from "../domain/custom-account.ts";
import { protocolDisplayName } from "../domain/provider-contracts.ts";
import FormSurface from "./FormSurface.vue";

export type AccountFormPayload = {
  name: string;
  username: string;
  key?: string;
  provider_id?: string;
  purchase_date?: string;
  notes: string;
  /** Custom API edit only; persisted via the dedicated custom-config route. */
  endpoint_url?: string;
  /** Custom API edit only; persisted via the dedicated custom-config route. */
  upstream_protocol?: AccountProtocol;
  /** Custom API edit only; atomically persisted with the dedicated custom-config route. */
  model_capabilities?: Array<{
    public_model: string;
    upstream_model: string;
    protocol: AccountProtocol;
  }>;
  ollama_billing_tier?: "pro" | "max" | "team";
};

type FormModel = {
  name: string;
  username: string;
  key: string;
  purchaseDate: number | null;
  notes: string;
  endpointUrl: string;
  upstreamProtocol: AccountProtocol | null;
  modelCapabilities: EditableModelCapability[];
  ollamaBillingTier: "pro" | "max" | "team" | null;
};

type EditableModelCapability = AccountCreateCapability & { row_id: number };

type ModelDiscoveryContext = {
  show: boolean;
  accountId: string;
  endpointUrl: string;
  upstreamProtocol: FormModel["upstreamProtocol"];
  key: string;
};

const props = withDefaults(defineProps<{
  show: boolean;
  account: Account | null;
  isCooling?: boolean;
  busy?: boolean;
  /** The selected plan family when creating an account. */
  plan?: PlanDefinition | null;
  /** Provider catalog; when null, only the legacy OpenCode Go path is supported. */
  catalog?: readonly ProviderCatalogEntry[] | null;
  /** Linked platform Key: the endpoint is parent-owned and read-only here. */
  endpointLocked?: boolean;
  /** Concise parent-owned hint shown in place of the endpoint guidance. */
  endpointLockHint?: string;
  /**
   * Platform "Add Key" create context: the endpoint is prefilled from the
   * parent-owned derivation and recalculates when the single editable
   * protocol changes; the field itself stays read-only.
   */
  platformParent?: { name: string; baseUrl: string } | null;
  /** Create-flow title override (e.g. the platform Add Key flow). */
  titleOverride?: string;
  /** Host-owned error (e.g. a failed platform create) shown above the form. */
  externalError?: string;
  /** Inline rendering inside the Add Account chooser instead of a modal. */
  embedded?: boolean;
}>(), {
  account: null,
  isCooling: false,
  busy: false,
  plan: null,
  catalog: null,
  endpointLocked: false,
  endpointLockHint: "",
  platformParent: null,
  titleOverride: "",
  externalError: "",
  embedded: false,
});

const emit = defineEmits<{
  (e: "update:show", value: boolean): void;
  (e: "save", payload: AccountInput | AccountFormPayload): void;
  (e: "resetCooldown"): void;
}>();

const formRef = ref<FormInst | null>(null);
const formElement = ref<HTMLElement | null>(null);
const form = ref<FormModel>(blankForm());
const nameWasEdited = ref(false);
const formError = ref("");
const discoveringModels = ref(false);
const discoveryError = ref("");
const discoverySuccess = ref("");
const discoveredModels = ref<string[]>([]);
const selectedDiscoveredModels = ref<string[]>([]);
let discoveryGeneration = 0;
let nextModelMappingRowId = 1;

const isEdit = computed(() => !!props.account);
const title = computed(() => {
  if (props.titleOverride) return props.titleOverride;
  if (isEdit.value) return t("编辑账号");
  const plan = effectivePlan.value;
  return plan
    ? t("添加 {plan} 账号", { plan: planFamilyLabel(plan, props.catalog) })
    : t("导入已有 Key");
});

const effectivePlan = computed<PlanDefinition | null>(() => {
  if (isEdit.value) {
    const account = props.account!;
    return planForAccount(account, props.catalog);
  }
  return props.plan;
});

const isCustomPlan = computed(() => effectivePlan.value?.kind === "custom");
const isOllamaPlan = computed(() => effectivePlan.value?.provider_id === "ollama");
const ollamaBillingOptions = [
  { value: "pro", label: "Pro · $60" },
  { value: "max", label: "Max · $300" },
  { value: "team", label: "Team · $1000" },
];
const ollamaPaidTier = computed(() => form.value.ollamaBillingTier !== null);
const isDynamicPlan = computed(() => effectivePlan.value?.dynamic === true);

const catalogEntry = computed<ProviderCatalogEntry | undefined>(() => {
  const plan = effectivePlan.value;
  if (!plan) return undefined;
  return findCatalogEntry(props.catalog, plan.provider_id);
});

const formFields = computed<ProviderCatalogFormField[]>(() => {
  return resolveAccountFormFields(effectivePlan.value, catalogEntry.value);
});

const fieldMap = computed(() => new Map(formFields.value.map((field) => [field.id, field])));

function hasField(id: string): boolean {
  return fieldMap.value.has(id);
}

function fieldRequired(id: string): boolean {
  return fieldMap.value.get(id)?.required ?? false;
}

const keyPlaceholder = computed(() => {
  const prefix = catalogEntry.value?.key_prefix;
  if (prefix) return prefix + "...";
  return "sk-...";
});

const purchaseDateRequired = computed(() => (
  fieldRequired("purchase_date") || (isOllamaPlan.value && ollamaPaidTier.value)
));
const isPurchaseDateToday = computed(() => (
  form.value.purchaseDate !== null
  && localDateString(form.value.purchaseDate) === localDateString()
));

const upstreamProtocolOptions = computed(() => {
  return CUSTOM_PROTOCOLS.map((value) => ({
    value,
    label: protocolDisplayName(value),
  }));
});

const endpointPlaceholder = computed(() => customApiUrlPlaceholder());
const canInferModelEndpoint = computed(() => isCustomPlan.value
  && customApiUrlSupportsModelDiscovery(form.value.endpointUrl, form.value.upstreamProtocol));
const showManualModelHint = computed(() => isCustomPlan.value
  && customApiUrlNeedsManualModels(form.value.endpointUrl, form.value.upstreamProtocol));
const canDiscoverModels = computed(() => canInferModelEndpoint.value
  && (!!form.value.key.trim() || !!props.account?.id));
const discoveryOptions = computed(() => {
  const existing = new Set(
    form.value.modelCapabilities.map((capability) => capability.public_model.trim().toLocaleLowerCase()),
  );
  return discoveredModels.value
    .filter((model) => !existing.has(model.toLocaleLowerCase()))
    .map((model) => ({ value: model, label: model }));
});

const rules = computed<FormRules>(() => {
  const base: FormRules = {
    name: {
      required: true,
      whitespace: true,
      message: t("名称不能为空"),
      trigger: ["input", "blur"],
    },
  };

  if (purchaseDateRequired.value) {
    base.purchaseDate = [
      {
        required: true,
        type: "number",
        message: t("请选择购买日期"),
        trigger: ["change", "blur"],
      },
      {
        validator: (_rule: unknown, value: number | null) => {
          if (value === null) return true;
          return localDateString(value) <= localDateString();
        },
        message: t("购买日期不能晚于今天"),
        trigger: ["change", "blur"],
      },
    ];
  }

  if (hasField("ollama_billing_tier")) {
    base.ollamaBillingTier = {
      required: true,
      message: t("选择计费档位"),
      trigger: ["change", "blur"],
    };
  }

  if (hasField("key") && !isEdit.value) {
    base.key = {
      required: true,
      whitespace: true,
      message: t("请填写 API Key"),
      trigger: ["input", "blur"],
    };
  }

  if (isCustomPlan.value) {
    base.endpointUrl = {
      required: true,
      validator: (_rule: unknown, value: string) => {
        const issue = customEndpointUrlIssue(value ?? "");
        return issue ? new Error(t(CUSTOM_ENDPOINT_URL_ISSUE_KEYS[issue])) : true;
      },
      trigger: ["input", "blur"],
    };
    base.upstreamProtocol = {
      required: true,
      type: "string",
      validator: (_rule: unknown, value: AccountProtocol | null) => !!value,
      message: t("请选择上游协议"),
      trigger: ["change", "blur"],
    };
    base.modelCapabilities = {
      required: true,
      type: "array",
      validator: (_rule: unknown, value: AccountCreateCapability[]) =>
        Array.isArray(value) && value.length > 0 && value.every((cap) => (
          cap.public_model.trim() && cap.upstream_model.trim()
        )),
      message: t("请至少添加一个完整模型映射"),
      trigger: ["change"],
    };
  }

  return base;
});

// Identity keys only: an account refresh with the same id (CAS reconciliation)
// must not wipe the user's in-progress edits, while a chooser selection change
// to another plan resets the create form even though `show` stays true.
const watchedAccountId = computed(() => props.account?.id ?? "");
const watchedPlanKey = computed(() => (
  props.plan ? `${props.plan.id}:${props.plan.provider_id}` : ""
));

watch(() => [props.show, watchedAccountId.value, watchedPlanKey.value], ([show]) => {
  if (show) {
    form.value = props.account ? formFromAccount(props.account) : blankForm();
    // Platform Add Key prefill: the parent-owned derivation seeds the
    // read-only endpoint for the default protocol.
    if (!props.account && props.platformParent) {
      form.value.endpointUrl = platformInferenceEndpoint(
        props.platformParent.baseUrl,
        form.value.upstreamProtocol ?? "chat_completions",
      ) ?? "";
    }
    nameWasEdited.value = isEdit.value;
    formRef.value?.restoreValidation();
    formError.value = "";
    discoveryError.value = "";
    discoverySuccess.value = "";
    discoveredModels.value = [];
    selectedDiscoveredModels.value = [];
  }
});

// The single editable protocol recalculates the parent-owned endpoint.
watch(() => form.value.upstreamProtocol, (protocol) => {
  if (!props.platformParent || !protocol) return;
  form.value.endpointUrl = platformInferenceEndpoint(props.platformParent.baseUrl, protocol) ?? "";
});

/**
 * Read-only connection summary for accounts of a saved user-defined Provider:
 * the exact endpoint/protocol and model count load from the existing details
 * API — never inferred from the display name, and never carrying the Key.
 * The generation guard keeps a slow or stale load from overwriting a newer
 * selection; the draft itself is untouched either way.
 */
const dynamicDetail = ref<ProviderDefinitionView | null>(null);
const dynamicDetailLoading = ref(false);
let dynamicDetailGeneration = 0;
watch(
  () => [props.show, isDynamicPlan.value, effectivePlan.value?.provider_id ?? ""] as const,
  ([visible, dynamic, providerId]) => {
    const generation = ++dynamicDetailGeneration;
    dynamicDetail.value = null;
    dynamicDetailLoading.value = false;
    if (!visible || !dynamic || !providerId) return;
    dynamicDetailLoading.value = true;
    providerApi.getProviderDefinition(providerId)
      .then((detail) => {
        if (generation === dynamicDetailGeneration) dynamicDetail.value = detail;
      })
      .catch(() => {
        // Neutral fallback copy stays; the form itself is fully usable.
      })
      .finally(() => {
        if (generation === dynamicDetailGeneration) dynamicDetailLoading.value = false;
      });
  },
  { immediate: true },
);

function currentModelDiscoveryContext(): ModelDiscoveryContext {
  return {
    show: props.show,
    accountId: props.account?.id ?? "",
    endpointUrl: form.value.endpointUrl,
    upstreamProtocol: form.value.upstreamProtocol,
    key: form.value.key,
  };
}

function modelDiscoveryContextMatches(expected: ModelDiscoveryContext): boolean {
  const current = currentModelDiscoveryContext();
  return current.show === expected.show
    && current.accountId === expected.accountId
    && current.endpointUrl === expected.endpointUrl
    && current.upstreamProtocol === expected.upstreamProtocol
    && current.key === expected.key;
}

watch(
  () => currentModelDiscoveryContext(),
  () => {
    discoveryGeneration += 1;
    discoveringModels.value = false;
    discoveryError.value = "";
    discoverySuccess.value = "";
    discoveredModels.value = [];
    selectedDiscoveredModels.value = [];
  },
  { flush: "sync" },
);

function timestampFromLocalDate(value: string): number | null {
  const parts = /^(\d{4})-(\d{2})-(\d{2})$/.exec(value);
  if (!parts) return null;
  const year = Number(parts[1]);
  const month = Number(parts[2]);
  const day = Number(parts[3]);
  const date = new Date(year, month - 1, day);
  return date.getFullYear() === year && date.getMonth() === month - 1 && date.getDate() === day
    ? date.getTime()
    : null;
}

function blankForm(): FormModel {
  return {
    name: "",
    username: "",
    key: "",
    purchaseDate: timestampFromLocalDate(localDateString()) ?? Date.now(),
    notes: "",
    endpointUrl: "",
    upstreamProtocol: "chat_completions",
    modelCapabilities: [],
    ollamaBillingTier: null,
  };
}

function formFromAccount(account: Account): FormModel {
  const modelCapabilities: EditableModelCapability[] = [];
  for (const capability of account.model_capabilities) {
    modelCapabilities.push(modelMapping({
      public_model: capability.public_model,
      upstream_model: capability.upstream_model,
    }));
  }
  return {
    name: account.name,
    username: account.username,
    key: "",
    purchaseDate: timestampFromLocalDate(account.purchase_date),
    notes: account.notes ?? "",
    endpointUrl: account.custom_config?.endpoint_url ?? "",
    upstreamProtocol: account.custom_config?.upstream_protocol ?? "chat_completions",
    modelCapabilities,
    ollamaBillingTier: account.ollama_billing_tier ?? null,
  };
}

function handleNameUpdate(value: string) {
  form.value.name = value;
  if (!isEdit.value && !nameWasEdited.value) {
    form.value.name = value;
  }
}

function isPurchaseDateDisabled(timestamp: number): boolean {
  return localDateString(timestamp) > localDateString();
}

function setPurchaseDateToday() {
  form.value.purchaseDate = timestampFromLocalDate(localDateString()) ?? Date.now();
}

function addModelMapping(): void {
  form.value.modelCapabilities.push(modelMapping({ public_model: "", upstream_model: "" }));
}

function removeModelMapping(index: number): void {
  form.value.modelCapabilities.splice(index, 1);
}

function modelMapping(capability: AccountCreateCapability): EditableModelCapability {
  return { ...capability, row_id: nextModelMappingRowId++ };
}

function importSelectedModels(): void {
  const existing = new Set(
    form.value.modelCapabilities.map((capability) => capability.public_model.trim().toLocaleLowerCase()),
  );
  let imported = 0;
  for (const model of selectedDiscoveredModels.value) {
    const identity = model.toLocaleLowerCase();
    if (existing.has(identity)) continue;
    existing.add(identity);
    form.value.modelCapabilities.push(modelMapping({ public_model: model, upstream_model: model }));
    imported += 1;
  }
  selectedDiscoveredModels.value = [];
  discoverySuccess.value = t("已导入 {count} 个模型", { count: imported });
}

async function discoverModels() {
  if (!canDiscoverModels.value || !form.value.upstreamProtocol) return;
  const context = currentModelDiscoveryContext();
  const generation = ++discoveryGeneration;
  discoveringModels.value = true;
  discoveryError.value = "";
  discoverySuccess.value = "";
  try {
    const result = await dashboardApi.discoverCustomModels({
      endpoint_url: form.value.endpointUrl.trim(),
      upstream_protocol: form.value.upstreamProtocol,
      ...(form.value.key.trim() ? { api_key: form.value.key.trim() } : {}),
      ...(props.account?.id ? { account_id: props.account.id } : {}),
    });
    if (generation !== discoveryGeneration || !modelDiscoveryContextMatches(context)) return;
    discoveredModels.value = result.models;
    selectedDiscoveredModels.value = [];
    if (result.models.length === 0) {
      discoverySuccess.value = t("未获取到模型，请手动添加模型 ID");
    } else if (result.truncated) {
      discoverySuccess.value = t("已获取 {count} 个模型（结果已截断）", { count: result.models.length });
    } else {
      discoverySuccess.value = t("已获取 {count} 个模型", { count: result.models.length });
    }
  } catch (error) {
    if (generation !== discoveryGeneration || !modelDiscoveryContextMatches(context)) return;
    discoveryError.value = error instanceof Error
      ? error.message
      : t("获取模型失败，请检查配置后重试");
  } finally {
    if (generation === discoveryGeneration && modelDiscoveryContextMatches(context)) {
      discoveringModels.value = false;
    }
  }
}

async function handleSave() {
  // The parent's mutation owns `busy`; never submit twice for one intent.
  if (props.busy) return;
  try {
    await formRef.value?.validate();
  } catch {
    await nextTick();
    formElement.value?.querySelector('.n-form-item-feedback--error')
      ?.closest('.n-form-item')
      ?.querySelector<HTMLElement>('input, textarea, [tabindex="0"]')?.focus();
    return;
  }

  if (isEdit.value) {
    const payload: AccountFormPayload = {
      name: form.value.name.trim(),
      username: form.value.username.trim(),
      notes: form.value.notes,
    };
    if (hasField("purchase_date")) {
      // Only catalog-declared subscription plans own a monthly purchase date.
      // Hidden form defaults must never reset other account types.
      payload.purchase_date = form.value.purchaseDate === null ? undefined : localDateString(form.value.purchaseDate);
    }
    if (form.value.key.trim()) {
      payload.key = form.value.key.trim();
    }
    if (isCustomPlan.value) {
      payload.endpoint_url = form.value.endpointUrl.trim();
      payload.upstream_protocol = form.value.upstreamProtocol ?? undefined;
      payload.model_capabilities = form.value.modelCapabilities.map((capability) => ({
        public_model: capability.public_model,
        upstream_model: capability.upstream_model,
        protocol: form.value.upstreamProtocol ?? "chat_completions",
      }));
    }
    if (hasField("ollama_billing_tier") && form.value.ollamaBillingTier) {
      payload.ollama_billing_tier = form.value.ollamaBillingTier;
    }
    emit("save", payload);
    return;
  }

  const plan = effectivePlan.value;
  if (!plan) {
    formError.value = t("无法确定账号方案，请关闭后重试");
    return;
  }

  const values: AccountCreateFormValues = {
    name: form.value.name,
    username: form.value.username,
    key: form.value.key,
    notes: form.value.notes,
  };
  if (hasField("purchase_date")) {
    values.purchase_date = form.value.purchaseDate === null
      ? undefined
      : localDateString(form.value.purchaseDate);
  }
  // The form model keeps a default Custom protocol so that opening the Custom
  // plan is convenient. Do not leak that hidden field into sealed built-in
  // plans: the payload builder correctly rejects Custom-only fields there.
  if (isCustomPlan.value) {
    values.endpoint_url = form.value.endpointUrl;
    values.upstream_protocol = form.value.upstreamProtocol ?? undefined;
    values.model_capabilities = form.value.modelCapabilities.length > 0
      ? form.value.modelCapabilities
      : undefined;
  }

  try {
    const payload = buildCreateAccountPayload(plan, values);
    if (hasField("ollama_billing_tier") && form.value.ollamaBillingTier) {
      payload.ollama_billing_tier = form.value.ollamaBillingTier;
    }
    emit("save", payload);
  } catch (error) {
    // Never submit a degraded payload: the backend rejects incomplete Custom
    // plans, so keep the draft editable instead.
    formError.value = t(accountCreatePayloadErrorKey(error));
  }
}
</script>

<style scoped>
.modal-grid {
  display: grid;
  grid-template-columns: repeat(2, minmax(0, 1fr));
  gap: 12px;
  align-items: start;
}

.form-error {
  margin-bottom: 12px;
}

.full-width-field,
.notes-field {
  grid-column: 1 / -1;
}

.field-hint {
  margin: 6px 0 0;
  color: var(--ocg-muted);
  font-size: var(--ocg-font-xs);
}

.connection-summary {
  display: grid;
  grid-template-columns: repeat(auto-fit, minmax(160px, 1fr));
  gap: 8px 16px;
  margin: 0 0 12px;
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

.connection-summary__note {
  grid-column: 1 / -1;
  margin: 0;
  color: var(--ocg-muted);
  font-size: var(--ocg-font-xs);
}

.capability-rows {
  display: grid;
  gap: 8px;
}

.capability-actions {
  display: flex;
  align-items: center;
  gap: 8px;
}

.discovery-import {
  display: grid;
  grid-template-columns: minmax(0, 1fr) auto;
  gap: 8px;
  align-items: center;
}

.mapping-rows {
  display: grid;
  gap: 8px;
}

.mapping-row {
  display: grid;
  grid-template-columns: minmax(0, 1fr) minmax(0, 1fr) auto;
  gap: 8px;
  align-items: center;
}

.capability-count {
  margin-top: 0;
  font-variant-numeric: tabular-nums;
}

.endpoint-field,
.protocol-field,
.billing-field {
  display: grid;
  gap: 4px;
  width: 100%;
}

.modal-grid :deep(.n-date-picker) {
  width: 100%;
}

.purchase-date-control {
  display: grid;
  grid-template-columns: minmax(0, 1fr) auto;
  gap: 8px;
  width: 100%;
}

.modal-footer {
  display: flex;
  justify-content: space-between;
  align-items: center;
  gap: 12px;
}

.modal-footer--embedded {
  justify-content: flex-end;
}

@media (max-width: 640px) {
  .modal-grid {
    grid-template-columns: 1fr;
  }

  .mapping-row {
    grid-template-columns: 1fr;
  }

  .discovery-import {
    grid-template-columns: 1fr;
  }

}
</style>
